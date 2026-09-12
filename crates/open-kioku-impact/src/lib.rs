use chrono::Utc;
use open_kioku_core::{
    identity, search_result_evidence_ids, AnalysisFact, ChurnSummary, CodeChunk, Confidence,
    Evidence, EvidenceId, EvidenceSourceType, File, FileId, FileRange, GraphEdge, GraphEdgeType,
    GraphNode, GraphNodeType, HistorySignalQuery, HistorySignalSummary, ImpactReport, NodeId,
    RelationshipImpact, RiskReport, ScoreComponent, SearchResult, Symbol, SymbolOccurrence,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_evidence::{RelationshipUseClass, RelationshipUsePolicy};
use open_kioku_search_regex::search_chunks;
use open_kioku_storage::{GraphStore, HistoryStore, MetadataStore, SearchIndex};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// Bounded number of changed-file symbols used to seed relationship-edge impact discovery.
const RELATIONSHIP_IMPACT_SYMBOL_SEEDS: usize = 16;
/// Bounded neighbor fan-out per seed node.
const RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT: usize = 40;
/// Bounded size of each relationship impact list in the report.
const RELATIONSHIP_IMPACT_LIMIT: usize = 25;

pub struct ImpactEngine<'a> {
    store: &'a dyn MetadataStore,
    search_index: Option<&'a dyn SearchIndex>,
    history_store: Option<&'a dyn HistoryStore>,
    graph_store: Option<&'a dyn GraphStore>,
}

impl<'a> ImpactEngine<'a> {
    pub fn new(store: &'a dyn MetadataStore) -> Self {
        Self {
            store,
            search_index: None,
            history_store: None,
            graph_store: None,
        }
    }

    pub fn with_search_index(mut self, search_index: Option<&'a dyn SearchIndex>) -> Self {
        self.search_index = search_index;
        self
    }

    pub fn with_history_store(mut self, history_store: Option<&'a dyn HistoryStore>) -> Self {
        self.history_store = history_store;
        self
    }

    /// Enable relationship-edge impact classification. Without a graph store the report's
    /// `proven_impact`/`possible_impact` lists stay empty rather than guessing.
    pub fn with_graph_store(mut self, graph_store: Option<&'a dyn GraphStore>) -> Self {
        self.graph_store = graph_store;
        self
    }

    pub fn for_file(&self, path: &Path) -> Result<ImpactReport> {
        let file = self.store.get_file_by_path(path)?;
        let target_symbols = if let Some(file) = &file {
            self.store.symbols_for_file(&file.id)?
        } else {
            Vec::new()
        };
        let runtime_facts = if let Some(file) = &file {
            runtime_facts_for_file(self.store, &file.id)?
        } else {
            Vec::new()
        };
        let git_facts = if let Some(file) = &file {
            git_history_facts_for_file(self.store, &file.id)?
        } else {
            Vec::new()
        };
        let service_facts = if let Some(file) = &file {
            service_boundary_facts_for_file(self.store, &file.id)?
        } else {
            Vec::new()
        };
        let complexity_facts = if let Some(file) = &file {
            complexity_facts_for_file(self.store, &file.id)?
        } else {
            Vec::new()
        };
        let churn_summary = if file.is_some() {
            self.history_store
                .map(|store| store.churn_for_file(path))
                .transpose()?
        } else {
            None
        };
        let history_signals = if file.is_some() {
            self.history_store
                .map(|store| {
                    store.history_score_components(
                        &HistorySignalQuery {
                            path: path.to_path_buf(),
                            task: None,
                            symbols: target_symbols
                                .iter()
                                .flat_map(|symbol| {
                                    [symbol.qualified_name.clone(), symbol.name.clone()]
                                })
                                .collect(),
                        },
                        8,
                    )
                })
                .transpose()?
        } else {
            None
        };

        // Without a configured lexical index every search below scans the store in memory.
        // Load that scan corpus once: it used to be reloaded per search term, which on a
        // 10k-file repository meant thirteen full-table loads and ~70 s per context pack.
        let fallback_index = match (self.search_index, &file) {
            (None, Some(_)) => Some(ChunkScanIndex::load(self.store)?),
            _ => None,
        };
        let search_index: Option<&dyn SearchIndex> = self.search_index.or(fallback_index
            .as_ref()
            .map(|index| index as &dyn SearchIndex));
        let search = |term: &str, limit: usize| -> Result<Vec<open_kioku_core::SearchResult>> {
            match search_index {
                Some(index) => index.search(term, limit),
                None => Ok(Vec::new()),
            }
        };

        let direct = if let Some(file) = &file {
            let mut direct = exact_reference_impacts(self.store, file, &target_symbols)?;
            direct.extend(git_cochange_impacts(self.store, file, &git_facts)?);
            direct.extend(runtime_impacts(
                self.store,
                search_index,
                file,
                &runtime_facts,
            )?);
            direct.extend(service_boundary_impacts(
                self.store,
                search_index,
                file,
                &service_facts,
            )?);
            for term in impact_terms(path, file, &target_symbols)
                .into_iter()
                .take(8)
            {
                let results = search(&term, 25)?;
                direct.extend(
                    results
                        .into_iter()
                        .filter(|result| result.path != file.path),
                );
            }
            direct = dedupe_results(direct);
            direct.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            direct.truncate(25);
            direct
        } else {
            Vec::new()
        };

        // Second-level: for each direct impact, search for that file's stem
        // to find indirect dependents (callers-of-callers).
        let mut indirect: Vec<open_kioku_core::SearchResult> = Vec::new();
        let direct_paths: std::collections::HashSet<_> =
            direct.iter().map(|r| r.path.clone()).collect();
        for direct_result in direct.iter().take(5) {
            let indirect_stem = direct_result
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if indirect_stem.is_empty() || indirect_stem.len() < 3 {
                continue;
            }
            let second = search(indirect_stem, 10)?;
            for result in second {
                if result.path != path && !direct_paths.contains(&result.path) {
                    indirect.push(result);
                }
            }
        }
        indirect.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        indirect.dedup_by(|a, b| a.path == b.path);
        indirect.truncate(15);
        let mut reasons = Vec::new();
        let exact_reference_count = direct
            .iter()
            .filter(|result| is_exact_reference_result(result))
            .count();
        if exact_reference_count > 0 {
            reasons.push(format!(
                "{exact_reference_count} exact indexed symbol reference(s) found"
            ));
        }
        if direct.len() > 10 {
            reasons.push("many lexical dependents reference this file or its symbols".into());
        }
        if !runtime_facts.is_empty() {
            reasons.push(format!(
                "{} local runtime trace/log/incident fact(s) touch this file",
                runtime_facts.len()
            ));
        }
        if !git_facts.is_empty() {
            reasons.push(format!(
                "{} git co-change or historical validation fact(s) touch this file",
                git_facts.len()
            ));
        }
        if !service_facts.is_empty() {
            reasons.push(format!(
                "{} static service-boundary fact(s) touch this file",
                service_facts.len()
            ));
        }
        if !complexity_facts.is_empty() {
            reasons.push(format!(
                "{} complexity/hot-path risk signal(s) touch this file",
                complexity_facts.len()
            ));
        }
        if let Some(churn) = churn_summary
            .as_ref()
            .filter(|churn| churn.stats.touch_count > 0)
        {
            reasons.push(format!(
                "history hotspot score {:.2} from {} touch(es), confidence {:?}",
                churn.stats.hotspot_score, churn.stats.touch_count, churn.confidence
            ));
        }
        if let Some(history) = &history_signals {
            reasons.extend(history.reasons.iter().take(4).cloned());
        }
        if path.to_string_lossy().contains("api") {
            reasons.push("API-layer path suggests public integration surface".into());
        }
        // A target the index does not hold cannot be measured. Saying so in the risk
        // report keeps `level: low` from reading as "nothing depends on this file".
        if file.is_none() {
            reasons.push(format!(
                "`{}` is not in the index; it may be excluded, unsupported, or added since the last `ok index`, so no dependents could be measured",
                path.display()
            ));
        }
        if reasons.is_empty() {
            reasons.push("limited indexed downstream references found".into());
        }
        let runtime_score = (runtime_facts.len() as f32 / 12.0).min(0.25);
        let git_score = (git_facts.len() as f32 / 12.0).min(0.18);
        let service_score = (service_facts.len() as f32 / 12.0).min(0.25);
        let complexity_score = complexity_risk_score(&complexity_facts);
        let churn_score = churn_risk_score(churn_summary.as_ref());
        let history_signal_score = history_signal_risk_score(history_signals.as_ref())
            .unwrap_or(churn_score)
            .min(0.25);
        let direct_reference_score = (direct.len() as f32 / 20.0).min(1.0);
        let score = (direct_reference_score
            + runtime_score
            + git_score
            + service_score
            + complexity_score
            + history_signal_score)
            .min(1.0);
        let evidence = Evidence {
            id: EvidenceId::new(format!("impact:{}", path.display())),
            source: "open-kioku-impact".into(),
            source_type: if exact_reference_count > 0 {
                EvidenceSourceType::Scip
            } else {
                EvidenceSourceType::Lexical
            },
            file_range: Some(FileRange {
                path: path.into(),
                line_range: None,
            }),
            symbol_id: None,
            confidence: if file.is_some() {
                if exact_reference_count > 0 {
                    Confidence::High
                } else {
                    Confidence::Medium
                }
            } else {
                Confidence::Low
            },
            message: if file.is_none() {
                "impact target is not in the index; no symbols or references were available".into()
            } else if exact_reference_count > 0 {
                "impact report derived from exact indexed symbol references and lexical references"
                    .into()
            } else {
                "impact report derived from indexed symbols and lexical references".into()
            },
            indexed_at: Utc::now(),
            ..Default::default()
        };
        let runtime_evidence = runtime_facts
            .iter()
            .map(|fact| runtime_fact_evidence(fact, path))
            .collect::<Vec<_>>();
        let git_evidence = git_facts
            .iter()
            .map(|fact| git_fact_evidence(fact, path))
            .collect::<Vec<_>>();
        let service_evidence = service_facts
            .iter()
            .map(|fact| service_fact_evidence(fact, path))
            .collect::<Vec<_>>();
        let complexity_evidence = complexity_facts
            .iter()
            .map(|fact| complexity_fact_evidence(fact, path))
            .collect::<Vec<_>>();
        let churn_evidence = churn_summary
            .as_ref()
            .filter(|churn| churn.stats.touch_count > 0)
            .map(|churn| churn_summary_evidence(churn, path));
        let history_signal_evidence = history_signals
            .as_ref()
            .map(|signals| history_signal_evidence(signals, path))
            .unwrap_or_default();
        let (proven_impact, possible_impact) = match (self.graph_store, &file) {
            (Some(graph), Some(file)) => {
                relationship_impacts(graph, self.store, file, &target_symbols)?
            }
            _ => (Vec::new(), Vec::new()),
        };
        let mut report = ImpactReport {
            target: path.display().to_string(),
            direct_impacts: direct,
            indirect_impacts: indirect,
            proven_impact,
            possible_impact,
            risk_report: RiskReport {
                // A target the index does not hold was not measured; a score of zero for it
                // is absence, not low risk, and `level` is the field consumers branch on.
                level: if file.is_none() {
                    "unknown"
                } else if score > 0.6 {
                    "high"
                } else if score > 0.25 {
                    "medium"
                } else {
                    "low"
                }
                .into(),
                score,
                reasons,
            },
            evidence: std::iter::once(evidence)
                .chain(runtime_evidence)
                .chain(git_evidence)
                .chain(service_evidence)
                .chain(complexity_evidence)
                .chain(churn_evidence)
                .chain(history_signal_evidence)
                .collect(),
            architecture_policy: None,
            score_breakdown: vec![ScoreComponent::single(
                "direct_reference_density",
                direct_reference_score,
                vec![format!("impact:{}", path.display())],
                "impact risk from count of exact and lexical direct dependents",
            )],
        };
        if runtime_score > 0.0 {
            report.score_breakdown.push(ScoreComponent::adjustment(
                "runtime_corroboration",
                runtime_score,
                runtime_facts.iter().map(|fact| fact.id.clone()).collect(),
                "impact risk adjusted by local runtime trace/log/incident facts",
            ));
        }
        if git_score > 0.0 {
            report.score_breakdown.push(ScoreComponent::adjustment(
                "similar_change_overlap",
                git_score,
                git_facts.iter().map(|fact| fact.id.clone()).collect(),
                "impact risk adjusted by git co-change and historical validation facts",
            ));
        }
        if service_score > 0.0 {
            report.score_breakdown.push(ScoreComponent::adjustment(
                "service_boundary",
                service_score,
                service_facts.iter().map(|fact| fact.id.clone()).collect(),
                "impact risk adjusted by static route, channel, config, and resource facts",
            ));
        }
        if complexity_score > 0.0 {
            report.score_breakdown.push(ScoreComponent::adjustment(
                "complexity_hot_path",
                complexity_score,
                complexity_facts.iter().map(|fact| fact.id.clone()).collect(),
                "impact risk adjusted by complexity, nested-loop, recursion, and hot-path static signals",
            ));
        }
        if let Some(history) = history_signals
            .as_ref()
            .filter(|history| history_signal_score > 0.0 && !history.components.is_empty())
        {
            report.score_breakdown.extend(history.components.clone());
        } else if let Some(churn) = churn_summary
            .as_ref()
            .filter(|churn| history_signal_score > 0.0 && churn.stats.touch_count > 0)
        {
            report.score_breakdown.push(ScoreComponent::adjustment(
                "history_churn",
                history_signal_score,
                vec![format!("history-churn:{}", churn.key)],
                "impact risk adjusted by materialized churn and hotspot history",
            ));
        }
        report.reconcile_score_breakdown();
        Ok(report)
    }
}

fn runtime_facts_for_file(
    store: &dyn MetadataStore,
    file_id: &FileId,
) -> Result<Vec<AnalysisFact>> {
    store.analysis_facts_for_file(file_id, Some(EvidenceSourceType::Runtime), 12)
}

fn git_history_facts_for_file(
    store: &dyn MetadataStore,
    file_id: &FileId,
) -> Result<Vec<AnalysisFact>> {
    store.analysis_facts_for_file(file_id, Some(EvidenceSourceType::GitHistory), 12)
}

fn service_boundary_facts_for_file(
    store: &dyn MetadataStore,
    file_id: &FileId,
) -> Result<Vec<AnalysisFact>> {
    Ok(store
        .analysis_facts_for_file(
            file_id,
            Some(EvidenceSourceType::StaticAnalysis),
            usize::MAX,
        )?
        .into_iter()
        .filter(is_service_boundary_fact)
        .take(24)
        .collect())
}

fn complexity_facts_for_file(
    store: &dyn MetadataStore,
    file_id: &FileId,
) -> Result<Vec<AnalysisFact>> {
    Ok(store
        .analysis_facts_for_file(
            file_id,
            Some(EvidenceSourceType::StaticAnalysis),
            usize::MAX,
        )?
        .into_iter()
        .filter(is_complexity_fact)
        .take(24)
        .collect())
}

fn is_complexity_fact(fact: &AnalysisFact) -> bool {
    fact.source == "open-kioku-relationships:complexity"
}

fn complexity_risk_score(facts: &[AnalysisFact]) -> f32 {
    facts
        .iter()
        .map(|fact| {
            if fact.message.contains("complexity_risk=high") {
                0.18
            } else if fact.message.contains("complexity_risk=medium") {
                0.08
            } else {
                0.02
            }
        })
        .sum::<f32>()
        .min(0.25)
}

fn churn_risk_score(summary: Option<&ChurnSummary>) -> f32 {
    let Some(summary) = summary else {
        return 0.0;
    };
    if summary.stats.touch_count == 0 {
        return 0.0;
    }
    if summary.stats.hotspot_score >= 3.0 {
        0.15
    } else if summary.stats.hotspot_score >= 1.5 {
        0.08
    } else {
        0.03
    }
}

fn history_signal_risk_score(summary: Option<&HistorySignalSummary>) -> Option<f32> {
    let summary = summary?;
    let score = summary
        .components
        .iter()
        .filter(|component| {
            matches!(
                component.signal.as_str(),
                "history_churn" | "ownership_risk" | "similar_change_overlap"
            )
        })
        .map(|component| component.contribution.max(0.0))
        .sum::<f32>()
        .min(0.25);
    (score > 0.0).then_some(score)
}

fn churn_summary_evidence(summary: &ChurnSummary, path: &Path) -> Evidence {
    Evidence {
        id: EvidenceId::new(format!("history-churn:{}", summary.key)),
        source: "open-kioku-history-churn".into(),
        source_type: EvidenceSourceType::GitHistory,
        file_range: Some(FileRange {
            path: path.into(),
            line_range: None,
        }),
        symbol_id: summary.symbol_id.clone(),
        confidence: summary.confidence,
        message: format!(
            "history_churn_hotspot_score={:.3}; touch_count={}; last_30d={}; last_90d={}; confidence={:?}",
            summary.stats.hotspot_score,
            summary.stats.touch_count,
            summary.stats.last_30d,
            summary.stats.last_90d,
            summary.confidence
        ).into(),
        indexed_at: summary.generated_at,
        ..Default::default()
    }
}

fn history_signal_evidence(summary: &HistorySignalSummary, path: &Path) -> Vec<Evidence> {
    summary
        .evidence_refs
        .iter()
        .take(12)
        .map(|id| Evidence {
            id: EvidenceId::new(id.clone()),
            source: "open-kioku-history-signals".into(),
            source_type: EvidenceSourceType::GitHistory,
            file_range: Some(FileRange {
                path: path.into(),
                line_range: None,
            }),
            symbol_id: None,
            confidence: if summary.components.is_empty() {
                Confidence::Low
            } else {
                Confidence::Medium
            },
            message: if summary.reasons.is_empty() {
                "bounded history signal evidence".into()
            } else {
                summary.reasons.join("; ").into()
            },
            indexed_at: summary.generated_at,
            ..Default::default()
        })
        .collect()
}

fn is_service_boundary_fact(fact: &AnalysisFact) -> bool {
    matches!(
        fact.edge_type,
        GraphEdgeType::ExposesEndpoint
            | GraphEdgeType::CallsEndpoint
            | GraphEdgeType::ReadsConfig
            | GraphEdgeType::WritesConfig
            | GraphEdgeType::ReadsTable
            | GraphEdgeType::WritesTable
            | GraphEdgeType::PublishesEvent
            | GraphEdgeType::ConsumesEvent
            | GraphEdgeType::DependsOn
            | GraphEdgeType::Defines
    ) && matches!(
        fact.target_kind,
        GraphNodeType::Endpoint
            | GraphNodeType::ConfigKey
            | GraphNodeType::DatabaseTable
            | GraphNodeType::Queue
            | GraphNodeType::Topic
            | GraphNodeType::Resource
    )
}

fn git_cochange_impacts(
    store: &dyn MetadataStore,
    target_file: &File,
    git_facts: &[AnalysisFact],
) -> Result<Vec<SearchResult>> {
    let mut results = Vec::new();
    for fact in git_facts.iter().take(12) {
        let target = Path::new(&fact.target);
        let Some(file) = store.get_file_by_path(target)? else {
            continue;
        };
        if file.path == target_file.path {
            continue;
        }
        let snippet = store
            .chunks_for_file(&file.id)?
            .first()
            .map(|chunk| chunk.text.clone())
            .unwrap_or_else(|| file.path.display().to_string());
        let evidence = vec![format!(
            "git co-change from local history: `{}` changed with `{}` ({})",
            target_file.path.display(),
            file.path.display(),
            fact.message
        )];
        results.push(SearchResult {
            path: file.path.clone(),
            line_range: None,
            snippet,
            symbol: None,
            score: 0.18 + (fact.confidence.score() * 0.05).min(0.05),
            match_reason: "historical git co-change with target file".into(),
            evidence,
            evidence_refs: vec![fact.id.clone()],
            confidence: fact.confidence.score(),
            score_breakdown: vec![ScoreComponent::single(
                "similar_change_overlap",
                0.18,
                vec![fact.id.clone()],
                "impact candidate historically changed with the target file",
            )],
        });
    }
    Ok(dedupe_results(results))
}

/// In-memory stand-in for the lexical index when none is configured. Answers with the same
/// `search_chunks` scan the per-term fallback used, so results are unchanged; only the number
/// of times the store is read changes.
struct ChunkScanIndex {
    files: Vec<File>,
    chunks: Vec<CodeChunk>,
    symbols: Vec<Symbol>,
}

impl ChunkScanIndex {
    fn load(store: &dyn MetadataStore) -> Result<Self> {
        Ok(Self {
            files: store.list_files(usize::MAX, 0)?,
            chunks: store.all_chunks()?,
            symbols: store.list_symbols(None, usize::MAX, 0)?,
        })
    }
}

impl SearchIndex for ChunkScanIndex {
    fn rebuild(&mut self, chunks: &[CodeChunk], files: &[File], symbols: &[Symbol]) -> Result<()> {
        self.chunks = chunks.to_vec();
        self.files = files.to_vec();
        self.symbols = symbols.to_vec();
        Ok(())
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<open_kioku_core::SearchResult>> {
        search_chunks(&self.chunks, &self.files, &self.symbols, query, limit)
    }
}

fn service_boundary_impacts(
    store: &dyn MetadataStore,
    search_index: Option<&dyn SearchIndex>,
    target_file: &File,
    service_facts: &[AnalysisFact],
) -> Result<Vec<SearchResult>> {
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = if search_index.is_none() {
        store.all_chunks()?
    } else {
        Vec::new()
    };
    let symbols = if search_index.is_none() {
        store.list_symbols(None, usize::MAX, 0)?
    } else {
        Vec::new()
    };
    let mut results = Vec::new();
    for fact in service_facts.iter().take(12) {
        for term in service_search_terms(fact).into_iter().take(4) {
            let matches = if let Some(index) = search_index {
                index.search(&term, 10)?
            } else {
                search_chunks(&chunks, &files, &symbols, &term, 10)?
            };
            for mut result in matches {
                if result.path == target_file.path {
                    continue;
                }
                annotate_service_impact(&mut result, fact);
                results.push(result);
            }
        }
    }
    Ok(dedupe_results(results))
}

fn runtime_impacts(
    store: &dyn MetadataStore,
    search_index: Option<&dyn SearchIndex>,
    target_file: &File,
    runtime_facts: &[AnalysisFact],
) -> Result<Vec<SearchResult>> {
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = if search_index.is_none() {
        store.all_chunks()?
    } else {
        Vec::new()
    };
    let symbols = if search_index.is_none() {
        store.list_symbols(None, usize::MAX, 0)?
    } else {
        Vec::new()
    };
    let mut results = Vec::new();
    for fact in runtime_facts.iter().take(6) {
        for term in runtime_search_terms(fact).into_iter().take(3) {
            let matches = if let Some(index) = search_index {
                index.search(&term, 10)?
            } else {
                search_chunks(&chunks, &files, &symbols, &term, 10)?
            };
            for mut result in matches {
                if result.path == target_file.path {
                    continue;
                }
                annotate_runtime_impact(&mut result, fact);
                results.push(result);
            }
        }
    }
    Ok(dedupe_results(results))
}

fn annotate_service_impact(result: &mut SearchResult, fact: &AnalysisFact) {
    let evidence = format!(
        "service-boundary evidence from `{}` targeting `{}`",
        fact.source, fact.target
    );
    if !result.evidence.contains(&evidence) {
        result.evidence.push(evidence);
    }
    if !result.evidence_refs.contains(&fact.id) {
        result.evidence_refs.push(fact.id.clone());
    }
    result.score += 0.18;
    result.confidence = result.confidence.max(fact.confidence.score());
    result.score_breakdown.push(ScoreComponent::adjustment(
        "service_boundary",
        0.18,
        vec![fact.id.clone()],
        "impact candidate matched route, channel, config, or resource evidence",
    ));
}

fn annotate_runtime_impact(result: &mut SearchResult, fact: &AnalysisFact) {
    let evidence = format!(
        "runtime corroboration from local artifact `{}` targeting `{}`",
        fact.source, fact.target
    );
    if !result.evidence.contains(&evidence) {
        result.evidence.push(evidence);
    }
    if !result.evidence_refs.contains(&fact.id) {
        result.evidence_refs.push(fact.id.clone());
    }
    result.score += 0.20;
    result.confidence = result.confidence.max(fact.confidence.score());
    result.score_breakdown.push(ScoreComponent::adjustment(
        "runtime_corroboration",
        0.20,
        vec![fact.id.clone()],
        "impact candidate matched observed runtime endpoint, SQL table, or incident",
    ));
}

fn service_fact_evidence(fact: &AnalysisFact, path: &Path) -> Evidence {
    Evidence {
        id: EvidenceId::new(fact.id.clone()),
        source: fact.source.clone(),
        source_type: EvidenceSourceType::StaticAnalysis,
        file_range: Some(FileRange {
            path: path.into(),
            line_range: fact.range.clone(),
        }),
        symbol_id: fact.symbol_id.clone(),
        confidence: fact.confidence,
        message: format!("{}: {}", fact.message, fact.target).into(),
        indexed_at: Utc::now(),
        ..Default::default()
    }
}

fn runtime_fact_evidence(fact: &AnalysisFact, path: &Path) -> Evidence {
    Evidence {
        id: EvidenceId::new(fact.id.clone()),
        source: fact.source.clone(),
        source_type: EvidenceSourceType::Runtime,
        file_range: Some(FileRange {
            path: path.into(),
            line_range: fact.range.clone(),
        }),
        symbol_id: fact.symbol_id.clone(),
        confidence: fact.confidence,
        message: format!("{}: {}", fact.message, fact.target).into(),
        indexed_at: Utc::now(),
        ..Default::default()
    }
}

fn complexity_fact_evidence(fact: &AnalysisFact, path: &Path) -> Evidence {
    Evidence {
        id: EvidenceId::new(fact.id.clone()),
        source: fact.source.clone(),
        source_type: EvidenceSourceType::StaticAnalysis,
        file_range: Some(FileRange {
            path: path.into(),
            line_range: fact.range.clone(),
        }),
        symbol_id: fact.symbol_id.clone(),
        confidence: fact.confidence,
        message: fact.message.clone(),
        indexed_at: Utc::now(),
        ..Default::default()
    }
}

fn git_fact_evidence(fact: &AnalysisFact, path: &Path) -> Evidence {
    Evidence {
        id: EvidenceId::new(fact.id.clone()),
        source: fact.source.clone(),
        source_type: EvidenceSourceType::GitHistory,
        file_range: Some(FileRange {
            path: path.into(),
            line_range: None,
        }),
        symbol_id: None,
        confidence: fact.confidence,
        message: format!("{}: {}", fact.message, fact.target).into(),
        indexed_at: Utc::now(),
        ..Default::default()
    }
}

fn runtime_search_terms(fact: &AnalysisFact) -> Vec<String> {
    let mut terms = vec![fact.target.clone()];
    terms.extend(
        fact.target
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
            .filter(|part| part.len() >= 4)
            .map(ToOwned::to_owned),
    );
    terms.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    terms.dedup();
    terms
}

fn service_search_terms(fact: &AnalysisFact) -> Vec<String> {
    let mut terms = vec![fact.target.clone()];
    if let Some((_, tail)) = fact.target.split_once(' ') {
        terms.push(tail.to_string());
    }
    terms.extend(
        fact.target
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '-'))
            .filter(|part| part.len() >= 3)
            .map(ToOwned::to_owned),
    );
    terms.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    terms.dedup();
    terms
}

/// Edge types whose source file is affected when the changed file is edited.
///
/// A structural dependency is one reason; derivation is the other. A generated file does not
/// import the source its banner names and a test imports far more than its subject, so neither
/// reaches the changed file through a dependency edge — yet editing the origin is exactly what
/// makes them stale. Direction carries the distinction: `DERIVED_FROM` runs derived -> origin,
/// and only incoming edges are walked here, so editing an origin surfaces what is derived from
/// it while editing a generated file does not claim to affect its source.
///
/// Authority still decides how the result may be presented: the caller classifies every edge
/// through [`RelationshipUsePolicy`], so a declared-origin banner (an authoritative proof) is
/// proven impact while a naming-convention pairing carries no proof and can only ever be
/// surfaced as a labeled possibility.
fn is_impacted_by_edge_type(edge_type: &GraphEdgeType) -> bool {
    is_dependency_edge_type(edge_type) || matches!(edge_type, GraphEdgeType::DerivedFrom)
}

/// Relationship edge types that assert a structural dependency on the changed code.
fn is_dependency_edge_type(edge_type: &GraphEdgeType) -> bool {
    matches!(
        edge_type,
        GraphEdgeType::Calls
            | GraphEdgeType::References
            | GraphEdgeType::UsesType
            | GraphEdgeType::Implements
            | GraphEdgeType::Extends
            | GraphEdgeType::Imports
            | GraphEdgeType::DependsOn
    )
}

/// Classify inbound relationship edges around the changed file into proven versus possible
/// impact. Authority is recomputed from typed proofs through the shared fail-closed policy, so a
/// heuristic same-name edge can only ever surface as a possibility.
fn relationship_impacts(
    graph: &dyn GraphStore,
    store: &dyn MetadataStore,
    target_file: &File,
    symbols: &[Symbol],
) -> Result<(Vec<RelationshipImpact>, Vec<RelationshipImpact>)> {
    let policy = RelationshipUsePolicy::proven_and_possible();
    let files_by_id = store
        .list_files(usize::MAX, 0)?
        .into_iter()
        .map(|file| (file.id.clone(), file))
        .collect::<HashMap<FileId, File>>();

    // Indexed once: the previous lookup linear-scanned every file per unresolved edge per seed.
    let files_by_path = files_by_id
        .values()
        .filter_map(|file| {
            identity::normalize_repo_path(&file.path)
                .ok()
                .map(|path| (path, file.id.clone()))
        })
        .collect::<HashMap<String, FileId>>();

    let mut seeds: Vec<(NodeId, String)> = Vec::new();
    if let Ok(node_id) = identity::try_file_node_id(&target_file.path) {
        seeds.push((node_id, target_file.path.display().to_string()));
    }
    for symbol in symbols
        .iter()
        .filter(|symbol| symbol.file_id == target_file.id)
        .take(RELATIONSHIP_IMPACT_SYMBOL_SEEDS)
    {
        seeds.push((
            identity::symbol_node_id(symbol),
            symbol.qualified_name.clone(),
        ));
    }

    let mut proven = Vec::new();
    let mut possible = Vec::new();
    for (node_id, source_label) in &seeds {
        // A store with no graph support has no relationship evidence to offer, and an empty
        // list is the documented answer for that. Every other failure — a graph awaiting
        // `ok index`, a stale analysis fingerprint, a read error — is not "no dependents", and
        // `ImpactReport` has no caveat channel to say so, so the report is refused instead.
        // Two surfaces (`ok impact`, MCP `impact_analysis`) answered `proven_impact: []` from
        // an index whose edges had been discarded on open before this propagated.
        let (nodes, mut edges) =
            match graph.neighbors(&node_id.0, RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT) {
                Ok(window) => window,
                Err(OkError::Unsupported(_)) => continue,
                Err(err) => return Err(err),
            };
        // `neighbors` is an untyped, unordered window over every edge touching the node. A file
        // node has one outgoing `DEFINES` edge per symbol, so on a 60-symbol source file the
        // window is exhausted by its own definitions and the incoming derived edges never come
        // back — measured on a real repository, where files with 61-65 symbols reported no
        // derived impact at all while an 8-symbol file reported it correctly. Ask for those
        // edges by type, which filters in SQL.
        let mut nodes = nodes;
        // Only the file seed can carry this edge: derived edges join two file nodes, so the
        // symbol seeds below it would always come back empty.
        //
        // One absence is still swallowed here that the context path reports as a caveat: an
        // origin with more derived files than the row cap loses the rest silently, because
        // `ImpactReport` has no caveat channel. Giving it one is out of scope here.
        if node_id.0.starts_with("file:") {
            let derived = match graph.edges_by_type_for_node(
                GraphEdgeType::DerivedFrom,
                &node_id.0,
                false,
                RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT,
                0,
            ) {
                Ok(derived) => derived,
                Err(OkError::Unsupported(_)) => Vec::new(),
                Err(err) => return Err(err),
            };
            for edge in derived {
                if edges.iter().any(|existing| existing.id == edge.id) {
                    continue;
                }
                // `neighbors` returned nodes for its own window only. A derived edge's other
                // endpoint is always a file node (`file:<path>`), so it is resolved from the
                // indexed files rather than with a second graph query.
                if !nodes.iter().any(|existing| existing.id == edge.from) {
                    let Some(node) = file_node_for_id(&edge.from, &files_by_path) else {
                        continue;
                    };
                    nodes.push(node);
                }
                edges.push(edge);
            }
        }
        let nodes_by_id = nodes
            .iter()
            .map(|node| (node.id.clone(), node))
            .collect::<HashMap<NodeId, &GraphNode>>();
        for edge in &edges {
            if edge.to != *node_id || !is_impacted_by_edge_type(&edge.edge_type) {
                continue;
            }
            let Some(impact) =
                relationship_impact_entry(edge, &nodes_by_id, &files_by_id, source_label)
            else {
                continue;
            };
            match policy.classify(edge) {
                RelationshipUseClass::Proven => proven.push(impact),
                RelationshipUseClass::Possible => possible.push(impact),
                RelationshipUseClass::Excluded => {}
            }
        }
    }

    for list in [&mut proven, &mut possible] {
        list.sort_by(|a, b| {
            (&a.path, &a.symbol, &a.edge_type).cmp(&(&b.path, &b.symbol, &b.edge_type))
        });
        list.dedup_by(|a, b| {
            a.path == b.path && a.symbol == b.symbol && a.edge_type == b.edge_type
        });
        list.truncate(RELATIONSHIP_IMPACT_LIMIT);
    }
    Ok((proven, possible))
}

/// Rebuild the `GraphNode` for a `file:<path>` id from the indexed files. Used for edges fetched
/// by type, whose endpoints are not in the `neighbors` window.
fn file_node_for_id(
    node_id: &NodeId,
    files_by_path: &HashMap<String, FileId>,
) -> Option<GraphNode> {
    let path = node_id.0.strip_prefix("file:")?;
    Some(GraphNode {
        id: node_id.clone(),
        node_type: GraphNodeType::File,
        label: path.to_string(),
        file_id: Some(files_by_path.get(path)?.clone()),
        ..Default::default()
    })
}

fn relationship_impact_entry(
    edge: &GraphEdge,
    nodes_by_id: &HashMap<NodeId, &GraphNode>,
    files_by_id: &HashMap<FileId, File>,
    source_label: &str,
) -> Option<RelationshipImpact> {
    let from_node = nodes_by_id.get(&edge.from)?;
    let path = from_node
        .file_id
        .as_ref()
        .and_then(|file_id| files_by_id.get(file_id))
        .map(|file| file.path.clone())?;
    let symbol = from_node
        .symbol_id
        .is_some()
        .then(|| from_node.label.clone());
    let authority = edge.relationship_authority();
    let mut proof_kinds = edge
        .relationship_proofs()
        .iter()
        .map(|proof| proof.kind)
        .collect::<Vec<_>>();
    proof_kinds.sort();
    proof_kinds.dedup();
    let ambiguous = open_kioku_evidence::edge_is_ambiguous(edge);
    let proof_summary = if proof_kinds.is_empty() {
        "no structural proof".to_string()
    } else {
        proof_kinds
            .iter()
            .map(|kind| format!("{kind:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let reason = format!(
        "{:?} edge from `{}` into `{source_label}` ({proof_summary})",
        edge.edge_type, from_node.label
    );
    Some(RelationshipImpact {
        path,
        symbol,
        source: source_label.to_string(),
        edge_type: edge.edge_type.clone(),
        authority,
        proof_kinds,
        ambiguous,
        reason,
    })
}

fn exact_reference_impacts(
    store: &dyn MetadataStore,
    target_file: &File,
    symbols: &[Symbol],
) -> Result<Vec<SearchResult>> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }

    let files = store.list_files(usize::MAX, 0)?;
    let files_by_id = files
        .iter()
        .map(|file| (file.id.clone(), file.clone()))
        .collect::<HashMap<FileId, File>>();
    let mut results = Vec::new();
    for symbol in symbols
        .iter()
        .filter(|symbol| symbol.file_id == target_file.id)
    {
        if is_generic_symbol_name(&symbol.name) {
            continue;
        }
        for occurrence in store.references_for_symbol(&symbol.id, 100)? {
            if occurrence.file_id == target_file.id {
                continue;
            }
            if let Some(result) = occurrence_result(store, &files_by_id, symbol, &occurrence)? {
                results.push(result);
            }
        }
    }

    Ok(dedupe_results(results))
}

/// `match_reason` prefix of a result produced from an indexed symbol occurrence.
const EXACT_REFERENCE_MATCH_REASON_PREFIX: &str = "exact symbol reference via ";

/// Whether `result` is an indexed symbol reference rather than a lexical or heuristic hit.
/// Consumers count exact references by this predicate instead of scanning result prose, so
/// a lexical hit whose query words include "exact" or "scip" cannot pass as one.
pub fn is_exact_reference_result(result: &SearchResult) -> bool {
    result
        .match_reason
        .starts_with(EXACT_REFERENCE_MATCH_REASON_PREFIX)
}

fn occurrence_result(
    store: &dyn MetadataStore,
    files_by_id: &HashMap<FileId, File>,
    symbol: &Symbol,
    occurrence: &SymbolOccurrence,
) -> Result<Option<SearchResult>> {
    let Some(file) = files_by_id.get(&occurrence.file_id) else {
        return Ok(None);
    };
    let chunks = store.chunks_for_file(&occurrence.file_id)?;
    let snippet = best_occurrence_snippet(&chunks, occurrence, &symbol.name);
    let source = match occurrence.provenance {
        EvidenceSourceType::Scip => "SCIP",
        EvidenceSourceType::TreeSitter => "tree-sitter",
        EvidenceSourceType::Lsp => "LSP",
        _ => "indexed",
    };
    let score = 1.25 + occurrence.confidence.score();
    let evidence = vec![format!(
        "exact reference to `{}` from `{source}` occurrence data",
        symbol.qualified_name
    )];
    let line_range = occurrence.range.clone();
    let evidence_ids = search_result_evidence_ids(&file.path, &line_range, evidence.len());
    Ok(Some(SearchResult {
        path: file.path.clone(),
        line_range,
        snippet,
        symbol: None,
        score,
        match_reason: format!("{EXACT_REFERENCE_MATCH_REASON_PREFIX}{source}"),
        evidence: evidence.clone(),
        evidence_refs: evidence_ids.clone(),
        confidence: occurrence.confidence.score(),
        score_breakdown: vec![ScoreComponent::single(
            "exact_symbol_reference",
            score,
            evidence_ids,
            format!("{source} occurrence confidence plus exact-reference base weight"),
        )],
    }))
}

fn best_occurrence_snippet(
    chunks: &[CodeChunk],
    occurrence: &SymbolOccurrence,
    symbol_name: &str,
) -> String {
    let occurrence_line = occurrence.range.as_ref().map(|range| range.start);
    let chunk = occurrence_line
        .and_then(|line| {
            chunks
                .iter()
                .find(|chunk| chunk.range.start <= line && line <= chunk.range.end)
        })
        .or_else(|| chunks.iter().find(|chunk| chunk.text.contains(symbol_name)))
        .or_else(|| chunks.first());

    chunk
        .and_then(|chunk| {
            chunk
                .text
                .lines()
                .find(|line| line.contains(symbol_name))
                .or_else(|| chunk.text.lines().next())
        })
        .unwrap_or(symbol_name)
        .trim()
        .chars()
        .take(240)
        .collect()
}

fn dedupe_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut by_path = BTreeMap::<String, SearchResult>::new();
    for result in results {
        let key = result_key(&result);
        match by_path.get_mut(&key) {
            Some(existing) => {
                if result.score > existing.score {
                    existing.score = result.score;
                    existing.snippet = result.snippet.clone();
                    existing.line_range = result.line_range.clone();
                    existing.match_reason = result.match_reason.clone();
                    existing.confidence = existing.confidence.max(result.confidence);
                    existing.score_breakdown = result.score_breakdown.clone();
                }
                for evidence in result.evidence {
                    if !existing.evidence.contains(&evidence) {
                        existing.evidence.push(evidence);
                    }
                }
                existing.reconcile_score_breakdown();
            }
            None => {
                by_path.insert(key, result);
            }
        }
    }
    by_path.into_values().collect()
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

fn impact_terms(
    path: &Path,
    file: &open_kioku_core::File,
    symbols: &[open_kioku_core::Symbol],
) -> Vec<String> {
    let mut terms = symbols
        .iter()
        .filter(|symbol| symbol.file_id == file.id)
        .filter(|symbol| !is_generic_symbol_name(&symbol.name))
        .map(|symbol| symbol.name.clone())
        .collect::<Vec<_>>();

    if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
        if !is_generic_symbol_name(stem) {
            terms.push(stem.into());
        }
    }

    terms.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    terms.dedup();
    terms
}

fn is_generic_symbol_name(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "" | "args"
            | "cli"
            | "command"
            | "commands"
            | "config"
            | "from"
            | "helpers"
            | "index"
            | "lib"
            | "main"
            | "mod"
            | "output"
            | "path"
            | "repo"
            | "run"
            | "test"
            | "tests"
            | "to"
            | "types"
            | "utils"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use open_kioku_core::{
        CodeChunk, File, FileId, GitChangeKind, GitCommitId, GitCommitRecord, GitFileTouch,
        HistoryRecordId, HistorySnapshot, IndexManifest, IndexQuality, Language, LineRange, Owner,
        Repository, RepositoryId, SymbolId, SymbolKind, HISTORY_SCHEMA_VERSION,
    };
    use open_kioku_storage::{HistoryStore, IndexData};
    use open_kioku_storage_sqlite::SqliteStore;
    use std::path::PathBuf;

    fn make_store() -> SqliteStore {
        SqliteStore::open(":memory:").unwrap()
    }

    #[test]
    fn derives_impacts_from_chunks() {
        let store = make_store();

        let manifest = IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository: Repository {
                id: RepositoryId::new("repo"),
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 3,
            symbol_count: 0,
            chunk_count: 2,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };

        let f1 = File {
            id: FileId::new("f1"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/core.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let f2 = File {
            id: FileId::new("f2"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/app.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        let f3 = File {
            id: FileId::new("f3"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/main.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };

        let c1 = CodeChunk {
            id: "c1".into(),
            file_id: FileId::new("f2"),
            symbol_id: None,
            language: Language::Rust,
            text: "use crate::core::something;".into(),
            range: open_kioku_core::LineRange::single(1),
        };
        let c2 = CodeChunk {
            id: "c2".into(),
            file_id: FileId::new("f3"),
            symbol_id: None,
            language: Language::Rust,
            text: "use crate::app::something;".into(),
            range: open_kioku_core::LineRange::single(1),
        };

        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[f1, f2, f3],
                symbols: &[],
                occurrences: &[],
                chunks: &[c1, c2],
                imports: &[],
                tests: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let engine = ImpactEngine::new(&store);

        let report = engine.for_file(Path::new("src/core.rs")).unwrap();

        // core is referenced by app (c1), so app is direct.
        assert_eq!(report.direct_impacts.len(), 1);
        assert_eq!(
            report.direct_impacts[0].path.display().to_string(),
            "src/app.rs"
        );

        // app is referenced by main (c2), so main is indirect.
        assert_eq!(report.indirect_impacts.len(), 1);
        assert_eq!(
            report.indirect_impacts[0].path.display().to_string(),
            "src/main.rs"
        );
    }

    #[test]
    fn exact_symbol_references_count_as_direct_impact() {
        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let source = File {
            id: FileId::new("source"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/rates.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "source".into(),
            is_generated: false,
            is_vendor: false,
        };
        let caller = File {
            id: FileId::new("caller"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/publisher.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "caller".into(),
            is_generated: false,
            is_vendor: false,
        };
        let symbol = Symbol {
            id: SymbolId::new("symbol:rate_validator"),
            name: "RateValidator".into(),
            qualified_name: "rates::RateValidator".into(),
            kind: SymbolKind::Class,
            file_id: source.id.clone(),
            range: Some(LineRange { start: 1, end: 5 }),
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
                id: "source-chunk".into(),
                file_id: source.id.clone(),
                range: LineRange { start: 1, end: 5 },
                language: Language::Rust,
                text: "pub struct RateValidator;".into(),
                symbol_id: Some(symbol.id.clone()),
            },
            CodeChunk {
                id: "caller-chunk".into(),
                file_id: caller.id.clone(),
                range: LineRange { start: 10, end: 12 },
                language: Language::Rust,
                text: "let validator = RateValidator::new();".into(),
                symbol_id: None,
            },
        ];
        let occurrences = vec![
            SymbolOccurrence {
                symbol_id: symbol.id.clone(),
                file_id: source.id.clone(),
                range: Some(LineRange { start: 1, end: 1 }),
                source_range: None,
                is_definition: true,
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
            },
            SymbolOccurrence {
                symbol_id: symbol.id.clone(),
                file_id: caller.id.clone(),
                range: Some(LineRange { start: 10, end: 10 }),
                source_range: None,
                is_definition: false,
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
            },
        ];
        let manifest = IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository: Repository {
                id: repo_id,
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 2,
            symbol_count: 1,
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };

        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[source, caller],
                symbols: &[symbol],
                occurrences: &occurrences,
                chunks: &chunks,
                imports: &[],
                tests: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let report = ImpactEngine::new(&store)
            .for_file(Path::new("src/rates.rs"))
            .unwrap();

        assert!(report
            .direct_impacts
            .iter()
            .any(|result| result.path == Path::new("src/publisher.rs")
                && result.match_reason.contains("exact symbol reference")));
        assert!(report
            .risk_report
            .reasons
            .iter()
            .any(|reason| reason.contains("exact indexed symbol reference")));
    }

    #[test]
    fn history_only_impacts_are_bounded_below_exact_evidence() {
        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let source = File {
            id: FileId::new("source"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/source.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "source".into(),
            is_generated: false,
            is_vendor: false,
        };
        let historical_neighbor = File {
            id: FileId::new("neighbor"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/neighbor.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "neighbor".into(),
            is_generated: false,
            is_vendor: false,
        };
        let manifest = IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository: Repository {
                id: repo_id,
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 2,
            symbol_count: 0,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };
        let history_fact = AnalysisFact {
            id: "history:source-neighbor".into(),
            file_id: source.id.clone(),
            symbol_id: None,
            target: historical_neighbor.path.display().to_string(),
            target_kind: GraphNodeType::File,
            edge_type: GraphEdgeType::SimilarTo,
            range: None,
            confidence: Confidence::High,
            source: "open-kioku-git-history".into(),
            source_type: EvidenceSourceType::GitHistory,
            message: "git co-change observed in 3 commit(s), recency weight 0.90".into(),
        };

        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[source, historical_neighbor],
                symbols: &[],
                occurrences: &[],
                chunks: &[],
                imports: &[],
                tests: &[],
                analysis_facts: &[history_fact],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let report = ImpactEngine::new(&store)
            .for_file(Path::new("src/source.rs"))
            .unwrap();
        let result = report
            .direct_impacts
            .iter()
            .find(|result| result.path == Path::new("src/neighbor.rs"))
            .expect("history co-change impact candidate");

        assert!(result.score <= 0.23, "{result:#?}");
        assert!(result
            .score_breakdown
            .iter()
            .any(|component| component.signal == "similar_change_overlap"
                && component.contribution <= 0.18));
    }

    #[test]
    fn service_boundary_facts_surface_cross_service_impacts() {
        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let provider = File {
            id: FileId::new("provider"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/orders_api.ts"),
            language: Language::TypeScript,
            size_bytes: 100,
            content_hash: "provider".into(),
            is_generated: false,
            is_vendor: false,
        };
        let client = File {
            id: FileId::new("client"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/orders_client.ts"),
            language: Language::TypeScript,
            size_bytes: 100,
            content_hash: "client".into(),
            is_generated: false,
            is_vendor: false,
        };
        let chunks = vec![
            CodeChunk {
                id: "provider-chunk".into(),
                file_id: provider.id.clone(),
                range: LineRange { start: 1, end: 3 },
                language: Language::TypeScript,
                text: "router.get('/v1/orders', handler);".into(),
                symbol_id: None,
            },
            CodeChunk {
                id: "client-chunk".into(),
                file_id: client.id.clone(),
                range: LineRange { start: 1, end: 3 },
                language: Language::TypeScript,
                text: "await fetch('/v1/orders');".into(),
                symbol_id: None,
            },
        ];
        let facts = vec![AnalysisFact {
            id: "route-provider".into(),
            file_id: provider.id.clone(),
            symbol_id: None,
            target: "GET /v1/orders".into(),
            target_kind: GraphNodeType::Endpoint,
            edge_type: GraphEdgeType::ExposesEndpoint,
            range: Some(LineRange { start: 1, end: 1 }),
            confidence: Confidence::Medium,
            source: "open-kioku-static/javascript".into(),
            source_type: EvidenceSourceType::StaticAnalysis,
            message: "JavaScript HTTP route".into(),
        }];
        let manifest = IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository: Repository {
                id: repo_id,
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 2,
            symbol_count: 0,
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };

        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[provider, client],
                symbols: &[],
                occurrences: &[],
                chunks: &chunks,
                imports: &[],
                tests: &[],
                analysis_facts: &facts,
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let report = ImpactEngine::new(&store)
            .for_file(Path::new("src/orders_api.ts"))
            .unwrap();

        assert!(report.direct_impacts.iter().any(|result| {
            result.path == Path::new("src/orders_client.ts")
                && result
                    .score_breakdown
                    .iter()
                    .any(|component| component.signal == "service_boundary")
        }));
        assert!(report
            .risk_report
            .reasons
            .iter()
            .any(|reason| reason.contains("static service-boundary")));
    }

    #[test]
    fn complexity_facts_raise_risk_without_exact_reference_evidence() {
        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let file = File {
            id: FileId::new("hot"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/hot_path.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hot".into(),
            is_generated: false,
            is_vendor: false,
        };
        let chunks = vec![CodeChunk {
            id: "hot-chunk".into(),
            file_id: file.id.clone(),
            range: LineRange { start: 1, end: 12 },
            language: Language::Rust,
            text: "fn hot_path() { for item in items { while ready { process(item); } } }".into(),
            symbol_id: Some(SymbolId::new("hot-symbol")),
        }];
        let facts = vec![AnalysisFact {
            id: "complexity-hot".into(),
            file_id: file.id.clone(),
            symbol_id: Some(SymbolId::new("hot-symbol")),
            target: "complexity:crate::hot_path".into(),
            target_kind: GraphNodeType::Resource,
            edge_type: GraphEdgeType::BelongsTo,
            range: Some(LineRange { start: 1, end: 12 }),
            confidence: Confidence::Medium,
            source: "open-kioku-relationships:complexity".into(),
            source_type: EvidenceSourceType::StaticAnalysis,
            message: "complexity_risk=high; cyclomatic=12; cognitive=18; loop_count=2; max_loop_depth=2; transitive_loop_depth=2; caveat=risk signal, not proof of complexity".into(),
        }];
        let manifest = IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository: Repository {
                id: repo_id,
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 1,
            symbol_count: 0,
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };

        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[file],
                symbols: &[],
                occurrences: &[],
                chunks: &chunks,
                imports: &[],
                tests: &[],
                analysis_facts: &facts,
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let report = ImpactEngine::new(&store)
            .for_file(Path::new("src/hot_path.rs"))
            .unwrap();

        assert!(report.risk_report.score >= 0.18);
        assert!(report
            .risk_report
            .reasons
            .iter()
            .any(|reason| reason.contains("complexity/hot-path")));
        assert!(report
            .score_breakdown
            .iter()
            .any(|component| component.signal == "complexity_hot_path"));
    }

    #[test]
    fn materialized_churn_surfaces_as_impact_risk_signal() {
        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let file = File {
            id: FileId::new("hot-history"),
            repository_id: repo_id.clone(),
            path: PathBuf::from("src/hot_history.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hot-history".into(),
            is_generated: false,
            is_vendor: false,
        };
        let manifest = IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository: Repository {
                id: repo_id,
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 1,
            symbol_count: 0,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: std::slice::from_ref(&file),
                symbols: &[],
                occurrences: &[],
                chunks: &[],
                imports: &[],
                tests: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let oldest = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let middle = Utc.with_ymd_and_hms(2026, 2, 1, 12, 0, 0).unwrap();
        let newest = Utc.with_ymd_and_hms(2026, 3, 1, 12, 0, 0).unwrap();
        let commits = [("oldest", oldest), ("middle", middle), ("newest", newest)]
            .into_iter()
            .map(|(id, at)| GitCommitRecord {
                id: GitCommitId::new(id),
                parent_ids: Vec::new(),
                author: Owner {
                    name: format!("{id} author"),
                    email: None,
                },
                committer: None,
                authored_at: at,
                committed_at: at,
                summary: format!("{id} change"),
                message: format!("{id} change"),
                file_count: 1,
            })
            .collect::<Vec<_>>();
        let file_touches = commits
            .iter()
            .map(|commit| GitFileTouch {
                id: HistoryRecordId::new(format!("touch-{}", commit.id.0)),
                commit_id: commit.id.clone(),
                path: PathBuf::from("src/hot_history.rs"),
                previous_path: None,
                change_kind: GitChangeKind::Modified,
                additions: Some(20),
                deletions: Some(10),
                touched_at: commit.committed_at,
            })
            .collect::<Vec<_>>();
        store
            .put_history_snapshot(&HistorySnapshot {
                schema_version: HISTORY_SCHEMA_VERSION,
                commits,
                file_touches,
                symbol_touches: Vec::new(),
                cochange_edges: Vec::new(),
                reviewer_evidence: Vec::new(),
            })
            .unwrap();

        let report = ImpactEngine::new(&store)
            .with_history_store(Some(&store))
            .for_file(Path::new("src/hot_history.rs"))
            .unwrap();

        assert!(report
            .risk_report
            .reasons
            .iter()
            .any(|reason| reason.contains("history hotspot score")));
        assert!(report
            .evidence
            .iter()
            .any(|evidence| evidence.id.0 == "history-churn:src/hot_history.rs"));
        assert!(report
            .score_breakdown
            .iter()
            .any(|component| component.signal == "history_churn"));
    }

    #[test]
    fn heuristic_same_name_edge_never_becomes_proven_impact() {
        use open_kioku_core::{
            identity, Evidence, GraphEdge, GraphNode, RelationshipAuthority, RelationshipProof,
            RelationshipProofKind,
        };

        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let make_file = |id: &str, path: &str| File {
            id: FileId::new(id),
            repository_id: repo_id.clone(),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: id.into(),
            is_generated: false,
            is_vendor: false,
        };
        let target_file = make_file("target", "src/auth.rs");
        let caller_file = make_file("caller", "src/session.rs");
        let unrelated_file = make_file("unrelated", "src/billing.rs");
        let make_symbol = |id: &str, name: &str, qualified: &str, file: &File| Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: qualified.into(),
            kind: SymbolKind::Function,
            file_id: file.id.clone(),
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
        let target_symbol = make_symbol(
            "symbol:auth::issue_token",
            "issue_token",
            "auth::issue_token",
            &target_file,
        );
        let caller_symbol = make_symbol(
            "symbol:session::refresh",
            "refresh",
            "session::refresh",
            &caller_file,
        );
        // Same short name as the target in an unrelated module: the classic false-impact trap.
        let unrelated_symbol = make_symbol(
            "symbol:billing::issue_token",
            "issue_token",
            "billing::issue_token",
            &unrelated_file,
        );

        let manifest = IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository: Repository {
                id: repo_id.clone(),
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 3,
            symbol_count: 3,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[
                    target_file.clone(),
                    caller_file.clone(),
                    unrelated_file.clone(),
                ],
                symbols: &[
                    target_symbol.clone(),
                    caller_symbol.clone(),
                    unrelated_symbol.clone(),
                ],
                occurrences: &[],
                chunks: &[],
                imports: &[],
                tests: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let symbol_node = |symbol: &Symbol, file: &File| GraphNode {
            id: identity::symbol_node_id(symbol),
            node_type: GraphNodeType::Function,
            label: symbol.qualified_name.clone(),
            file_id: Some(file.id.clone()),
            symbol_id: Some(symbol.id.clone()),
            ..Default::default()
        };
        let nodes = vec![
            symbol_node(&target_symbol, &target_file),
            symbol_node(&caller_symbol, &caller_file),
            symbol_node(&unrelated_symbol, &unrelated_file),
        ];

        let target_node_id = identity::symbol_node_id(&target_symbol);
        let mut proven_edge = GraphEdge {
            id: open_kioku_core::EdgeId::new("edge:proven"),
            from: identity::symbol_node_id(&caller_symbol),
            to: target_node_id.clone(),
            edge_type: GraphEdgeType::References,
            evidence: Evidence::default(),
            ..Default::default()
        };
        proven_edge
            .set_relationship_proofs(vec![RelationshipProof::new(
                RelationshipProofKind::ExactReference,
                "test-exact-reference",
                1,
            )])
            .unwrap();
        // No proofs at all: a same-name heuristic guess.
        let heuristic_edge = GraphEdge {
            id: open_kioku_core::EdgeId::new("edge:heuristic"),
            from: identity::symbol_node_id(&unrelated_symbol),
            to: target_node_id,
            edge_type: GraphEdgeType::Calls,
            evidence: Evidence::default(),
            ..Default::default()
        };
        store
            .replace_graph(&nodes, &[proven_edge, heuristic_edge])
            .unwrap();

        let report = ImpactEngine::new(&store)
            .with_graph_store(Some(&store))
            .for_file(Path::new("src/auth.rs"))
            .unwrap();

        assert_eq!(report.proven_impact.len(), 1, "{:?}", report.proven_impact);
        let proven = &report.proven_impact[0];
        assert_eq!(proven.path, PathBuf::from("src/session.rs"));
        assert_eq!(proven.symbol.as_deref(), Some("session::refresh"));
        assert_eq!(proven.authority, RelationshipAuthority::Authoritative);
        assert_eq!(
            proven.proof_kinds,
            vec![RelationshipProofKind::ExactReference]
        );

        assert!(
            report
                .possible_impact
                .iter()
                .any(|impact| impact.path == Path::new("src/billing.rs")
                    && impact.authority == RelationshipAuthority::Heuristic),
            "{:?}",
            report.possible_impact
        );
        assert!(
            !report
                .proven_impact
                .iter()
                .any(|impact| impact.path == Path::new("src/billing.rs")),
            "heuristic same-name edge must never be presented as proven impact"
        );
    }

    #[test]
    fn editing_an_origin_surfaces_the_files_derived_from_it() {
        use open_kioku_core::{
            identity, Evidence, GraphEdge, GraphNode, RelationshipAuthority, RelationshipProof,
            RelationshipProofKind,
        };

        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let make_file = |id: &str, path: &str, generated: bool| File {
            id: FileId::new(id),
            repository_id: repo_id.clone(),
            path: PathBuf::from(path),
            language: Language::TypeScript,
            size_bytes: 100,
            content_hash: id.into(),
            is_generated: generated,
            is_vendor: false,
        };
        // The edit target; a declaration file generated from it by a banner; and its test.
        let source = make_file("source", "src/client.ts", false);
        let declaration = make_file("declaration", "src/client.d.ts", true);
        let test = make_file("test", "src/client_test.ts", false);
        let manifest = IndexManifest {
            repository: Repository {
                id: repo_id.clone(),
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 3,
            symbol_count: 0,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            // Graph reads fail closed unless the index declares current analysis semantics.
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            quality: IndexQuality::default(),
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[source.clone(), declaration.clone(), test.clone()],
                symbols: &[],
                occurrences: &[],
                chunks: &[],
                imports: &[],
                tests: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let file_node = |file: &File| GraphNode {
            id: identity::file_node_id(&file.path),
            node_type: GraphNodeType::File,
            label: file.path.display().to_string(),
            file_id: Some(file.id.clone()),
            ..Default::default()
        };
        let source_node = identity::file_node_id(&source.path);
        // A banner that names its origin: an authoritative declared-origin proof.
        let mut declared = GraphEdge {
            id: open_kioku_core::EdgeId::new("edge:declared"),
            from: identity::file_node_id(&declaration.path),
            to: source_node.clone(),
            edge_type: GraphEdgeType::DerivedFrom,
            evidence: Evidence::default(),
            ..Default::default()
        };
        let mut proof = RelationshipProof::new(
            RelationshipProofKind::DeclaredOrigin,
            open_kioku_core::DERIVED_FILE_DECLARED_ORIGIN_SOURCE,
            1,
        );
        proof.authority = RelationshipAuthority::Authoritative;
        declared.set_relationship_proofs(vec![proof]).unwrap();
        // A naming-convention pairing: no proof at all.
        let paired = GraphEdge {
            id: open_kioku_core::EdgeId::new("edge:paired"),
            from: identity::file_node_id(&test.path),
            to: source_node,
            edge_type: GraphEdgeType::DerivedFrom,
            evidence: Evidence::default(),
            ..Default::default()
        };
        store
            .replace_graph(
                &[
                    file_node(&source),
                    file_node(&declaration),
                    file_node(&test),
                ],
                &[declared, paired],
            )
            .unwrap();

        let report = ImpactEngine::new(&store)
            .with_graph_store(Some(&store))
            .for_file(Path::new("src/client.ts"))
            .unwrap();

        // A banner is prose, so even a declared origin is corroborating, never proven. It is
        // surfaced with its proof kind so a reader can tell it from a naming guess.
        assert!(
            report
                .possible_impact
                .iter()
                .any(|impact| impact.path == Path::new("src/client.d.ts")
                    && impact.authority == RelationshipAuthority::Corroborating
                    && impact.proof_kinds == vec![RelationshipProofKind::DeclaredOrigin]),
            "{:?}",
            report.possible_impact
        );
        assert!(
            report.proven_impact.is_empty(),
            "a generation banner must never reach proven impact: {:?}",
            report.proven_impact
        );
        // The naming pairing is a guess: surfaced, but never as structural truth.
        assert!(
            report
                .possible_impact
                .iter()
                .any(|impact| impact.path == Path::new("src/client_test.ts")
                    && impact.authority == RelationshipAuthority::Heuristic),
            "{:?}",
            report.possible_impact
        );
        assert!(
            !report
                .proven_impact
                .iter()
                .any(|impact| impact.path == Path::new("src/client_test.ts")),
            "a naming-convention pairing must never be presented as proven impact"
        );

        // Direction holds: editing the generated file does not claim to affect its origin.
        let reverse = ImpactEngine::new(&store)
            .with_graph_store(Some(&store))
            .for_file(Path::new("src/client.d.ts"))
            .unwrap();
        assert!(
            !reverse
                .proven_impact
                .iter()
                .chain(reverse.possible_impact.iter())
                .any(|impact| impact.path == Path::new("src/client.ts")),
            "{:?} {:?}",
            reverse.proven_impact,
            reverse.possible_impact
        );
    }

    /// The marker is the only record that a pre-4.0 index's edges were discarded on open. The
    /// graph store refuses every relationship read on such a store, and that refusal has to
    /// reach the caller rather than become an empty, confident `proven_impact`.
    #[test]
    fn relationship_impacts_refuse_a_graph_awaiting_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.sqlite");
        let manifest = IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository: Repository {
                id: RepositoryId::new("repo"),
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 1,
            symbol_count: 0,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        };
        let file = File {
            id: FileId::new("f1"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/core.rs"),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        {
            let store = SqliteStore::open(&path).unwrap();
            store
                .replace_index(IndexData {
                    manifest: &manifest,
                    files: &[file],
                    symbols: &[],
                    occurrences: &[],
                    chunks: &[],
                    imports: &[],
                    tests: &[],
                    analysis_facts: &[],
                    scopes: &[],
                    bindings: &[],
                    call_sites: &[],
                })
                .unwrap();
        }
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL); \
                 INSERT OR REPLACE INTO schema_meta(key, value) VALUES('graph_rebuild_required_v4', '1');",
            )
            .unwrap();
        let store = SqliteStore::open(&path).unwrap();

        let error = ImpactEngine::new(&store)
            .with_graph_store(Some(&store))
            .for_file(Path::new("src/core.rs"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("older index format") && error.contains("ok index"),
            "expected the rebuild instruction, got: {error}"
        );
    }

    #[test]
    fn relationship_impacts_are_empty_without_a_graph_store() {
        let store = make_store();
        let report = ImpactEngine::new(&store)
            .for_file(Path::new("src/missing.rs"))
            .unwrap();
        assert!(report.proven_impact.is_empty());
        assert!(report.possible_impact.is_empty());
    }

    #[test]
    fn unindexed_target_is_named_in_the_risk_report() {
        let store = make_store();
        let report = ImpactEngine::new(&store)
            .for_file(Path::new("does/not/exist.rs"))
            .unwrap();
        assert_eq!(report.risk_report.level, "unknown");
        assert!(
            report
                .risk_report
                .reasons
                .iter()
                .any(|reason| reason.contains("`does/not/exist.rs` is not in the index")),
            "{:?}",
            report.risk_report.reasons
        );
        assert!(!report
            .risk_report
            .reasons
            .iter()
            .any(|reason| reason.contains("limited indexed downstream references")));
        assert!(report
            .evidence
            .iter()
            .any(|evidence| evidence.message.contains("not in the index")));
    }
}
