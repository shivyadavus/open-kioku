use chrono::Utc;
use open_kioku_core::{
    AnalysisFact, ChangeBoundary, CodeChunk, Confidence, ConfidenceBreakdown,
    ConfidenceSignalInput, ContextBudget, ContextPack, ContextSelectedUnit, Evidence, EvidenceId,
    EvidenceSourceType, File, FileRange, GraphEdge, GraphEdgeType, GraphNodeType,
    HistorySignalQuery, NegativeEvidence, RetrievalAuthority, RetrievalDiagnostics,
    RetrievalSourceCount, RetrievalSourceKind, RetrievalTrace, RetrievalUnitKey, RiskReport,
    RuntimeSignal, ScoreComponent, SearchResult, Symbol, ValidationPlan,
};
use open_kioku_errors::Result;
use open_kioku_impact::ImpactEngine;
use open_kioku_ranking::{rerank_with_options, RankingOptions};
use open_kioku_search_regex::search_chunks;
use open_kioku_storage::{HistoryStore, OkStore};
use open_kioku_tests::TestSelector;

pub mod candidates;
pub mod routing;

fn is_trusted_context_dependency_edge(edge: &GraphEdge) -> bool {
    match &edge.edge_type {
        GraphEdgeType::Calls
        | GraphEdgeType::References
        | GraphEdgeType::UsesType
        | GraphEdgeType::Implements
        | GraphEdgeType::Extends
        | GraphEdgeType::Imports
        | GraphEdgeType::DependsOn => edge.is_authoritative_relationship(),
        _ => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ContextPackFormat {
    Json,
    Markdown,
    PromptText,
    Toon,
}

impl ContextPackFormat {
    pub fn render(&self, pack: &ContextPack) -> Result<String> {
        match self {
            Self::Json => Ok(serde_json::to_string_pretty(pack)?),
            Self::Toon => Ok(open_kioku_format::render_context_pack_toon(pack)),
            Self::Markdown => {
                let mut out = String::new();
                out.push_str(&format!("# Task: {}\n\n", pack.task));
                out.push_str("## Confidence\n\n");
                out.push_str(&format!(
                    "- Overall: `{:?}` (`{:.2}`)\n",
                    pack.confidence_breakdown.overall_enum, pack.confidence_breakdown.overall_score
                ));
                write_markdown_confidence_breakdown(&mut out, &pack.confidence_breakdown);
                write_markdown_retrieval_diagnostics(&mut out, &pack.retrieval_diagnostics);
                out.push('\n');
                out.push_str("## Primary Context\n\n");
                for result in &pack.primary_files {
                    out.push_str(&format!("### {}\n", result.path.display()));
                    if let Some(range) = &result.line_range {
                        out.push_str(&format!("Lines {}-{}\n", range.start, range.end));
                    }
                    out.push_str("```\n");
                    out.push_str(&result.snippet);
                    out.push_str("\n```\n\n");
                }

                out.push_str("## Supporting Impact\n\n");
                for result in &pack.supporting_files {
                    out.push_str(&format!("- {}\n", result.path.display()));
                }

                out.push_str("\n## Runtime Signals\n\n");
                if pack.runtime_signals.is_empty() {
                    out.push_str("- None found\n");
                } else {
                    for signal in &pack.runtime_signals {
                        let location = signal
                            .file_range
                            .as_ref()
                            .map(|range| {
                                let lines = range
                                    .line_range
                                    .as_ref()
                                    .map(|line_range| {
                                        format!(":{}-{}", line_range.start, line_range.end)
                                    })
                                    .unwrap_or_default();
                                format!("{}{}", range.path.display(), lines)
                            })
                            .unwrap_or_else(|| "unknown location".into());
                        out.push_str(&format!(
                            "- `{}` at `{}` ({:?})\n",
                            signal.message, location, signal.confidence
                        ));
                    }
                }

                out.push_str("\n## Validation Plan\n\n");
                for test in &pack.validation_plan.tests {
                    out.push_str(&format!("- {}\n", test.name));
                }

                Ok(out)
            }
            Self::PromptText => {
                let mut out = String::new();
                out.push_str(&format!("TASK: {}\n", pack.task));
                write_prompt_retrieval_diagnostics(&mut out, &pack.retrieval_diagnostics);
                for result in &pack.primary_files {
                    out.push_str(&format!("[FILE: {}]\n", result.path.display()));
                    if let Some(range) = &result.line_range {
                        out.push_str(&format!("SYM: lines {}-{}\n", range.start, range.end));
                    }
                    out.push_str(&result.snippet);
                    out.push_str("\n[END FILE]\n");
                }
                for result in &pack.supporting_files {
                    out.push_str(&format!("IMPACT: {}\n", result.path.display()));
                }
                for test in &pack.validation_plan.tests {
                    out.push_str(&format!("TEST: {}\n", test.name));
                }
                Ok(out)
            }
        }
    }
}

fn retrieval_source_label(source: RetrievalSourceKind) -> &'static str {
    candidates::retrieval_source_label(source)
}

fn retrieval_source_list(sources: &[RetrievalSourceKind]) -> String {
    sources
        .iter()
        .copied()
        .map(retrieval_source_label)
        .collect::<Vec<_>>()
        .join(", ")
}

fn retrieval_trace_for_result<'a>(
    diagnostics: &'a RetrievalDiagnostics,
    result: &SearchResult,
) -> Option<&'a RetrievalTrace> {
    let expected = RetrievalUnitKey::from_result(result);
    if let Some(trace) = diagnostics
        .traces
        .iter()
        .find(|trace| trace.unit_key.as_ref() == Some(&expected))
    {
        return Some(trace);
    }

    // Backward compatibility for serialized diagnostics created before unit identities existed:
    // path-only fallback is safe only when exactly one legacy trace exists for that path. If two
    // sections share a path, fail closed rather than borrowing authority from an arbitrary section.
    let path = normalize_path(&result.path);
    let mut legacy = diagnostics
        .traces
        .iter()
        .filter(|trace| trace.unit_key.is_none() && normalize_path(&trace.path) == path);
    let first = legacy.next()?;
    if legacy.next().is_none() {
        Some(first)
    } else {
        None
    }
}

fn refresh_context_pack_retrieval_telemetry(
    diagnostics: &mut RetrievalDiagnostics,
    selected: &[SearchResult],
    confidence: &ConfidenceBreakdown,
) {
    let selected_units = selected
        .iter()
        .map(RetrievalUnitKey::from_result)
        .collect::<std::collections::BTreeSet<_>>();
    let mut source_paths =
        std::collections::BTreeMap::<RetrievalSourceKind, std::collections::BTreeSet<String>>::new(
        );
    let mut exact_units = std::collections::BTreeSet::new();
    let mut traced_selected_units = std::collections::BTreeSet::new();

    for result in selected {
        let unit = RetrievalUnitKey::from_result(result);
        let Some(trace) = retrieval_trace_for_result(diagnostics, result) else {
            continue;
        };
        traced_selected_units.insert(unit.clone());
        if trace.authority == RetrievalAuthority::Exact {
            exact_units.insert(unit);
        }
        let path = normalize_path(&result.path);
        for contribution in &trace.contributions {
            source_paths
                .entry(contribution.source)
                .or_default()
                .insert(path.clone());
        }
    }

    diagnostics.selection.source_stream_mix = source_paths
        .into_iter()
        .map(|(source, paths)| RetrievalSourceCount {
            source,
            selected_file_count: paths.len(),
        })
        .collect();
    diagnostics.selection.exact_evidence_count = exact_units.len();
    diagnostics.selection.unattributed_selected_file_count =
        selected_units.difference(&traced_selected_units).count();
    if diagnostics.selection.unattributed_selected_file_count > 0 {
        let caveat = format!(
            "{} selected retrieval unit(s) lack retrieval-trace source attribution because unit identity is ambiguous or unavailable",
            diagnostics.selection.unattributed_selected_file_count
        );
        if !diagnostics.selection.caveats.contains(&caveat) {
            diagnostics.selection.caveats.push(caveat);
        }
    }
    diagnostics.selection.ambiguity_unresolved_count = diagnostics
        .caveats
        .iter()
        .filter(|caveat| {
            let caveat = caveat.to_ascii_lowercase();
            caveat.contains("ambiguous") || caveat.contains("unresolved")
        })
        .count();
    diagnostics.selection.retrieval_confidence = Some(confidence.overall_enum);
    if diagnostics.selection.abstention_reason.is_none() {
        diagnostics.selection.abstention_reason = if selected.is_empty() {
            Some(
                if diagnostics.selection.budget.max_tokens > 0
                    && diagnostics.selection.available_context_tokens == 0
                {
                    "context_budget_exhausted".into()
                } else if diagnostics.traces.is_empty() {
                    "no_task_relevant_candidates".into()
                } else {
                    "no_candidate_fit_context_selection".into()
                },
            )
        } else {
            None
        };
    }
}

fn write_markdown_retrieval_diagnostics(out: &mut String, diagnostics: &RetrievalDiagnostics) {
    if diagnostics.sources_attempted.is_empty() && diagnostics.caveats.is_empty() {
        return;
    }
    out.push_str("## Retrieval\n\n");
    out.push_str(&format!(
        "- Task family: `{:?}` (confidence `{:.2}`)\n",
        diagnostics.routing.task_family, diagnostics.routing.confidence
    ));
    for reason in &diagnostics.routing.reasons {
        out.push_str(&format!("  - Routing rationale: {reason}\n"));
    }
    if !diagnostics.sources_attempted.is_empty() {
        out.push_str(&format!(
            "- Attempted: `{}`\n",
            retrieval_source_list(&diagnostics.sources_attempted)
        ));
    }
    if !diagnostics.sources_succeeded.is_empty() {
        out.push_str(&format!(
            "- Succeeded: `{}`\n",
            retrieval_source_list(&diagnostics.sources_succeeded)
        ));
    }
    if diagnostics.selection.budget.max_tokens > 0 {
        out.push_str(&format!(
            "- Context budget: `{}` tokens (`{}` available after reserves); selected estimate `{}`\n",
            diagnostics.selection.budget.max_tokens,
            diagnostics.selection.available_context_tokens,
            diagnostics.selection.estimated_tokens_selected
        ));
        if !diagnostics.selection.source_stream_mix.is_empty() {
            let source_mix = diagnostics
                .selection
                .source_stream_mix
                .iter()
                .map(|entry| {
                    format!(
                        "{}={}",
                        retrieval_source_label(entry.source),
                        entry.selected_file_count
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("- Selected source mix: `{source_mix}`\n"));
        }
        out.push_str(&format!(
            "- Exact-evidence selections: `{}`; ambiguity/unresolved signals: `{}`\n",
            diagnostics.selection.exact_evidence_count,
            diagnostics.selection.ambiguity_unresolved_count
        ));
        if diagnostics.selection.unattributed_selected_file_count > 0 {
            out.push_str(&format!(
                "- Selected files without retrieval-trace attribution: `{}`\n",
                diagnostics.selection.unattributed_selected_file_count
            ));
        }
        if let Some(confidence) = diagnostics.selection.retrieval_confidence {
            out.push_str(&format!(
                "- Retrieval confidence: `{:?}` (qualitative ContextPack confidence, not a calibrated probability)\n",
                confidence
            ));
        }
        if let Some(reason) = &diagnostics.selection.abstention_reason {
            out.push_str(&format!("- Abstention reason: `{reason}`\n"));
        }
        if !diagnostics.selection.omitted_high_value.is_empty() {
            out.push_str("- High-value omissions:\n");
            for omission in &diagnostics.selection.omitted_high_value {
                out.push_str(&format!("  - {omission}\n"));
            }
        }
    }
    if !diagnostics.caveats.is_empty() {
        out.push_str("- Caveats:\n");
        for caveat in &diagnostics.caveats {
            out.push_str(&format!("  - {caveat}\n"));
        }
    }
    out.push('\n');
}

fn write_prompt_retrieval_diagnostics(out: &mut String, diagnostics: &RetrievalDiagnostics) {
    out.push_str(&format!(
        "TASK_FAMILY: {:?} confidence={:.2}\n",
        diagnostics.routing.task_family, diagnostics.routing.confidence
    ));
    for reason in &diagnostics.routing.reasons {
        out.push_str(&format!("TASK_ROUTING_RATIONALE: {reason}\n"));
    }
    if !diagnostics.sources_attempted.is_empty() {
        out.push_str(&format!(
            "RETRIEVAL_SOURCES_ATTEMPTED: {}\n",
            retrieval_source_list(&diagnostics.sources_attempted)
        ));
    }
    if !diagnostics.sources_succeeded.is_empty() {
        out.push_str(&format!(
            "RETRIEVAL_SOURCES_SUCCEEDED: {}\n",
            retrieval_source_list(&diagnostics.sources_succeeded)
        ));
    }
    if diagnostics.selection.budget.max_tokens > 0 {
        out.push_str(&format!(
            "CONTEXT_BUDGET: max={} available={} selected_estimate={}\n",
            diagnostics.selection.budget.max_tokens,
            diagnostics.selection.available_context_tokens,
            diagnostics.selection.estimated_tokens_selected
        ));
        if !diagnostics.selection.source_stream_mix.is_empty() {
            let source_mix = diagnostics
                .selection
                .source_stream_mix
                .iter()
                .map(|entry| {
                    format!(
                        "{}={}",
                        retrieval_source_label(entry.source),
                        entry.selected_file_count
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            out.push_str(&format!("RETRIEVAL_SELECTED_SOURCE_MIX: {source_mix}\n"));
        }
        out.push_str(&format!(
            "RETRIEVAL_EXACT_EVIDENCE_COUNT: {}\nRETRIEVAL_AMBIGUITY_UNRESOLVED_COUNT: {}\n",
            diagnostics.selection.exact_evidence_count,
            diagnostics.selection.ambiguity_unresolved_count
        ));
        if diagnostics.selection.unattributed_selected_file_count > 0 {
            out.push_str(&format!(
                "RETRIEVAL_UNATTRIBUTED_SELECTED_FILE_COUNT: {}\n",
                diagnostics.selection.unattributed_selected_file_count
            ));
        }
        if let Some(confidence) = diagnostics.selection.retrieval_confidence {
            out.push_str(&format!("RETRIEVAL_CONFIDENCE: {:?}\n", confidence));
        }
        if let Some(reason) = &diagnostics.selection.abstention_reason {
            out.push_str(&format!("RETRIEVAL_ABSTENTION_REASON: {reason}\n"));
        }
        for omission in &diagnostics.selection.omitted_high_value {
            out.push_str(&format!("CONTEXT_HIGH_VALUE_OMISSION: {omission}\n"));
        }
    }
    for caveat in &diagnostics.caveats {
        out.push_str(&format!("RETRIEVAL_CAVEAT: {caveat}\n"));
    }
}

fn write_markdown_confidence_breakdown(out: &mut String, breakdown: &ConfidenceBreakdown) {
    if !breakdown.blockers.is_empty() {
        out.push_str("- Blockers:\n");
        for blocker in &breakdown.blockers {
            out.push_str(&format!("  - {blocker}\n"));
        }
    }
    if !breakdown.caveats.is_empty() {
        out.push_str("- Caveats:\n");
        for caveat in &breakdown.caveats {
            out.push_str(&format!("  - {caveat}\n"));
        }
    }
    out.push_str("- Components:\n");
    for component in &breakdown.components {
        out.push_str(&format!(
            "  - `{}` score `{:.2}`, weight `{:.2}`, contribution `{:.2}`\n",
            component.signal, component.normalized_value, component.weight, component.contribution
        ));
    }
}

pub struct ContextPackBuilder<'a> {
    store: &'a dyn OkStore,
    history_store: Option<&'a dyn HistoryStore>,
    ranking_options: RankingOptions,
}

pub fn expanded_task_search_terms(task: &str) -> Vec<String> {
    TaskSearchIntent::parse(task).search_terms(task)
}

impl<'a> ContextPackBuilder<'a> {
    pub fn new(store: &'a dyn OkStore) -> Self {
        Self {
            store,
            history_store: None,
            ranking_options: RankingOptions::default(),
        }
    }

    pub fn with_history_store(mut self, history_store: Option<&'a dyn HistoryStore>) -> Self {
        self.history_store = history_store;
        self
    }

    pub fn with_ranking_options(mut self, ranking_options: RankingOptions) -> Self {
        self.ranking_options = ranking_options;
        self
    }

    pub fn build(&self, task: &str, limit: usize) -> Result<ContextPack> {
        self.build_with_budget_and_sources(task, ContextBudget::from_file_limit(limit), &[])
    }

    pub fn build_with_budget(&self, task: &str, budget: ContextBudget) -> Result<ContextPack> {
        self.build_with_budget_and_sources(task, budget, &[])
    }

    pub fn build_with_sources(
        &self,
        task: &str,
        limit: usize,
        external_sources: &[&dyn candidates::ContextCandidateSource],
    ) -> Result<ContextPack> {
        self.build_with_budget_and_sources(
            task,
            ContextBudget::from_file_limit(limit),
            external_sources,
        )
    }

    pub fn build_with_budget_and_sources(
        &self,
        task: &str,
        budget: ContextBudget,
        external_sources: &[&dyn candidates::ContextCandidateSource],
    ) -> Result<ContextPack> {
        let limit = budget.max_primary_files;
        let files = self.store.list_files(usize::MAX, 0)?;
        let chunks = self.store.all_chunks()?;
        let symbols = self.store.list_symbols(None, usize::MAX, 0)?;
        let intent = TaskSearchIntent::parse(task);
        let routing = routing::classify_task(task);
        let candidate_limit = routing.policy.request_limit(limit).clamp(20, 200);
        let (path_prefixes, scope_caveats) =
            validated_candidate_path_scope(&intent.path_anchors, &files);
        let request =
            candidates::CandidateRequest::new(task, intent.search_terms(task), candidate_limit)
                .with_path_prefixes(path_prefixes);
        let routed_external_sources = external_sources
            .iter()
            .copied()
            .filter(|source| routing.policy.allows(source.source()))
            .collect::<Vec<_>>();
        let external_streams =
            candidates::retrieve_candidate_streams(&routed_external_sources, &request);
        let overridden_sources = external_streams
            .iter()
            .filter(|stream| stream.available)
            .map(|stream| stream.source)
            .collect::<std::collections::BTreeSet<_>>();
        let mut streams = candidates::builtins::BuiltinCandidateContext {
            store: self.store,
            history_store: self.history_store,
            files: &files,
            chunks: &chunks,
            symbols: &symbols,
        }
        .collect_excluding(&request, &overridden_sources);
        streams.retain(|stream| routing.policy.allows(stream.source));
        streams.extend(external_streams);
        for stream in &mut streams {
            stream
                .candidates
                .truncate(routing.policy.candidate_cap(stream.source, limit));
        }
        // Task routing changes which evidence families run and how much candidate headroom they
        // receive. It deliberately does not introduce uncalibrated fusion weights: the measured
        // product default remains unweighted RRF unless repository ranking configuration says otherwise.
        let fusion_config = candidates::FusionConfig::from_ranking_options(&self.ranking_options);
        let fused = candidates::fuse_candidate_streams(&streams, candidate_limit, &fusion_config);
        let mut diagnostics = fused.diagnostics;
        diagnostics.routing = routing.diagnostics();
        diagnostics.caveats.extend(scope_caveats);
        diagnostics.caveats.sort();
        diagnostics.caveats.dedup();
        let blocked = apply_required_evidence_policy(&routing.policy, &budget, &mut diagnostics);
        let primary = if blocked {
            Vec::new()
        } else {
            let primary = rerank_fused_for_task_with_options(
                fused.results,
                &intent,
                &diagnostics,
                &self.ranking_options,
            );
            select_context_units(primary, &budget, &mut diagnostics)
        };
        self.build_from_primary_with_impact(task, limit, primary, true, false, diagnostics)
    }

    pub fn build_from_primary(
        &self,
        task: &str,
        limit: usize,
        primary: Vec<SearchResult>,
    ) -> Result<ContextPack> {
        self.build_from_primary_with_impact(
            task,
            limit,
            rerank_with_options(primary, &self.ranking_options),
            false,
            true,
            {
                let mut diagnostics = open_kioku_core::RetrievalDiagnostics::default();
                diagnostics.routing = routing::classify_task(task).diagnostics();
                diagnostics
            },
        )
    }

    fn build_from_primary_with_impact(
        &self,
        task: &str,
        limit: usize,
        primary: Vec<SearchResult>,
        expand_impact: bool,
        augment_runtime_candidates: bool,
        mut retrieval_diagnostics: open_kioku_core::RetrievalDiagnostics,
    ) -> Result<ContextPack> {
        let mut primary = primary;
        if augment_runtime_candidates {
            augment_primary_with_runtime(self.store, task, &mut primary, limit)?;
        }
        let primary_symbols = primary
            .iter()
            .filter_map(|result| result.symbol.clone())
            .take(10)
            .collect::<Vec<_>>();
        let impact = if expand_impact {
            if let Some(first) = primary.first() {
                ImpactEngine::new(self.store as &dyn open_kioku_storage::MetadataStore)
                    .with_history_store(self.history_store)
                    .for_file(&first.path)?
            } else {
                empty_impact(task)
            }
        } else if primary.is_empty() {
            empty_impact(task)
        } else {
            bounded_impact(task)
        };

        let mut dependency_edges: Vec<GraphEdge> = Vec::new();
        for result in primary.iter().take(5) {
            let node_id = format!("file:{}", result.path.display());
            if let Ok((_nodes, edges)) = self.store.neighbors(&node_id, 20) {
                dependency_edges
                    .extend(edges.into_iter().filter(is_trusted_context_dependency_edge));
            }
        }
        dependency_edges.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        dependency_edges.dedup_by(|a, b| a.id == b.id);
        dependency_edges.truncate(50);

        let mut primary_files = primary.iter().take(limit).cloned().collect::<Vec<_>>();
        let mut supporting_files = impact
            .direct_impacts
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>();
        let runtime_signals =
            runtime_signals_for_context(self.store, task, &primary_files, &supporting_files, 12)?;
        annotate_results_with_runtime(&mut primary_files, &runtime_signals);
        annotate_results_with_runtime(&mut supporting_files, &runtime_signals);
        annotate_results_with_git_history(
            self.store,
            self.history_store,
            task,
            &mut primary_files,
        )?;
        annotate_results_with_git_history(
            self.store,
            self.history_store,
            task,
            &mut supporting_files,
        )?;

        let selector = TestSelector::new(self.store as &dyn open_kioku_storage::MetadataStore);
        let mut tests_by_id = std::collections::BTreeMap::new();
        for result in validation_seed_results(&primary_files, &supporting_files, 5) {
            for test in selector.for_changed_path_with_evidence(&result.path, 5)? {
                // Validation seeds are ordered by evidence strength. Keep the first observation
                // of a test so runtime-corroborated selection is not overwritten by a weaker path.
                tests_by_id.entry(test.id.clone()).or_insert(test);
            }
        }
        let mut tests = tests_by_id.into_values().collect::<Vec<_>>();
        tests.sort_by(|left, right| {
            right
                .confidence
                .score()
                .partial_cmp(&left.confidence.score())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.name.cmp(&right.name))
        });
        tests.truncate(10);

        let runtime_evidence = runtime_signals
            .iter()
            .map(runtime_signal_evidence)
            .collect::<Vec<_>>();
        let git_evidence = git_history_evidence_for_results(self.store, &primary_files)?;

        let evidence = primary_files
            .iter()
            .take(20)
            .flat_map(|result| {
                result.evidence.iter().map(|msg| Evidence {
                    id: EvidenceId::new(format!("context:{}", result.path.display())),
                    source: "open-kioku-search".into(),
                    source_type: EvidenceSourceType::Lexical,
                    file_range: result
                        .line_range
                        .clone()
                        .map(|lr| open_kioku_core::FileRange {
                            path: result.path.clone(),
                            line_range: Some(lr),
                        }),
                    symbol_id: result.symbol.as_ref().map(|s| s.id.clone()),
                    confidence: Confidence::Medium,
                    message: msg.clone(),
                    indexed_at: Utc::now(),
                    ..Default::default()
                })
            })
            .chain(impact.evidence.clone())
            .chain(runtime_evidence.clone())
            .chain(git_evidence)
            .collect::<Vec<_>>();
        let allowed_files = primary
            .iter()
            .take(8)
            .map(|result| result.path.clone())
            .collect::<Vec<_>>();
        let mut confidence_breakdown = confidence_for_context(
            &primary_files,
            &supporting_files,
            &tests,
            &impact.risk_report,
            allowed_files.len(),
            evidence.len(),
            runtime_signals.len(),
        );
        if let Some(missing) = retrieval_diagnostics
            .selection
            .abstention_reason
            .as_deref()
            .and_then(|reason| reason.strip_prefix("missing_required_evidence:"))
        {
            confidence_breakdown.blockers.push(format!(
                "context retrieval blocked because task-family required evidence was missing: {missing}"
            ));
        }
        let negative_evidence = negative_evidence_for_context(
            task,
            &primary_files,
            &supporting_files,
            &tests,
            &impact.risk_report,
            &runtime_signals,
        );
        let boundary_evidence_refs = primary_files
            .iter()
            .flat_map(|result| result.derived_evidence_ids())
            .collect::<Vec<_>>();
        refresh_context_pack_retrieval_telemetry(
            &mut retrieval_diagnostics,
            &primary_files,
            &confidence_breakdown,
        );
        let confidence_summary = confidence_summary(&confidence_breakdown);
        Ok(ContextPack {
            task: task.into(),
            intent: classify_intent(task).into(),
            retrieval_diagnostics,
            primary_files,
            primary_symbols,
            supporting_files,
            dependency_edges,
            runtime_signals,
            test_candidates: tests.clone(),
            risk_report: impact.risk_report,
            recommended_change_boundary: ChangeBoundary {
                allowed_files,
                caution_files: impact
                    .direct_impacts
                    .iter()
                    .take(8)
                    .map(|result| result.path.clone())
                    .collect(),
                forbidden_files: Vec::new(),
                evidence_refs: boundary_evidence_refs,
                ..Default::default()
            },
            validation_plan: ValidationPlan {
                commands: tests
                    .iter()
                    .filter_map(|test| test.command.clone())
                    .collect(),
                tests,
                requires_approval: true,
                evidence: evidence.clone(),
            },
            evidence,
            negative_evidence,
            architecture_policy: None,
            confidence_summary,
            confidence_breakdown,
        })
    }
}

fn apply_required_evidence_policy(
    policy: &routing::RetrievalPolicy,
    budget: &ContextBudget,
    diagnostics: &mut RetrievalDiagnostics,
) -> bool {
    let missing = diagnostics
        .routing
        .required_evidence
        .iter()
        .copied()
        .filter(|required| {
            !diagnostics.traces.iter().any(|trace| {
                trace
                    .contributions
                    .iter()
                    .any(|contribution| contribution.source == *required)
            })
        })
        .collect::<Vec<_>>();

    for required in &missing {
        let requirement = if policy.missing_required_evidence_is_blocker {
            "blocking requirement"
        } else {
            "required evidence"
        };
        diagnostics.caveats.push(format!(
            "task-family {requirement}: {} did not contribute task-relevant evidence",
            retrieval_source_label(*required)
        ));
    }

    if !policy.missing_required_evidence_is_blocker || missing.is_empty() {
        return false;
    }

    // Initialize selection accounting without selecting heuristic substitutes. This is a
    // deterministic routing-contract blocker, not calibrated CC6 abstention.
    let _ = select_context_units(Vec::new(), budget, diagnostics);
    diagnostics.selection.abstention_reason = Some(format!(
        "missing_required_evidence:{}",
        missing
            .iter()
            .copied()
            .map(retrieval_source_label)
            .collect::<Vec<_>>()
            .join(",")
    ));
    true
}

fn select_context_units(
    ranked: Vec<SearchResult>,
    budget: &ContextBudget,
    diagnostics: &mut RetrievalDiagnostics,
) -> Vec<SearchResult> {
    let available = budget.available_context_tokens();
    diagnostics.selection = Default::default();
    diagnostics.selection.budget = *budget;
    diagnostics.selection.available_context_tokens = available;

    if budget.max_primary_files == 0 || available == 0 {
        diagnostics.selection.omitted_due_to_budget.extend(
            ranked
                .iter()
                .map(|result| format!("{}: no context budget available", result.path.display())),
        );
        return Vec::new();
    }

    // File-count callers historically select the reranked prefix. Preserve that behavior exactly;
    // the compatibility budget only routes the old API through the new accounting model.
    if is_file_limit_compatibility_budget(budget) {
        let selected = ranked
            .into_iter()
            .take(budget.max_primary_files)
            .collect::<Vec<_>>();
        record_selected_units(&selected, diagnostics);
        return selected;
    }

    let mut selected_indices = std::collections::BTreeSet::new();
    let mut terminally_rejected = std::collections::BTreeSet::new();
    let mut selected_token_sets = Vec::<std::collections::BTreeSet<String>>::new();
    let mut selected_sources = std::collections::BTreeSet::<RetrievalSourceKind>::new();
    let mut selected_tokens = 0usize;
    let mut per_file_units = std::collections::BTreeMap::<std::path::PathBuf, usize>::new();

    while selected_indices.len() < budget.max_primary_files {
        let remaining_tokens = available.saturating_sub(selected_tokens);
        let mut best: Option<(usize, u8, f32, usize, std::collections::BTreeSet<String>)> = None;

        for (index, result) in ranked.iter().enumerate() {
            if selected_indices.contains(&index) || terminally_rejected.contains(&index) {
                continue;
            }
            let authority = retrieval_authority_for_result(diagnostics, result);
            let sources = retrieval_sources_for_result(diagnostics, result);
            let high_value = is_high_value_context(authority, &sources);
            let tokens = estimate_search_result_tokens(result);
            let units_for_file = per_file_units
                .get(&result.path)
                .copied()
                .unwrap_or_default();

            if units_for_file >= budget.max_per_file {
                let message = format!(
                    "{}: per-file context unit cap {} reached",
                    result.path.display(),
                    budget.max_per_file
                );
                diagnostics
                    .selection
                    .omitted_due_to_caps
                    .push(message.clone());
                if high_value {
                    record_high_value_omission(
                        diagnostics,
                        result,
                        &format!("high-value evidence omitted by per-file cap: {message}"),
                    );
                }
                terminally_rejected.insert(index);
                continue;
            }

            if tokens > remaining_tokens {
                let message = format!(
                    "{}: estimated {} tokens exceeds remaining context budget {}",
                    result.path.display(),
                    tokens,
                    remaining_tokens
                );
                diagnostics
                    .selection
                    .omitted_due_to_budget
                    .push(message.clone());
                if high_value {
                    record_high_value_omission(
                        diagnostics,
                        result,
                        &format!("high-value evidence omitted by hard context budget: {message}"),
                    );
                }
                terminally_rejected.insert(index);
                continue;
            }

            let token_set = context_unit_tokens(result);
            let redundancy = selected_token_sets
                .iter()
                .map(|selected| token_set_overlap(&token_set, selected))
                .fold(0.0_f32, f32::max);
            if redundancy >= 0.90 && !high_value {
                diagnostics.selection.redundancy_omissions.push(format!(
                    "{}: near-duplicate context unit omitted ({redundancy:.2} overlap)",
                    result.path.display()
                ));
                terminally_rejected.insert(index);
                continue;
            }

            let utility = context_value_per_token(
                index,
                tokens,
                authority,
                &sources,
                &selected_sources,
                redundancy,
            );
            let safety_priority = if authority == RetrievalAuthority::Exact {
                2
            } else if sources.contains(&RetrievalSourceKind::Validation)
                || sources.contains(&RetrievalSourceKind::Graph)
            {
                1
            } else {
                0
            };
            match &best {
                Some((best_index, best_priority, best_utility, _, _))
                    if *best_priority > safety_priority
                        || (*best_priority == safety_priority
                            && (*best_utility > utility
                                || (*best_utility == utility && *best_index < index))) => {}
                _ => best = Some((index, safety_priority, utility, tokens, token_set)),
            }
        }

        let Some((index, _priority, _utility, tokens, token_set)) = best else {
            break;
        };
        let result = &ranked[index];
        selected_indices.insert(index);
        selected_tokens = selected_tokens.saturating_add(tokens);
        *per_file_units.entry(result.path.clone()).or_default() += 1;
        selected_token_sets.push(token_set);
        selected_sources.extend(retrieval_sources_for_result(diagnostics, result));
    }

    let selected = ranked
        .into_iter()
        .enumerate()
        .filter_map(|(index, result)| selected_indices.contains(&index).then_some(result))
        .collect::<Vec<_>>();
    record_selected_units(&selected, diagnostics);
    for caveat in &diagnostics.selection.caveats {
        if !diagnostics.caveats.contains(caveat) {
            diagnostics.caveats.push(caveat.clone());
        }
    }
    selected
}

fn is_file_limit_compatibility_budget(budget: &ContextBudget) -> bool {
    budget.max_tokens >= usize::MAX / 8 && budget.max_per_file >= usize::MAX / 8
}

fn retrieval_authority_for_result(
    diagnostics: &RetrievalDiagnostics,
    result: &SearchResult,
) -> RetrievalAuthority {
    retrieval_trace_for_result(diagnostics, result)
        .map(|trace| trace.authority)
        .unwrap_or(RetrievalAuthority::Heuristic)
}

fn retrieval_sources_for_result(
    diagnostics: &RetrievalDiagnostics,
    result: &SearchResult,
) -> std::collections::BTreeSet<RetrievalSourceKind> {
    retrieval_trace_for_result(diagnostics, result)
        .map(|trace| {
            trace
                .contributions
                .iter()
                .map(|contribution| contribution.source)
                .collect()
        })
        .unwrap_or_default()
}

fn is_high_value_context(
    authority: RetrievalAuthority,
    sources: &std::collections::BTreeSet<RetrievalSourceKind>,
) -> bool {
    authority == RetrievalAuthority::Exact
        || sources.contains(&RetrievalSourceKind::Validation)
        || sources.contains(&RetrievalSourceKind::Graph)
}

fn record_high_value_omission(
    diagnostics: &mut RetrievalDiagnostics,
    result: &SearchResult,
    message: &str,
) {
    diagnostics.selection.omitted_high_value.push(format!(
        "{}{}: {message}",
        result.path.display(),
        result
            .line_range
            .as_ref()
            .map(|range| format!(":{}-{}", range.start, range.end))
            .unwrap_or_default()
    ));
    diagnostics.selection.caveats.push(message.to_string());
}

fn context_value_per_token(
    rank_index: usize,
    tokens: usize,
    authority: RetrievalAuthority,
    sources: &std::collections::BTreeSet<RetrievalSourceKind>,
    selected_sources: &std::collections::BTreeSet<RetrievalSourceKind>,
    redundancy: f32,
) -> f32 {
    let rank_value = 1.0 / (rank_index.saturating_add(1) as f32);
    let authority_weight = match authority {
        RetrievalAuthority::Exact => 3.0,
        RetrievalAuthority::Corroborating => 1.35,
        RetrievalAuthority::Heuristic => 1.0,
    };
    let source_diversity = if sources
        .iter()
        .any(|source| !selected_sources.contains(source))
    {
        1.10
    } else {
        1.0
    };
    let redundancy_discount = 1.0 - redundancy.min(0.85) * 0.50;
    // sqrt(cost) avoids pathological preference for tiny fragments while still rewarding useful
    // compact context. The upstream task-aware rank remains the dominant relevance prior.
    rank_value * authority_weight * source_diversity * redundancy_discount
        / (tokens.max(1) as f32).sqrt()
}

fn record_selected_units(selected: &[SearchResult], diagnostics: &mut RetrievalDiagnostics) {
    diagnostics.selection.selected_units.clear();
    diagnostics.selection.per_file_tokens.clear();
    diagnostics.selection.estimated_tokens_selected = 0;
    for result in selected {
        let estimated_tokens = estimate_search_result_tokens(result);
        let authority = retrieval_authority_for_result(diagnostics, result);
        diagnostics.selection.estimated_tokens_selected = diagnostics
            .selection
            .estimated_tokens_selected
            .saturating_add(estimated_tokens);
        *diagnostics
            .selection
            .per_file_tokens
            .entry(result.path.clone())
            .or_default() += estimated_tokens;
        diagnostics.selection.selected_units.push(ContextSelectedUnit {
            path: result.path.clone(),
            line_range: result.line_range.clone(),
            estimated_tokens,
            authority,
            evidence_refs: result.derived_evidence_ids(),
            rationale: format!(
                "selected under context budget after task-aware retrieval ranking ({authority:?} authority)"
            ),
        });
    }
}

fn estimate_search_result_tokens(result: &SearchResult) -> usize {
    // Deliberately model-independent and deterministic. Four UTF-8 chars/token is a conservative
    // local estimate for mixed source/code prose, with fixed metadata overhead.
    let content = result.snippet.chars().count()
        + result.path.to_string_lossy().chars().count()
        + result.match_reason.chars().count();
    content.saturating_add(3) / 4 + 12
}

fn context_unit_tokens(result: &SearchResult) -> std::collections::BTreeSet<String> {
    result
        .snippet
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 4)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn token_set_overlap(
    left: &std::collections::BTreeSet<String>,
    right: &std::collections::BTreeSet<String>,
) -> f32 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count() as f32;
    let smaller = left.len().min(right.len()) as f32;
    intersection / smaller
}

fn validation_seed_results<'a>(
    primary_files: &'a [SearchResult],
    supporting_files: &'a [SearchResult],
    limit: usize,
) -> Vec<&'a SearchResult> {
    let mut selected = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let ordered = primary_files
        .iter()
        .filter(|result| has_runtime_corroboration(result))
        .chain(
            supporting_files
                .iter()
                .filter(|result| has_runtime_corroboration(result)),
        )
        .chain(primary_files.iter())
        .chain(supporting_files.iter());

    for result in ordered {
        if is_docs_or_test_path(&result.path.to_string_lossy()) {
            continue;
        }
        let normalized = normalize_path(&result.path);
        if !seen.insert(normalized) {
            continue;
        }
        selected.push(result);
        if selected.len() >= limit {
            break;
        }
    }
    selected
}

fn has_runtime_corroboration(result: &SearchResult) -> bool {
    result.score_breakdown.iter().any(|component| {
        component.signal == "runtime_corroboration" && component.contribution > 0.0
    }) || result.evidence.iter().any(|evidence| {
        evidence
            .to_ascii_lowercase()
            .contains("runtime corroboration")
    })
}

fn negative_evidence_for_context(
    task: &str,
    primary_files: &[SearchResult],
    supporting_files: &[SearchResult],
    tests: &[open_kioku_core::TestTarget],
    risk: &RiskReport,
    runtime_signals: &[RuntimeSignal],
) -> Vec<NegativeEvidence> {
    let mut items = Vec::new();
    if primary_files.is_empty() {
        items.push(NegativeEvidence {
            query: task.into(),
            scope: "primary_context".into(),
            inspected_sources: vec!["lexical_search".into(), "ranking_fusion".into()],
            reason: "no primary context matched the task".into(),
            confidence: 0.95,
            suggested_next_probe: Some("Run `ok search <task> --explain-ranking` with named symbols or paths from the ticket.".into()),
        });
    }
    if exact_reference_count(primary_files, supporting_files) == 0 {
        items.push(NegativeEvidence {
            query: task.into(),
            scope: "exact_references".into(),
            inspected_sources: vec![
                "search_result.evidence".into(),
                "search_result.match_reason".into(),
            ],
            reason: "no explicit exact symbol reference or SCIP evidence was found".into(),
            confidence: 0.85,
            suggested_next_probe: Some(
                "Run `ok scip setup .` and re-index with `ok index . --with-scip auto`.".into(),
            ),
        });
    }
    if tests.is_empty() {
        items.push(NegativeEvidence {
            query: task.into(),
            scope: "validation".into(),
            inspected_sources: vec!["indexed_tests".into(), "test_selector".into()],
            reason: "no nearby validation target was selected".into(),
            confidence: 0.80,
            suggested_next_probe: primary_files.first().map(|result| {
                format!(
                    "Run `ok tests {}` to inspect validation candidates for the top file.",
                    result.path.display()
                )
            }),
        });
    }
    if runtime_signals.is_empty() && runtime_signal_count(primary_files, supporting_files) == 0 {
        items.push(NegativeEvidence {
            query: task.into(),
            scope: "runtime".into(),
            inspected_sources: vec!["runtime_signals".into(), "search_result.evidence".into()],
            reason:
                "no runtime trace, incident, or error artifact corroborated the selected context"
                    .into(),
            confidence: 0.75,
            suggested_next_probe: Some(
                "Import or configure runtime artifacts, then rerun `ok plan`.".into(),
            ),
        });
    }
    if docs_or_tests_only(primary_files) {
        items.push(NegativeEvidence {
            query: task.into(),
            scope: "boundary".into(),
            inspected_sources: vec!["primary_context.paths".into()],
            reason: "task anchors only matched docs or test fixtures, not source edit targets"
                .into(),
            confidence: 0.90,
            suggested_next_probe: Some(
                "Search for the production symbol or source path named by the ticket.".into(),
            ),
        });
    }
    for reason in &risk.reasons {
        let lower = reason.to_ascii_lowercase();
        if lower.contains("low confidence") || lower.contains("no matching") {
            items.push(NegativeEvidence {
                query: task.into(),
                scope: "risk".into(),
                inspected_sources: vec!["risk_report.reasons".into()],
                reason: reason.clone(),
                confidence: 0.85,
                suggested_next_probe: Some(
                    "Resolve the missing task anchor before editing.".into(),
                ),
            });
        }
    }
    items
}

fn confidence_for_context(
    primary_files: &[SearchResult],
    supporting_files: &[SearchResult],
    tests: &[open_kioku_core::TestTarget],
    risk: &RiskReport,
    allowed_file_count: usize,
    evidence_count: usize,
    runtime_signal_count_value: usize,
) -> ConfidenceBreakdown {
    ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
        primary_file_count: primary_files.len(),
        evidence_count,
        exact_reference_count: exact_reference_count(primary_files, supporting_files),
        validation_count: tests.len(),
        validation_with_command_count: tests.iter().filter(|test| test.command.is_some()).count(),
        negative_evidence_count: negative_evidence_count(risk),
        allowed_file_count,
        runtime_signal_count: runtime_signal_count_value
            + runtime_signal_count(primary_files, supporting_files),
    })
}

fn confidence_summary(breakdown: &ConfidenceBreakdown) -> String {
    let mut parts = vec![format!(
        "overall {:?} ({:.2}) from explainable evidence signals",
        breakdown.overall_enum, breakdown.overall_score
    )];
    if let Some(blocker) = breakdown.blockers.first() {
        parts.push(format!("blocker: {blocker}"));
    }
    if let Some(caveat) = breakdown.caveats.first() {
        parts.push(format!("caveat: {caveat}"));
    }
    parts.join("; ")
}

fn exact_reference_count(
    primary_files: &[SearchResult],
    supporting_files: &[SearchResult],
) -> usize {
    primary_files
        .iter()
        .chain(supporting_files.iter())
        .filter(|result| has_exact_reference_signal(result))
        .count()
}

fn has_exact_reference_signal(result: &SearchResult) -> bool {
    result
        .evidence
        .iter()
        .any(|evidence| contains_exact_reference(evidence))
        || contains_exact_reference(&result.match_reason)
}

fn contains_exact_reference(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("exact reference")
        || lower.contains("exact symbol reference")
        || lower.contains("scip")
}

fn runtime_signal_count(
    primary_files: &[SearchResult],
    supporting_files: &[SearchResult],
) -> usize {
    primary_files
        .iter()
        .chain(supporting_files.iter())
        .filter(|result| {
            result.score_breakdown.iter().any(|component| {
                component.signal == "runtime_corroboration" && component.contribution > 0.0
            }) || result
                .evidence
                .iter()
                .any(|evidence| evidence.to_ascii_lowercase().contains("runtime"))
        })
        .count()
}

fn runtime_signals_for_context(
    store: &dyn OkStore,
    task: &str,
    primary_files: &[SearchResult],
    supporting_files: &[SearchResult],
    limit: usize,
) -> Result<Vec<RuntimeSignal>> {
    let facts = store.analysis_facts(Some(EvidenceSourceType::Runtime), 500)?;
    if facts.is_empty() {
        return Ok(Vec::new());
    }
    let files = store.list_files(usize::MAX, 0)?;
    let files_by_id = files
        .into_iter()
        .map(|file| (file.id.clone(), file))
        .collect::<std::collections::HashMap<_, _>>();
    let selected_paths = primary_files
        .iter()
        .chain(supporting_files.iter())
        .map(|result| normalize_path(&result.path))
        .collect::<std::collections::HashSet<_>>();
    let searchable_context = primary_files
        .iter()
        .chain(supporting_files.iter())
        .flat_map(|result| {
            [
                result.path.display().to_string(),
                result.snippet.clone(),
                result.match_reason.clone(),
                result.evidence.join(" "),
            ]
        })
        .chain(std::iter::once(task.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let mut signals = facts
        .into_iter()
        .filter_map(|fact| {
            let file = files_by_id.get(&fact.file_id)?;
            if selected_paths.contains(&normalize_path(&file.path))
                || runtime_fact_matches_query(&fact, &searchable_context)
            {
                Some(runtime_signal_from_fact(&fact, file))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    signals.sort_by(|a, b| a.id.cmp(&b.id));
    signals.dedup_by(|a, b| a.id == b.id);
    signals.truncate(limit);
    Ok(signals)
}

fn augment_primary_with_runtime(
    store: &dyn OkStore,
    task: &str,
    primary: &mut Vec<SearchResult>,
    limit: usize,
) -> Result<()> {
    let facts = store.analysis_facts(Some(EvidenceSourceType::Runtime), 500)?;
    if facts.is_empty() {
        return Ok(());
    }
    let task = task.to_ascii_lowercase();
    let files = store.list_files(usize::MAX, 0)?;
    let files_by_id = files
        .into_iter()
        .map(|file| (file.id.clone(), file))
        .collect::<std::collections::HashMap<_, _>>();
    let mut existing_paths = primary
        .iter()
        .map(|result| normalize_path(&result.path))
        .collect::<std::collections::HashSet<_>>();
    let mut additions = Vec::new();
    for fact in facts
        .into_iter()
        .filter(|fact| runtime_fact_matches_query(fact, &task))
    {
        let Some(file) = files_by_id.get(&fact.file_id) else {
            continue;
        };
        let normalized_path = normalize_path(&file.path);
        if !existing_paths.insert(normalized_path) {
            continue;
        }
        if let Some(result) = runtime_seed_result(store, file, &fact)? {
            additions.push(result);
        }
        if additions.len() >= limit {
            break;
        }
    }
    primary.extend(additions);
    primary.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    primary.truncate(limit.max(1));
    Ok(())
}

fn runtime_seed_result(
    store: &dyn OkStore,
    file: &File,
    fact: &AnalysisFact,
) -> Result<Option<SearchResult>> {
    let chunks = store.chunks_for_file(&file.id)?;
    let snippet = chunks
        .iter()
        .find(|chunk| {
            fact.range
                .as_ref()
                .map(|range| chunk.range.start <= range.start && range.start <= chunk.range.end)
                .unwrap_or(false)
        })
        .or_else(|| chunks.first())
        .map(|chunk| chunk.text.clone())
        .unwrap_or_else(|| fact.target.clone());
    let evidence = vec![format!(
        "runtime corroboration from local artifact `{}` targeting `{}`",
        fact.source, fact.target
    )];
    Ok(Some(SearchResult {
        path: file.path.clone(),
        line_range: fact.range.clone(),
        snippet,
        symbol: None,
        score: 1.35,
        match_reason: "runtime artifact matched task intent".into(),
        evidence,
        evidence_refs: vec![fact.id.clone()],
        confidence: fact.confidence.score(),
        score_breakdown: vec![ScoreComponent::single(
            "runtime_corroboration",
            1.35,
            vec![fact.id.clone()],
            "local runtime trace/log/incident artifact matched the task",
        )],
    }))
}

fn annotate_results_with_runtime(results: &mut [SearchResult], signals: &[RuntimeSignal]) {
    if signals.is_empty() {
        return;
    }
    for result in results {
        let result_path = normalize_path(&result.path);
        let searchable = format!(
            "{} {} {}",
            result.snippet,
            result.match_reason,
            result.evidence.join(" ")
        )
        .to_ascii_lowercase();
        let matched = signals
            .iter()
            .filter(|signal| {
                signal
                    .file_range
                    .as_ref()
                    .map(|range| normalize_path(&range.path) == result_path)
                    .unwrap_or(false)
                    || runtime_message_tokens(&signal.message)
                        .iter()
                        .any(|token| searchable.contains(token))
            })
            .take(3)
            .collect::<Vec<_>>();
        if matched.is_empty() {
            continue;
        }
        let evidence_ids = matched
            .iter()
            .map(|signal| signal.id.clone())
            .collect::<Vec<_>>();
        let labels = matched
            .iter()
            .map(|signal| signal.kind.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        for signal in &matched {
            let evidence = format!(
                "runtime corroboration `{}`: {}",
                signal.kind, signal.message
            );
            if !result.evidence.contains(&evidence) {
                result.evidence.push(evidence);
            }
        }
        for id in &evidence_ids {
            if !result.evidence_refs.contains(id) {
                result.evidence_refs.push(id.clone());
            }
        }
        result.score += 0.15 * matched.len() as f32;
        result.confidence = result.confidence.max(0.75);
        result.score_breakdown.push(ScoreComponent::adjustment(
            "runtime_corroboration",
            0.15 * matched.len() as f32,
            evidence_ids,
            format!("local runtime artifact corroborates this context result: {labels}"),
        ));
    }
}

fn runtime_signal_from_fact(fact: &AnalysisFact, file: &File) -> RuntimeSignal {
    RuntimeSignal {
        id: fact.id.clone(),
        kind: runtime_kind(fact),
        message: format!("{}: {}", fact.message, fact.target),
        file_range: Some(FileRange {
            path: file.path.clone(),
            line_range: fact.range.clone(),
        }),
        occurred_at: None,
        confidence: fact.confidence,
    }
}

fn runtime_signal_evidence(signal: &RuntimeSignal) -> Evidence {
    Evidence {
        id: EvidenceId::new(signal.id.clone()),
        source: "open-kioku-runtime".into(),
        source_type: EvidenceSourceType::Runtime,
        file_range: signal.file_range.clone(),
        symbol_id: None,
        confidence: signal.confidence,
        message: signal.message.clone(),
        indexed_at: Utc::now(),
        ..Default::default()
    }
}

fn annotate_results_with_git_history(
    store: &dyn OkStore,
    history_store: Option<&dyn HistoryStore>,
    task: &str,
    results: &mut [SearchResult],
) -> Result<()> {
    if results.is_empty() {
        return Ok(());
    }
    if let Some(history_store) = history_store {
        for result in &mut *results {
            let symbols = result
                .symbol
                .as_ref()
                .map(|symbol| vec![symbol.qualified_name.clone(), symbol.name.clone()])
                .unwrap_or_default();
            let summary = history_store.history_score_components(
                &HistorySignalQuery {
                    path: result.path.clone(),
                    task: Some(task.to_string()),
                    symbols,
                },
                8,
            )?;
            if summary.components.is_empty() {
                continue;
            }
            for reason in &summary.reasons {
                let evidence = format!("history signal for `{}`: {reason}", result.path.display());
                if !result.evidence.contains(&evidence) {
                    result.evidence.push(evidence);
                }
            }
            for evidence_ref in &summary.evidence_refs {
                if !result.evidence_refs.contains(evidence_ref) {
                    result.evidence_refs.push(evidence_ref.clone());
                }
            }
            let contribution = summary
                .components
                .iter()
                .map(|component| component.contribution)
                .sum::<f32>()
                .min(0.30);
            result.score += contribution;
            result.confidence = result.confidence.max(0.70);
            result.score_breakdown.extend(summary.components);
        }
    }

    let facts = store.analysis_facts(Some(EvidenceSourceType::GitHistory), 10_000)?;
    if facts.is_empty() {
        return Ok(());
    }
    let files = store.list_files(usize::MAX, 0)?;
    let files_by_path = files
        .into_iter()
        .map(|file| (normalize_path(&file.path), file))
        .collect::<std::collections::HashMap<_, _>>();
    for result in results {
        let Some(file) = files_by_path.get(&normalize_path(&result.path)) else {
            continue;
        };
        let matched = facts
            .iter()
            .filter(|fact| fact.file_id == file.id)
            .take(3)
            .collect::<Vec<_>>();
        if matched.is_empty() {
            continue;
        }
        let evidence_ids = matched
            .iter()
            .map(|fact| fact.id.clone())
            .collect::<Vec<_>>();
        let labels = matched
            .iter()
            .map(|fact| fact.target.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        for fact in &matched {
            let evidence = format!(
                "git co-change from local history: `{}` ({})",
                fact.target, fact.message
            );
            if !result.evidence.contains(&evidence) {
                result.evidence.push(evidence);
            }
        }
        for id in &evidence_ids {
            if !result.evidence_refs.contains(id) {
                result.evidence_refs.push(id.clone());
            }
        }
        result.score += (0.12 * matched.len() as f32).min(0.18);
        result.confidence = result.confidence.max(0.70);
        result.score_breakdown.push(ScoreComponent::adjustment(
            "similar_change_overlap",
            (0.12 * matched.len() as f32).min(0.18),
            evidence_ids,
            format!("bounded local git history says this file co-changed with: {labels}"),
        ));
    }
    Ok(())
}

fn git_history_evidence_for_results(
    store: &dyn OkStore,
    results: &[SearchResult],
) -> Result<Vec<Evidence>> {
    if results.is_empty() {
        return Ok(Vec::new());
    }
    let facts = store.analysis_facts(Some(EvidenceSourceType::GitHistory), 10_000)?;
    if facts.is_empty() {
        return Ok(Vec::new());
    }
    let files = store.list_files(usize::MAX, 0)?;
    let paths_by_id = files
        .into_iter()
        .map(|file| (file.id, file.path))
        .collect::<std::collections::HashMap<_, _>>();
    let selected_paths = results
        .iter()
        .map(|result| normalize_path(&result.path))
        .collect::<std::collections::HashSet<_>>();
    let mut evidence = Vec::new();
    for fact in facts {
        let Some(path) = paths_by_id.get(&fact.file_id) else {
            continue;
        };
        if !selected_paths.contains(&normalize_path(path)) {
            continue;
        }
        evidence.push(Evidence {
            id: EvidenceId::new(fact.id.clone()),
            source: fact.source.clone(),
            source_type: EvidenceSourceType::GitHistory,
            file_range: Some(FileRange {
                path: path.clone(),
                line_range: None,
            }),
            symbol_id: None,
            confidence: fact.confidence,
            message: format!("{}: {}", fact.message, fact.target),
            indexed_at: Utc::now(),
            ..Default::default()
        });
        if evidence.len() >= 20 {
            break;
        }
    }
    Ok(evidence)
}

fn runtime_kind(fact: &AnalysisFact) -> String {
    match (&fact.target_kind, &fact.edge_type) {
        (GraphNodeType::Endpoint, GraphEdgeType::ExposesEndpoint) => "endpoint".into(),
        (GraphNodeType::DatabaseTable, GraphEdgeType::ReadsTable) => "sql_read".into(),
        (GraphNodeType::DatabaseTable, GraphEdgeType::WritesTable) => "sql_write".into(),
        (GraphNodeType::RuntimeError, _) => "incident".into(),
        (_, edge) => format!("{edge:?}").to_ascii_lowercase(),
    }
}

fn runtime_fact_matches_query(fact: &AnalysisFact, searchable_context: &str) -> bool {
    runtime_message_tokens(&fact.target)
        .iter()
        .any(|token| searchable_context.contains(token))
        || runtime_message_tokens(&fact.message)
            .iter()
            .any(|token| searchable_context.contains(token))
}

fn runtime_message_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '/' || ch == '.'))
        .map(|token| token.trim_matches('/').to_ascii_lowercase())
        .filter(|token| token.len() >= 4)
        .take(8)
        .collect()
}

fn normalize_path(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

fn validated_candidate_path_scope(
    path_anchors: &[String],
    files: &[File],
) -> (Vec<String>, Vec<String>) {
    let indexed_paths = files
        .iter()
        .map(|file| normalize_path(&file.path))
        .collect::<Vec<_>>();
    let mut validated = Vec::new();
    let mut caveats = Vec::new();

    for anchor in path_anchors {
        let normalized = anchor
            .replace('\\', "/")
            .trim_start_matches("./")
            .trim_matches('/')
            .to_string();
        let invalid = normalized.is_empty()
            || normalized
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..");
        if invalid {
            caveats.push(format!(
                "query path scope `{anchor}` is not a safe repository-relative prefix and was not enforced"
            ));
            continue;
        }
        let directory_prefix = format!("{normalized}/");
        if indexed_paths
            .iter()
            .any(|path| path == &normalized || path.starts_with(&directory_prefix))
        {
            if !validated.contains(&normalized) {
                validated.push(normalized);
            }
        } else {
            caveats.push(format!(
                "query path scope `{anchor}` did not match indexed repository paths and was not enforced"
            ));
        }
    }
    validated.sort();
    caveats.sort();
    (validated, caveats)
}

fn negative_evidence_count(risk: &RiskReport) -> usize {
    risk.reasons
        .iter()
        .filter(|reason| {
            let lower = reason.to_ascii_lowercase();
            lower.contains("low confidence")
                || lower.contains("no matching")
                || lower.contains("missing")
                || lower.contains("absent")
                || lower.contains("unavailable")
                || lower.contains("weak")
                || lower.contains("unknown")
        })
        .count()
}

fn docs_or_tests_only(results: &[SearchResult]) -> bool {
    !results.is_empty()
        && results
            .iter()
            .all(|result| is_docs_or_test_path(&result.path.to_string_lossy()))
}

fn is_docs_or_test_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.starts_with("docs/")
        || path.starts_with("test/")
        || path.starts_with("tests/")
        || path.contains("/docs/")
        || path.ends_with(".md")
        || path.ends_with(".mdx")
        || path.contains("/test/")
        || path.contains("/tests/")
        || path.contains("_test.")
        || path.contains("test_")
}

#[derive(Debug, Clone, Default)]
struct TaskSearchIntent {
    primary_anchors: Vec<String>,
    reference_anchors: Vec<String>,
    ticket_anchors: Vec<String>,
    path_anchors: Vec<String>,
    lexical_anchors: Vec<String>,
    documentation_target: bool,
}

impl TaskSearchIntent {
    fn parse(task: &str) -> Self {
        let mut intent = Self {
            documentation_target: task_targets_documentation(task),
            ..Self::default()
        };
        let lower = task.to_ascii_lowercase();
        let reference_start = reference_marker_start(&lower).unwrap_or(task.len());
        let edit_side = task.get(..reference_start).unwrap_or(task);
        let reference_side = task.get(reference_start..).unwrap_or_default();
        let all_identifiers = identifiers(task);

        intent.primary_anchors = identifiers(edit_side);
        intent.reference_anchors = identifiers(reference_side);
        if intent.primary_anchors.is_empty() {
            if let Some(first) = all_identifiers.first() {
                intent.primary_anchors.push(first.clone());
            }
        }
        for value in all_identifiers {
            if !intent.primary_anchors.contains(&value)
                && !intent.reference_anchors.contains(&value)
            {
                intent.reference_anchors.push(value);
            }
        }

        for token in task.split_whitespace() {
            let cleaned = token.trim_matches(|ch: char| {
                !(ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '/' || ch == '.')
            });
            if is_ticket_id(cleaned) && !intent.ticket_anchors.iter().any(|v| v == cleaned) {
                intent.ticket_anchors.push(cleaned.to_string());
            }
            if is_path_like(cleaned) {
                let normalized = cleaned.trim_matches('/');
                if !normalized.is_empty() && !intent.path_anchors.iter().any(|v| v == normalized) {
                    intent.path_anchors.push(normalized.to_string());
                }
            }
        }

        intent.lexical_anchors = task_lexical_terms(task);
        intent
    }

    fn search_terms(&self, task: &str) -> Vec<String> {
        let mut terms = vec![task.to_string()];
        let alias_terms = task_alias_terms(task);
        for term in self
            .ticket_anchors
            .iter()
            .chain(self.path_anchors.iter())
            .chain(self.primary_anchors.iter())
            .chain(self.reference_anchors.iter())
            .chain(self.lexical_anchors.iter())
            .chain(alias_terms.iter())
        {
            if term.len() >= 3 && !terms.iter().any(|existing| existing == term) {
                terms.push(term.clone());
            }
        }
        terms
    }
}

fn search_candidates(
    chunks: &[CodeChunk],
    files: &[File],
    symbols: &[Symbol],
    task: &str,
    limit: usize,
    intent: &TaskSearchIntent,
) -> Result<Vec<SearchResult>> {
    let mut merged = std::collections::BTreeMap::<String, SearchResult>::new();
    let per_anchor_limit = limit.clamp(8, 40);
    for term in intent.search_terms(task) {
        for mut result in search_chunks(chunks, files, symbols, &term, per_anchor_limit)? {
            if term != task {
                result
                    .evidence
                    .push(format!("task anchor `{term}` matched"));
                result.match_reason = format!("{}; task anchor `{term}`", result.match_reason);
            }
            let key = result_key(&result);
            match merged.get_mut(&key) {
                Some(existing) => {
                    if result.score > existing.score {
                        existing.score = result.score;
                        existing.snippet = result.snippet;
                        existing.line_range = result.line_range;
                        existing.symbol = result.symbol;
                        existing.score_breakdown = result.score_breakdown;
                    }
                    for evidence in result.evidence {
                        if !existing.evidence.contains(&evidence) {
                            existing.evidence.push(evidence);
                        }
                    }
                    if !existing.match_reason.contains(&term) {
                        existing.match_reason =
                            format!("{}; task anchor `{term}`", existing.match_reason);
                    }
                    existing.reconcile_score_breakdown();
                }
                None => {
                    merged.insert(key, result);
                }
            }
        }
    }

    Ok(merged.into_values().collect())
}

#[cfg(test)]
fn rerank_for_task(
    results: Vec<SearchResult>,
    intent: &TaskSearchIntent,
    ranking_options: &RankingOptions,
) -> Vec<SearchResult> {
    let ranked = rerank_with_options(results, ranking_options);
    rerank_fused_for_task(ranked, intent, &RetrievalDiagnostics::default())
}

#[cfg(test)]
fn rerank_fused_for_task(
    results: Vec<SearchResult>,
    intent: &TaskSearchIntent,
    diagnostics: &RetrievalDiagnostics,
) -> Vec<SearchResult> {
    rerank_fused_for_task_with_options(results, intent, diagnostics, &RankingOptions::default())
}

fn rerank_fused_for_task_with_options(
    results: Vec<SearchResult>,
    intent: &TaskSearchIntent,
    diagnostics: &RetrievalDiagnostics,
    ranking_options: &RankingOptions,
) -> Vec<SearchResult> {
    // Candidate streams have already been fused by rank. Only apply deterministic task-anchor
    // adjustments here; running the legacy weighted fusion again would reinterpret RRF as text
    // relevance and erase source provenance from score_breakdown.
    let mut results = results;
    for result in &mut results {
        let haystack = searchable_result_text(result);
        for anchor in &intent.primary_anchors {
            if contains_anchor(&haystack, anchor) {
                result.score += 0.65;
                result.confidence = result.confidence.max(0.85);
                result
                    .evidence
                    .push(format!("primary task anchor `{anchor}` matched"));
                result.add_score_component(ScoreComponent::adjustment(
                    "primary_task_anchor_boost",
                    0.65,
                    result.derived_evidence_ids(),
                    format!("primary task anchor `{anchor}` matched result text"),
                ));
            }
        }
        for anchor in &intent.reference_anchors {
            if contains_anchor(&haystack, anchor) {
                result.score += 0.25;
                result.confidence = result.confidence.max(0.65);
                result
                    .evidence
                    .push(format!("reference task anchor `{anchor}` matched"));
                result.add_score_component(ScoreComponent::adjustment(
                    "reference_task_anchor_boost",
                    0.25,
                    result.derived_evidence_ids(),
                    format!("reference task anchor `{anchor}` matched result text"),
                ));
            }
        }
        for anchor in intent
            .ticket_anchors
            .iter()
            .chain(intent.path_anchors.iter())
        {
            if contains_anchor(&haystack, anchor) {
                result.score += 0.35;
                result.confidence = result.confidence.max(0.75);
                result
                    .evidence
                    .push(format!("ticket/path task anchor `{anchor}` matched"));
                result.add_score_component(ScoreComponent::adjustment(
                    "ticket_or_path_anchor_boost",
                    0.35,
                    result.derived_evidence_ids(),
                    format!("ticket/path anchor `{anchor}` matched result text"),
                ));
            }
        }
        result.reconcile_score_breakdown();
    }
    results.sort_by(|a, b| {
        let a_haystack = searchable_result_text(a);
        let b_haystack = searchable_result_text(b);
        task_relevance_tier(&b.path, &b_haystack, intent)
            .cmp(&task_relevance_tier(&a.path, &a_haystack, intent))
            .then_with(|| {
                retrieval_authority_for_result(diagnostics, b)
                    .cmp(&retrieval_authority_for_result(diagnostics, a))
            })
            .then_with(|| {
                context_quality_tier(&b.path, ranking_options)
                    .cmp(&context_quality_tier(&a.path, ranking_options))
            })
            .then_with(|| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.path.cmp(&b.path))
    });
    results
}

fn context_quality_tier(path: &std::path::Path, options: &RankingOptions) -> u8 {
    let normalized = normalize_path(path).to_ascii_lowercase();
    let boundary_fit_enabled = ranking_signal_enabled(
        options,
        open_kioku_ranking::RankingSignal::BoundaryFit,
        options.weights.boundary_fit,
    );
    let path_quality_enabled = ranking_signal_enabled(
        options,
        open_kioku_ranking::RankingSignal::PathQuality,
        options.weights.path_quality,
    );

    if path_quality_enabled && is_generated_or_vendor_path(&normalized) {
        return 0;
    }
    if boundary_fit_enabled && is_docs_or_test_path(&normalized) {
        return 1;
    }
    2
}

fn ranking_signal_enabled(
    options: &RankingOptions,
    signal: open_kioku_ranking::RankingSignal,
    weight: f32,
) -> bool {
    if weight.abs() <= f32::EPSILON
        || matches!(options.mode, open_kioku_ranking::RankingMode::Baseline)
    {
        return false;
    }
    !matches!(
        options.mode,
        open_kioku_ranking::RankingMode::WithoutSignal(disabled) if disabled == signal
    )
}

fn is_generated_or_vendor_path(path: &str) -> bool {
    path.contains("vendor")
        || path.contains("generated")
        || path.contains("_pb.rs")
        || path.contains(".pb.go")
        || path.contains("schema.json")
}

fn task_relevance_tier(path: &std::path::Path, haystack: &str, intent: &TaskSearchIntent) -> u8 {
    if intent
        .primary_anchors
        .iter()
        .any(|anchor| contains_anchor(haystack, anchor))
    {
        3
    } else if intent
        .ticket_anchors
        .iter()
        .chain(intent.path_anchors.iter())
        .any(|anchor| contains_anchor(haystack, anchor))
        || (intent.documentation_target && is_documentation_path(&normalize_path(path)))
    {
        2
    } else if intent
        .reference_anchors
        .iter()
        .any(|anchor| contains_anchor(haystack, anchor))
    {
        1
    } else {
        0
    }
}

fn task_targets_documentation(task: &str) -> bool {
    task.split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .any(|token| {
            matches!(
                token.as_str(),
                "document" | "documentation" | "docs" | "readme" | "guide" | "guides"
            )
        })
}

fn is_documentation_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.starts_with("docs/")
        || path.contains("/docs/")
        || path.ends_with("readme.md")
        || path.ends_with(".md")
        || path.ends_with(".mdx")
}

fn result_key(result: &SearchResult) -> String {
    format!(
        "{}:{}-{}",
        result.path.display(),
        result
            .line_range
            .as_ref()
            .map(|range| range.start)
            .unwrap_or_default(),
        result
            .line_range
            .as_ref()
            .map(|range| range.end)
            .unwrap_or_default()
    )
}

fn searchable_result_text(result: &SearchResult) -> String {
    format!(
        "{} {} {} {}",
        result.path.display(),
        result.snippet,
        result
            .symbol
            .as_ref()
            .map(|symbol| symbol.qualified_name.as_str())
            .unwrap_or_default(),
        result
            .symbol
            .as_ref()
            .map(|symbol| symbol.name.as_str())
            .unwrap_or_default()
    )
    .to_ascii_lowercase()
}

fn contains_anchor(haystack: &str, anchor: &str) -> bool {
    haystack.contains(&anchor.to_ascii_lowercase())
        || haystack.contains(&normalize_identifier(anchor))
}

fn reference_marker_start(lower: &str) -> Option<usize> {
    [
        " similar to ",
        " like ",
        " copy from ",
        " copied from ",
        " mirror ",
        " mirrored from ",
        " based on ",
        " reference ",
    ]
    .iter()
    .filter_map(|marker| lower.find(marker))
    .min()
}

fn identifiers(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in value.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')) {
        let token = token.trim_matches('-');
        if is_named_identifier(token) && !out.iter().any(|existing| existing == token) {
            out.push(token.to_string());
        }
    }
    out
}

fn is_named_identifier(value: &str) -> bool {
    if value.len() < 3 || is_ticket_id(value) {
        return false;
    }
    let has_lower = value.chars().any(|ch| ch.is_ascii_lowercase());
    let has_upper = value.chars().any(|ch| ch.is_ascii_uppercase());
    let has_digit = value.chars().any(|ch| ch.is_ascii_digit());
    let has_separator = value.contains('_') || value.contains('-');
    (has_lower && has_upper) || has_separator || (has_digit && has_upper)
}

fn is_ticket_id(value: &str) -> bool {
    let Some((prefix, number)) = value.split_once('-') else {
        return false;
    };
    prefix.len() >= 2
        && prefix.chars().all(|ch| ch.is_ascii_uppercase())
        && number.len() >= 2
        && number.chars().all(|ch| ch.is_ascii_digit())
}

fn is_path_like(value: &str) -> bool {
    value.contains('/')
        || value.ends_with(".rs")
        || value.ends_with(".ts")
        || value.ends_with(".tsx")
        || value.ends_with(".js")
        || value.ends_with(".jsx")
        || value.ends_with(".java")
        || value.ends_with(".py")
        || value.ends_with(".go")
        || value.ends_with(".md")
}

fn task_lexical_terms(task: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    for token in task
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| token.len() >= 4)
    {
        if is_task_stopword(&token) || tokens.iter().any(|existing| existing == &token) {
            continue;
        }
        tokens.push(token);
        if tokens.len() >= 8 {
            break;
        }
    }

    let mut terms = tokens.clone();
    for pair in tokens.windows(2).take(6) {
        push_unique_alias(&mut terms, &format!("{} {}", pair[0], pair[1]));
    }
    terms
}

fn is_task_stopword(token: &str) -> bool {
    matches!(
        token,
        "about"
            | "after"
            | "against"
            | "before"
            | "between"
            | "from"
            | "into"
            | "that"
            | "their"
            | "there"
            | "these"
            | "this"
            | "those"
            | "through"
            | "under"
            | "using"
            | "with"
            | "without"
    )
}

fn task_alias_terms(task: &str) -> Vec<String> {
    let tokens = task
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| token.len() >= 3)
        .collect::<Vec<_>>();
    let aliased_tokens = tokens
        .iter()
        .map(|token| task_token_alias(token))
        .collect::<Vec<_>>();
    let mut aliases = Vec::new();
    for (token, alias) in tokens.iter().zip(aliased_tokens.iter()) {
        if token != alias {
            push_unique_alias(&mut aliases, alias);
        }
    }
    for pair in tokens.windows(2).zip(aliased_tokens.windows(2)) {
        let (original, aliased) = pair;
        if original != aliased {
            push_unique_alias(&mut aliases, &aliased.join(" "));
        }
    }
    aliases
}

fn task_token_alias(token: &str) -> String {
    match token {
        "configuration" | "configurations" | "configured" | "configuring" => "config".into(),
        "defaults" => "default".into(),
        "histories" => "history".into(),
        _ => token.into(),
    }
}

fn push_unique_alias(aliases: &mut Vec<String>, alias: &str) {
    if alias.len() >= 3 && !aliases.iter().any(|existing| existing == alias) {
        aliases.push(alias.to_string());
    }
}

fn normalize_identifier(value: &str) -> String {
    let mut out = String::new();
    let mut previous_lower_or_digit = false;
    for ch in value.chars() {
        if ch == '_' || ch == '-' || ch == '/' || ch == '.' {
            out.push(' ');
            previous_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_uppercase() && previous_lower_or_digit {
            out.push(' ');
        }
        out.push(ch.to_ascii_lowercase());
        previous_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn classify_intent(task: &str) -> &'static str {
    let lower = task.to_ascii_lowercase();
    if lower.contains("fix")
        || lower.contains("add")
        || lower.contains("change")
        || lower.contains("implement")
    {
        "code_change"
    } else if lower.contains("test") {
        "validation"
    } else {
        "understanding"
    }
}

fn empty_impact(task: &str) -> open_kioku_core::ImpactReport {
    open_kioku_core::ImpactReport {
        target: task.into(),
        direct_impacts: Vec::new(),
        indirect_impacts: Vec::new(),
        risk_report: RiskReport {
            level: "unknown".into(),
            score: 0.0,
            reasons: vec!["no matching indexed files found".into()],
        },
        evidence: vec![Evidence {
            id: EvidenceId::new("context:no-match"),
            source: "open-kioku-context".into(),
            source_type: EvidenceSourceType::Lexical,
            file_range: None,
            symbol_id: None,
            confidence: Confidence::Low,
            message: "context pack search did not find indexed evidence".into(),
            indexed_at: Utc::now(),
            ..Default::default()
        }],
        architecture_policy: None,
        score_breakdown: vec![ScoreComponent::single(
            "no_context_found",
            0.0,
            vec!["context:no-match".into()],
            "no indexed context matched the task",
        )],
    }
}

fn bounded_impact(task: &str) -> open_kioku_core::ImpactReport {
    open_kioku_core::ImpactReport {
        target: task.into(),
        direct_impacts: Vec::new(),
        indirect_impacts: Vec::new(),
        risk_report: RiskReport {
            level: "low".into(),
            score: 0.1,
            reasons: vec!["bounded context built from persisted search results".into()],
        },
        evidence: vec![Evidence {
            id: EvidenceId::new("context:bounded-search"),
            source: "open-kioku-context".into(),
            source_type: EvidenceSourceType::Lexical,
            file_range: None,
            symbol_id: None,
            confidence: Confidence::Medium,
            message:
                "context pack used persisted search results without full-table impact expansion"
                    .into(),
            indexed_at: Utc::now(),
            ..Default::default()
        }],
        architecture_policy: None,
        score_breakdown: vec![ScoreComponent::single(
            "bounded_context_risk",
            0.1,
            vec!["context:bounded-search".into()],
            "bounded context used persisted search results without full impact expansion",
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{FileId, Language, LineRange, RepositoryId, SymbolId, SymbolKind};
    use std::path::Path;

    #[test]
    fn primary_edit_anchor_outranks_reference_pattern_anchor() {
        let repo_id = RepositoryId::new("repo");
        let mutation_file = File {
            id: FileId::new("mutation"),
            repository_id: repo_id.clone(),
            path: "src/PublishRestrictionsMutation.java".into(),
            language: Language::Java,
            size_bytes: 100,
            content_hash: "mutation".into(),
            is_generated: false,
            is_vendor: false,
        };
        let validator_file = File {
            id: FileId::new("validator"),
            repository_id: repo_id,
            path: "src/EnterpriseRateValidator.java".into(),
            language: Language::Java,
            size_bytes: 100,
            content_hash: "validator".into(),
            is_generated: false,
            is_vendor: false,
        };
        let mutation_symbol = Symbol {
            id: SymbolId::new("mutation-symbol"),
            name: "PublishRestrictionsMutation".into(),
            qualified_name: "api.PublishRestrictionsMutation".into(),
            kind: SymbolKind::Class,
            file_id: mutation_file.id.clone(),
            range: Some(LineRange { start: 1, end: 20 }),
            language: Language::Java,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        };
        let validator_symbol = Symbol {
            id: SymbolId::new("validator-symbol"),
            name: "EnterpriseRateValidator".into(),
            qualified_name: "api.EnterpriseRateValidator".into(),
            kind: SymbolKind::Class,
            file_id: validator_file.id.clone(),
            range: Some(LineRange { start: 1, end: 20 }),
            language: Language::Java,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        };
        let chunks = vec![
            CodeChunk {
                id: "mutation-chunk".into(),
                file_id: mutation_file.id.clone(),
                range: LineRange { start: 1, end: 10 },
                language: Language::Java,
                text: "class PublishRestrictionsMutation { void mutate() {} }".into(),
                symbol_id: Some(mutation_symbol.id.clone()),
            },
            CodeChunk {
                id: "validator-chunk".into(),
                file_id: validator_file.id.clone(),
                range: LineRange { start: 1, end: 10 },
                language: Language::Java,
                text: "class EnterpriseRateValidator { boolean validate() { return true; } }"
                    .into(),
                symbol_id: Some(validator_symbol.id.clone()),
            },
        ];
        let files = vec![mutation_file, validator_file];
        let symbols = vec![mutation_symbol, validator_symbol];
        let task =
            "add validation in PublishRestrictionsMutation similar to EnterpriseRateValidator";
        let intent = TaskSearchIntent::parse(task);
        let results = rerank_for_task(
            search_candidates(&chunks, &files, &symbols, task, 10, &intent).unwrap(),
            &intent,
            &RankingOptions::default(),
        );

        assert_eq!(
            results[0].path,
            Path::new("src/PublishRestrictionsMutation.java")
        );
        assert!(results[0]
            .evidence
            .iter()
            .any(|evidence| evidence.contains("primary task anchor")));
    }

    #[test]
    fn equal_task_relevance_prefers_exact_authority_over_higher_rrf_score() {
        let exact = SearchResult {
            path: "src/ExactTarget.rs".into(),
            line_range: None,
            snippet: "fn target() {}".into(),
            symbol: None,
            score: 0.01,
            match_reason: "authority-ordering fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let heuristic = SearchResult {
            path: "src/HeuristicTarget.rs".into(),
            line_range: None,
            snippet: "fn target() {}".into(),
            symbol: None,
            score: 10.0,
            match_reason: "authority-ordering fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let diagnostics = RetrievalDiagnostics {
            traces: vec![
                open_kioku_core::RetrievalTrace {
                    path: exact.path.clone(),
                    unit_key: None,
                    fused_score: exact.score,
                    authority: RetrievalAuthority::Exact,
                    contributions: Vec::new(),
                },
                open_kioku_core::RetrievalTrace {
                    path: heuristic.path.clone(),
                    unit_key: None,
                    fused_score: heuristic.score,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: Vec::new(),
                },
            ],
            ..Default::default()
        };
        let intent = TaskSearchIntent::parse("change target");
        let ranked = rerank_fused_for_task(vec![heuristic, exact], &intent, &diagnostics);
        assert_eq!(ranked[0].path, Path::new("src/ExactTarget.rs"));
    }

    #[test]
    fn primary_task_relevance_beats_reference_only_exact_evidence() {
        let primary = SearchResult {
            path: "src/PublishRestrictionsMutation.java".into(),
            line_range: None,
            snippet: "class PublishRestrictionsMutation {}".into(),
            symbol: None,
            score: 0.01,
            match_reason: "primary-task fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let reference = SearchResult {
            path: "src/EnterpriseRateValidator.java".into(),
            line_range: None,
            snippet: "class EnterpriseRateValidator {}".into(),
            symbol: None,
            score: 10.0,
            match_reason: "reference fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let diagnostics = RetrievalDiagnostics {
            traces: vec![
                open_kioku_core::RetrievalTrace {
                    path: primary.path.clone(),
                    unit_key: None,
                    fused_score: primary.score,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: Vec::new(),
                },
                open_kioku_core::RetrievalTrace {
                    path: reference.path.clone(),
                    unit_key: None,
                    fused_score: reference.score,
                    authority: RetrievalAuthority::Exact,
                    contributions: Vec::new(),
                },
            ],
            ..Default::default()
        };
        let intent = TaskSearchIntent::parse(
            "add validation in PublishRestrictionsMutation similar to EnterpriseRateValidator",
        );
        let ranked = rerank_fused_for_task(vec![reference, primary], &intent, &diagnostics);
        assert_eq!(
            ranked[0].path,
            Path::new("src/PublishRestrictionsMutation.java")
        );
    }

    #[test]
    fn documentation_task_target_precedes_unrelated_exact_code_authority() {
        let docs = SearchResult {
            path: "docs/guides/agent-workflows.md".into(),
            line_range: None,
            snippet: "Agent Workflows for contributors".into(),
            symbol: None,
            score: 0.01,
            match_reason: "documentation target fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let code = SearchResult {
            path: "src/ContributorEngine.rs".into(),
            line_range: None,
            snippet: "struct ContributorEngine;".into(),
            symbol: None,
            score: 10.0,
            match_reason: "exact code fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let diagnostics = RetrievalDiagnostics {
            traces: vec![
                open_kioku_core::RetrievalTrace {
                    path: docs.path.clone(),
                    unit_key: None,
                    fused_score: docs.score,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: Vec::new(),
                },
                open_kioku_core::RetrievalTrace {
                    path: code.path.clone(),
                    unit_key: None,
                    fused_score: code.score,
                    authority: RetrievalAuthority::Exact,
                    contributions: Vec::new(),
                },
            ],
            ..Default::default()
        };
        let intent = TaskSearchIntent::parse("document agent workflows for contributors");
        assert!(intent.documentation_target);
        let ranked = rerank_fused_for_task(vec![code, docs], &intent, &diagnostics);
        assert_eq!(ranked[0].path, Path::new("docs/guides/agent-workflows.md"));
    }

    #[test]
    fn non_documentation_task_does_not_promote_docs_over_exact_code() {
        let docs = SearchResult {
            path: "docs/guides/engine.md".into(),
            line_range: None,
            snippet: "Engine internals".into(),
            symbol: None,
            score: 10.0,
            match_reason: "docs fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let code = SearchResult {
            path: "src/engine.rs".into(),
            line_range: None,
            snippet: "fn engine() {}".into(),
            symbol: None,
            score: 0.01,
            match_reason: "code fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let diagnostics = RetrievalDiagnostics {
            traces: vec![
                open_kioku_core::RetrievalTrace {
                    path: docs.path.clone(),
                    unit_key: None,
                    fused_score: docs.score,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: Vec::new(),
                },
                open_kioku_core::RetrievalTrace {
                    path: code.path.clone(),
                    unit_key: None,
                    fused_score: code.score,
                    authority: RetrievalAuthority::Exact,
                    contributions: Vec::new(),
                },
            ],
            ..Default::default()
        };
        let intent = TaskSearchIntent::parse("change engine behavior");
        assert!(!intent.documentation_target);
        let ranked = rerank_fused_for_task(vec![docs, code], &intent, &diagnostics);
        assert_eq!(ranked[0].path, Path::new("src/engine.rs"));
    }

    #[test]
    fn context_pack_telemetry_counts_selected_sources_once_per_file_and_preserves_exact_authority()
    {
        let selected = vec![SearchResult {
            path: "src/a.rs".into(),
            line_range: None,
            snippet: "fn a() {}".into(),
            symbol: None,
            score: 1.0,
            match_reason: "fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 1.0,
            score_breakdown: Vec::new(),
        }];
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![open_kioku_core::RetrievalTrace {
                path: "src/a.rs".into(),
                unit_key: None,
                fused_score: 1.0,
                authority: RetrievalAuthority::Exact,
                contributions: vec![
                    open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::Lexical,
                        rank: 1,
                        raw_score: Some(1.0),
                        rrf_contribution: 0.1,
                        authority: RetrievalAuthority::Heuristic,
                        symbol_id: None,
                        evidence_refs: Vec::new(),
                        rationale: "lexical fixture".into(),
                    },
                    open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::Lexical,
                        rank: 2,
                        raw_score: Some(0.9),
                        rrf_contribution: 0.09,
                        authority: RetrievalAuthority::Heuristic,
                        symbol_id: None,
                        evidence_refs: Vec::new(),
                        rationale: "lexical fixture".into(),
                    },
                    open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::ExactSemantic,
                        rank: 1,
                        raw_score: Some(1.0),
                        rrf_contribution: 0.1,
                        authority: RetrievalAuthority::Exact,
                        symbol_id: None,
                        evidence_refs: vec!["symbol:a".into()],
                        rationale: "exact semantic fixture".into(),
                    },
                ],
            }],
            caveats: vec![
                "ambiguous exact symbol anchor".into(),
                "unresolved import reduced graph confidence".into(),
            ],
            selection: open_kioku_core::ContextSelectionDiagnostics {
                budget: ContextBudget::from_file_limit(10),
                available_context_tokens: 1_000,
                estimated_tokens_selected: 100,
                ..Default::default()
            },
            ..Default::default()
        };
        let confidence = ConfidenceBreakdown {
            overall_enum: Confidence::High,
            overall_score: 0.85,
            ..Default::default()
        };

        refresh_context_pack_retrieval_telemetry(&mut diagnostics, &selected, &confidence);

        assert_eq!(diagnostics.selection.exact_evidence_count, 1);
        assert_eq!(diagnostics.selection.ambiguity_unresolved_count, 2);
        assert_eq!(
            diagnostics.selection.retrieval_confidence,
            Some(Confidence::High)
        );
        assert_eq!(diagnostics.selection.abstention_reason, None);
        assert_eq!(diagnostics.selection.source_stream_mix.len(), 2);
        assert_eq!(diagnostics.selection.unattributed_selected_file_count, 0);
        assert_eq!(
            diagnostics
                .selection
                .source_stream_mix
                .iter()
                .find(|entry| entry.source == RetrievalSourceKind::Lexical)
                .map(|entry| entry.selected_file_count),
            Some(1)
        );
    }

    #[test]
    fn context_pack_telemetry_fails_closed_when_selected_file_lacks_trace_attribution() {
        let selected = vec![SearchResult {
            path: "src/external.rs".into(),
            line_range: None,
            snippet: "fn external() {}".into(),
            symbol: None,
            score: 1.0,
            match_reason: "externally supplied primary".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        }];
        let mut diagnostics = RetrievalDiagnostics::default();
        let confidence = ConfidenceBreakdown::default();

        refresh_context_pack_retrieval_telemetry(&mut diagnostics, &selected, &confidence);

        assert_eq!(diagnostics.selection.unattributed_selected_file_count, 1);
        assert!(diagnostics
            .selection
            .caveats
            .iter()
            .any(|caveat| caveat.contains("lack retrieval-trace source attribution")));
        assert!(diagnostics.selection.source_stream_mix.is_empty());
    }

    #[test]
    fn context_pack_telemetry_abstains_explicitly_when_no_candidate_survives_selection() {
        let mut diagnostics = RetrievalDiagnostics {
            selection: open_kioku_core::ContextSelectionDiagnostics {
                budget: ContextBudget {
                    max_tokens: 100,
                    reserve_for_instructions: 100,
                    ..ContextBudget::from_file_limit(10)
                },
                available_context_tokens: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let confidence = ConfidenceBreakdown::default();

        refresh_context_pack_retrieval_telemetry(&mut diagnostics, &[], &confidence);

        assert_eq!(
            diagnostics.selection.abstention_reason.as_deref(),
            Some("context_budget_exhausted")
        );
        assert_eq!(
            diagnostics.selection.retrieval_confidence,
            Some(Confidence::Low)
        );
    }

    #[test]
    fn retrieval_unit_provenance_does_not_bleed_between_sections_of_same_file() {
        let heuristic = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(open_kioku_core::LineRange { start: 1, end: 10 }),
            snippet: "heuristic section".into(),
            symbol: None,
            score: 1.0,
            match_reason: "fixture".into(),
            evidence: vec!["document section".into()],
            evidence_refs: vec!["doc:section:one".into()],
            confidence: 0.6,
            score_breakdown: Vec::new(),
        };
        let exact = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(open_kioku_core::LineRange { start: 20, end: 30 }),
            snippet: "other exact section".into(),
            symbol: None,
            score: 2.0,
            match_reason: "fixture".into(),
            evidence: vec!["exact fixture".into()],
            evidence_refs: vec!["symbol:exact-other-section".into()],
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let heuristic_key = RetrievalUnitKey::from_result(&heuristic);
        let exact_key = RetrievalUnitKey::from_result(&exact);
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![
                RetrievalTrace {
                    path: heuristic.path.clone(),
                    unit_key: Some(heuristic_key),
                    fused_score: 1.0,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: vec![open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::Document,
                        rank: 1,
                        raw_score: Some(1.0),
                        rrf_contribution: 1.0,
                        authority: RetrievalAuthority::Heuristic,
                        symbol_id: None,
                        evidence_refs: heuristic.evidence_refs.clone(),
                        rationale: "document section".into(),
                    }],
                },
                RetrievalTrace {
                    path: exact.path.clone(),
                    unit_key: Some(exact_key),
                    fused_score: 2.0,
                    authority: RetrievalAuthority::Exact,
                    contributions: vec![open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::ExactSemantic,
                        rank: 1,
                        raw_score: Some(2.0),
                        rrf_contribution: 2.0,
                        authority: RetrievalAuthority::Exact,
                        symbol_id: None,
                        evidence_refs: exact.evidence_refs.clone(),
                        rationale: "exact other section".into(),
                    }],
                },
            ],
            ..Default::default()
        };

        assert_eq!(
            retrieval_authority_for_result(&diagnostics, &heuristic),
            RetrievalAuthority::Heuristic
        );
        assert_eq!(
            retrieval_sources_for_result(&diagnostics, &heuristic),
            std::collections::BTreeSet::from([RetrievalSourceKind::Document])
        );

        diagnostics.selection.budget.max_tokens = 100;
        diagnostics.selection.available_context_tokens = 100;
        refresh_context_pack_retrieval_telemetry(
            &mut diagnostics,
            std::slice::from_ref(&heuristic),
            &ConfidenceBreakdown::default(),
        );
        assert_eq!(diagnostics.selection.exact_evidence_count, 0);
        assert_eq!(diagnostics.selection.unattributed_selected_file_count, 0);
        assert_eq!(diagnostics.selection.source_stream_mix.len(), 1);
        assert_eq!(
            diagnostics.selection.source_stream_mix[0].source,
            RetrievalSourceKind::Document
        );
    }

    #[test]
    fn ambiguous_legacy_same_path_traces_fail_closed_for_unit_attribution() {
        let result = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(open_kioku_core::LineRange { start: 1, end: 10 }),
            snippet: "section".into(),
            symbol: None,
            score: 1.0,
            match_reason: "fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let diagnostics = RetrievalDiagnostics {
            traces: vec![
                RetrievalTrace {
                    path: result.path.clone(),
                    unit_key: None,
                    fused_score: 1.0,
                    authority: RetrievalAuthority::Exact,
                    contributions: Vec::new(),
                },
                RetrievalTrace {
                    path: result.path.clone(),
                    unit_key: None,
                    fused_score: 0.5,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: Vec::new(),
                },
            ],
            ..Default::default()
        };
        assert!(retrieval_trace_for_result(&diagnostics, &result).is_none());
        assert_eq!(
            retrieval_authority_for_result(&diagnostics, &result),
            RetrievalAuthority::Heuristic
        );
    }

    #[test]
    fn trace_to_code_missing_runtime_evidence_blocks_without_heuristic_substitution() {
        let routing = routing::classify_task("investigate runtime error stack trace in checkout");
        assert_eq!(routing.family, open_kioku_core::TaskFamily::TraceToCode);
        let mut diagnostics = RetrievalDiagnostics::default();
        diagnostics.routing = routing.diagnostics();
        let budget = ContextBudget::from_file_limit(10);

        assert!(apply_required_evidence_policy(
            &routing.policy,
            &budget,
            &mut diagnostics
        ));
        assert_eq!(
            diagnostics.selection.abstention_reason.as_deref(),
            Some("missing_required_evidence:runtime")
        );
        assert!(diagnostics
            .caveats
            .iter()
            .any(|caveat| caveat.contains("blocking requirement") && caveat.contains("runtime")));

        let confidence = ConfidenceBreakdown::default();
        refresh_context_pack_retrieval_telemetry(&mut diagnostics, &[], &confidence);
        assert_eq!(
            diagnostics.selection.abstention_reason.as_deref(),
            Some("missing_required_evidence:runtime")
        );
    }

    #[test]
    fn edit_to_ripple_missing_exact_and_graph_evidence_blocks_deterministically() {
        let routing =
            routing::classify_task("show dependency ripple across callers and public API boundary");
        assert_eq!(routing.family, open_kioku_core::TaskFamily::EditToRipple);
        let mut diagnostics = RetrievalDiagnostics::default();
        diagnostics.routing = routing.diagnostics();
        let budget = ContextBudget::from_file_limit(10);

        assert!(apply_required_evidence_policy(
            &routing.policy,
            &budget,
            &mut diagnostics
        ));
        assert_eq!(
            diagnostics.selection.abstention_reason.as_deref(),
            Some("missing_required_evidence:exact_semantic,graph")
        );
    }

    #[test]
    fn non_blocking_issue_to_code_missing_lexical_evidence_remains_a_caveat() {
        let routing = routing::classify_task("fix issue with frobnication behavior");
        assert_eq!(routing.family, open_kioku_core::TaskFamily::IssueToCode);
        assert!(!routing.policy.missing_required_evidence_is_blocker);
        let mut diagnostics = RetrievalDiagnostics::default();
        diagnostics.routing = routing.diagnostics();
        let budget = ContextBudget::from_file_limit(10);

        assert!(!apply_required_evidence_policy(
            &routing.policy,
            &budget,
            &mut diagnostics
        ));
        assert!(diagnostics.selection.abstention_reason.is_none());
        assert!(diagnostics
            .caveats
            .iter()
            .any(|caveat| caveat.contains("required evidence") && caveat.contains("lexical")));
    }

    #[test]
    fn compact_retrieval_diagnostics_surface_sources_and_caveats() {
        let diagnostics = RetrievalDiagnostics {
            sources_attempted: vec![
                RetrievalSourceKind::Lexical,
                RetrievalSourceKind::SemanticVector,
            ],
            sources_succeeded: vec![RetrievalSourceKind::Lexical],
            caveats: vec!["semantic index is stale".into()],
            traces: Vec::new(),
            selection: Default::default(),
            routing: Default::default(),
        };
        let mut markdown = String::new();
        write_markdown_retrieval_diagnostics(&mut markdown, &diagnostics);
        assert!(markdown.contains("## Retrieval"));
        assert!(markdown.contains("lexical, semantic_vector"));
        assert!(markdown.contains("semantic index is stale"));

        let mut prompt = String::new();
        write_prompt_retrieval_diagnostics(&mut prompt, &diagnostics);
        assert!(prompt.contains("RETRIEVAL_SOURCES_ATTEMPTED: lexical, semantic_vector"));
        assert!(prompt.contains("RETRIEVAL_CAVEAT: semantic index is stale"));
        assert!(!prompt.contains("fused_score"));
    }

    #[test]
    fn post_fusion_quality_tier_preserves_boundary_and_path_quality_policy() {
        let options = RankingOptions::default();
        assert_eq!(
            context_quality_tier(Path::new("src/service.rs"), &options),
            2
        );
        assert_eq!(
            context_quality_tier(Path::new("tests/service_test.rs"), &options),
            1
        );
        assert_eq!(
            context_quality_tier(Path::new("src/generated/service.rs"), &options),
            0
        );
        assert_eq!(
            context_quality_tier(Path::new("vendor/service.rs"), &options),
            0
        );

        let baseline = RankingOptions {
            mode: open_kioku_ranking::RankingMode::Baseline,
            ..RankingOptions::default()
        };
        assert_eq!(
            context_quality_tier(Path::new("src/generated/service.rs"), &baseline),
            2
        );

        let without_path_quality = RankingOptions {
            mode: open_kioku_ranking::RankingMode::WithoutSignal(
                open_kioku_ranking::RankingSignal::PathQuality,
            ),
            ..RankingOptions::default()
        };
        assert_eq!(
            context_quality_tier(Path::new("src/generated/service.rs"), &without_path_quality,),
            2
        );
    }

    #[test]
    fn token_budget_prevents_one_large_heuristic_unit_from_monopolizing_context() {
        let huge = SearchResult {
            path: "src/huge.rs".into(),
            line_range: Some(LineRange { start: 1, end: 400 }),
            snippet: "large implementation block ".repeat(500),
            symbol: None,
            score: 10.0,
            match_reason: "heuristic".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let compact = SearchResult {
            path: "src/compact.rs".into(),
            line_range: Some(LineRange { start: 10, end: 20 }),
            snippet: "fn compact_target() { validate(); }".into(),
            symbol: None,
            score: 5.0,
            match_reason: "heuristic".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics::default();
        let budget = ContextBudget {
            max_tokens: 800,
            reserve_for_instructions: 100,
            reserve_for_validation: 100,
            max_per_file: 2,
            max_primary_files: 4,
        };

        let selected = select_context_units(vec![huge, compact.clone()], &budget, &mut diagnostics);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].path, compact.path);
        assert!(!diagnostics.selection.omitted_due_to_budget.is_empty());
        assert!(diagnostics.selection.estimated_tokens_selected <= 600);
    }

    #[test]
    fn exact_evidence_is_considered_before_cheaper_heuristics_under_budget() {
        let heuristic = SearchResult {
            path: "src/cheap.rs".into(),
            line_range: None,
            snippet: "cheap candidate".into(),
            symbol: None,
            score: 100.0,
            match_reason: "heuristic".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let exact = SearchResult {
            path: "src/exact.rs".into(),
            line_range: Some(LineRange { start: 20, end: 24 }),
            snippet: "fn exact_target() {}".into(),
            symbol: None,
            score: 0.01,
            match_reason: "exact".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["symbol:exact".into()],
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![open_kioku_core::RetrievalTrace {
                path: exact.path.clone(),
                unit_key: None,
                fused_score: exact.score,
                authority: RetrievalAuthority::Exact,
                contributions: Vec::new(),
            }],
            ..Default::default()
        };
        let budget = ContextBudget {
            max_tokens: 300,
            reserve_for_instructions: 100,
            reserve_for_validation: 100,
            max_per_file: 2,
            max_primary_files: 1,
        };

        let selected =
            select_context_units(vec![heuristic, exact.clone()], &budget, &mut diagnostics);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].path, exact.path);
    }

    #[test]
    fn exact_evidence_cannot_be_displaced_by_many_cheaper_heuristics() {
        let exact = SearchResult {
            path: "src/exact_target.rs".into(),
            line_range: Some(LineRange { start: 20, end: 24 }),
            snippet: "fn exact_target() { validate_boundary(); }".into(),
            symbol: None,
            score: 0.01,
            match_reason: "exact symbol evidence".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["symbol:exact-target".into()],
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let mut ranked = (0..8)
            .map(|index| SearchResult {
                path: format!("src/cheap_{index}.rs").into(),
                line_range: None,
                snippet: "tiny semantic candidate".into(),
                symbol: None,
                score: 100.0 - index as f32,
                match_reason: "heuristic semantic similarity".into(),
                evidence: Vec::new(),
                evidence_refs: Vec::new(),
                confidence: 0.5,
                score_breakdown: Vec::new(),
            })
            .collect::<Vec<_>>();
        ranked.push(exact.clone());
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![open_kioku_core::RetrievalTrace {
                path: exact.path.clone(),
                unit_key: None,
                fused_score: exact.score,
                authority: RetrievalAuthority::Exact,
                contributions: Vec::new(),
            }],
            ..Default::default()
        };
        let budget = ContextBudget {
            max_tokens: 300,
            reserve_for_instructions: 100,
            reserve_for_validation: 100,
            max_per_file: 2,
            max_primary_files: 1,
        };
        let selected = select_context_units(ranked, &budget, &mut diagnostics);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].path, exact.path);
    }

    #[test]
    fn token_selection_preserves_document_section_range_and_dedupes_redundant_units() {
        let first = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(LineRange { start: 40, end: 55 }),
            snippet: "configure agent workflow validation boundary evidence".into(),
            symbol: None,
            score: 2.0,
            match_reason: "document section".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["document:guide:section".into()],
            confidence: 0.7,
            score_breakdown: Vec::new(),
        };
        let duplicate = SearchResult {
            path: "docs/copy.md".into(),
            line_range: Some(LineRange { start: 1, end: 8 }),
            snippet: first.snippet.clone(),
            symbol: None,
            score: 1.0,
            match_reason: "document section".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["document:copy:section".into()],
            confidence: 0.6,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics::default();
        let budget = ContextBudget {
            max_tokens: 1_000,
            reserve_for_instructions: 100,
            reserve_for_validation: 100,
            max_per_file: 2,
            max_primary_files: 4,
        };

        let selected =
            select_context_units(vec![first.clone(), duplicate], &budget, &mut diagnostics);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].path, first.path);
        assert_eq!(selected[0].line_range, first.line_range);
        assert_eq!(diagnostics.selection.redundancy_omissions.len(), 1);
        assert_eq!(diagnostics.selection.selected_units.len(), 1);
        assert_eq!(
            diagnostics.selection.selected_units[0].line_range,
            first.line_range
        );
        assert_eq!(
            diagnostics.selection.selected_units[0].evidence_refs,
            first.evidence_refs
        );
    }

    #[test]
    fn default_retrieval_diagnostics_do_not_claim_a_budget_was_applied() {
        let diagnostics = RetrievalDiagnostics::default();
        assert_eq!(diagnostics.selection.budget.max_tokens, 0);
        assert_eq!(diagnostics.selection.available_context_tokens, 0);
        assert!(diagnostics.selection.selected_units.is_empty());
    }

    #[test]
    fn explicit_budget_enforces_context_unit_cap_per_file() {
        let first = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(LineRange { start: 1, end: 10 }),
            snippet: "first distinct section about setup".into(),
            symbol: None,
            score: 2.0,
            match_reason: "section one".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["doc:first".into()],
            confidence: 0.7,
            score_breakdown: Vec::new(),
        };
        let second = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(LineRange { start: 40, end: 50 }),
            snippet: "second distinct section about deployment".into(),
            symbol: None,
            score: 1.0,
            match_reason: "section two".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["doc:second".into()],
            confidence: 0.6,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics::default();
        let budget = ContextBudget {
            max_tokens: 1_000,
            reserve_for_instructions: 100,
            reserve_for_validation: 100,
            max_per_file: 1,
            max_primary_files: 4,
        };

        let selected = select_context_units(vec![first, second], &budget, &mut diagnostics);
        assert_eq!(selected.len(), 1);
        assert_eq!(diagnostics.selection.omitted_due_to_caps.len(), 1);
    }

    #[test]
    fn expanded_task_search_terms_include_config_aliases() {
        let terms = expanded_task_search_terms("add history configuration defaults");

        assert!(terms.iter().any(|term| term == "config"));
        assert!(terms.iter().any(|term| term == "default"));
        assert!(terms.iter().any(|term| term == "history config"));
        assert!(terms.iter().any(|term| term == "config default"));
    }

    #[test]
    fn natural_language_workflow_terms_retrieve_patch_verifier_context() {
        let repo_id = RepositoryId::new("repo");
        let patch_file = File {
            id: FileId::new("patch"),
            repository_id: repo_id.clone(),
            path: "crates/open-kioku-patch/src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "patch".into(),
            is_generated: false,
            is_vendor: false,
        };
        let noise_file = File {
            id: FileId::new("noise"),
            repository_id: repo_id,
            path: "crates/open-kioku-cli/src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "noise".into(),
            is_generated: false,
            is_vendor: false,
        };
        let patch_symbol = Symbol {
            id: SymbolId::new("change-verifier"),
            name: "ChangeVerifier".into(),
            qualified_name: "open_kioku_patch::ChangeVerifier".into(),
            kind: SymbolKind::Class,
            file_id: patch_file.id.clone(),
            range: Some(LineRange { start: 1, end: 8 }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        };
        let chunks = vec![
            CodeChunk {
                id: "patch-chunk".into(),
                file_id: patch_file.id.clone(),
                range: LineRange { start: 1, end: 8 },
                language: Language::Rust,
                text: "pub struct ChangeVerifier; impl ChangeVerifier { fn verify(&self, changed_files: Vec<PathBuf>, plan: &PlanReport) {} }".into(),
                symbol_id: Some(patch_symbol.id.clone()),
            },
            CodeChunk {
                id: "noise-chunk".into(),
                file_id: noise_file.id.clone(),
                range: LineRange { start: 1, end: 4 },
                language: Language::Rust,
                text: "fn save_workspace_files() {}".into(),
                symbol_id: None,
            },
        ];
        let files = vec![patch_file, noise_file];
        let symbols = vec![patch_symbol];
        let task = "verify changed files against saved plans";
        let intent = TaskSearchIntent::parse(task);
        let results = rerank_for_task(
            search_candidates(&chunks, &files, &symbols, task, 10, &intent).unwrap(),
            &intent,
            &RankingOptions::default(),
        );

        assert_eq!(
            results.first().map(|result| result.path.as_path()),
            Some(Path::new("crates/open-kioku-patch/src/lib.rs"))
        );
    }

    #[test]
    fn configuration_alias_keeps_config_crate_in_context_candidates() {
        let repo_id = RepositoryId::new("repo");
        let config_file = File {
            id: FileId::new("config"),
            repository_id: repo_id.clone(),
            path: "crates/open-kioku-config/src/lib.rs".into(),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "config".into(),
            is_generated: false,
            is_vendor: false,
        };
        let history_file = File {
            id: FileId::new("history"),
            repository_id: repo_id,
            path: "crates/open-kioku-git/benches/history.rs".into(),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "history".into(),
            is_generated: false,
            is_vendor: false,
        };
        let config_symbol = Symbol {
            id: SymbolId::new("default-history-max-commits"),
            name: "default_history_max_commits".into(),
            qualified_name: "open_kioku_config::default_history_max_commits".into(),
            kind: SymbolKind::Function,
            file_id: config_file.id.clone(),
            range: Some(LineRange { start: 1, end: 4 }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        };
        let history_symbol = Symbol {
            id: SymbolId::new("benchmark-history-ingest"),
            name: "benchmark_history_ingest".into(),
            qualified_name: "open_kioku_git::benchmark_history_ingest".into(),
            kind: SymbolKind::Function,
            file_id: history_file.id.clone(),
            range: Some(LineRange { start: 1, end: 4 }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
        };
        let chunks = vec![
            CodeChunk {
                id: "config-chunk".into(),
                file_id: config_file.id.clone(),
                range: LineRange { start: 1, end: 4 },
                language: Language::Rust,
                text: "fn default_history_max_commits() -> usize { 100 }".into(),
                symbol_id: Some(config_symbol.id.clone()),
            },
            CodeChunk {
                id: "history-chunk".into(),
                file_id: history_file.id.clone(),
                range: LineRange { start: 1, end: 4 },
                language: Language::Rust,
                text: "fn benchmark_history_ingest() { /* add history configuration defaults */ }"
                    .into(),
                symbol_id: Some(history_symbol.id.clone()),
            },
        ];
        let files = vec![config_file, history_file];
        let symbols = vec![config_symbol, history_symbol];
        let task = "add history configuration defaults";
        let intent = TaskSearchIntent::parse(task);
        let results = rerank_for_task(
            search_candidates(&chunks, &files, &symbols, task, 10, &intent).unwrap(),
            &intent,
            &RankingOptions::default(),
        );

        assert!(
            results
                .iter()
                .take(8)
                .any(|result| result.path == Path::new("crates/open-kioku-config/src/lib.rs")),
            "config crate should stay in the planner-visible context: {results:#?}"
        );
    }
}

#[cfg(test)]
mod ri3_context_dependency_authority_tests {
    use super::is_trusted_context_dependency_edge;
    use open_kioku_core::{GraphEdge, GraphEdgeType, RelationshipProof, RelationshipProofKind};

    #[test]
    fn proof_gated_context_edges_fail_closed_but_ordinary_graph_structure_remains_available() {
        let ordinary = GraphEdge {
            edge_type: GraphEdgeType::Defines,
            ..GraphEdge::default()
        };
        assert!(is_trusted_context_dependency_edge(&ordinary));

        let mut import = GraphEdge {
            edge_type: GraphEdgeType::Imports,
            ..GraphEdge::default()
        };
        assert!(!is_trusted_context_dependency_edge(&import));

        let proof = RelationshipProof::new(
            RelationshipProofKind::ModuleOrPackageBinding,
            "test_static_import",
            1,
        );
        import.set_relationship_proofs(vec![proof]).unwrap();
        assert!(is_trusted_context_dependency_edge(&import));
    }
}
