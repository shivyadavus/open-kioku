use chrono::Utc;
use open_kioku_core::{
    AnalysisFact, ChangeBoundary, CodeChunk, Confidence, ConfidenceBreakdown,
    ConfidenceSignalInput, ContextPack, Evidence, EvidenceId, EvidenceSourceType, File, FileRange,
    GraphEdge, GraphEdgeType, GraphNodeType, NegativeEvidence, RiskReport, RuntimeSignal,
    ScoreComponent, SearchResult, Symbol, ValidationPlan,
};
use open_kioku_errors::Result;
use open_kioku_impact::ImpactEngine;
use open_kioku_ranking::{rerank_with_options, RankingOptions};
use open_kioku_search_regex::search_chunks;
use open_kioku_storage::OkStore;
use open_kioku_tests::TestSelector;

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
    ranking_options: RankingOptions,
}

impl<'a> ContextPackBuilder<'a> {
    pub fn new(store: &'a dyn OkStore) -> Self {
        Self {
            store,
            ranking_options: RankingOptions::default(),
        }
    }

    pub fn with_ranking_options(mut self, ranking_options: RankingOptions) -> Self {
        self.ranking_options = ranking_options;
        self
    }

    pub fn build(&self, task: &str, limit: usize) -> Result<ContextPack> {
        let files = self.store.list_files(usize::MAX, 0)?;
        let chunks = self.store.all_chunks()?;
        let symbols = self.store.list_symbols(None, usize::MAX, 0)?;
        let intent = TaskSearchIntent::parse(task);
        let primary = rerank_for_task(
            search_candidates(&chunks, &files, &symbols, task, limit, &intent)?,
            &intent,
            &self.ranking_options,
        );
        self.build_from_primary_with_impact(task, limit, primary, true)
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
        )
    }

    fn build_from_primary_with_impact(
        &self,
        task: &str,
        limit: usize,
        primary: Vec<SearchResult>,
        expand_impact: bool,
    ) -> Result<ContextPack> {
        let mut primary = primary;
        augment_primary_with_runtime(self.store, task, &mut primary, limit)?;
        let primary_symbols = primary
            .iter()
            .filter_map(|result| result.symbol.clone())
            .take(10)
            .collect::<Vec<_>>();
        let mut tests = Vec::new();
        let selector = TestSelector::new(self.store as &dyn open_kioku_storage::MetadataStore);
        for result in primary.iter().take(3) {
            tests.extend(selector.for_changed_path_with_evidence(&result.path, 5)?);
        }
        tests.truncate(10);
        let impact = if expand_impact {
            if let Some(first) = primary.first() {
                ImpactEngine::new(self.store as &dyn open_kioku_storage::MetadataStore)
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
                dependency_edges.extend(edges);
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
        let runtime_evidence = runtime_signals
            .iter()
            .map(runtime_signal_evidence)
            .collect::<Vec<_>>();

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
                })
            })
            .chain(impact.evidence.clone())
            .chain(runtime_evidence.clone())
            .collect::<Vec<_>>();
        let allowed_files = primary
            .iter()
            .take(8)
            .map(|result| result.path.clone())
            .collect::<Vec<_>>();
        let confidence_breakdown = confidence_for_context(
            &primary_files,
            &supporting_files,
            &tests,
            &impact.risk_report,
            allowed_files.len(),
            evidence.len(),
            runtime_signals.len(),
        );
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
        let confidence_summary = confidence_summary(&confidence_breakdown);
        Ok(ContextPack {
            task: task.into(),
            intent: classify_intent(task).into(),
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
            confidence_summary,
            confidence_breakdown,
        })
    }
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
    }
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
}

impl TaskSearchIntent {
    fn parse(task: &str) -> Self {
        let mut intent = Self::default();
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

        intent
    }

    fn search_terms(&self, task: &str) -> Vec<String> {
        let mut terms = vec![task.to_string()];
        for term in self
            .ticket_anchors
            .iter()
            .chain(self.path_anchors.iter())
            .chain(self.primary_anchors.iter())
            .chain(self.reference_anchors.iter())
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

fn rerank_for_task(
    results: Vec<SearchResult>,
    intent: &TaskSearchIntent,
    ranking_options: &RankingOptions,
) -> Vec<SearchResult> {
    let mut results = rerank_with_options(results, ranking_options);
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
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    results
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
        }],
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
        }],
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
}
