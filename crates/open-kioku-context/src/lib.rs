use chrono::Utc;
use open_kioku_core::{
    AnalysisFact, ChangeBoundary, CodeChunk, Confidence, ConfidenceBreakdown,
    ConfidenceSignalInput, ContextBudget, ContextPack, ContextSelectedUnit, ContextUnitKind,
    Evidence, EvidenceId, EvidenceSourceType, File, FileRange, GraphEdge, GraphEdgeType,
    GraphNodeType, HistorySignalQuery, NegativeEvidence, RetrievalAuthority, RetrievalDiagnostics,
    RetrievalSourceCount, RetrievalSourceKind, RetrievalTrace, RetrievalUnitKey, RiskReport,
    RuntimeSignal, ScoreComponent, SearchResult, Symbol, ValidationPlan,
};
use open_kioku_errors::Result;
use open_kioku_impact::ImpactEngine;
use open_kioku_ranking::{rerank_with_options, RankingOptions};
use open_kioku_search_regex::search_chunks;
use open_kioku_storage::{HistoryStore, OkStore, SearchIndex};
use open_kioku_tests::TestSelector;

pub mod candidates;
mod lattice;
mod region;
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
        // Not a dependency at all, on the same reasoning that keeps it out of `dependency_path`
        // and `module_dependencies`. Falling through to the permissive arm would have admitted a
        // proofless naming pairing on easier terms than an `Imports` edge that failed resolution.
        GraphEdgeType::DerivedFrom => false,
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
        .chain(diagnostics.selection.caveats.iter())
        .filter_map(|caveat| {
            let caveat = caveat.to_ascii_lowercase();
            (caveat.contains("ambiguous") || caveat.contains("unresolved")).then_some(caveat)
        })
        .collect::<std::collections::BTreeSet<_>>()
        .len();
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

fn has_selection_retrieval_telemetry(diagnostics: &RetrievalDiagnostics) -> bool {
    let selection = &diagnostics.selection;
    !selection.source_stream_mix.is_empty()
        || selection.exact_evidence_count > 0
        || selection.ambiguity_unresolved_count > 0
        || selection.unattributed_selected_file_count > 0
        || selection.retrieval_confidence.is_some()
        || selection.abstention_reason.is_some()
        || !selection.omitted_high_value.is_empty()
}

fn write_markdown_retrieval_diagnostics(out: &mut String, diagnostics: &RetrievalDiagnostics) {
    let has_selection_telemetry = has_selection_retrieval_telemetry(diagnostics);
    if diagnostics.sources_attempted.is_empty()
        && diagnostics.caveats.is_empty()
        && diagnostics.selection.caveats.is_empty()
        && !has_selection_telemetry
    {
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
    }
    if diagnostics.selection.budget.max_tokens > 0 || has_selection_telemetry {
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
    if !diagnostics.selection.caveats.is_empty() {
        out.push_str("- Selection caveats:\n");
        for caveat in &diagnostics.selection.caveats {
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
    }
    if diagnostics.selection.budget.max_tokens > 0 || has_selection_retrieval_telemetry(diagnostics)
    {
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
    for caveat in &diagnostics.selection.caveats {
        out.push_str(&format!("RETRIEVAL_SELECTION_CAVEAT: {caveat}\n"));
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
    /// Lexical index for impact expansion. Without it the impact engine scans every chunk in
    /// the store in memory, which is the dominant cost of a context pack on large repositories.
    search_index: Option<&'a dyn SearchIndex>,
    ranking_options: RankingOptions,
    abstention_policy: Option<open_kioku_core::abstention::RuntimeAbstentionPolicy>,
}

pub fn expanded_task_search_terms(task: &str) -> Vec<String> {
    TaskSearchIntent::parse(task).search_terms(task)
}

/// The candidate request the context builder builds for `task`, identifier-lattice terms
/// included. Benchmarks must construct requests the way production does, or they measure a
/// retrieval path the product does not ship.
pub fn task_candidate_request(
    task: &str,
    limit: usize,
    files: &[File],
    symbols: &[Symbol],
) -> candidates::CandidateRequest {
    let intent = TaskSearchIntent::parse(task).with_repository_vocabulary(files, symbols);
    candidates::CandidateRequest::new(task, intent.search_terms(task), limit)
        .with_lattice_terms(intent.lattice_terms())
}

impl<'a> ContextPackBuilder<'a> {
    pub fn new(store: &'a dyn OkStore) -> Self {
        Self {
            store,
            history_store: None,
            search_index: None,
            ranking_options: RankingOptions::default(),
            abstention_policy: None,
        }
    }

    pub fn with_search_index(mut self, search_index: Option<&'a dyn SearchIndex>) -> Self {
        self.search_index = search_index;
        self
    }

    /// Activate a calibrated abstention policy (CC6). Callers must only pass a policy from
    /// an activation artifact that passed the fail-closed readiness gate; without one the
    /// feature stays off and packs are never suppressed.
    pub fn with_abstention_policy(
        mut self,
        policy: Option<open_kioku_core::abstention::RuntimeAbstentionPolicy>,
    ) -> Self {
        self.abstention_policy = policy;
        self
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
        let intent = TaskSearchIntent::parse(task).with_repository_vocabulary(&files, &symbols);
        let routing = routing::classify_task(task);
        let candidate_limit = routing.policy.request_limit(limit).clamp(20, 200);
        let (path_prefixes, scope_caveats) =
            validated_candidate_path_scope(&intent.path_anchors, &files);
        let request =
            candidates::CandidateRequest::new(task, intent.search_terms(task), candidate_limit)
                .with_path_prefixes(path_prefixes)
                .with_lattice_terms(intent.lattice_terms());
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
        // Generated files go to the back of every stream before the cap is applied: they are
        // indexed so an agent can read them, but a `modeling_*.py` regenerated from its modular
        // twin matches the same vocabulary and, competing for the lexical stream's slots, pushed
        // the real module out of the pool before fusion ever ranked it (the Python ML library
        // holdout read MRR 0.565 -> 0.552 with generated files admitted to the streams unranked).
        let generated: std::collections::BTreeSet<String> = files
            .iter()
            .filter(|file| file.is_generated)
            .map(|file| normalize_path(&file.path))
            .collect();
        for stream in &mut streams {
            if !generated.is_empty() {
                let (source, derived): (Vec<_>, Vec<_>) =
                    stream.candidates.drain(..).partition(|candidate| {
                        !is_generated_result(&candidate.result.path, &generated)
                    });
                stream.candidates = source;
                stream.candidates.extend(derived);
            }
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
        diagnostics.caveats.extend(intent.vocabulary_caveats());
        diagnostics.caveats.sort();
        diagnostics.caveats.dedup();
        let blocked = apply_required_evidence_policy(&routing.policy, &budget, &mut diagnostics);
        let primary = if blocked {
            Vec::new()
        } else {
            let mut results = fused.results;
            append_scope_entry_points(&mut results, &files, &chunks, &intent);
            let ranked = rerank_fused_for_task_with_files(
                results,
                &intent,
                &diagnostics,
                &self.ranking_options,
                &generated,
            );
            // Derived siblings join the ranked list before selection, so the budget sees them
            // and widening below still receives the full list it ranks against.
            let ranked = admit_derived_siblings(
                ranked,
                &files,
                &chunks,
                &intent,
                &self.ranking_options,
                limit,
                &mut diagnostics,
                &mut |path| derived_siblings(self.store, path),
            );
            let selected = select_context_units(ranked.clone(), &budget, &mut diagnostics);
            // Widening runs after selection and only grows or appends units, so it cannot
            // remove or reorder what selection chose.
            let selected = region::widen_selected_regions(
                selected,
                &ranked,
                &files,
                &chunks,
                &symbols,
                &budget,
                &mut diagnostics,
            );
            record_selected_units(&selected, &mut diagnostics);
            selected
        };
        // Selection already bounded the units to `limit`; widening only adds units of files
        // that are already in the pack, and the primary bound must not cut a lower-ranked
        // file's unit to make room for them.
        let primary_limit = limit.max(primary.len());
        self.build_from_primary_with_impact(task, primary_limit, primary, true, false, diagnostics)
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
        // Materialize the caller-visible primary selection once. Downstream authority must be
        // derived only from evidence that survived the primary limit; hidden retrieval candidates
        // cannot widen symbols, dependency seeds, or the allowed edit boundary.
        let mut primary_files = bounded_primary_results(primary, limit);
        let primary_symbols = primary_files
            .iter()
            .filter_map(|result| result.symbol.clone())
            .take(10)
            .collect::<Vec<_>>();
        let impact = if expand_impact {
            if let Some(first) = primary_files.first() {
                ImpactEngine::new(self.store as &dyn open_kioku_storage::MetadataStore)
                    .with_search_index(self.search_index)
                    .with_history_store(self.history_store)
                    .with_graph_store(Some(self.store as &dyn open_kioku_storage::GraphStore))
                    .for_file(&first.path)?
            } else {
                empty_impact(task)
            }
        } else if primary_files.is_empty() {
            empty_impact(task)
        } else {
            bounded_impact(task)
        };

        let mut dependency_edges: Vec<GraphEdge> = Vec::new();
        for result in primary_files.iter().take(5) {
            let node_id = format!("file:{}", result.path.display());
            if let Ok((_nodes, edges)) = self.store.neighbors(&node_id, 20) {
                dependency_edges
                    .extend(edges.into_iter().filter(is_trusted_context_dependency_edge));
            }
        }
        dependency_edges.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        dependency_edges.dedup_by(|a, b| a.id == b.id);
        dependency_edges.truncate(50);

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
        annotate_results_with_git_history(self.store, self.history_store, &mut primary_files)?;
        annotate_results_with_git_history(self.store, self.history_store, &mut supporting_files)?;

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
                            path: result.path.as_path().into(),
                            line_range: Some(lr),
                        }),
                    symbol_id: result.symbol.as_ref().map(|s| s.id.clone()),
                    confidence: Confidence::Medium,
                    message: msg.clone().into(),
                    indexed_at: Utc::now(),
                    ..Default::default()
                })
            })
            .chain(impact.evidence.clone())
            .chain(runtime_evidence.clone())
            .chain(git_evidence)
            .collect::<Vec<_>>();
        let allowed_files = primary_files
            .iter()
            .take(8)
            .map(|result| result.path.clone())
            .collect::<Vec<_>>();
        let mut confidence_breakdown = confidence_for_context(ContextConfidenceInputs {
            task,
            primary_files: &primary_files,
            supporting_files: &supporting_files,
            tests: &tests,
            risk: &impact.risk_report,
            allowed_file_count: allowed_files.len(),
            evidence_count: evidence.len(),
            runtime_signal_count_value: runtime_signals.len(),
        });
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
        append_supporting_units(&supporting_files, &mut retrieval_diagnostics);
        let confidence_summary = confidence_summary(&confidence_breakdown);
        let mut pack = ContextPack {
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
        };
        crate::apply_calibrated_abstention(self.abstention_policy.as_ref(), &mut pack);
        Ok(pack)
    }
}

/// Apply an activated CC6 abstention policy to a completed pack.
///
/// Deterministic routing-contract blockers keep precedence; a pack whose selection
/// already carries an abstention reason is left untouched. Signal derivation is
/// fail-closed: when exact trace provenance is unavailable the policy does not run,
/// because a decision it cannot justify is worse than no decision.
pub fn apply_calibrated_abstention(
    policy: Option<&open_kioku_core::abstention::RuntimeAbstentionPolicy>,
    pack: &mut open_kioku_core::ContextPack,
) {
    use open_kioku_core::abstention::{
        derive_abstention_signals, CALIBRATED_ABSTENTION_REASON_PREFIX,
    };
    let Some(policy) = policy else {
        return;
    };
    if pack
        .retrieval_diagnostics
        .selection
        .abstention_reason
        .is_some()
    {
        return;
    }
    let Some(signals) = derive_abstention_signals(pack) else {
        return;
    };
    if !policy.should_abstain(&signals) {
        return;
    }
    let explain = policy.explain(&signals);
    pack.retrieval_diagnostics.selection.abstention_reason =
        Some(format!("{CALIBRATED_ABSTENTION_REASON_PREFIX}: {explain}"));
    pack.retrieval_diagnostics.caveats.push(format!(
        "calibrated abstention: the selected context did not meet the calibrated evidence gates ({explain}); treat this pack as insufficient evidence, not as an answer"
    ));
    pack.confidence_summary = format!(
        "Calibrated abstention: {explain}. {}",
        pack.confidence_summary
    );

    // Abstaining means returning nothing, not returning everything with a
    // disclaimer attached. This previously set the reason and the caveat above
    // and then handed over every selected file unchanged - a pack whose own
    // caveat read "not as an answer" while carrying the answer.
    //
    // The shape below is the contract the deterministic no-match path already
    // satisfies and that `cc6_abstention_smoke` already asserts: no primary or
    // supporting context, no derived edges, signals, tests, boundary or
    // validation plan. Diagnostics, caveats, evidence and negative evidence are
    // deliberately kept - they are the explanation for the abstention, and
    // discarding them would leave a caller unable to tell an abstention from an
    // empty repository.
    pack.primary_files.clear();
    pack.primary_symbols.clear();
    pack.supporting_files.clear();
    pack.dependency_edges.clear();
    pack.runtime_signals.clear();
    pack.test_candidates.clear();
    pack.validation_plan.tests.clear();
    pack.recommended_change_boundary.allowed_files.clear();
    pack.recommended_change_boundary.caution_files.clear();
    pack.recommended_change_boundary.forbidden_files.clear();
}

fn bounded_primary_results(primary: Vec<SearchResult>, limit: usize) -> Vec<SearchResult> {
    primary.into_iter().take(limit).collect()
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

    // A required source that could not run at all (no runtime traces ingested, history not
    // configured) is a repository-level absence: it is reported and lowers confidence, but it
    // cannot be a blocker, or a task that merely says "panic" returns nothing on a repository
    // that has never ingested a trace while lexical evidence sits at rank 3. Only a source that
    // ran and stayed silent blocks.
    let (silent, absent): (Vec<_>, Vec<_>) = missing
        .iter()
        .copied()
        .partition(|required| diagnostics.sources_succeeded.contains(required));
    for required in &absent {
        diagnostics.caveats.push(format!(
            "task-family required evidence: {} is unavailable in this repository",
            retrieval_source_label(*required)
        ));
    }
    for required in &silent {
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
    let missing = silent;

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
        for result in &ranked {
            let message = format!("{}: no context budget available", result.path.display());
            diagnostics
                .selection
                .omitted_due_to_budget
                .push(message.clone());
            let authority = retrieval_authority_for_result(diagnostics, result);
            let sources = retrieval_sources_for_result(diagnostics, result);
            if is_high_value_context(authority, &sources) {
                record_high_value_omission(
                    diagnostics,
                    result,
                    &format!(
                        "high-value evidence omitted because no context capacity is available: {message}"
                    ),
                );
            }
        }
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
        diagnostics
            .selection
            .selected_units
            .push(ContextSelectedUnit {
                path: result.path.clone(),
                line_range: result.line_range.clone(),
                estimated_tokens,
                authority,
                evidence_refs: result.derived_evidence_ids(),
                rationale: selection_rationale(result, authority),
                kind: ContextUnitKind::Primary,
            });
    }
}

fn selection_rationale(result: &SearchResult, authority: RetrievalAuthority) -> String {
    let steps = region::region_steps(result);
    let mut rationale = if steps.contains(&region::RANKED_UNIT_REF) {
        format!(
            "re-admitted by region widening of a top-ranked file after task-aware retrieval ranking ({authority:?} authority)"
        )
    } else {
        format!(
            "selected under context budget after task-aware retrieval ranking ({authority:?} authority)"
        )
    };
    let widening = steps
        .iter()
        .filter(|step| **step != region::RANKED_UNIT_REF)
        .fold(Vec::<(&str, usize)>::new(), |mut counts, step| {
            match counts.iter_mut().find(|(tag, _)| *tag == *step) {
                Some((_, count)) => *count += 1,
                None => counts.push((step, 1)),
            }
            counts
        });
    if !widening.is_empty() {
        rationale.push_str("; region widened: ");
        rationale.push_str(
            &widening
                .iter()
                .map(|(tag, count)| {
                    if *count > 1 {
                        format!("{tag} x{count}")
                    } else {
                        (*tag).to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    rationale
}

/// Supporting files are listed by the pack, not selected under the budget. Costing them as
/// units (at their listing size: path and reason, not the impact snippet) makes the selection
/// ledger cover everything the pack presents, so a yield measured against it tracks the files
/// the pack actually returns.
fn append_supporting_units(supporting: &[SearchResult], diagnostics: &mut RetrievalDiagnostics) {
    if diagnostics.selection.selected_units.is_empty() {
        return;
    }
    for result in supporting {
        let estimated_tokens = estimate_listing_tokens(result);
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
            rationale: "supporting file listed from impact expansion of the top primary file; costed at its listing size, not selected under the context budget".into(),
            kind: ContextUnitKind::Supporting,
        });
    }
}

fn estimate_listing_tokens(result: &SearchResult) -> usize {
    let content =
        result.path.to_string_lossy().chars().count() + result.match_reason.chars().count();
    content.saturating_add(3) / 4 + 12
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

/// Inputs to the context confidence calculation.
///
/// Grouped rather than passed positionally: adding `task` took the argument
/// list past the point where the order is memorable, and these are all facets
/// of one pack.
struct ContextConfidenceInputs<'a> {
    task: &'a str,
    primary_files: &'a [SearchResult],
    supporting_files: &'a [SearchResult],
    tests: &'a [open_kioku_core::TestTarget],
    risk: &'a RiskReport,
    allowed_file_count: usize,
    evidence_count: usize,
    runtime_signal_count_value: usize,
}

fn confidence_for_context(inputs: ContextConfidenceInputs<'_>) -> ConfidenceBreakdown {
    let ContextConfidenceInputs {
        task,
        primary_files,
        supporting_files,
        tests,
        risk,
        allowed_file_count,
        evidence_count,
        runtime_signal_count_value,
    } = inputs;
    // Relevance is measured over what the caller will actually be handed.
    let mut selected = primary_files.to_vec();
    selected.extend_from_slice(supporting_files);
    ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
        task_relevance: open_kioku_core::task_relevance_score(task, &selected),
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
            path: file.path.as_path().into(),
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
        message: signal.message.clone().into(),
        indexed_at: Utc::now(),
        ..Default::default()
    }
}

fn annotate_results_with_git_history(
    store: &dyn OkStore,
    history_store: Option<&dyn HistoryStore>,
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
            // Per-file history evidence: churn, co-change neighbours, reviewers of *this* file.
            // The task text is deliberately not part of the query; commit-message similarity to
            // task prose measured as noise for ranking (see the history candidate stream) and it
            // made this annotation a full similar-change scan per primary result.
            let summary = history_store.history_score_components(
                &HistorySignalQuery {
                    path: result.path.clone(),
                    task: None,
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

    // Indexed per-file lookups: this used to load up to 10k git-history facts and every file
    // row on each call, then filter in memory per result.
    for result in results {
        let Some(file) = store.get_file_by_path(&result.path)? else {
            continue;
        };
        let matched =
            store.analysis_facts_for_file(&file.id, Some(EvidenceSourceType::GitHistory), 3)?;
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
                path: path.as_path().into(),
                line_range: None,
            }),
            symbol_id: None,
            confidence: fact.confidence,
            message: format!("{}: {}", fact.message, fact.target).into(),
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
    let lower = path.to_ascii_lowercase();
    lower.starts_with("docs/")
        || lower.contains("/docs/")
        || lower.ends_with(".md")
        || lower.ends_with(".mdx")
        || open_kioku_core::is_test_path(path)
}

#[derive(Debug, Clone, Default)]
struct TaskSearchIntent {
    primary_anchors: Vec<String>,
    reference_anchors: Vec<String>,
    ticket_anchors: Vec<String>,
    path_anchors: Vec<String>,
    /// Tokens of a commit-style scope prefix — `docs(fs): …`, `pkg/render: …`, `[Scheduler] …` —
    /// which name the package or directory the change lives in.
    scope_anchors: Vec<String>,
    lexical_anchors: Vec<String>,
    /// Repository identifiers the task's identifiers reach through the identifier lattice
    /// (`CollectionsUtils` → `CollectionUtils`). Heuristic links to exact names: they rank and
    /// tier like a primary anchor but never seed the exact-symbol stream.
    lattice_anchors: Vec<lattice::LatticeTerm>,

    /// Task identifiers that name nothing in the repository, exactly or through the lattice.
    unreached_identifiers: Vec<String>,
    /// The task is about tests, so test files are legitimate primary context rather than
    /// lower-tier support material.
    wants_tests: bool,
    documentation_target: bool,
}

impl TaskSearchIntent {
    fn parse(task: &str) -> Self {
        let mut intent = Self {
            documentation_target: task_targets_documentation(task),
            wants_tests: open_kioku_core::query_wants_tests(task),
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

        intent.scope_anchors = commit_scope_tokens(task);
        intent.lexical_anchors = task_lexical_terms(task);
        intent
    }

    /// Reach the repository's own spelling of the task's identifiers: symbol names and file
    /// stems whose parts are the task identifier's parts under a light stem, or one edit from
    /// a spelling the repository does not use anywhere. Built per query from the loaded symbol
    /// and file lists; without it the intent only knows the task's literal vocabulary.
    fn with_repository_vocabulary(mut self, files: &[File], symbols: &[Symbol]) -> Self {
        let task_identifiers = self
            .primary_anchors
            .iter()
            .chain(self.reference_anchors.iter())
            .cloned()
            .collect::<Vec<_>>();
        let expansion = lattice::expand(
            &task_identifiers,
            self.primary_anchors.len(),
            files,
            symbols,
        );
        self.lattice_anchors = expansion.identifiers;
        self.unreached_identifiers = expansion.unreached_identifiers;
        self
    }

    /// Terms for the lexical stream: the shared search terms plus lattice expansions. The
    /// exact-symbol stream keeps using `search_terms`, so a lattice hop can never claim exact
    /// authority for a symbol the task did not name.
    fn lexical_search_terms(&self, task: &str) -> Vec<String> {
        let mut terms = self.search_terms(task);
        for term in &self.lattice_anchors {
            if term.term.len() >= 3 && !terms.iter().any(|existing| existing == &term.term) {
                terms.push(term.term.clone());
            }
        }
        terms
    }

    fn lattice_terms(&self) -> Vec<lattice::LatticeTerm> {
        self.lattice_anchors.clone()
    }

    /// Adopt lattice terms the caller already built. The context builder scans the symbol
    /// table once and hands the result to every stream; recomputing it inside a stream doubled
    /// the per-query cost of the feature for an identical answer.
    fn with_lattice_terms(mut self, terms: Vec<lattice::LatticeTerm>) -> Self {
        self.lattice_anchors = terms;
        self
    }

    /// Hops that name one edit target, and so may carry the named-target tier and its boost.
    /// A name dozens of files share widens retrieval but points at nothing in particular.
    fn naming_lattice_anchors(&self) -> impl Iterator<Item = &lattice::LatticeTerm> {
        self.lattice_anchors.iter().filter(|hop| !hop.ambiguous)
    }

    fn lattice_term(&self, term: &str) -> Option<&lattice::LatticeTerm> {
        self.lattice_anchors
            .iter()
            .find(|candidate| candidate.term == term)
    }

    /// Caveats for task identifiers the repository does not know in any spelling. Absence is
    /// evidence: retrieval for such a task rests on its remaining words.
    fn vocabulary_caveats(&self) -> Vec<String> {
        let mut caveats = self
            .unreached_identifiers
            .iter()
            .map(|identifier| {
                format!(
                    "task identifier `{identifier}` names no indexed multi-part code identifier, and no identifier within a stem or one edit of its parts exists; retrieval relies on the task's other words"
                )
            })
            .collect::<Vec<_>>();
        // Absence of a usable name is evidence too: the reach still widened the search, but it
        // conferred no relevance, and the pack would otherwise show the hop unqualified.
        caveats.extend(
            self.lattice_anchors
                .iter()
                .filter(|hop| hop.ambiguous)
                .map(|hop| {
                    format!(
                        "identifier lattice reached `{}` from task term `{}`, but more than {} files carry that name; it widened retrieval and was denied anchor relevance",
                        hop.term,
                        hop.origin,
                        lattice::MAX_FILES_PER_NAMED_TERM
                    )
                }),
        );
        caveats
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
    for term in intent.lexical_search_terms(task) {
        let lattice_evidence = intent.lattice_term(&term).map(|hop| hop.evidence());
        for mut result in search_chunks(chunks, files, symbols, &term, per_anchor_limit)? {
            if term != task {
                result
                    .evidence
                    .push(format!("task anchor `{term}` matched"));
                result.match_reason = format!("{}; task anchor `{term}`", result.match_reason);
            }
            if let Some(evidence) = &lattice_evidence {
                result.evidence.push(evidence.clone());
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
    rerank_fused_for_task_with_files(
        results,
        intent,
        diagnostics,
        &RankingOptions::default(),
        &std::collections::BTreeSet::new(),
    )
}

/// Candidate paths are repository-relative, like the index; an exact match on the normalized
/// path is the only safe test (a suffix match would mark `src/proto/types.py` generated because
/// `proto/types.py` is).
fn is_generated_result(
    path: &std::path::Path,
    generated_paths: &std::collections::BTreeSet<String>,
) -> bool {
    !generated_paths.is_empty() && generated_paths.contains(&normalize_path(path))
}

/// The file's own path (not a symbol it defines) names a primary task anchor.
fn path_names_primary_anchor(path: &std::path::Path, intent: &TaskSearchIntent) -> bool {
    let path_text = normalize_path(path).to_ascii_lowercase();
    intent
        .primary_anchors
        .iter()
        .any(|anchor| contains_anchor(&path_text, anchor))
}

/// `generated_paths`: files the index flagged as generated (a "do not edit" banner). They are
/// indexed so an agent can read them and so derived-file links can be built, but they rank at
/// the lowest quality tier: on a Python ML library where every implementation module is generated
/// from a specification module, letting them compete as source cost 0.015 R@5 because they share the
/// modular file's vocabulary and displaced it.
fn rerank_fused_for_task_with_files(
    results: Vec<SearchResult>,
    intent: &TaskSearchIntent,
    diagnostics: &RetrievalDiagnostics,
    ranking_options: &RankingOptions,
    generated_paths: &std::collections::BTreeSet<String>,
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
        // Only a file the reached identifier *names* is boosted. Applying this to any file
        // that mentions it flattened whole directories into one tier: every file under a
        // module mentions the module's main class, and the gold file lost its lead.
        let identity = result_identity_text(result);
        for hop in intent.naming_lattice_anchors() {
            if contains_anchor(&identity, &hop.term) {
                let boost = if hop.from_primary {
                    LATTICE_ANCHOR_BOOST
                } else {
                    LATTICE_REFERENCE_ANCHOR_BOOST
                };
                result.score += boost;
                result.confidence = result.confidence.max(0.7);
                result.evidence.push(hop.evidence());
                result.add_score_component(ScoreComponent::adjustment(
                    "identifier_lattice_anchor_boost",
                    boost,
                    result.derived_evidence_ids(),
                    format!(
                        "repository identifier `{}` reached from task term `{}` names this result",
                        hop.term, hop.origin
                    ),
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
        if path_matches_scope(&normalize_path(&result.path), &intent.scope_anchors) {
            let scope = intent.scope_anchors.join("/");
            result.score += 0.35;
            result.confidence = result.confidence.max(0.75);
            result
                .evidence
                .push(format!("commit scope `{scope}` names this path"));
            result.add_score_component(ScoreComponent::adjustment(
                "commit_scope_path_boost",
                0.35,
                result.derived_evidence_ids(),
                format!("commit scope `{scope}` matched a path segment"),
            ));
        }
        result.reconcile_score_breakdown();
    }
    // Quality tier first: docs and tests are support material for a task that is not about
    // them, however strongly they mention its anchors; then anchor relevance, authority, score.
    // The one exception is a file the task names outright ("Guard pool shutdown in
    // ConnectionPoolMetricsIT"): it is the edit target whatever kind of file it is, and demoting
    // it put twenty source files that merely share vocabulary above the file three streams had
    // ranked first.
    for result in results.iter_mut() {
        if is_generated_result(&result.path, generated_paths)
            && !path_names_primary_anchor(&result.path, intent)
        {
            result
                .evidence
                .push("generated file: ranked below hand-written source".to_string());
            result.add_score_component(ScoreComponent::adjustment(
                "generated_file_demotion",
                0.0,
                result.derived_evidence_ids(),
                "generated file (do-not-edit banner) ranked at the lowest quality tier",
            ));
        }
    }
    results.sort_by(|a, b| {
        let a_haystack = searchable_result_text(a);
        let b_haystack = searchable_result_text(b);
        let a_relevance = task_relevance_tier(&a.path, a, &a_haystack, intent);
        let b_relevance = task_relevance_tier(&b.path, b, &b_haystack, intent);
        let quality = |result: &SearchResult, relevance: u8| {
            if is_generated_result(&result.path, generated_paths)
                && !path_names_primary_anchor(&result.path, intent)
            {
                // A generated file defines the same symbols as the file it was generated
                // from, so a symbol match cannot exempt it; only its own path can.
                0
            } else {
                result_quality_tier(result, relevance, intent, ranking_options)
            }
        };
        quality(b, b_relevance)
            .cmp(&quality(a, a_relevance))
            .then_with(|| b_relevance.cmp(&a_relevance))
            .then_with(|| {
                retrieval_authority_for_result(diagnostics, b)
                    .cmp(&retrieval_authority_for_result(diagnostics, a))
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

/// Relevance tier at which a file *is* the task's named target (its path or symbol names a
/// primary anchor) and quality demotion no longer applies.
const NAMED_TARGET_RELEVANCE_TIER: u8 = 4;
/// A lattice hop is a guess at what the task meant, so it sits a whole relevance tier below a
/// name the task spelled exactly — score only orders results that already tie on quality,
/// relevance and authority, so the tier is what actually keeps an exact fact on top.
const LATTICE_ANCHOR_BOOST: f32 = 0.45;
/// A hop from a trailing reference mention ("… similar to X") is a guess about a secondary
/// name, and ranks with the reference anchors it came from.
const LATTICE_REFERENCE_ANCHOR_BOOST: f32 = 0.25;
/// The quality tier of ordinary source, which a named target is always treated as.
const SOURCE_QUALITY_TIER: u8 = 2;

/// Post-fusion quality tier: 2 for source, 1 for docs and (unless the task asks for them) tests,
/// 0 for generated or vendored code. Tests are demoted rather than dropped: on a large Java
/// repository they outnumber source files and match the same vocabulary, so without this tier
/// an ordinary "geoip processor" task returns twenty test files and no processor.
fn context_quality_tier(
    path: &std::path::Path,
    options: &RankingOptions,
    wants_tests: bool,
    wants_docs: bool,
) -> u8 {
    // Test detection needs the original case: `internalClusterTest` and `GeoIpDownloaderIT`
    // are recognised at a CamelCase boundary that lowercasing erases.
    let original = normalize_path(path);
    let normalized = original.to_ascii_lowercase();
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
    if boundary_fit_enabled {
        let is_test = open_kioku_core::is_test_path(&original);
        let is_doc = !is_test && is_docs_or_test_path(&normalized);
        if (is_test && !wants_tests) || (is_doc && !wants_docs) {
            return 1;
        }
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

/// 4: a primary anchor names the file or one of its symbols (definition-like); 3: an explicit
/// ticket or path anchor, or a documentation target for a documentation task; 2: a primary
/// anchor merely mentioned in the snippet; 1: a reference anchor; 0: none.
///
/// Snippet mentions used to share the top tier with definitions. On a Go repository, where
/// commit subjects routinely name an identifier that dozens of small files reference, every
/// mention outranked the best full-task lexical hit. A file that *names* the anchor is the
/// edit target; one that mentions it is a reference and competes on score with the rest.
fn task_relevance_tier(
    path: &std::path::Path,
    result: &SearchResult,
    haystack: &str,
    intent: &TaskSearchIntent,
) -> u8 {
    let identity = format!(
        "{} {} {}",
        result.path.display(),
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
    .to_ascii_lowercase();
    if intent
        .primary_anchors
        .iter()
        .any(|anchor| contains_anchor(&identity, anchor))
    {
        4
    } else if intent
        .ticket_anchors
        .iter()
        .chain(intent.path_anchors.iter())
        .any(|anchor| contains_anchor(haystack, anchor))
        || path_matches_scope(&normalize_path(path), &intent.scope_anchors)
        || (intent.documentation_target && is_documentation_path(&normalize_path(path)))
        // A hop is a guess at the repository's spelling, so a file it names ranks with an
        // explicitly typed path — never with a name the task spelled exactly, and never with
        // the named-target exemption that would lift it over the docs/tests demotion.
        || intent
            .naming_lattice_anchors()
            .filter(|hop| hop.from_primary)
            .any(|hop| contains_anchor(&identity, &hop.term))
    {
        3
    } else if intent
        .primary_anchors
        .iter()
        .any(|anchor| contains_anchor(haystack, anchor))
    {
        2
    } else if intent
        .reference_anchors
        .iter()
        .any(|anchor| contains_anchor(haystack, anchor))
        || intent
            .naming_lattice_anchors()
            .filter(|hop| !hop.from_primary)
            .any(|hop| contains_anchor(&identity, &hop.term))
    {
        1
    } else {
        0
    }
}

/// Tokens of the scope a commit-style subject carries: `docs(fs): …` and `feat(path/posix): …`
/// (conventional commits), `pkg/render: …` and `commands: …` (Go style, where the prefix is
/// the package), and `[Scheduler] …` (bracketed area). The scope names where the change lives,
/// which the subject body usually does not repeat. Empty when the subject has no such prefix.
fn commit_scope_tokens(task: &str) -> Vec<String> {
    const TYPES: &[&str] = &[
        "feat", "fix", "docs", "doc", "chore", "refactor", "test", "tests", "ci", "build", "perf",
        "style", "revert", "deps", "release", "wip", "misc", "cleanup",
    ];
    let first_line = task.lines().next().unwrap_or_default().trim();
    let scope: Option<&str> = if let Some(rest) = first_line.strip_prefix('[') {
        rest.split_once(']').map(|(scope, _)| scope)
    } else if let Some((prefix, _)) = first_line.split_once(':') {
        let prefix = prefix.trim().trim_end_matches('!');
        if prefix.is_empty() || prefix.len() > 64 || prefix.contains(char::is_whitespace) {
            None
        } else if let Some((_, scoped)) = prefix.split_once('(') {
            scoped.strip_suffix(')')
        } else if TYPES.contains(&prefix.to_ascii_lowercase().as_str())
            || prefix.chars().any(|ch| ch.is_ascii_uppercase())
        {
            // A capitalised word before a colon ("Note:", "Followup:") is prose; package
            // prefixes in Go-style subjects are lowercase paths.
            None
        } else {
            Some(prefix)
        }
    } else {
        None
    };
    let mut tokens: Vec<String> = scope
        .unwrap_or_default()
        .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .filter(|token| token.len() >= 2)
        .map(|token| token.to_ascii_lowercase())
        .collect();
    tokens.dedup();
    tokens
}

/// File names that are a module's public entry point, the file a repository convention edits
/// when a module gains, loses, or stabilises an export.
const MODULE_ENTRY_FILE_NAMES: &[&str] = &[
    "mod.ts",
    "mod.js",
    "mod.rs",
    "lib.rs",
    "index.ts",
    "index.tsx",
    "index.js",
    "__init__.py",
];

/// When a commit scope names a directory, that directory's entry file is a candidate even if
/// it shares no vocabulary with the task: `feat(async): stabilize Channel` edits `async/mod.ts`,
/// which does not mention Channel until the commit lands. Injected at a low score so a file
/// that actually matches the task still outranks it inside the scope tier.
/// The quality tier the pack ordering gives a result: a file the task names is source whatever
/// kind of file it is; otherwise the path decides.
fn result_quality_tier(
    result: &SearchResult,
    relevance: u8,
    intent: &TaskSearchIntent,
    ranking_options: &RankingOptions,
) -> u8 {
    if relevance >= NAMED_TARGET_RELEVANCE_TIER {
        SOURCE_QUALITY_TIER
    } else {
        context_quality_tier(
            &result.path,
            ranking_options,
            intent.wants_tests,
            intent.documentation_target,
        )
    }
}

/// A file the graph records as a derived sibling of another: generated from it, its test, or
/// its declaration file (`DERIVED_FROM`, built at index time; `docs/graph-model.md`).
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedSibling {
    /// Repository-relative path of the sibling.
    pub path: String,
    /// Persisted graph edge the admission cites as `derived:<edge>`.
    pub edge_id: String,
    /// True when the edge carries a declared-origin proof; a naming-convention pairing is not.
    pub authoritative: bool,
    /// `declared-origin`, `test-pairing`, or `declaration-pairing`.
    pub derivation: String,
    pub message: String,
}

/// One file's derived siblings, and whether either direction hit the read cap.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DerivedSiblings {
    pub siblings: Vec<DerivedSibling>,
    /// True only when a single direction filled its window, so the caveat reports evidence that
    /// is genuinely missing rather than the sum of two partial reads.
    pub truncated: bool,
}

/// Edges read per direction for one file; a file with more derived siblings than this is a
/// fixture directory, not a module.
const DERIVED_SIBLING_EDGE_LIMIT: usize = 8;
/// Origins expanded: twice the pack's file limit, at least this many, since only a sibling of
/// a file that can itself reach the pack changes it.
const DERIVED_SIBLING_MIN_WINDOW: usize = 20;

/// Both directions of the `DERIVED_FROM` edges incident to a repository-relative path.
fn derived_siblings(store: &dyn OkStore, path: &str) -> Result<DerivedSiblings> {
    let node = open_kioku_core::identity::try_file_node_id(std::path::Path::new(path))?;
    let mut siblings = Vec::new();
    // The cap is per direction, so the two reads have to be compared separately: four outgoing
    // plus four incoming is eight siblings and nothing truncated.
    let mut truncated = false;
    for outgoing in [true, false] {
        let edges = store.edges_by_type_for_node(
            GraphEdgeType::DerivedFrom,
            &node.0,
            outgoing,
            DERIVED_SIBLING_EDGE_LIMIT,
            0,
        )?;
        truncated |= edges.len() >= DERIVED_SIBLING_EDGE_LIMIT;
        for edge in edges {
            let other = if outgoing { &edge.to } else { &edge.from };
            let Some(sibling_path) = other.0.strip_prefix("file:") else {
                continue;
            };
            siblings.push(DerivedSibling {
                path: sibling_path.to_string(),
                edge_id: edge.id.0.clone(),
                authoritative: edge.is_authoritative_relationship(),
                derivation: edge
                    .properties
                    .get("derivation")
                    .and_then(|value| value.as_str())
                    .unwrap_or("derived")
                    .to_string(),
                message: edge.evidence.message.to_string(),
            });
        }
    }
    Ok(DerivedSiblings {
        siblings,
        truncated,
    })
}

/// A candidate's derived siblings join the ordered list with the candidate's score and the
/// edge as evidence: the generated module and its modular source, the test and its subject,
/// the declaration and its implementation are one edit, and a task that reaches one has
/// reached the other. A generated sibling keeps the generated-file demotion unless the task
/// names it; a sibling already in the list is left where it is.
#[allow(clippy::too_many_arguments)]
fn admit_derived_siblings(
    ranked: Vec<SearchResult>,
    files: &[File],
    chunks: &[CodeChunk],
    intent: &TaskSearchIntent,
    ranking_options: &RankingOptions,
    limit: usize,
    diagnostics: &mut RetrievalDiagnostics,
    siblings_for: &mut dyn FnMut(&str) -> Result<DerivedSiblings>,
) -> Vec<SearchResult> {
    if ranked.is_empty() {
        return ranked;
    }
    let files_by_path: std::collections::BTreeMap<String, &File> = files
        .iter()
        .map(|file| (normalize_path(&file.path), file))
        .collect();
    // The same rule the ordering applied: a generated file is exempt from its demotion only
    // when the task names its path.
    let quality = |result: &SearchResult, is_generated: bool| -> u8 {
        if is_generated && !path_names_primary_anchor(&result.path, intent) {
            return 0;
        }
        let haystack = searchable_result_text(result);
        let relevance = task_relevance_tier(&result.path, result, &haystack, intent);
        result_quality_tier(result, relevance, intent, ranking_options)
    };
    let relative: Vec<Option<String>> = ranked
        .iter()
        .map(|result| repository_relative_path(&result.path, &files_by_path))
        .collect();
    let tiers: Vec<u8> = ranked
        .iter()
        .zip(&relative)
        .map(|(result, path)| {
            let generated = path
                .as_ref()
                .and_then(|path| files_by_path.get(path))
                .is_some_and(|file| file.is_generated);
            quality(result, generated)
        })
        .collect();
    let mut present: std::collections::BTreeSet<String> =
        relative.iter().flatten().cloned().collect();
    let mut before: Vec<Vec<SearchResult>> = ranked.iter().map(|_| Vec::new()).collect();
    let mut end = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut truncated: Vec<String> = Vec::new();
    let window = ranked
        .len()
        .min(limit.max(DERIVED_SIBLING_MIN_WINDOW).saturating_mul(2));
    for index in 0..window {
        let Some(origin_path) = relative[index].as_deref() else {
            continue;
        };
        let read = match siblings_for(origin_path) {
            Ok(read) => read,
            Err(err) => {
                // One path failing is not proof the store is refusing every read, so the pass
                // continues; each distinct failure is reported against the path that produced it.
                failures.push(format!("`{origin_path}`: {err}"));
                continue;
            }
        };
        // A file at the cap has siblings the pack cannot see, and absence has to stay visible.
        if read.truncated {
            truncated.push(origin_path.to_string());
        }
        let siblings = read.siblings;
        for sibling in siblings {
            if present.contains(&sibling.path) {
                continue;
            }
            let Some(file) = files_by_path.get(&sibling.path) else {
                continue;
            };
            let origin = &ranked[index];
            let mut result = derived_sibling_result(origin, origin_path, file, &sibling, chunks);
            let tier = quality(&result, file.is_generated);
            // Admitted, not ranked: the sibling scores below everything already in its tier, so
            // neither the ordering nor the budget selection — which compares scores again —
            // can float it past a result that earned its place. Inheriting the origin's score
            // instead put two siblings above a gold file on the Go corpus (rank 4 -> 7).
            result.score = tier_floor_score(&ranked, &tiers, tier);
            let authority = if sibling.authoritative {
                RetrievalAuthority::Corroborating
            } else {
                RetrievalAuthority::Heuristic
            };
            diagnostics.traces.push(RetrievalTrace {
                path: result.path.clone(),
                unit_key: Some(RetrievalUnitKey::from_result(&result)),
                fused_score: result.score,
                authority,
                contributions: vec![open_kioku_core::RetrievalContribution {
                    source: RetrievalSourceKind::DerivedSibling,
                    rank: index + 1,
                    raw_score: Some(origin.score),
                    rrf_contribution: 0.0,
                    authority,
                    symbol_id: None,
                    evidence_refs: result.evidence_refs.clone(),
                    rationale: format!(
                        "derived-file sibling ({}) of `{origin_path}`",
                        sibling.derivation
                    ),
                }],
            });
            present.insert(sibling.path.clone());
            // A sibling closes its own quality tier: the ranked list is ordered by tier, so the
            // first result below the sibling's tier is the end of that block. It is admitted,
            // not ranked — nothing about the edge says it beats a result already there — so it
            // never displaces one. Inserting at the head of the block instead cost R@5 on all
            // three corpora (a gold file at rank 5 moved to 6) and gained nothing.
            match tiers.iter().position(|&t| t < tier) {
                // Never rank 0: an admitted sibling has no score of its own, and the pack
                // headline is also the impact seed. If nothing ranked reaches its tier it goes
                // after the best result rather than in front of it.
                Some(0) if ranked.len() > 1 => before[1].push(result),
                Some(0) => end.push(result),
                Some(at) => before[at].push(result),
                None => end.push(result),
            }
        }
    }
    if !failures.is_empty() {
        failures.sort();
        failures.dedup();
        diagnostics.caveats.push(format!(
            "derived-file siblings could not be read for {} path(s): {}",
            failures.len(),
            failures.join("; ")
        ));
    }
    if !truncated.is_empty() {
        truncated.sort();
        truncated.dedup();
        diagnostics.caveats.push(format!(
            "derived-file siblings truncated at {DERIVED_SIBLING_EDGE_LIMIT} edge(s) for {} path(s): {}",
            truncated.len(),
            truncated.join(", ")
        ));
    }
    let mut admitted = Vec::with_capacity(ranked.len() + end.len());
    for (index, result) in ranked.into_iter().enumerate() {
        admitted.append(&mut before[index]);
        admitted.push(result);
    }
    admitted.append(&mut end);
    admitted
}

/// A score strictly below every ranked result in `tier`, and never negative. An empty tier
/// (the sibling opens one) sits below the whole list.
fn tier_floor_score(ranked: &[SearchResult], tiers: &[u8], tier: u8) -> f32 {
    let floor = ranked
        .iter()
        .zip(tiers)
        .filter(|(_, &result_tier)| result_tier == tier)
        .map(|(result, _)| result.score)
        .fold(f32::INFINITY, f32::min);
    let floor = if floor.is_finite() {
        floor
    } else {
        ranked
            .iter()
            .map(|result| result.score)
            .fold(f32::INFINITY, f32::min)
    };
    if floor.is_finite() {
        (floor - f32::EPSILON).max(0.0)
    } else {
        0.0
    }
}

fn derived_sibling_result(
    origin: &SearchResult,
    origin_path: &str,
    file: &File,
    sibling: &DerivedSibling,
    chunks: &[CodeChunk],
) -> SearchResult {
    let chunk = chunks.iter().find(|chunk| chunk.file_id == file.id);
    SearchResult {
        path: file.path.clone(),
        line_range: chunk.map(|chunk| chunk.range.clone()),
        snippet: chunk.map(|chunk| chunk.text.clone()).unwrap_or_default(),
        symbol: None,
        score: origin.score,
        match_reason: format!("derived-file sibling of `{origin_path}`"),
        evidence: vec![format!("{}: {}", sibling.derivation, sibling.message)],
        evidence_refs: vec![format!("derived:{}", sibling.edge_id)],
        confidence: origin.confidence * if sibling.authoritative { 0.9 } else { 0.7 },
        score_breakdown: Vec::new(),
    }
}

/// Result paths may be absolute while the index stores repository-relative paths; the
/// longest suffix that names an indexed file is the file.
fn repository_relative_path(
    path: &std::path::Path,
    files_by_path: &std::collections::BTreeMap<String, &File>,
) -> Option<String> {
    let normalized = normalize_path(path);
    let mut candidate = normalized.as_str();
    loop {
        if files_by_path.contains_key(candidate) {
            return Some(candidate.to_string());
        }
        let (_, rest) = candidate.split_once('/')?;
        candidate = rest;
    }
}

fn append_scope_entry_points(
    results: &mut Vec<SearchResult>,
    files: &[File],
    chunks: &[CodeChunk],
    intent: &TaskSearchIntent,
) {
    if intent.scope_anchors.is_empty() {
        return;
    }
    let present: std::collections::BTreeSet<String> = results
        .iter()
        .map(|result| normalize_path(&result.path))
        .collect();
    // Where the entry point sits among the files the scope already matched depends on whether
    // any of them matched the task's own words. `feat(async): stabilize Channel` matches
    // async/ files on "async" alone — nothing in the directory knows "Channel" yet — so the
    // barrel is the best guess and goes just below the group's best. `[Scheduler] Fix speculative
    // prefetch` matches real modules on "speculative" and "prefetch", so the barrel goes last:
    // as runner-up it pushed rank-2 modules to rank 3 on a Python monorepo (MRR -0.011), and
    // at the floor it never surfaced on the TypeScript standard library at all (gain 0.000).
    let scope_group: Vec<&SearchResult> = results
        .iter()
        .filter(|result| path_matches_scope(&normalize_path(&result.path), &intent.scope_anchors))
        .collect();
    let task_words: Vec<&String> = intent
        .lexical_anchors
        .iter()
        .filter(|word| !intent.scope_anchors.iter().any(|scope| scope == *word))
        .collect();
    let group_knows_task = scope_group.iter().any(|result| {
        let text = searchable_result_text(result).to_ascii_lowercase();
        task_words.iter().any(|word| text.contains(word.as_str()))
    });
    let scores = scope_group.iter().map(|result| result.score);
    let runner_up = if group_knows_task {
        scores
            .fold(None, |worst: Option<f32>, score| {
                Some(worst.map_or(score, |w| w.min(score)))
            })
            .map(|worst| (worst - f32::EPSILON).max(0.0))
            .unwrap_or(SCOPE_ENTRY_POINT_SCORE)
    } else {
        scores
            .fold(None, |best: Option<f32>, score| {
                Some(best.map_or(score, |b| b.max(score)))
            })
            .map(|best| best - f32::EPSILON)
            .unwrap_or(SCOPE_ENTRY_POINT_SCORE)
    };
    for file in files {
        let path = normalize_path(&file.path);
        let Some((dir, name)) = path.rsplit_once('/') else {
            continue;
        };
        if !MODULE_ENTRY_FILE_NAMES.contains(&name)
            || !path_matches_scope(dir, &intent.scope_anchors)
            || present.contains(&path)
            || file.size_bytes < MODULE_ENTRY_MIN_BYTES
        {
            continue;
        }
        let chunk = chunks.iter().find(|chunk| chunk.file_id == file.id);
        let scope = intent.scope_anchors.join("/");
        results.push(SearchResult {
            path: file.path.clone(),
            line_range: chunk.map(|chunk| chunk.range.clone()),
            snippet: chunk.map(|chunk| chunk.text.clone()).unwrap_or_default(),
            symbol: None,
            score: runner_up,
            match_reason: format!("module entry point of commit scope `{scope}`"),
            evidence: vec![format!(
                "commit scope `{scope}` names this directory; `{name}` is its public entry point"
            )],
            evidence_refs: vec![format!("scope:entry-point:{path}")],
            confidence: 0.6,
            score_breakdown: Vec::new(),
        });
    }
}

/// Score for an entry point when nothing in its scope matched at all.
const SCOPE_ENTRY_POINT_SCORE: f32 = 0.05;
/// An empty `__init__.py` marks a package; it is not where a module's public surface lives.
const MODULE_ENTRY_MIN_BYTES: u64 = 64;

/// Every scope token names a path segment (a directory, or a file stem without its extension).
fn path_matches_scope(path: &str, scope_tokens: &[String]) -> bool {
    if scope_tokens.is_empty() {
        return false;
    }
    let segments: Vec<String> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            segment
                .split_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(segment)
                .to_ascii_lowercase()
        })
        .collect();
    scope_tokens
        .iter()
        .all(|token| segments.iter().any(|segment| segment == token))
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

/// Path and symbol names only. A file that *names* an identifier is the edit target; one that
/// merely mentions it in a snippet is a reference.
fn result_identity_text(result: &SearchResult) -> String {
    format!(
        "{} {} {}",
        result.path.display(),
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

/// Whether a task token names a code identifier rather than a word of prose.
///
/// A capital first letter is not enough: "Enable", "Fix", "Assert" open almost every
/// commit-style task, and treating them as the primary edit anchor boosted every file that
/// merely contained "enable" or "fix" (as in `prefix`) by +0.65 above the real lexical hits.
/// An identifier shows a case change *inside* the token (`SystemIndexDescriptor`, `getFoo`),
/// a separator (`random_score`, `max-age`), or digits next to capitals (`ES819`).
fn is_named_identifier(value: &str) -> bool {
    if value.len() < 3 || is_ticket_id(value) {
        return false;
    }
    let has_upper = value.chars().any(|ch| ch.is_ascii_uppercase());
    let has_digit = value.chars().any(|ch| ch.is_ascii_digit());
    let has_separator = value.contains('_') || value.contains('-');
    has_inner_case_change(value) || has_separator || (has_digit && has_upper)
}

/// `getFoo`, `SystemIndexDescriptor`, `ES819x`: an upper-case letter after a lower-case letter
/// or digit. `Enable`, `HTTP`, and `index` have none.
pub(crate) fn has_inner_case_change(value: &str) -> bool {
    value
        .chars()
        .zip(value.chars().skip(1))
        .any(|(prev, next)| {
            (prev.is_ascii_lowercase() || prev.is_ascii_digit()) && next.is_ascii_uppercase()
        })
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

pub(crate) fn is_task_stopword(token: &str) -> bool {
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
        proven_impact: Vec::new(),
        possible_impact: Vec::new(),
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
        proven_impact: Vec::new(),
        possible_impact: Vec::new(),
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
    fn commit_scope_prefixes_yield_path_tokens() {
        assert_eq!(
            commit_scope_tokens("docs(fs): fix walk examples"),
            vec!["fs"]
        );
        assert_eq!(
            commit_scope_tokens("feat(path/posix)!: add join"),
            vec!["path", "posix"]
        );
        assert_eq!(
            commit_scope_tokens("pkg/render: Fix template lookup"),
            vec!["pkg", "render"]
        );
        assert_eq!(
            commit_scope_tokens("[Scheduler] fix retry config"),
            vec!["scheduler"]
        );
        assert!(commit_scope_tokens("docs: fix typo").is_empty());
        assert!(commit_scope_tokens("Fix geoip processor timeout").is_empty());
        assert!(commit_scope_tokens("Note: this is prose with a colon").is_empty());
    }

    #[test]
    fn generated_files_rank_below_source_unless_the_task_names_them() {
        let intent = TaskSearchIntent::parse("Fix Alpha MoE hidden size");
        let result = |path: &str, score: f32| SearchResult {
            path: path.into(),
            line_range: None,
            snippet: String::new(),
            symbol: None,
            score,
            match_reason: String::new(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let generated: std::collections::BTreeSet<String> =
            ["src/models/alpha/impl_alpha.py".to_string()]
                .into_iter()
                .collect();
        let ranked = rerank_fused_for_task_with_files(
            vec![
                result("src/models/alpha/impl_alpha.py", 0.9),
                result("src/models/alpha/spec_alpha.py", 0.5),
            ],
            &intent,
            &RetrievalDiagnostics::default(),
            &RankingOptions::default(),
            &generated,
        );
        assert!(ranked[0].path.ends_with("spec_alpha.py"));
        assert!(ranked[1]
            .evidence
            .iter()
            .any(|line| line.contains("generated file")));
        // Both files define the same class, so a symbol anchor must not exempt the generated one.
        let intent = TaskSearchIntent::parse("Fix AlphaAttention rotary embedding");
        let ranked = rerank_fused_for_task_with_files(
            vec![
                result("src/models/alpha/impl_alpha.py", 0.9),
                result("src/models/alpha/spec_alpha.py", 0.5),
            ],
            &intent,
            &RetrievalDiagnostics::default(),
            &RankingOptions::default(),
            &generated,
        );
        assert!(ranked[0].path.ends_with("spec_alpha.py"));
        // A task that names the generated file's own path is asking for it.
        let intent = TaskSearchIntent::parse("Regenerate impl_alpha after spec change");
        let ranked = rerank_fused_for_task_with_files(
            vec![
                result("src/models/alpha/impl_alpha.py", 0.9),
                result("src/models/alpha/spec_alpha.py", 0.5),
            ],
            &intent,
            &RetrievalDiagnostics::default(),
            &RankingOptions::default(),
            &generated,
        );
        assert!(ranked[0].path.ends_with("impl_alpha.py"));
    }

    #[test]
    fn a_test_file_the_task_names_is_not_demoted_below_source() {
        let intent = TaskSearchIntent::parse("Guard pool shutdown in ConnectionPoolMetricsIT");
        let result = |path: &str, score: f32| SearchResult {
            path: path.into(),
            line_range: None,
            snippet: String::new(),
            symbol: None,
            score,
            match_reason: String::new(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let ranked = rerank_fused_for_task(
            vec![
                result("server/src/main/java/com/acme/shutdown/ShutdownAction.java", 0.9),
                result(
                    "modules/netpool/src/internalClusterTest/java/com/acme/netpool/ConnectionPoolMetricsIT.java",
                    0.8,
                ),
            ],
            &intent,
            &RetrievalDiagnostics::default(),
        );
        assert!(
            ranked[0].path.ends_with("ConnectionPoolMetricsIT.java"),
            "the named test file must lead: {:?}",
            ranked
                .iter()
                .map(|r| r.path.display().to_string())
                .collect::<Vec<_>>()
        );
        // A test the task does not name is still support material below source.
        let intent = TaskSearchIntent::parse("Guard pool shutdown in netpool");
        let ranked = rerank_fused_for_task(
            vec![
                result(
                    "modules/netpool/src/main/java/com/acme/netpool/Pooler.java",
                    0.5,
                ),
                result(
                    "modules/netpool/src/test/java/com/acme/netpool/PoolerTests.java",
                    0.9,
                ),
            ],
            &intent,
            &RetrievalDiagnostics::default(),
        );
        assert!(ranked[0].path.ends_with("Pooler.java"));
    }

    #[test]
    fn scope_entry_points_are_injected_once_and_only_for_matching_directories() {
        let intent = TaskSearchIntent::parse("feat(async): stabilize Channel");
        let file = |path: &str, id: &str| File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language: Language::TypeScript,
            size_bytes: 200,
            content_hash: id.into(),
            is_generated: false,
            is_vendor: false,
        };
        let files = vec![
            file("async/mod.ts", "f1"),
            file("async/tee.ts", "f2"),
            file("streams/mod.ts", "f3"),
        ];
        let mut results = Vec::new();
        append_scope_entry_points(&mut results, &files, &[], &intent);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, std::path::PathBuf::from("async/mod.ts"));
        assert!(results[0].evidence_refs[0].starts_with("scope:entry-point:"));
        append_scope_entry_points(&mut results, &files, &[], &intent);
        assert_eq!(
            results.len(),
            1,
            "already-present entry points are not duplicated"
        );
        let mut none = Vec::new();
        append_scope_entry_points(
            &mut none,
            &files,
            &[],
            &TaskSearchIntent::parse("Fix panic"),
        );
        assert!(none.is_empty());
    }

    fn derived_fixture_files() -> Vec<File> {
        let file = |path: &str, language: Language, is_generated: bool| File {
            id: FileId::new(path),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language,
            size_bytes: 200,
            content_hash: path.into(),
            is_generated,
            is_vendor: false,
        };
        vec![
            file("src/orbit/pipeline.py", Language::Python, false),
            file("tests/orbit/test_pipeline.py", Language::Python, false),
            file("src/orbit/scheduler.py", Language::Python, false),
            file("src/orbit/models/modular_orbit.py", Language::Python, false),
            file("src/orbit/models/modeling_orbit.py", Language::Python, true),
        ]
    }

    /// The `DERIVED_FROM` edges the fixture index would hold, read from either endpoint.
    fn derived_fixture_siblings(path: &str) -> Result<DerivedSiblings> {
        let pairs = [
            (
                "tests/orbit/test_pipeline.py",
                "src/orbit/pipeline.py",
                "test-pairing",
                false,
            ),
            (
                "src/orbit/models/modeling_orbit.py",
                "src/orbit/models/modular_orbit.py",
                "declared-origin",
                true,
            ),
        ];
        Ok(DerivedSiblings {
            siblings: pairs
                .iter()
                .filter(|(derived, origin, _, _)| *derived == path || *origin == path)
                .map(
                    |(derived, origin, derivation, authoritative)| DerivedSibling {
                        path: if *derived == path { origin } else { derived }.to_string(),
                        edge_id: format!("edge:{derivation}:{derived}"),
                        authoritative: *authoritative,
                        derivation: derivation.to_string(),
                        message: "fixture edge".into(),
                    },
                )
                .collect(),
            truncated: false,
        })
    }

    fn ranked_result(path: &str, score: f32) -> SearchResult {
        SearchResult {
            path: path.into(),
            line_range: None,
            snippet: String::new(),
            symbol: None,
            score,
            match_reason: "lexical".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.8,
            score_breakdown: Vec::new(),
        }
    }

    fn admitted_paths(
        task: &str,
        ranked: Vec<SearchResult>,
    ) -> (Vec<String>, RetrievalDiagnostics) {
        let mut diagnostics = RetrievalDiagnostics::default();
        let admitted = admit_derived_siblings(
            ranked,
            &derived_fixture_files(),
            &[],
            &TaskSearchIntent::parse(task),
            &RankingOptions::default(),
            5,
            &mut diagnostics,
            &mut derived_fixture_siblings,
        );
        (
            admitted
                .iter()
                .map(|result| normalize_path(&result.path))
                .collect(),
            diagnostics,
        )
    }

    #[test]
    fn a_full_pack_of_siblings_split_across_directions_is_not_reported_as_truncated() {
        // The read cap is per direction. Four out plus four in is eight siblings with nothing
        // missing, and claiming otherwise reports absent evidence that is not absent.
        let mut diagnostics = RetrievalDiagnostics::default();
        admit_derived_siblings(
            vec![ranked_result("src/orbit/pipeline.py", 0.9)],
            &derived_fixture_files(),
            &[],
            &TaskSearchIntent::parse("Fix pipeline batching"),
            &RankingOptions::default(),
            5,
            &mut diagnostics,
            &mut |_| {
                Ok(DerivedSiblings {
                    siblings: (0..DERIVED_SIBLING_EDGE_LIMIT)
                        .map(|index| DerivedSibling {
                            path: format!("src/orbit/other{index}.py"),
                            edge_id: format!("edge:{index}"),
                            authoritative: false,
                            derivation: "test-pairing".into(),
                            message: "fixture".into(),
                        })
                        .collect(),
                    truncated: false,
                })
            },
        );
        assert!(
            !diagnostics
                .caveats
                .iter()
                .any(|caveat| caveat.contains("truncated")),
            "{:?}",
            diagnostics.caveats
        );

        let mut diagnostics = RetrievalDiagnostics::default();
        admit_derived_siblings(
            vec![ranked_result("src/orbit/pipeline.py", 0.9)],
            &derived_fixture_files(),
            &[],
            &TaskSearchIntent::parse("Fix pipeline batching"),
            &RankingOptions::default(),
            5,
            &mut diagnostics,
            &mut |_| {
                Ok(DerivedSiblings {
                    siblings: Vec::new(),
                    truncated: true,
                })
            },
        );
        assert!(
            diagnostics
                .caveats
                .iter()
                .any(|caveat| caveat.contains("truncated")),
            "a direction that filled its window must still be reported"
        );
    }

    #[test]
    fn an_admitted_sibling_is_not_graph_evidence_for_budget_selection() {
        // Budget selection ranks by source kind before utility and exempts graph/validation
        // evidence from the redundancy cull. An admitted sibling has no score that earned
        // either, so it must not arrive wearing the graph source kind.
        let mut diagnostics = RetrievalDiagnostics::default();
        admit_derived_siblings(
            vec![ranked_result("src/orbit/pipeline.py", 0.9)],
            &derived_fixture_files(),
            &[],
            &TaskSearchIntent::parse("Fix pipeline batching"),
            &RankingOptions::default(),
            5,
            &mut diagnostics,
            &mut derived_fixture_siblings,
        );
        let trace = diagnostics
            .traces
            .iter()
            .find(|trace| trace.path.ends_with("test_pipeline.py"))
            .expect("the sibling is traced");
        let sources = trace
            .contributions
            .iter()
            .map(|contribution| contribution.source)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            sources,
            [RetrievalSourceKind::DerivedSibling].into_iter().collect()
        );
        assert!(!is_high_value_context(trace.authority, &sources));
    }

    #[test]
    fn an_admitted_sibling_never_takes_the_first_rank() {
        // Rank 1 is the pack headline and the impact seed; a file admitted without a score of
        // its own must not hold it even when nothing ranked shares its tier.
        let (paths, _) = admitted_paths(
            "Fix pipeline batching",
            vec![ranked_result("tests/orbit/test_pipeline.py", 0.9)],
        );
        assert_eq!(
            paths,
            vec!["tests/orbit/test_pipeline.py", "src/orbit/pipeline.py"]
        );
    }

    #[test]
    fn an_admitted_sibling_scores_below_every_ranked_result_in_its_tier() {
        let mut diagnostics = RetrievalDiagnostics::default();
        let admitted = admit_derived_siblings(
            vec![
                ranked_result("src/orbit/pipeline.py", 0.9),
                ranked_result("src/orbit/scheduler.py", 0.5),
            ],
            &derived_fixture_files(),
            &[],
            &TaskSearchIntent::parse("Fix pipeline batching"),
            &RankingOptions::default(),
            5,
            &mut diagnostics,
            &mut derived_fixture_siblings,
        );
        let sibling = admitted
            .iter()
            .find(|result| result.path.ends_with("test_pipeline.py"))
            .expect("the test sibling is admitted");
        // Below the whole list: its own (test) tier is empty, so it opens one at the floor.
        assert!(
            sibling.score < 0.5,
            "sibling scored {}, at or above a ranked result",
            sibling.score
        );
        assert!(sibling.score >= 0.0);
    }

    #[test]
    fn an_admitted_sibling_never_displaces_a_result_already_in_its_tier() {
        // Every rank loss the first placement caused was this: a sibling inserted at the head
        // of its tier pushed a gold file from rank 5 to 6.
        let (paths, _) = admitted_paths(
            "Fix pipeline batching",
            vec![
                ranked_result("src/orbit/pipeline.py", 0.9),
                ranked_result("src/orbit/scheduler.py", 0.5),
                ranked_result("tests/orbit/other_test.py", 0.4),
            ],
        );
        assert_eq!(
            paths,
            vec![
                "src/orbit/pipeline.py",
                "src/orbit/scheduler.py",
                "tests/orbit/other_test.py",
                "tests/orbit/test_pipeline.py",
            ]
        );
    }

    #[test]
    fn a_source_candidate_admits_its_test_sibling_with_the_edge_as_evidence() {
        let (paths, diagnostics) = admitted_paths(
            "Fix pipeline batching",
            vec![
                ranked_result("/repo/src/orbit/pipeline.py", 0.9),
                ranked_result("src/orbit/scheduler.py", 0.5),
            ],
        );
        // The test is below every source (the task is not about tests) but is now in the pack;
        // the origin's own (absolute) path is left as the stream produced it.
        assert_eq!(
            paths,
            vec![
                "/repo/src/orbit/pipeline.py",
                "src/orbit/scheduler.py",
                "tests/orbit/test_pipeline.py",
            ]
        );
        let trace = diagnostics
            .traces
            .iter()
            .find(|trace| trace.path.ends_with("test_pipeline.py"))
            .expect("an admitted sibling is traced");
        assert_eq!(trace.authority, RetrievalAuthority::Heuristic);
        assert_eq!(
            trace.contributions[0].evidence_refs,
            vec!["derived:edge:test-pairing:tests/orbit/test_pipeline.py".to_string()]
        );
    }

    #[test]
    fn a_test_candidate_admits_its_source_sibling_into_the_source_tier() {
        let (paths, _) = admitted_paths(
            "Fix pipeline batching",
            vec![
                ranked_result("src/orbit/scheduler.py", 0.9),
                ranked_result("tests/orbit/test_pipeline.py", 0.7),
            ],
        );
        assert_eq!(
            paths,
            vec![
                "src/orbit/scheduler.py",
                "src/orbit/pipeline.py",
                "tests/orbit/test_pipeline.py",
            ]
        );
    }

    #[test]
    fn a_generated_sibling_keeps_its_demotion_unless_the_task_names_it() {
        let (paths, diagnostics) = admitted_paths(
            "Fix orbit batching",
            vec![
                ranked_result("src/orbit/models/modular_orbit.py", 0.9),
                ranked_result("src/orbit/scheduler.py", 0.5),
            ],
        );
        assert_eq!(
            paths,
            vec![
                "src/orbit/models/modular_orbit.py",
                "src/orbit/scheduler.py",
                "src/orbit/models/modeling_orbit.py",
            ]
        );
        let trace = diagnostics
            .traces
            .iter()
            .find(|trace| trace.path.ends_with("modeling_orbit.py"))
            .unwrap();
        assert_eq!(trace.authority, RetrievalAuthority::Corroborating);

        // Named by the task, the generated sibling joins the source tier instead of the
        // bottom — it still closes that tier rather than displacing a source already ranked
        // there, which here leaves the order unchanged but its tier is now 2, not 0.
        let (paths, _) = admitted_paths(
            "Fix modeling_orbit hidden size",
            vec![
                ranked_result("src/orbit/models/modular_orbit.py", 0.9),
                ranked_result("src/orbit/scheduler.py", 0.5),
                ranked_result("tests/orbit/test_pipeline.py", 0.4),
            ],
        );
        assert_eq!(
            paths,
            vec![
                "src/orbit/models/modular_orbit.py",
                "src/orbit/scheduler.py",
                "src/orbit/models/modeling_orbit.py",
                // the test candidate's own source sibling, admitted into the same tier
                "src/orbit/pipeline.py",
                "tests/orbit/test_pipeline.py",
            ],
            "a named generated sibling closes the source tier, above the demoted test block"
        );
    }

    #[test]
    fn present_siblings_are_not_duplicated_and_a_failed_lookup_is_a_caveat() {
        let (paths, _) = admitted_paths(
            "Fix pipeline batching",
            vec![
                ranked_result("src/orbit/pipeline.py", 0.9),
                ranked_result("tests/orbit/test_pipeline.py", 0.4),
            ],
        );
        assert_eq!(
            paths,
            vec!["src/orbit/pipeline.py", "tests/orbit/test_pipeline.py"]
        );

        let mut diagnostics = RetrievalDiagnostics::default();
        let admitted = admit_derived_siblings(
            vec![ranked_result("src/orbit/pipeline.py", 0.9)],
            &derived_fixture_files(),
            &[],
            &TaskSearchIntent::parse("Fix pipeline batching"),
            &RankingOptions::default(),
            5,
            &mut diagnostics,
            &mut |_| {
                Err(open_kioku_errors::OkError::Storage(
                    "semantics mismatch".into(),
                ))
            },
        );
        assert_eq!(admitted.len(), 1);
        assert!(
            diagnostics.caveats.iter().any(|caveat| caveat
                .starts_with("derived-file siblings could not be read")
                && caveat.contains("src/orbit/pipeline.py")),
            "{:?}",
            diagnostics.caveats
        );
    }

    #[test]
    fn scope_tokens_match_path_segments_and_stems() {
        let fs = vec!["fs".to_string()];
        assert!(path_matches_scope("fs/walk.ts", &fs));
        assert!(path_matches_scope("src/fs.rs", &fs));
        assert!(!path_matches_scope("fsync/mod.ts", &fs));
        let posix = vec!["path".to_string(), "posix".to_string()];
        assert!(path_matches_scope("path/posix/join.ts", &posix));
        assert!(!path_matches_scope("path/windows/join.ts", &posix));
        assert!(!path_matches_scope("anything", &[]));
        let intent = TaskSearchIntent::parse("docs(yaml): correct minor typo");
        assert_eq!(intent.scope_anchors, vec!["yaml"]);
        assert_eq!(
            task_relevance_tier(
                std::path::Path::new("yaml/yaml.ts"),
                &SearchResult {
                    path: "yaml/yaml.ts".into(),
                    line_range: None,
                    snippet: String::new(),
                    symbol: None,
                    score: 1.0,
                    match_reason: String::new(),
                    evidence: Vec::new(),
                    evidence_refs: Vec::new(),
                    confidence: 0.5,
                    score_breakdown: Vec::new(),
                },
                "yaml/yaml.ts",
                &intent
            ),
            3
        );
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
    fn primary_limit_removes_hidden_candidates_before_downstream_authority_derivation() {
        let visible = SearchResult {
            path: "src/visible.rs".into(),
            line_range: None,
            snippet: "fn visible() {}".into(),
            symbol: None,
            score: 2.0,
            match_reason: "visible fixture".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["visible:evidence".into()],
            confidence: 0.9,
            score_breakdown: Vec::new(),
        };
        let hidden = SearchResult {
            path: "src/hidden.rs".into(),
            line_range: None,
            snippet: "fn hidden() {}".into(),
            symbol: None,
            score: 1.0,
            match_reason: "hidden fixture".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["hidden:evidence".into()],
            confidence: 0.8,
            score_breakdown: Vec::new(),
        };
        let bounded = bounded_primary_results(vec![visible.clone(), hidden], 1);
        assert_eq!(bounded.len(), 1);
        assert_eq!(bounded[0].path, visible.path);
        assert_eq!(bounded[0].evidence_refs, visible.evidence_refs);
        assert!(bounded
            .iter()
            .all(|result| result.path != Path::new("src/hidden.rs")));
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
                caveats: vec!["ambiguous exact symbol anchor".into()],
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
    fn zero_token_renderers_preserve_fail_closed_selection_telemetry() {
        let diagnostics = RetrievalDiagnostics {
            selection: open_kioku_core::ContextSelectionDiagnostics {
                budget: ContextBudget {
                    max_tokens: 0,
                    ..ContextBudget::from_file_limit(4)
                },
                exact_evidence_count: 1,
                ambiguity_unresolved_count: 2,
                retrieval_confidence: Some(Confidence::Low),
                abstention_reason: Some("no_candidate_fit_context_selection".into()),
                omitted_high_value: vec![
                    "src/high_value.rs:10-20 exact evidence omitted by zero-token budget".into(),
                ],
                ..Default::default()
            },
            ..Default::default()
        };

        let mut markdown = String::new();
        write_markdown_retrieval_diagnostics(&mut markdown, &diagnostics);
        assert!(markdown.contains("Abstention reason: `no_candidate_fit_context_selection`"));
        assert!(markdown.contains("High-value omissions:"));
        assert!(markdown
            .contains("src/high_value.rs:10-20 exact evidence omitted by zero-token budget"));
        assert!(markdown.contains("Retrieval confidence: `Low`"));
        assert!(
            markdown.contains("Exact-evidence selections: `1`; ambiguity/unresolved signals: `2`")
        );
        assert!(!markdown.contains("Context budget:"));

        let mut prompt = String::new();
        write_prompt_retrieval_diagnostics(&mut prompt, &diagnostics);
        assert!(prompt.contains("RETRIEVAL_ABSTENTION_REASON: no_candidate_fit_context_selection"));
        assert!(prompt.contains(
            "CONTEXT_HIGH_VALUE_OMISSION: src/high_value.rs:10-20 exact evidence omitted by zero-token budget"
        ));
        assert!(prompt.contains("RETRIEVAL_CONFIDENCE: Low"));
        assert!(prompt.contains("RETRIEVAL_EXACT_EVIDENCE_COUNT: 1"));
        assert!(prompt.contains("RETRIEVAL_AMBIGUITY_UNRESOLVED_COUNT: 2"));
        assert!(!prompt.contains("CONTEXT_BUDGET:"));
    }

    #[test]
    fn positive_budget_renderers_keep_budget_summary_and_selection_telemetry() {
        let diagnostics = RetrievalDiagnostics {
            selection: open_kioku_core::ContextSelectionDiagnostics {
                budget: ContextBudget {
                    max_tokens: 1_000,
                    reserve_for_instructions: 100,
                    reserve_for_validation: 100,
                    ..ContextBudget::from_file_limit(4)
                },
                available_context_tokens: 800,
                exact_evidence_count: 1,
                retrieval_confidence: Some(Confidence::High),
                abstention_reason: Some("positive_budget_control".into()),
                omitted_high_value: vec!["control omission".into()],
                ..Default::default()
            },
            ..Default::default()
        };

        let mut markdown = String::new();
        write_markdown_retrieval_diagnostics(&mut markdown, &diagnostics);
        assert!(markdown.contains("Context budget: `1000` tokens (`800` available after reserves)"));
        assert!(markdown.contains("Abstention reason: `positive_budget_control`"));
        assert!(markdown.contains("control omission"));

        let mut prompt = String::new();
        write_prompt_retrieval_diagnostics(&mut prompt, &diagnostics);
        assert!(prompt.contains("CONTEXT_BUDGET: max=1000 available=800"));
        assert!(prompt.contains("RETRIEVAL_ABSTENTION_REASON: positive_budget_control"));
        assert!(prompt.contains("CONTEXT_HIGH_VALUE_OMISSION: control omission"));
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
        assert_eq!(diagnostics.selection.ambiguity_unresolved_count, 0);
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
        let mut diagnostics = RetrievalDiagnostics {
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
        diagnostics.selection.budget.max_tokens = 1_000;
        diagnostics.selection.available_context_tokens = 900;

        assert!(retrieval_trace_for_result(&diagnostics, &result).is_none());
        assert_eq!(
            retrieval_authority_for_result(&diagnostics, &result),
            RetrievalAuthority::Heuristic
        );

        refresh_context_pack_retrieval_telemetry(
            &mut diagnostics,
            std::slice::from_ref(&result),
            &ConfidenceBreakdown::default(),
        );

        assert_eq!(diagnostics.selection.unattributed_selected_file_count, 1);
        assert_eq!(diagnostics.selection.ambiguity_unresolved_count, 1);
        assert_eq!(diagnostics.selection.exact_evidence_count, 0);
        assert!(diagnostics.selection.source_stream_mix.is_empty());
        assert!(diagnostics
            .selection
            .caveats
            .iter()
            .any(|caveat| caveat.contains("ambiguous or unavailable")));

        let json = serde_json::to_string(&diagnostics).unwrap();
        assert!(json.contains("\"ambiguity_unresolved_count\":1"));

        let mut markdown = String::new();
        write_markdown_retrieval_diagnostics(&mut markdown, &diagnostics);
        assert!(markdown.contains("ambiguity/unresolved signals: `1`"));
        assert!(markdown.contains("Selection caveats:"));
        assert!(markdown.contains("ambiguous or unavailable"));

        let mut prompt = String::new();
        write_prompt_retrieval_diagnostics(&mut prompt, &diagnostics);
        assert!(prompt.contains("RETRIEVAL_AMBIGUITY_UNRESOLVED_COUNT: 1"));
        assert!(prompt.contains("RETRIEVAL_SELECTION_CAVEAT:"));
        assert!(prompt.contains("ambiguous or unavailable"));
    }

    #[test]
    fn trace_to_code_missing_runtime_evidence_blocks_without_heuristic_substitution() {
        let routing = routing::classify_task("investigate runtime error stack trace in checkout");
        assert_eq!(routing.family, open_kioku_core::TaskFamily::TraceToCode);
        let mut diagnostics = RetrievalDiagnostics::default();
        diagnostics.routing = routing.diagnostics();
        // The runtime stream ran (traces are ingested) and found nothing for this task.
        diagnostics.sources_succeeded = vec![RetrievalSourceKind::Runtime];
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
        diagnostics.sources_succeeded = vec![
            RetrievalSourceKind::ExactSemantic,
            RetrievalSourceKind::Graph,
        ];
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
    fn required_source_absent_from_the_repository_is_a_caveat_not_a_blocker() {
        // "panic" routes to trace_to_code; on a repository that has never ingested a runtime
        // trace the runtime stream is unavailable, which must not blank the pack.
        let routing = routing::classify_task("Fix missing method NameNormalized panic");
        assert_eq!(routing.family, open_kioku_core::TaskFamily::TraceToCode);
        let mut diagnostics = RetrievalDiagnostics::default();
        diagnostics.routing = routing.diagnostics();
        diagnostics.sources_attempted = vec![RetrievalSourceKind::Runtime];
        diagnostics.sources_succeeded = Vec::new();
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
            .any(|caveat| caveat.contains("runtime") && caveat.contains("unavailable")));
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

    fn tier_probe(path: &str, score: f32) -> SearchResult {
        SearchResult {
            path: path.into(),
            line_range: Some(LineRange { start: 1, end: 10 }),
            snippet: "class GeoIpProcessor implements Processor".into(),
            symbol: None,
            score,
            match_reason: "probe".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        }
    }

    #[test]
    fn sentence_initial_capitals_are_not_edit_anchors() {
        let intent = TaskSearchIntent::parse("Enable the index refresh block");
        assert!(
            intent.primary_anchors.is_empty(),
            "{:?}",
            intent.primary_anchors
        );
        let intent = TaskSearchIntent::parse("Fix append writes test for backing indices");
        assert!(
            intent.primary_anchors.is_empty(),
            "{:?}",
            intent.primary_anchors
        );
        let intent = TaskSearchIntent::parse("Assert SystemIndexDescriptor is not used");
        assert_eq!(
            intent.primary_anchors,
            vec!["SystemIndexDescriptor".to_string()]
        );
        for token in [
            "random_score",
            "getFoo",
            "ES819",
            "max-age",
            "IpPrefixAutomatonUtil",
        ] {
            assert!(is_named_identifier(token), "{token}");
        }
        for token in ["Enable", "Fix", "HTTP", "index", "Index"] {
            assert!(!is_named_identifier(token), "{token}");
        }
    }

    #[test]
    fn source_outranks_equally_authoritative_tests_unless_the_task_wants_tests() {
        let source = "modules/ip-location/src/main/java/org/es/GeoIpProcessor.java";
        let unit_test = "modules/ip-location/src/test/java/org/es/GeoIpProcessorTests.java";
        let cluster_test =
            "modules/ip-location/src/internalClusterTest/java/org/es/GeoIpDownloaderIT.java";
        // The tests carry the higher fused score: on a real repository they were voted for by
        // both the lexical and the validation stream while the processor had one vote.
        let candidates = || {
            vec![
                tier_probe(cluster_test, 0.9),
                tier_probe(unit_test, 0.8),
                tier_probe(source, 0.3),
            ]
        };

        let ordered = rerank_fused_for_task(
            candidates(),
            &TaskSearchIntent::parse("geoip processor"),
            &RetrievalDiagnostics::default(),
        );
        assert_eq!(
            ordered
                .iter()
                .map(|r| r.path.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
            vec![
                source.to_string(),
                cluster_test.to_string(),
                unit_test.to_string()
            ]
        );

        let ordered = rerank_fused_for_task(
            candidates(),
            &TaskSearchIntent::parse("tests for the geoip processor"),
            &RetrievalDiagnostics::default(),
        );
        assert_eq!(
            ordered
                .first()
                .map(|r| r.path.to_string_lossy().to_string()),
            Some(cluster_test.to_string()),
            "a task about tests lets the best-scoring test lead"
        );
    }

    #[test]
    fn post_fusion_quality_tier_preserves_boundary_and_path_quality_policy() {
        let options = RankingOptions::default();
        assert_eq!(
            context_quality_tier(Path::new("src/service.rs"), &options, false, false),
            2
        );
        assert_eq!(
            context_quality_tier(Path::new("tests/service_test.rs"), &options, false, false),
            1
        );
        assert_eq!(
            context_quality_tier(
                Path::new(
                    "modules/ip-location/src/internalClusterTest/java/GeoIpDownloaderIT.java"
                ),
                &options,
                false,
                false
            ),
            1
        );
        assert_eq!(
            context_quality_tier(Path::new("tests/service_test.rs"), &options, true, false),
            2,
            "a task about tests keeps test files in the source tier"
        );
        assert_eq!(
            context_quality_tier(Path::new("docs/guide.md"), &options, true, false),
            1,
            "wanting tests does not promote docs"
        );
        assert_eq!(
            context_quality_tier(Path::new("docs/guide.md"), &options, false, true),
            2,
            "a documentation task keeps docs in the source tier"
        );
        assert_eq!(
            context_quality_tier(
                Path::new("src/generated/service.rs"),
                &options,
                false,
                false
            ),
            0
        );
        assert_eq!(
            context_quality_tier(Path::new("vendor/service.rs"), &options, false, false),
            0
        );

        let baseline = RankingOptions {
            mode: open_kioku_ranking::RankingMode::Baseline,
            ..RankingOptions::default()
        };
        assert_eq!(
            context_quality_tier(
                Path::new("src/generated/service.rs"),
                &baseline,
                false,
                false
            ),
            2
        );

        let without_path_quality = RankingOptions {
            mode: open_kioku_ranking::RankingMode::WithoutSignal(
                open_kioku_ranking::RankingSignal::PathQuality,
            ),
            ..RankingOptions::default()
        };
        assert_eq!(
            context_quality_tier(
                Path::new("src/generated/service.rs"),
                &without_path_quality,
                false,
                false
            ),
            2
        );
    }

    #[test]
    fn zero_available_tokens_record_exact_high_value_omission_and_render_it() {
        let exact = SearchResult {
            path: "src/exact.rs".into(),
            line_range: Some(LineRange { start: 7, end: 11 }),
            snippet: "fn exact_target() {}".into(),
            symbol: None,
            score: 1.0,
            match_reason: "exact fixture".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["symbol:exact".into()],
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![RetrievalTrace {
                path: exact.path.clone(),
                unit_key: Some(RetrievalUnitKey::from_result(&exact)),
                fused_score: exact.score,
                authority: RetrievalAuthority::Exact,
                contributions: Vec::new(),
            }],
            ..Default::default()
        };
        let budget = ContextBudget {
            max_tokens: 100,
            reserve_for_instructions: 50,
            reserve_for_validation: 50,
            max_per_file: 2,
            max_primary_files: 4,
            ..ContextBudget::default()
        };

        let selected = select_context_units(vec![exact], &budget, &mut diagnostics);

        assert!(selected.is_empty());
        assert_eq!(diagnostics.selection.omitted_due_to_budget.len(), 1);
        assert_eq!(diagnostics.selection.omitted_high_value.len(), 1);
        assert!(diagnostics.selection.omitted_high_value[0].contains("src/exact.rs:7-11"));

        let json = serde_json::to_string(&diagnostics).unwrap();
        assert!(json.contains("omitted_high_value"));
        assert!(json.contains("src/exact.rs:7-11"));

        let mut markdown = String::new();
        write_markdown_retrieval_diagnostics(&mut markdown, &diagnostics);
        assert!(markdown.contains("High-value omissions:"));
        assert!(markdown.contains("src/exact.rs:7-11"));

        let mut prompt = String::new();
        write_prompt_retrieval_diagnostics(&mut prompt, &diagnostics);
        assert!(prompt.contains("CONTEXT_HIGH_VALUE_OMISSION: src/exact.rs:7-11"));
    }

    #[test]
    fn zero_primary_file_capacity_records_graph_high_value_omission() {
        let graph = SearchResult {
            path: "src/graph.rs".into(),
            line_range: None,
            snippet: "fn graph_target() {}".into(),
            symbol: None,
            score: 0.8,
            match_reason: "graph fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.8,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![RetrievalTrace {
                path: graph.path.clone(),
                unit_key: Some(RetrievalUnitKey::from_result(&graph)),
                fused_score: graph.score,
                authority: RetrievalAuthority::Corroborating,
                contributions: vec![open_kioku_core::RetrievalContribution {
                    source: RetrievalSourceKind::Graph,
                    rank: 1,
                    raw_score: Some(0.8),
                    rrf_contribution: 0.1,
                    authority: RetrievalAuthority::Corroborating,
                    symbol_id: None,
                    evidence_refs: vec!["graph:edge".into()],
                    rationale: "graph fixture".into(),
                }],
            }],
            ..Default::default()
        };
        let budget = ContextBudget {
            max_tokens: 1_000,
            reserve_for_instructions: 100,
            reserve_for_validation: 100,
            max_per_file: 2,
            max_primary_files: 0,
            ..ContextBudget::default()
        };

        let selected = select_context_units(vec![graph], &budget, &mut diagnostics);

        assert!(selected.is_empty());
        assert_eq!(diagnostics.selection.omitted_due_to_budget.len(), 1);
        assert_eq!(diagnostics.selection.omitted_high_value.len(), 1);
        assert!(diagnostics.selection.omitted_high_value[0].contains("src/graph.rs"));
    }

    #[test]
    fn zero_capacity_heuristic_candidate_is_not_marked_high_value() {
        let heuristic = SearchResult {
            path: "src/heuristic.rs".into(),
            line_range: None,
            snippet: "fn maybe_target() {}".into(),
            symbol: None,
            score: 0.5,
            match_reason: "heuristic fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics::default();
        let budget = ContextBudget {
            max_tokens: 100,
            reserve_for_instructions: 50,
            reserve_for_validation: 50,
            max_per_file: 2,
            max_primary_files: 4,
            ..ContextBudget::default()
        };

        let selected = select_context_units(vec![heuristic], &budget, &mut diagnostics);

        assert!(selected.is_empty());
        assert_eq!(diagnostics.selection.omitted_due_to_budget.len(), 1);
        assert!(diagnostics.selection.omitted_high_value.is_empty());
    }

    #[test]
    fn zero_capacity_ambiguous_legacy_path_traces_do_not_borrow_high_value_authority() {
        let result = SearchResult {
            path: "src/shared.rs".into(),
            line_range: Some(LineRange { start: 3, end: 9 }),
            snippet: "fn shared() {}".into(),
            symbol: None,
            score: 1.0,
            match_reason: "ambiguous legacy fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics {
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
                    authority: RetrievalAuthority::Corroborating,
                    contributions: vec![open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::Graph,
                        rank: 1,
                        raw_score: Some(0.5),
                        rrf_contribution: 0.05,
                        authority: RetrievalAuthority::Corroborating,
                        symbol_id: None,
                        evidence_refs: Vec::new(),
                        rationale: "ambiguous graph fixture".into(),
                    }],
                },
            ],
            ..Default::default()
        };
        let budget = ContextBudget {
            max_tokens: 100,
            reserve_for_instructions: 50,
            reserve_for_validation: 50,
            max_per_file: 2,
            max_primary_files: 4,
            ..ContextBudget::default()
        };

        let selected = select_context_units(vec![result], &budget, &mut diagnostics);

        assert!(selected.is_empty());
        assert_eq!(diagnostics.selection.omitted_due_to_budget.len(), 1);
        assert!(diagnostics.selection.omitted_high_value.is_empty());
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
            ..ContextBudget::default()
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
            ..ContextBudget::default()
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
            ..ContextBudget::default()
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
            ..ContextBudget::default()
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
            ..ContextBudget::default()
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

    fn lattice_fixture() -> (Vec<CodeChunk>, Vec<File>, Vec<Symbol>) {
        let repo_id = RepositoryId::new("repo");
        let file = |id: &str, path: &str| File {
            id: FileId::new(id),
            repository_id: repo_id.clone(),
            path: path.into(),
            language: Language::Java,
            size_bytes: 100,
            content_hash: id.into(),
            is_generated: false,
            is_vendor: false,
        };
        let symbol = |id: &str, name: &str| Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("org.example.{name}"),
            kind: SymbolKind::Class,
            file_id: FileId::new(id),
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
        let chunk = |id: &str, text: &str| CodeChunk {
            id: format!("{id}-chunk"),
            file_id: FileId::new(id),
            range: LineRange { start: 1, end: 10 },
            language: Language::Java,
            text: text.into(),
            symbol_id: Some(SymbolId::new(id)),
        };
        let files = vec![
            file(
                "utils",
                "server/src/main/java/org/example/util/CollectionUtils.java",
            ),
            file(
                "utils-tests",
                "server/src/test/java/org/example/util/CollectionUtilsTests.java",
            ),
            file(
                "other",
                "server/src/test/java/org/example/other/SomeOtherTests.java",
            ),
        ];
        let symbols = vec![
            symbol("utils", "CollectionUtils"),
            symbol("utils-tests", "CollectionUtilsTests"),
            symbol("other", "SomeOtherTests"),
            symbol("utils-sort", "sortArray"),
        ];
        let chunks = vec![
            chunk(
                "utils",
                "public class CollectionUtils { static int[] sort(int[] a) {} }",
            ),
            chunk(
                "utils-tests",
                "public class CollectionUtilsTests { public void testSort() {} }",
            ),
            chunk(
                "other",
                "public class SomeOtherTests { public void testSomething() {} } // tests",
            ),
        ];
        (chunks, files, symbols)
    }

    #[test]
    fn misinflected_task_identifier_reaches_the_file_that_names_it() {
        let (chunks, files, symbols) = lattice_fixture();
        let task = "CollectionsUtils Tests";
        let intent = TaskSearchIntent::parse(task).with_repository_vocabulary(&files, &symbols);
        assert_eq!(intent.lattice_anchors.len(), 1);
        assert_eq!(intent.lattice_anchors[0].term, "CollectionUtils");
        let results = rerank_for_task(
            search_candidates(&chunks, &files, &symbols, task, 10, &intent).unwrap(),
            &intent,
            &RankingOptions::default(),
        );
        let paths = results
            .iter()
            .map(|result| result.path.display().to_string())
            .collect::<Vec<_>>();
        assert!(
            paths[0].ends_with("CollectionUtilsTests.java")
                || paths[0].ends_with("CollectionUtils.java"),
            "the reached identifier's files must lead: {paths:?}"
        );
        assert!(
            paths.iter().take(2).all(|path| path.contains("CollectionUtils")),
            "both files naming the reached identifier outrank the file that merely says tests: {paths:?}"
        );
        let first = &results[0];
        assert!(
            first
                .evidence
                .iter()
                .any(|line| line.starts_with("identifier lattice:")
                    && line.contains("`CollectionsUtils`")
                    && line.contains("`CollectionUtils`")),
            "a lattice hop must be visible in evidence: {:?}",
            first.evidence
        );
        assert!(
            first
                .score_breakdown
                .iter()
                .any(|component| component.signal == "identifier_lattice_anchor_boost"),
            "the boost must be a traceable score component: {:?}",
            first.score_breakdown
        );
        // Without the repository vocabulary the misspelling reaches nothing: the same fixture
        // ranks the file that merely mentions "tests" level with the real target.
        let plain = TaskSearchIntent::parse(task);
        assert!(plain.lattice_anchors.is_empty());
    }

    #[test]
    fn lattice_hops_feed_lexical_terms_but_not_exact_symbol_terms() {
        let (_, files, symbols) = lattice_fixture();
        let task = "CollectionsUtils Tests";
        let intent = TaskSearchIntent::parse(task).with_repository_vocabulary(&files, &symbols);
        let shared = intent.search_terms(task);
        let lexical = intent.lexical_search_terms(task);
        assert!(
            !shared.iter().any(|term| term == "CollectionUtils"),
            "shared terms seed exact-symbol anchors and must not carry lattice hops: {shared:?}"
        );
        assert!(
            lexical.iter().any(|term| term == "CollectionUtils"),
            "lexical terms carry the hop: {lexical:?}"
        );
        assert!(intent.vocabulary_caveats().is_empty());
    }

    fn lattice_hop(
        term: &str,
        origin: &str,
        from_primary: bool,
        ambiguous: bool,
    ) -> lattice::LatticeTerm {
        lattice::LatticeTerm {
            term: term.into(),
            origin: origin.into(),
            from_primary,
            relation: lattice::LatticeRelation::Stem,
            ambiguous,
        }
    }

    fn plain_result(path: &str, score: f32) -> SearchResult {
        SearchResult {
            path: path.into(),
            line_range: None,
            snippet: String::new(),
            symbol: None,
            score,
            match_reason: String::new(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        }
    }

    fn tier_of(intent: &TaskSearchIntent, result: &SearchResult) -> u8 {
        let haystack = searchable_result_text(result);
        task_relevance_tier(&result.path.clone(), result, &haystack, intent)
    }

    #[test]
    fn a_lattice_hop_ranks_below_a_name_the_task_spelled_exactly() {
        // Score is the fourth sort key, so what keeps an exact fact on top is the relevance
        // tier, not the size of the boost: a hop must not reach the named-target tier.
        let intent = TaskSearchIntent::parse("Fix retry in the HttpPollers loop")
            .with_lattice_terms(vec![lattice_hop("HttpPoller", "HttpPollers", true, false)]);
        let exact = plain_result("src/poll/HttpPollers.java", 0.1);
        let hopped = plain_result("src/poll/HttpPoller.java", 0.9);
        assert_eq!(tier_of(&intent, &exact), NAMED_TARGET_RELEVANCE_TIER);
        assert_eq!(tier_of(&intent, &hopped), 3);
        let ranked = rerank_fused_for_task(
            vec![hopped, exact],
            &intent,
            &RetrievalDiagnostics::default(),
        );
        assert!(
            ranked[0].path.ends_with("HttpPollers.java"),
            "the exactly-spelled name must lead its re-spelling: {:?}",
            ranked
                .iter()
                .map(|r| r.path.display().to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_lattice_hop_does_not_exempt_a_test_file_from_quality_demotion() {
        let intent = TaskSearchIntent::parse("Fix the HttpPollers backoff")
            .with_lattice_terms(vec![lattice_hop("HttpPoller", "HttpPollers", true, false)]);
        let source = plain_result("src/main/java/net/Backoff.java", 0.2);
        let test = plain_result("src/test/java/net/HttpPollerTests.java", 0.9);
        let ranked = rerank_fused_for_task(
            vec![test, source],
            &intent,
            &RetrievalDiagnostics::default(),
        );
        assert!(
            ranked[0].path.ends_with("Backoff.java"),
            "a test named only through a hop stays support material: {:?}",
            ranked
                .iter()
                .map(|r| r.path.display().to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_ambiguous_hop_gets_neither_the_boost_nor_anchor_relevance() {
        let intent = TaskSearchIntent::parse("Warn when NumFrames exceeds the total")
            .with_lattice_terms(vec![lattice_hop("num_frames", "NumFrames", true, true)]);
        let result = plain_result("src/video/num_frames_reader.py", 0.5);
        assert_eq!(tier_of(&intent, &result), 0);
        let ranked = rerank_fused_for_task(vec![result], &intent, &RetrievalDiagnostics::default());
        assert!(
            !ranked[0]
                .score_breakdown
                .iter()
                .any(|component| component.signal == "identifier_lattice_anchor_boost"),
            "an ambiguous hop must not score: {:?}",
            ranked[0].score_breakdown
        );
        assert!(
            intent
                .vocabulary_caveats()
                .iter()
                .any(|caveat| caveat.contains("denied anchor relevance")),
            "withholding must be disclosed: {:?}",
            intent.vocabulary_caveats()
        );
    }

    #[test]
    fn a_hop_from_a_reference_mention_ranks_with_reference_anchors() {
        let intent = TaskSearchIntent::parse("Add retry to Uploader similar to HttpPollers")
            .with_lattice_terms(vec![lattice_hop("HttpPoller", "HttpPollers", false, false)]);
        assert_eq!(
            tier_of(&intent, &plain_result("src/poll/HttpPoller.java", 0.9)),
            1,
            "a guessed re-spelling of a secondary mention is not an edit target"
        );
    }

    #[test]
    fn unknown_task_identifier_is_a_caveat_not_silence() {
        let (_, files, symbols) = lattice_fixture();
        let intent = TaskSearchIntent::parse("Guard cleanup in QuantumFluxCapacitor")
            .with_repository_vocabulary(&files, &symbols);
        assert!(intent.lattice_anchors.is_empty());
        let caveats = intent.vocabulary_caveats();
        assert_eq!(caveats.len(), 1, "{caveats:?}");
        assert!(caveats[0].contains("`QuantumFluxCapacitor`"));
    }
}

#[cfg(test)]
mod selection_ledger_tests {
    use super::*;
    use open_kioku_core::LineRange;

    fn result(path: &str, snippet: &str) -> SearchResult {
        SearchResult {
            path: path.into(),
            line_range: Some(LineRange { start: 1, end: 4 }),
            snippet: snippet.into(),
            symbol: None,
            score: 0.5,
            match_reason: "impact".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        }
    }

    #[test]
    fn supporting_files_are_costed_at_listing_size_after_the_primary_units() {
        let primary = result("src/primary.rs", "fn primary() {}");
        let mut diagnostics = RetrievalDiagnostics::default();
        record_selected_units(std::slice::from_ref(&primary), &mut diagnostics);
        let primary_tokens = diagnostics.selection.estimated_tokens_selected;
        let supporting = result("src/impacted.rs", &"x".repeat(4_000));

        append_supporting_units(std::slice::from_ref(&supporting), &mut diagnostics);

        let units = &diagnostics.selection.selected_units;
        assert_eq!(units.len(), 2);
        assert_eq!(units[1].path, supporting.path);
        // The listing (path and reason), not the 4,000-character impact snippet, is what costs.
        assert!(
            units[1].estimated_tokens < 40,
            "{}",
            units[1].estimated_tokens
        );
        assert!(units[1]
            .rationale
            .contains("not selected under the context budget"));
        assert_eq!(
            diagnostics.selection.estimated_tokens_selected,
            primary_tokens + units[1].estimated_tokens
        );
        assert_eq!(
            diagnostics.selection.per_file_tokens[&supporting.path],
            units[1].estimated_tokens
        );
    }

    /// The scorer that measures what retrieval selected splits the ledger on `kind`. A pack
    /// whose supporting units were not marked would be scored as if impact expansion's files
    /// were retrieved, so the two kinds must stay distinguishable by field, not by prose.
    #[test]
    fn ledger_units_carry_the_kind_that_tells_selection_from_impact_expansion() {
        let primary = result("src/primary.rs", "fn primary() {}");
        let mut diagnostics = RetrievalDiagnostics::default();
        record_selected_units(std::slice::from_ref(&primary), &mut diagnostics);
        append_supporting_units(
            std::slice::from_ref(&result("src/impacted.rs", "fn impacted() {}")),
            &mut diagnostics,
        );

        let units = &diagnostics.selection.selected_units;
        assert_eq!(units[0].kind, ContextUnitKind::Primary);
        assert_eq!(units[1].kind, ContextUnitKind::Supporting);
        let json = serde_json::to_value(&diagnostics.selection).unwrap();
        let serialized = &json["selected_units"];
        assert_eq!(serialized[0]["kind"], "primary");
        assert_eq!(serialized[1]["kind"], "supporting");
        // A ledger deserialized without the field predates it and is primary-only.
        let legacy: ContextSelectedUnit = serde_json::from_value(serde_json::json!({
            "path": "src/legacy.rs",
            "estimated_tokens": 10,
            "authority": "heuristic",
            "rationale": "legacy pack"
        }))
        .unwrap();
        assert_eq!(legacy.kind, ContextUnitKind::Primary);
    }

    #[test]
    fn supporting_files_are_not_listed_when_no_selection_ran() {
        let mut diagnostics = RetrievalDiagnostics::default();
        append_supporting_units(&[result("src/impacted.rs", "fn f() {}")], &mut diagnostics);
        assert!(diagnostics.selection.selected_units.is_empty());
        assert_eq!(diagnostics.selection.estimated_tokens_selected, 0);
    }

    #[test]
    fn rationale_names_each_region_step() {
        let mut result = result("src/a.rs", "fn a() {}");
        result.evidence_refs = vec![
            "search:src/a.rs:1-4:0".into(),
            "region:enclosing-symbol:sym".into(),
            "region:adjacent-unit:5-9".into(),
            "region:adjacent-unit:10-14".into(),
        ];
        let rationale = selection_rationale(&result, RetrievalAuthority::Heuristic);
        assert!(rationale.starts_with("selected under context budget"));
        assert!(
            rationale.ends_with("region widened: region:enclosing-symbol, region:adjacent-unit x2")
        );

        result.evidence_refs = vec!["region:ranked-unit:4".into()];
        assert!(selection_rationale(&result, RetrievalAuthority::Exact)
            .starts_with("re-admitted by region widening"));
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

    fn abstention_test_policy() -> open_kioku_core::abstention::RuntimeAbstentionPolicy {
        open_kioku_core::abstention::RuntimeAbstentionPolicy {
            min_top_score_margin: None,
            min_independent_streams: Some(2),
            max_ambiguity_unresolved: None,
        }
    }

    #[test]
    fn calibrated_abstention_marks_weakly_supported_packs() {
        let mut pack = open_kioku_core::ContextPack {
            confidence_summary: "baseline summary".into(),
            ..Default::default()
        };
        crate::apply_calibrated_abstention(Some(&abstention_test_policy()), &mut pack);
        let reason = pack
            .retrieval_diagnostics
            .selection
            .abstention_reason
            .expect("weakly supported pack should carry a calibrated abstention reason");
        assert!(
            reason.starts_with(open_kioku_core::abstention::CALIBRATED_ABSTENTION_REASON_PREFIX)
        );
        assert!(pack
            .retrieval_diagnostics
            .caveats
            .iter()
            .any(|caveat| caveat.contains("insufficient evidence")));
        assert!(pack.confidence_summary.starts_with("Calibrated abstention"));
    }

    #[test]
    fn calibrated_abstention_never_overrides_routing_blockers_or_exact_evidence() {
        // Routing-contract blockers keep precedence.
        let mut blocked = open_kioku_core::ContextPack::default();
        blocked.retrieval_diagnostics.selection.abstention_reason =
            Some("no_candidate_fit_context_selection".into());
        crate::apply_calibrated_abstention(Some(&abstention_test_policy()), &mut blocked);
        assert_eq!(
            blocked.retrieval_diagnostics.selection.abstention_reason,
            Some("no_candidate_fit_context_selection".into())
        );

        // Exact evidence shields against weak margin/stream gates.
        let mut exact = open_kioku_core::ContextPack::default();
        exact.retrieval_diagnostics.selection.exact_evidence_count = 1;
        crate::apply_calibrated_abstention(Some(&abstention_test_policy()), &mut exact);
        assert_eq!(
            exact.retrieval_diagnostics.selection.abstention_reason,
            None
        );

        // No activated policy: never abstain.
        let mut off = open_kioku_core::ContextPack::default();
        crate::apply_calibrated_abstention(None, &mut off);
        assert_eq!(off.retrieval_diagnostics.selection.abstention_reason, None);
    }
}
