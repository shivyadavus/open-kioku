use chrono::Utc;
use open_kioku_core::{
    identity, search_result_evidence_ids, AnalysisFact, ChurnSummary, CodeChunk, Confidence,
    Evidence, EvidenceId, EvidenceSourceType, File, FileId, FileRange, GitChangeKind, GraphEdge,
    GraphEdgeType, GraphNode, GraphNodeType, HistorySignalQuery, HistorySignalSummary,
    ImpactReport, LineRange, NodeId, RelationshipImpact, RelationshipImpactReads, RiskReport,
    ScoreComponent, SearchResult, Symbol, SymbolId, SymbolOccurrence,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_evidence::{RelationshipUseClass, RelationshipUsePolicy};
use open_kioku_git::DiffFile;
use open_kioku_search_regex::search_chunks;
use open_kioku_storage::{EdgeCount, GraphStore, HistoryStore, MetadataStore, SearchIndex};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

mod cargo;
use cargo::{CargoWorkspace, Membership, CRATE_IMPORT_SIGNAL, CRATE_IMPORT_USE_SIGNAL};

/// Most changed-file symbols whose inbound edges one report reads, taken in importance order
/// (see [`seed_order`]). Only symbols with an inbound edge that can carry impact are read at all.
const RELATIONSHIP_IMPACT_SYMBOL_SEEDS: usize = 256;
/// Inbound edges of one type read into one seed node.
const RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT: usize = 40;
/// Proven edges one read into one seed node takes when the store counted more of them than
/// [`RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT`] holds; see [`proven_window`]. Measured on this
/// repository at commit 6fcaae00, 31 windows held more than 40 proven edges, the most 121, so the
/// bound is a guard against a generated or vendored hub, not a cap a real symbol meets. It stays
/// under the store's 1,000-edge page.
const RELATIONSHIP_IMPACT_PROVEN_WINDOW_LIMIT: usize = 400;
/// Edges past [`RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT`] that the widened reads of one report take in
/// all, in the order the nodes are read. At the same commit every window of this repository
/// together held 735 proven edges past their first 40, so no report there met it; and the
/// unwidened reads of one report can take up to `256 seeds x 7 types x 41` edges, so widening
/// adds at most a few percent to the worst read.
const RELATIONSHIP_IMPACT_WIDENING_BUDGET: usize = 2_000;
/// Bounded size of each relationship impact list in the report.
const RELATIONSHIP_IMPACT_LIMIT: usize = 25;
/// Bound on the proven dependents of the symbols a change touches, which
/// [`RELATIONSHIP_IMPACT_LIMIT`] does not cut: a bound for a diff that rewrites a large file, not
/// a cap a focused change meets (the most measured on this repository's last twelve commits was
/// 114, from one large file).
const RELATIONSHIP_IMPACT_TOUCHED_LIMIT: usize = 200;
/// Number of the changed file's names searched for lexical dependents; see [`impact_terms`].
const MAX_IMPACT_TERMS: usize = 8;

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

    /// Impact of changing `path`, with nothing known about which part of it changed.
    pub fn for_file(&self, path: &Path) -> Result<ImpactReport> {
        self.for_change(path, &ChangeFocus::default())
    }

    /// Impact of changing `path`, where `focus` says which part changed. The relationship reads
    /// start from the symbols the change touches, so their dependents are read and listed first;
    /// every other part of the report is as [`ImpactEngine::for_file`] makes it.
    pub fn for_change(&self, path: &Path, focus: &ChangeFocus) -> Result<ImpactReport> {
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

        // Exact references are counted per indexed occurrence before direct impacts are grouped
        // by path: twelve call sites in one file are twelve references in one impacted file.
        let mut exact_reference_count = 0;
        let mut exact_reference_files = 0;
        let mut exact_reference_sources = Vec::new();
        let mut omitted_direct = 0;
        let mut omitted_direct_exact = 0;
        let mut omitted_direct_imports = 0;
        let mut omitted_direct_import_uses = 0;
        // The Rust package model and import resolutions indexing stored answer which downstream
        // crates import this file, and which lexical matches no dependency path can reach.
        let rust_packages = match &file {
            Some(file) if file.language == open_kioku_core::Language::Rust => {
                let files = self.store.list_files(usize::MAX, 0)?;
                let workspace = CargoWorkspace::load(self.store, &files)?;
                let dependents = cargo::crate_dependent_impacts(
                    self.store,
                    &workspace,
                    &files,
                    file,
                    &target_symbols,
                )?;
                Some((workspace, dependents))
            }
            _ => None,
        };
        let reachability = match (&rust_packages, &file) {
            (Some((workspace, _)), Some(file)) => match workspace.membership(&file.path) {
                Membership::Package(package) => Some(RustReachability {
                    workspace,
                    package,
                    reachable: workspace.dependents_closure(package),
                }),
                _ => None,
            },
            _ => None,
        };
        let mut unreachable_lexical = std::collections::BTreeSet::new();
        let direct = if let Some(file) = &file {
            let mut direct = exact_reference_impacts(self.store, file, &target_symbols)?;
            exact_reference_count = direct.len();
            exact_reference_sources = exact_reference_sources_by_authority(&direct);
            exact_reference_files = direct
                .iter()
                .map(|result| result.path.as_path())
                .collect::<std::collections::BTreeSet<_>>()
                .len();
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
            let file_tests = self.store.tests_for_files(std::slice::from_ref(&file.id))?;
            for term in impact_terms(path, file, &target_symbols, &file_tests)
                .into_iter()
                .take(MAX_IMPACT_TERMS)
            {
                let results = search(&term, 25)?;
                direct.extend(
                    results
                        .into_iter()
                        .filter(|result| result.path != file.path),
                );
            }
            if let Some((_, dependents)) = &rust_packages {
                direct.extend(dependents.results.iter().cloned());
            }
            direct = group_direct_impacts(dedupe_results(direct));
            if let Some(reachability) = &reachability {
                direct.retain(|result| {
                    let keep = direct_impact_kind(result) != DirectImpactKind::Lexical
                        || reachability.may_depend(&result.path);
                    if !keep {
                        unreachable_lexical.insert(result.path.clone());
                    }
                    keep
                });
            }
            direct.sort_by(compare_impact_results);
            // Exact references rank first, so any cut here is one only when they alone
            // overflow the cap; that is counted separately so it cannot pass unnoticed.
            omitted_direct_exact = direct
                .iter()
                .skip(MAX_DIRECT_IMPACTS)
                .filter(|result| result.is_exact_reference())
                .count();
            for result in direct.iter().skip(MAX_DIRECT_IMPACTS) {
                match direct_impact_kind(result) {
                    DirectImpactKind::CrateImport => omitted_direct_imports += 1,
                    DirectImpactKind::CrateImportUse => omitted_direct_import_uses += 1,
                    _ => {}
                }
            }
            omitted_direct = cap_impacts(&mut direct, MAX_DIRECT_IMPACTS);
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
            // A Rust crate or module root's stem names no dependent: searching `lib` matched
            // every crate root in a workspace.
            if indirect_stem.len() < 3 || matches!(indirect_stem, "lib" | "mod" | "main") {
                continue;
            }
            let second = search(indirect_stem, 10)?;
            for result in second {
                if result.path == path || direct_paths.contains(&result.path) {
                    continue;
                }
                if reachability
                    .as_ref()
                    .is_some_and(|reachability| !reachability.may_depend(&result.path))
                {
                    unreachable_lexical.insert(result.path.clone());
                    continue;
                }
                indirect.push(result);
            }
        }
        indirect.sort_by(compare_impact_results);
        // One entry per path, the best-ranked. `dedup_by` only folds adjacent entries, and a
        // path found at two different scores is not adjacent after the sort.
        let mut indirect_paths = std::collections::HashSet::new();
        indirect.retain(|result| indirect_paths.insert(result.path.clone()));
        let omitted_indirect = cap_impacts(&mut indirect, MAX_INDIRECT_IMPACTS);
        let mut reasons = Vec::new();
        if exact_reference_count > 0 {
            reasons.push(format!(
                "{exact_reference_count} exact indexed symbol reference(s) found in {exact_reference_files} file(s)"
            ));
        }
        if let Some((workspace, dependents)) = &rust_packages {
            if let Some(reason) = &dependents.not_measured {
                reasons.push(format!(
                    "downstream crates were not measured: {reason}; an absent crate-import impact is not evidence that no other crate depends on this file"
                ));
            }
            if dependents.importing_files > 0 {
                reasons.push(format!(
                    "{} file(s) in {} package(s) import public items of this file by crate path; {} further file(s) of those packages name an imported item",
                    dependents.importing_files, dependents.packages, dependents.use_files
                ));
            }
            if dependents.unresolved_imports > 0 || dependents.unchecked_import_files > 0 {
                reasons.push(format!(
                    "{} `use` declaration(s) naming this file's crate in its package or its dependents resolved to no file{}; an importer of this file may be among them",
                    dependents.unresolved_imports,
                    if dependents.unchecked_import_files > 0 {
                        format!(
                            ", and {} further file(s) holding such declarations were not checked (scan cap reached)",
                            dependents.unchecked_import_files
                        )
                    } else {
                        String::new()
                    }
                ));
            }
            if dependents.unscanned_files > 0 {
                reasons.push(format!(
                    "{} file(s) of importing packages were not read for uses of the imported items (scan cap reached)",
                    dependents.unscanned_files
                ));
            }
            if let Some(reachability) = &reachability {
                if !unreachable_lexical.is_empty() {
                    let sample = unreachable_lexical
                        .iter()
                        .take(3)
                        .map(|path| format!("`{}`", path.display()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    reasons.push(format!(
                        "{} lexical match(es) left out: Rust files in no workspace package, or in packages with no Cargo dependency path to {} (e.g. {sample})",
                        unreachable_lexical.len(),
                        workspace.package_label(reachability.package)
                    ));
                }
            }
        }
        if direct.len() > 10 {
            reasons.push("many lexical dependents reference this file or its symbols".into());
        }
        // The lists are capped; a dependent past the cap is still a dependent. Say how many
        // were cut so a short list is not read as the whole blast radius.
        if omitted_direct > 0 {
            reasons.push(format!(
                "{omitted_direct} further direct impact(s) omitted beyond the {MAX_DIRECT_IMPACTS}-entry cap, {omitted_direct_exact} of them exact-reference entries, {omitted_direct_imports} crate-import entries and {omitted_direct_import_uses} imported-name uses; exact references are cut last, then crate imports and imported-name uses"
            ));
        }
        if omitted_indirect > 0 {
            reasons.push(format!(
                "{omitted_indirect} further indirect impact(s) omitted beyond the {MAX_INDIRECT_IMPACTS}-entry cap"
            ));
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
            // One record summarises every exact reference, so it takes the strongest source
            // among them and its message names them all: a tree-sitter or LSP reference is
            // never labelled SCIP.
            source_type: exact_reference_sources
                .first()
                .cloned()
                .unwrap_or(EvidenceSourceType::Lexical),
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
                format!(
                    "impact report derived from exact indexed symbol references ({}) and lexical references",
                    exact_reference_sources
                        .iter()
                        .filter_map(exact_reference_label)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
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
        let relationships = match (self.graph_store, &file) {
            (Some(graph), Some(file)) => {
                relationship_impacts(graph, self.store, file, &target_symbols, focus)?
            }
            _ => RelationshipImpacts::default(),
        };
        let mut report = ImpactReport {
            target: path.display().to_string(),
            direct_impacts: direct,
            indirect_impacts: indirect,
            direct_impacts_omitted: omitted_direct,
            indirect_impacts_omitted: omitted_indirect,
            proven_impact: relationships.proven,
            proven_impact_omitted: relationships.proven_omitted,
            proven_impact_omitted_files: relationships.proven_omitted_files,
            possible_impact: relationships.possible,
            possible_impact_omitted: relationships.possible_omitted,
            possible_impact_omitted_files: relationships.possible_omitted_files,
            relationship_impact_caveats: relationships.caveats,
            relationship_impact_reads: relationships.reads,
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
        // Why the focus is narrower than the caller asked for matters only where it decided
        // something: which symbols were read, or which proven dependents the cap kept.
        if focus_mattered(&report) {
            report
                .relationship_impact_caveats
                .extend(focus.caveats.iter().cloned());
        }
        Ok(report)
    }

    /// Answer an impact request. `ok impact` and MCP `impact_analysis` both answer through this,
    /// so one request reads the same symbols first and keeps the same dependents on either.
    ///
    /// A path, or the file defining `symbol`, is one report, focused on the symbol and on the
    /// lines `diff` changes in that file. With neither, the paths `diff` changes are reported one
    /// by one, focused on their changed lines: at most [`DIFF_REPORT_LIMIT`] of them, those whose
    /// touched symbols have the most proven dependents first, and none started after `deadline`.
    /// A diff of an old revision can change hundreds of files, each a full impact read; the
    /// paths left out are counted, and `changed_files` still lists every one.
    ///
    /// A path the diff deletes, or renames away, is reported from the dependents the index last
    /// held for it, the file and every symbol it defined touched: its dependents are the ones most certain to
    /// break. When the index does not hold it there are none to read, and it is listed in
    /// [`DiffImpact::removed_paths_not_indexed`] instead. A rename is a removal of its previous
    /// path beside a change to its new one, each a changed path of its own.
    pub fn answer(&self, repo_root: &Path, request: ImpactRequest<'_>) -> Result<ImpactAnswer> {
        let target = match (request.path, request.symbol) {
            (Some(path), _) => Some(path.to_path_buf()),
            (None, Some(symbol)) => Some(
                self.store
                    .file_by_id(&symbol.file_id)?
                    .map(|file| file.path)
                    .unwrap_or_else(|| PathBuf::from(&symbol.qualified_name)),
            ),
            (None, None) => None,
        };
        if let Some(path) = target {
            // A path the diff removes is reported as every other removal is, so `path` with
            // `since` names the dependents `since` alone lists for it.
            // Compared as repository paths, so `./src/a.rs` names the `src/a.rs` git reports.
            let same_path = |removed: &Path| {
                removed == path
                    || matches!(
                        (
                            identity::normalize_repo_path(removed),
                            identity::normalize_repo_path(&path),
                        ),
                        (Ok(removed), Ok(asked)) if removed == asked
                    )
            };
            if let Some(removal) = request.diff.and_then(|diff| {
                removals(diff)
                    .into_iter()
                    .find(|removal| same_path(removal.path))
            }) {
                let report = match self.removal_focus(&removal)? {
                    Some(focus) => self.removed_report(&removal, &focus)?,
                    None => {
                        let mut report = self.for_change(removal.path, &ChangeFocus::default())?;
                        report
                            .relationship_impact_caveats
                            .insert(0, removal.unindexed_caveat());
                        report
                    }
                };
                return Ok(ImpactAnswer::File(Box::new(report)));
            }
            let mut focus = match request.diff {
                Some(diff) => self.diff_focus(repo_root, &path, diff)?,
                None => ChangeFocus::default(),
            };
            focus
                .symbols
                .extend(request.symbol.map(|symbol| symbol.id.clone()));
            return Ok(ImpactAnswer::File(Box::new(
                self.for_change(&path, &focus)?,
            )));
        }
        let Some(diff) = request.diff else {
            return Err(OkError::InvalidInput(
                "impact needs a file path, a symbol, or a git revision to diff against".into(),
            ));
        };
        // Every path the diff leaves in place, and every path it removes: a deleted file, or the
        // previous path of a rename, which counts as a removal beside the addition of its new
        // path. A removed path the index still holds is reported from the dependents it last
        // indexed, every symbol of it touched; one the index does not hold has none to read
        // and is listed apart.
        let removed = removals(diff);
        let mut paths = Vec::new();
        for change in diff {
            paths.extend(change.new_path.as_deref().map(|path| (path, None)));
            paths.extend(
                removed
                    .iter()
                    .filter(|removal| change.old_path.as_deref() == Some(removal.path))
                    .map(|removal| (removal.path, Some(removal))),
            );
        }
        let renames = removed
            .iter()
            .filter(|removal| removal.renamed_to.is_some())
            .count();
        // Ranked before any is read, by counts alone, so the cap keeps the files whose change
        // most certainly breaks something: proven dependents of touched symbols, then any
        // dependents of them, then diff order. Of two paths with as many proven dependents, a
        // removal comes first: nothing it defined is left for them to use.
        let past_deadline = || {
            request
                .deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        };
        let mut ranked = Vec::with_capacity(paths.len());
        let mut removed_paths_not_indexed = Vec::new();
        let mut unranked = 0usize;
        for (position, (path, removal)) in paths.into_iter().enumerate() {
            let focus = match removal {
                Some(removal) => match self.removal_focus(removal)? {
                    Some(focus) => focus,
                    None => {
                        removed_paths_not_indexed.push(path.to_path_buf());
                        continue;
                    }
                },
                None => self.diff_focus(repo_root, path, diff)?,
            };
            // Past the deadline the rest keep their diff order, behind every ranked path.
            let late = past_deadline();
            let (proven, total) = if late {
                unranked += 1;
                (0, 0)
            } else {
                self.touched_inbound(path, &focus)?
            };
            ranked.push((
                (
                    late,
                    std::cmp::Reverse(proven),
                    removal.is_none(),
                    std::cmp::Reverse(total),
                    position,
                ),
                path,
                removal,
                focus,
            ));
        }
        ranked.sort_by_key(|(key, _, _, _)| *key);
        let reportable = ranked.len();
        let changed_paths = reportable + removed_paths_not_indexed.len();
        let mut reports = Vec::new();
        let mut stopped_at_deadline = false;
        for (_, path, removal, focus) in ranked.into_iter().take(DIFF_REPORT_LIMIT) {
            // One report can take as long as the whole budget on a large repository, so none is
            // started past the deadline, the first included.
            if past_deadline() {
                stopped_at_deadline = true;
                break;
            }
            reports.push(match removal {
                Some(removal) => self.removed_report(removal, &focus)?,
                None => self.for_change(path, &focus)?,
            });
        }
        let reports_omitted = reportable - reports.len();
        let mut caveats = Vec::new();
        if reports_omitted > 0 {
            caveats.push(format!(
                "impact reports cover {} of the {changed_paths} changed paths{}, those whose \
                 touched symbols have the most proven dependents first; {reports_omitted} {}; \
                 every changed path is in `changed_files`, and `path` gives any one of them its \
                 own report",
                reports.len(),
                if renames > 0 {
                    format!(" (each of {renames} rename(s) counted as its old and its new path)")
                } else {
                    String::new()
                },
                if stopped_at_deadline {
                    "were not started within the time this request allows".to_string()
                } else {
                    format!("were left out, as at most {DIFF_REPORT_LIMIT} are reported")
                }
            ));
        }
        if !removed_paths_not_indexed.is_empty() {
            caveats.push(format!(
                "{} path(s) the diff deletes or renames away are not in the index (it was \
                 rebuilt since the change, or never held them), so the dependents they had \
                 cannot be read from it and they have no report; `removed_paths_not_indexed` \
                 lists them, and a text search for the names they defined finds code that still \
                 uses them",
                removed_paths_not_indexed.len()
            ));
        }
        if unranked > 0 {
            caveats.push(format!(
                "{unranked} changed path(s) were not ranked within the time this request allows \
                 and follow the ranked ones in diff order"
            ));
        }
        Ok(ImpactAnswer::Diff(DiffImpact {
            reports,
            reports_omitted,
            removed_paths_not_indexed,
            caveats,
        }))
    }

    /// The focus of a path the diff removes, as the index last held it: the file itself and every
    /// symbol it defined, since none of them is left. `None` when the index does not hold the
    /// path.
    fn removal_focus(&self, removal: &Removal<'_>) -> Result<Option<ChangeFocus>> {
        let Some(file) = self.store.get_file_by_path(removal.path)? else {
            return Ok(None);
        };
        Ok(Some(ChangeFocus {
            symbols: self
                .store
                .symbols_for_file(&file.id)?
                .into_iter()
                .filter(|symbol| symbol.file_id == file.id)
                .map(|symbol| symbol.id)
                .collect(),
            whole_file: true,
            ..ChangeFocus::default()
        }))
    }

    /// The report of a path the diff removes, read from its last-indexed dependents, saying so
    /// first in its caveats and its risk reasons.
    fn removed_report(&self, removal: &Removal<'_>, focus: &ChangeFocus) -> Result<ImpactReport> {
        let mut report = self.for_change(removal.path, focus)?;
        let caveat = removal.caveat();
        report.risk_report.reasons.insert(0, caveat.clone());
        report.relationship_impact_caveats.insert(0, caveat);
        Ok(report)
    }

    /// Proven and all inbound impact edges of the nodes `focus` touches in `path` (its symbols,
    /// and the file node when the change removes the whole file), counted without reading them;
    /// zero where the index or the graph cannot say.
    fn touched_inbound(&self, path: &Path, focus: &ChangeFocus) -> Result<(usize, usize)> {
        let Some(graph) = self.graph_store else {
            return Ok((0, 0));
        };
        let Some(file) = self.store.get_file_by_path(path)? else {
            return Ok((0, 0));
        };
        let symbols = self.store.symbols_for_file(&file.id)?;
        let own = symbols
            .iter()
            .filter(|symbol| symbol.file_id == file.id)
            .collect::<Vec<_>>();
        let nodes = own
            .iter()
            .zip(focus.touched(&own))
            .filter(|(_, touched)| *touched)
            .map(|(symbol, _)| identity::symbol_node_id(symbol).0)
            .chain(
                focus
                    .whole_file
                    .then(|| identity::try_file_node_id(&file.path).ok())
                    .flatten()
                    .map(|node| node.0),
            )
            .collect::<Vec<_>>();
        if nodes.is_empty() {
            return Ok((0, 0));
        }
        let ids = nodes.iter().map(String::as_str).collect::<Vec<_>>();
        let counts = match graph.edge_counts_for_nodes(&IMPACT_EDGE_TYPES, &ids, false) {
            Ok(counts) => counts,
            Err(OkError::Unsupported(_)) => return Ok((0, 0)),
            Err(err) => return Err(err),
        };
        Ok(counts
            .values()
            .flat_map(|by_type| by_type.values())
            .fold((0, 0), |(proven, total), count| {
                (proven + count.proven.unwrap_or(0), total + count.total)
            }))
    }

    /// The diff of `since` against the working tree, for an [`ImpactRequest`]. `since` compares
    /// against git history, so a directory that is not a git work tree is the caller's mistake,
    /// not a repository with nothing changed.
    pub fn changes_since(repo_root: &Path, since: &str) -> Result<Vec<DiffFile>> {
        if !repo_root.join(".git").exists() {
            return Err(OkError::InvalidInput(format!(
                "`since` compares against git history, and `{}` is not a git repository",
                repo_root.display()
            )));
        }
        let changes = open_kioku_git::diff_unified_zero_since(repo_root, since)?;
        Ok(changes
            .into_iter()
            .filter(|change| change.old_path.is_some() || change.new_path.is_some())
            .collect())
    }

    /// The change focus `diff` gives `path`: the lines its diff adds or modifies, matched to the
    /// file's symbols as the index holds them.
    ///
    /// Those line numbers are the working tree's, so they name the indexed symbols only while the
    /// index holds the file as it is on disk. When it does not (the file changed since `ok index`,
    /// or cannot be read), the lines are dropped rather than pointed at whichever symbols now sit
    /// on them. A hunk that only removes lines has no line on the new side and touches no symbol.
    /// A path the diff renames or removes is numbered as the revision had it, and one the diff
    /// does not change has no lines at all. Each case leaves a caveat on the focus, reported where
    /// it could have changed the answer.
    pub fn diff_focus(
        &self,
        repo_root: &Path,
        path: &Path,
        diff: &[DiffFile],
    ) -> Result<ChangeFocus> {
        let shown = path.display();
        let Some(change) = diff
            .iter()
            .find(|change| change.new_path.as_deref() == Some(path))
        else {
            let caveat = if diff
                .iter()
                .any(|change| change.old_path.as_deref() == Some(path))
            {
                format!(
                    "the diff renames or removes `{shown}`, so its changed lines, numbered as the \
                     revision had them, were not matched to its symbols"
                )
            } else {
                format!("the diff changes no line of `{shown}`, so no symbol of it was read first")
            };
            return Ok(ChangeFocus {
                caveats: vec![caveat],
                ..ChangeFocus::default()
            });
        };
        let indexed = self.store.get_file_by_path(path)?;
        let on_disk = std::fs::read(repo_root.join(path))
            .ok()
            .map(|bytes| format!("{:x}", Sha256::digest(&bytes)));
        if indexed.is_some() && indexed.as_ref().map(|file| &file.content_hash) != on_disk.as_ref()
        {
            return Ok(ChangeFocus {
                caveats: vec![format!(
                    "`{shown}` differs from the indexed copy, so the lines the diff changed were \
                     not matched to its symbols; run `ok index` for the dependents of the changed \
                     symbols to be read first"
                )],
                ..ChangeFocus::default()
            });
        }
        let removals = change
            .hunks
            .iter()
            .filter(|hunk| hunk.new_range.is_none())
            .count();
        let mut focus = ChangeFocus::lines(change.changed_line_ranges());
        if removals > 0 {
            focus.caveats.push(format!(
                "{removals} hunk(s) of `{shown}` only remove lines, which touch no symbol, so the \
                 symbols around them were not read first"
            ));
        }
        Ok(focus)
    }
}

/// Changed paths one `since`-only request reports on; see [`ImpactEngine::answer`].
pub const DIFF_REPORT_LIMIT: usize = 25;

/// A path a diff removes: a deleted file, or the previous path of a rename.
#[derive(Debug, Clone, Copy)]
struct Removal<'d> {
    path: &'d Path,
    renamed_to: Option<&'d Path>,
}

impl Removal<'_> {
    /// Why a removed path's report reads the index rather than the working tree.
    fn caveat(&self) -> String {
        let path = self.path.display();
        match self.renamed_to {
            Some(new) => format!(
                "the diff renames `{path}` to `{}`: these are the dependents the index last \
                 held for `{path}`, read through the file and every symbol it defined, and one that names it \
                 by path or module breaks unless the change updates it; `{}` is a changed path \
                 of its own",
                new.display(),
                new.display()
            ),
            None => format!(
                "the diff deletes `{path}`: these are the dependents the index last held for it, \
                 read through the file and every symbol it defined, and each that still uses it breaks unless \
                 the change updates it"
            ),
        }
    }

    /// Why a removed path the index does not hold has no dependents in its report.
    fn unindexed_caveat(&self) -> String {
        format!(
            "the diff {} `{}` and the index does not hold it (it was rebuilt since the change, \
             or never held it), so the dependents it had cannot be read; a text search for the \
             names it defined finds code that still uses them",
            if self.renamed_to.is_some() {
                "renames away"
            } else {
                "deletes"
            },
            self.path.display()
        )
    }
}

/// The paths `diff` removes, in diff order: those it deletes, and the previous paths of those it
/// renames. A path another change of the diff writes (a rename onto a deleted path, or two files
/// swapping names) is still there, and is not a removal.
fn removals(diff: &[DiffFile]) -> Vec<Removal<'_>> {
    let written = diff
        .iter()
        .filter_map(|change| change.new_path.as_deref())
        .collect::<HashSet<_>>();
    diff.iter()
        .filter_map(|change| {
            let old = change.old_path.as_deref()?;
            let removed = match change.new_path.as_deref() {
                None => true,
                Some(new) => change.status == GitChangeKind::Renamed && new != old,
            };
            (removed && !written.contains(old)).then_some(Removal {
                path: old,
                renamed_to: change.new_path.as_deref(),
            })
        })
        .collect()
}

/// What `ok impact` and MCP `impact_analysis` are asked; see [`ImpactEngine::answer`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ImpactRequest<'r> {
    /// The file to analyze.
    pub path: Option<&'r Path>,
    /// A symbol the change touches; its file is analyzed when no path is given.
    pub symbol: Option<&'r Symbol>,
    /// The diff of a revision (or range) against the working tree, from
    /// [`ImpactEngine::changes_since`].
    pub diff: Option<&'r [DiffFile]>,
    /// When a diff's paths are reported one by one, none is started after this.
    pub deadline: Option<std::time::Instant>,
}

/// The answer to an [`ImpactRequest`]: one file's report, or the reports of a diff's paths.
#[derive(Debug, Clone)]
pub enum ImpactAnswer {
    File(Box<ImpactReport>),
    Diff(DiffImpact),
}

/// The reports of the paths a diff changes, and how many it left out.
#[derive(Debug, Clone, Default)]
pub struct DiffImpact {
    /// Reports, those whose touched symbols have the most proven dependents first.
    pub reports: Vec<ImpactReport>,
    /// Changed paths with no report: past [`DIFF_REPORT_LIMIT`], or not started by the deadline.
    pub reports_omitted: usize,
    /// Paths the diff deletes or renames away that the index does not hold (rebuilt since the
    /// change, or never indexed, as a secret-like path is not), so no dependents of theirs can be
    /// read; counted apart from `reports_omitted`.
    pub removed_paths_not_indexed: Vec<PathBuf>,
    /// Why paths were left out, when they were.
    pub caveats: Vec<String>,
}

impl DiffImpact {
    /// The shape `ok impact --since --json` prints and MCP `impact_analysis` returns for
    /// `since` alone.
    pub fn to_json(&self, since: &str, changed_files: &[DiffFile]) -> serde_json::Value {
        let mut value = serde_json::json!({
            "since": since,
            "changed_files": changed_files,
            "impact_reports": self.reports,
        });
        if self.reports_omitted > 0 {
            value["impact_reports_omitted"] = serde_json::json!(self.reports_omitted);
        }
        if !self.removed_paths_not_indexed.is_empty() {
            value["removed_paths_not_indexed"] = serde_json::json!(self.removed_paths_not_indexed);
        }
        if !self.caveats.is_empty() {
            value["caveats"] = serde_json::json!(self.caveats);
        }
        value
    }
}

/// Whether a change focus decided anything in `report`: it orders which symbols are read and
/// which proven dependents the cap may not cut, so it matters only where a symbol with
/// dependents went unread or the proven list was cut.
fn focus_mattered(report: &ImpactReport) -> bool {
    report.proven_impact_omitted > 0
        || report
            .relationship_impact_reads
            .as_ref()
            .is_some_and(|reads| reads.symbols_unread_with_dependents != Some(0))
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
            match_reason: GIT_COCHANGE_MATCH_REASON.into(),
            evidence,
            evidence_refs: vec![fact.id.clone()],
            confidence: fact.confidence.score(),
            score_breakdown: vec![ScoreComponent::single(
                "similar_change_overlap",
                0.18,
                vec![fact.id.clone()],
                "impact candidate historically changed with the target file",
            )],
            exact_reference_provenance: None,
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

/// What a caller knows about which part of a changed file the change touches.
///
/// Impact reads the dependents of a bounded number of the file's symbols. Without a focus it
/// takes them by evidence, visibility and how many inbound edges each has; with one, the symbols
/// the change touches come first, so their dependents are read and listed whatever else the file
/// holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeFocus {
    /// Lines of the file, as the index holds it, that the change adds or modifies. A line touches
    /// the innermost symbols around it: a class or `impl` block is touched only by a changed line
    /// none of its members holds.
    pub lines: Vec<LineRange>,
    /// Symbols the caller names as changed.
    pub symbols: Vec<SymbolId>,
    /// Whether the change removes the file itself, as deleting it or renaming it away does. The
    /// file's own node is then touched too, so what imports the file by path is a dependent of
    /// the change, as a caller of one of its symbols is. A change to some of its lines is not.
    pub whole_file: bool,
    /// Why the focus is narrower than the change, such as lines that could not be matched to
    /// the indexed symbols. A report carries them where the focus decided something.
    pub caveats: Vec<String>,
}

impl ChangeFocus {
    /// A change to these lines of the file.
    pub fn lines(lines: Vec<LineRange>) -> Self {
        Self {
            lines,
            ..Self::default()
        }
    }

    /// A change to these symbols.
    pub fn symbols(symbols: Vec<SymbolId>) -> Self {
        Self {
            symbols,
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.symbols.is_empty() && !self.whole_file
    }

    /// Which of `symbols` (one file's) the focus touches, in their order. A named symbol is
    /// touched. A changed line touches a symbol only where none of the symbols nested in it holds
    /// the line, so a one-line edit inside a method of a large class touches the method, not
    /// every member of the class with it.
    fn touched(&self, symbols: &[&Symbol]) -> Vec<bool> {
        symbols
            .iter()
            .map(|symbol| {
                if self.symbols.contains(&symbol.id) {
                    return true;
                }
                let Some(range) = &symbol.range else {
                    return false;
                };
                let mut nested = symbols
                    .iter()
                    .filter_map(|other| other.range.as_ref())
                    .filter(|inner| {
                        range.start <= inner.start
                            && inner.end <= range.end
                            && (inner.start, inner.end) != (range.start, range.end)
                    })
                    .map(|inner| (inner.start, inner.end))
                    .collect::<Vec<_>>();
                nested.sort_unstable();
                self.lines.iter().any(|line| {
                    let (start, end) = (line.start.max(range.start), line.end.min(range.end));
                    start <= end && !lines_covered(start, end, &nested)
                })
            })
            .collect()
    }
}

/// Whether every line of `start..=end` lies in one of `ranges` (sorted by start).
fn lines_covered(start: u32, end: u32, ranges: &[(u32, u32)]) -> bool {
    let mut next = start;
    for &(from, to) in ranges {
        if from > next {
            break;
        }
        if to >= next {
            match to.checked_add(1) {
                Some(after) => next = after,
                None => return true,
            }
            if next > end {
                return true;
            }
        }
    }
    next > end
}

/// Edge types whose source is affected when the changed file is edited, as read into a seed.
const IMPACT_EDGE_TYPES: [GraphEdgeType; 8] = [
    GraphEdgeType::Calls,
    GraphEdgeType::References,
    GraphEdgeType::UsesType,
    GraphEdgeType::Implements,
    GraphEdgeType::Extends,
    GraphEdgeType::Imports,
    GraphEdgeType::DependsOn,
    GraphEdgeType::DerivedFrom,
];

/// The impact edge types read into `node_id`. Derived edges join two file nodes, so a symbol seed
/// never has one, and one counted there would be an edge no read could reach.
fn impact_edge_types_for(node_id: &NodeId) -> Vec<GraphEdgeType> {
    let file = node_id.0.starts_with("file:");
    IMPACT_EDGE_TYPES
        .into_iter()
        .filter(|edge_type| file || *edge_type != GraphEdgeType::DerivedFrom)
        .collect()
}

/// What `relationship_impacts` found, and what its bounded reads left unread.
#[derive(Default)]
struct RelationshipImpacts {
    proven: Vec<RelationshipImpact>,
    proven_omitted: usize,
    proven_omitted_files: usize,
    possible: Vec<RelationshipImpact>,
    possible_omitted: usize,
    possible_omitted_files: usize,
    caveats: Vec<String>,
    reads: Option<RelationshipImpactReads>,
}

/// One node whose inbound edges impact reads: the changed file, or one of its symbols.
struct Seed {
    node_id: NodeId,
    label: String,
    /// Whether the change touches it. The file node is only when the change removes the file
    /// ([`ChangeFocus::whole_file`]): a change to some of its lines is not a change to everything
    /// that imports it.
    touched: bool,
    /// The symbol's position in the file's symbol list (the file node is 0, symbols from 1): the
    /// tie-break that keeps one entry per dependent the same whatever order seeds are read in.
    position: usize,
}

/// A node's counted inbound impact edges: all of them, and the proven ones when the store can
/// tell.
#[derive(Debug, Clone, Copy, Default)]
struct InboundCount {
    total: usize,
    proven: Option<usize>,
}

/// The order in which a changed file's symbols are read, most likely to have dependents that
/// matter first: symbols the change touches; then symbols with a proven dependent, so a run of
/// name matches never pushes a proof out of the read; then by visibility (public, then package-
/// or crate-wide or unknown, then private); then by proven, then all, inbound edges, most first;
/// then in file order.
fn seed_order(touched: bool, symbol: &Symbol, inbound: InboundCount, position: usize) -> impl Ord {
    let visibility = match symbol.visibility {
        open_kioku_core::Visibility::Public => 0u8,
        open_kioku_core::Visibility::Private => 2,
        _ => 1,
    };
    let proven = inbound.proven.unwrap_or(0);
    (
        !touched,
        proven == 0,
        visibility,
        std::cmp::Reverse(proven),
        std::cmp::Reverse(inbound.total),
        position,
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
    focus: &ChangeFocus,
) -> Result<RelationshipImpacts> {
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

    let file_seed = identity::try_file_node_id(&target_file.path)
        .ok()
        .map(|node_id| Seed {
            node_id,
            label: target_file.path.display().to_string(),
            touched: focus.whole_file,
            position: 0,
        });
    let own = symbols
        .iter()
        .filter(|symbol| symbol.file_id == target_file.id)
        .collect::<Vec<_>>();
    let touched = focus.touched(&own);
    let own_symbols = own
        .iter()
        .zip(touched)
        .enumerate()
        .map(|(index, (symbol, touched))| {
            let seed = Seed {
                node_id: identity::symbol_node_id(symbol),
                label: symbol.qualified_name.clone(),
                touched,
                position: index + 1,
            };
            (seed, *symbol)
        })
        .collect::<Vec<_>>();
    let symbols_total = own_symbols.len();
    let symbols_touched = own_symbols.iter().filter(|(seed, _)| seed.touched).count();

    // Counted, not read: which nodes have any inbound edge that can carry impact, how many, and
    // how many of those are proven. A symbol with none needs no read, the rest are read most
    // important first, and what the bounded reads leave out is a number rather than a guess. A
    // store that cannot count leaves every symbol a candidate, in visibility and file order, and
    // an unread one may have dependents.
    let count_ids = file_seed
        .iter()
        .chain(own_symbols.iter().map(|(seed, _)| seed))
        .map(|seed| seed.node_id.0.as_str())
        .collect::<Vec<_>>();
    let counts = match graph.edge_counts_for_nodes(&IMPACT_EDGE_TYPES, &count_ids, false) {
        Ok(counts) => Some(counts),
        Err(OkError::Unsupported(_)) => None,
        Err(err) => return Err(err),
    };
    let inbound_counts = |node_id: &NodeId| -> Option<BTreeMap<GraphEdgeType, EdgeCount>> {
        let counts = counts.as_ref()?;
        let types = impact_edge_types_for(node_id);
        Some(
            counts
                .get(&node_id.0)
                .map(|by_type| {
                    by_type
                        .iter()
                        .filter(|(edge_type, _)| types.contains(edge_type))
                        .map(|(edge_type, count)| (edge_type.clone(), *count))
                        .collect()
                })
                .unwrap_or_default(),
        )
    };
    let inbound = |node_id: &NodeId| -> Option<InboundCount> {
        inbound_counts(node_id).map(|by_type| InboundCount {
            total: by_type.values().map(|count| count.total).sum(),
            proven: by_type
                .values()
                .map(|count| count.proven)
                .sum::<Option<usize>>(),
        })
    };
    let has_edges = |node_id: &NodeId| inbound(node_id).is_none_or(|count| count.total > 0);

    let mut candidates = own_symbols
        .into_iter()
        .filter(|(seed, _)| has_edges(&seed.node_id))
        .map(|(seed, symbol)| {
            let count = inbound(&seed.node_id).unwrap_or_default();
            (seed_order(seed.touched, symbol, count, seed.position), seed)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left, _), (right, _)| left.cmp(right));
    let unread = candidates
        .split_off(RELATIONSHIP_IMPACT_SYMBOL_SEEDS.min(candidates.len()))
        .into_iter()
        .map(|(_, seed)| seed)
        .collect::<Vec<_>>();
    let symbols_read = candidates.len();
    let seeds = file_seed
        .into_iter()
        .filter(|seed| has_edges(&seed.node_id))
        .chain(candidates.into_iter().map(|(_, seed)| seed))
        .collect::<Vec<_>>();

    let unread_counts = unread
        .iter()
        .filter_map(|seed| inbound(&seed.node_id))
        .collect::<Vec<_>>();
    let unread_edges = counts
        .as_ref()
        .map(|_| unread_counts.iter().map(|count| count.total).sum::<usize>());
    let unread_proven = counts.as_ref().and_then(|_| {
        unread_counts
            .iter()
            .map(|count| count.proven)
            .sum::<Option<usize>>()
    });
    let mut edges_unread = unread_edges;
    let mut proven_edges_unread = unread_proven;
    let symbols_unread_with_dependents = counts.as_ref().map(|_| unread.len());
    let mut windows_at_limit = 0usize;
    let mut windows_cutting_proven = 0usize;
    let mut windows_widened = 0usize;
    // Spent in the order the nodes are read: the changed file's own node, then its symbols in
    // seed order, so the symbols the change touches, then those with proven dependents, widen
    // their reads before the rest.
    let mut widening_budget = RELATIONSHIP_IMPACT_WIDENING_BUDGET;
    let mut proven = Vec::new();
    let mut possible = Vec::new();
    for seed in &seeds {
        // A store with no graph support has no relationship evidence to offer, and an empty
        // list is the documented answer for that. Every other failure — a graph awaiting
        // `ok index`, a stale analysis fingerprint, a read error — is not "no dependents", and
        // the report is refused instead. Two surfaces (`ok impact`, MCP `impact_analysis`)
        // answered `proven_impact: []` from an index whose edges had been discarded on open
        // before this propagated.
        let read = match inbound_impact_edges(
            graph,
            &seed.node_id,
            inbound_counts(&seed.node_id).as_ref(),
            &files_by_path,
            &policy,
            &mut widening_budget,
        ) {
            Ok(read) => read,
            Err(OkError::Unsupported(_)) => continue,
            Err(err) => return Err(err),
        };
        windows_at_limit += read.windows_at_limit;
        windows_cutting_proven += read.windows_cutting_proven;
        windows_widened += read.windows_widened;
        // A read that could not count what it cut leaves the total unknown.
        edges_unread = edges_unread
            .zip(read.edges_cut)
            .map(|(total, cut)| total + cut);
        proven_edges_unread = proven_edges_unread
            .zip(read.proven_edges_cut)
            .map(|(total, cut)| total + cut);
        let nodes_by_id = read
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node))
            .collect::<HashMap<NodeId, &GraphNode>>();
        for edge in &read.edges {
            if edge.to != seed.node_id || !is_impacted_by_edge_type(&edge.edge_type) {
                continue;
            }
            let Some(impact) =
                relationship_impact_entry(edge, &nodes_by_id, &files_by_id, &seed.label)
            else {
                continue;
            };
            let entry = (impact, seed.touched, seed.position);
            match policy.classify(edge) {
                RelationshipUseClass::Proven => proven.push(entry),
                RelationshipUseClass::Possible => possible.push(entry),
                RelationshipUseClass::Excluded => {}
            }
        }
    }

    let mut caveats = Vec::new();
    match symbols_unread_with_dependents {
        Some(skipped) if skipped > 0 => caveats.push(format!(
            "relationship impact read the dependents of {symbols_read} of the changed file's \
             {} symbols that have any ({symbols_total} symbols in all), taking the symbols the \
             change touches first, then those with a proven dependent, then public ones, then \
             those with the most inbound edges; the {skipped} others have {} inbound edge(s){} \
             that were not read",
            symbols_read + skipped,
            unread_edges.unwrap_or_default(),
            match unread_proven {
                Some(proven) => format!(", {proven} of them proven,"),
                None => String::new(),
            }
        )),
        None if !unread.is_empty() => caveats.push(format!(
            "relationship impact read the dependents of {symbols_read} of the changed file's \
             {symbols_total} symbols; this graph store cannot count edges, so the other {} may \
             have dependents that were not read",
            unread.len()
        )),
        _ => {}
    }

    // One entry per dependent, edge type and list. Of two entries for the same dependent through
    // different changed symbols, the one from a symbol the change touches is kept, then the one
    // from the symbol earliest in the file, so the kept entry does not depend on read order.
    for list in [&mut proven, &mut possible] {
        list.sort_by(|(a, a_touched, a_position), (b, b_touched, b_position)| {
            (&a.path, &a.symbol, &a.edge_type, !a_touched, a_position).cmp(&(
                &b.path,
                &b.symbol,
                &b.edge_type,
                !b_touched,
                b_position,
            ))
        });
        list.dedup_by(|(a, _, _), (b, _, _)| {
            a.path == b.path && a.symbol == b.symbol && a.edge_type == b.edge_type
        });
    }
    // Both lists are cut by one rule, and say how many they cut. Dependents of a symbol the
    // change touches come first, and a proven one is cut only past a far higher bound: it is the
    // dependent a change most certainly breaks, and no ordering of a capped list keeps every one
    // a list cut by path kept before. Among the rest, a dependent file is what the blast radius
    // is about: possibilities far outnumber proofs where name matches reach real symbols (cut by
    // path, 502 of the 835 possible entries listed for 43 files of this repository were inside
    // the changed file), and one dependent file can name the change from dozens of symbols. So
    // each other file's first entry is kept before any file's second, in path order, and the
    // changed file's own entries go last.
    let capped = cap_relationship_impacts(proven, target_file, true);
    let (mut proven, proven_omitted, proven_omitted_files) =
        (capped.kept, capped.omitted, capped.omitted_files);
    if capped.touched_omitted > 0 {
        caveats.push(format!(
            "proven_impact lists {RELATIONSHIP_IMPACT_TOUCHED_LIMIT} of the {} proven \
             dependents read of the symbols the change touches",
            RELATIONSHIP_IMPACT_TOUCHED_LIMIT + capped.touched_omitted
        ));
    }
    // Proven entries are listed by path, as they always were; the cut only chose which.
    proven.sort_by(|a, b| {
        (&a.path, &a.symbol, &a.edge_type).cmp(&(&b.path, &b.symbol, &b.edge_type))
    });
    // A dependent the report lists as proven, through any changed symbol and any edge type, is
    // not also a possibility: the weaker entry would only repeat the dependent, and take a slot
    // in the capped list from one that is not proven at all. Compared after the cut, so an entry
    // is removed only for a proven entry the report actually lists.
    let proven_dependents = proven
        .iter()
        .map(|impact| (&impact.path, &impact.symbol))
        .collect::<HashSet<_>>();
    possible.retain(|(impact, _, _)| !proven_dependents.contains(&(&impact.path, &impact.symbol)));
    let capped = cap_relationship_impacts(possible, target_file, false);
    let (possible, possible_omitted, possible_omitted_files) =
        (capped.kept, capped.omitted, capped.omitted_files);
    // A read that stopped before every proven edge left proven dependents out, which no count
    // shows. One that stopped after them left out possibilities only: where `possible_impact`
    // is already cut, its omitted count reads as "at least", and `edges_unread` says by how much,
    // so a sentence here would only repeat them; where nothing was cut, the list would read as
    // complete, so the caveat says it is not.
    if windows_cutting_proven > 0 {
        caveats.push(format!(
            "{windows_at_limit} inbound edge read(s) stopped at their limit for one edge type of \
             one node ({RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT} edges, widened to take up to \
             {RELATIONSHIP_IMPACT_PROVEN_WINDOW_LIMIT} counted proven edges, \
             {RELATIONSHIP_IMPACT_WIDENING_BUDGET} more in all), {windows_cutting_proven} of them \
             possibly before every proven edge was read{}, so proven dependents through them may \
             be missing",
            match proven_edges_unread {
                Some(unread) => format!(" ({unread} proven edge(s) in all were not read)"),
                None => String::new(),
            }
        ));
    } else if windows_at_limit > 0 && possible_omitted == 0 {
        caveats.push(format!(
            "{windows_at_limit} inbound edge read(s) stopped at their limit for one edge type of \
             one node after every proven edge, so further possible (heuristic) dependents \
             through them were not read"
        ));
    }
    Ok(RelationshipImpacts {
        proven,
        proven_omitted,
        proven_omitted_files,
        possible,
        possible_omitted,
        possible_omitted_files,
        caveats,
        reads: Some(RelationshipImpactReads {
            symbols_total,
            symbols_touched,
            symbols_read,
            symbols_unread_with_dependents,
            edges_unread,
            proven_edges_unread,
            windows_at_limit,
            windows_cutting_proven,
            windows_widened,
        }),
    })
}

/// A relationship list after [`cap_relationship_impacts`].
struct CappedImpacts {
    kept: Vec<RelationshipImpact>,
    /// Entries cut, of every kind.
    omitted: usize,
    /// Of those, entries from a seed the change touches.
    touched_omitted: usize,
    /// Dependent files an entry was cut from that the kept list does not name at all.
    omitted_files: usize,
}

/// Cut a path-ordered relationship list at [`RELATIONSHIP_IMPACT_LIMIT`]: entries from a seed the
/// change touches first, then each other file's first entry before any file's second, the changed
/// file's own last. With `keep_touched`, entries from a touched seed are kept up to
/// [`RELATIONSHIP_IMPACT_TOUCHED_LIMIT`], and [`RELATIONSHIP_IMPACT_LIMIT`] bounds the rest.
fn cap_relationship_impacts(
    entries: Vec<(RelationshipImpact, bool, usize)>,
    target_file: &File,
    keep_touched: bool,
) -> CappedImpacts {
    let mut seen_per_path = HashMap::<PathBuf, usize>::new();
    let mut keyed = entries
        .into_iter()
        .map(|(impact, touched, _)| {
            let nth = seen_per_path.entry(impact.path.clone()).or_default();
            *nth += 1;
            ((!touched, impact.path == target_file.path, *nth), impact)
        })
        .collect::<Vec<_>>();
    keyed.sort_by_key(|(key, _)| *key);
    let touched = keyed
        .iter()
        .filter(|((untouched, _, _), _)| !untouched)
        .count();
    let kept = if keep_touched {
        touched.clamp(RELATIONSHIP_IMPACT_LIMIT, RELATIONSHIP_IMPACT_TOUCHED_LIMIT)
    } else {
        RELATIONSHIP_IMPACT_LIMIT
    };
    let omitted = keyed.len().saturating_sub(kept);
    let touched_omitted = touched.saturating_sub(kept);
    let cut = keyed.split_off(kept.min(keyed.len()));
    let kept_files = keyed
        .iter()
        .map(|(_, impact)| &impact.path)
        .collect::<HashSet<_>>();
    let omitted_files = cut
        .iter()
        .map(|(_, impact)| &impact.path)
        // The changed file is no dependent of itself: its own entries go last, and cutting them
        // leaves no dependent file unnamed.
        .filter(|path| !kept_files.contains(path) && **path != target_file.path)
        .collect::<HashSet<_>>()
        .len();
    CappedImpacts {
        kept: keyed.into_iter().map(|(_, impact)| impact).collect(),
        omitted,
        touched_omitted,
        omitted_files,
    }
}

/// One seed's inbound impact edges, with the nodes they come from, and what the per-type reads
/// left unread.
struct InboundImpactEdges {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
    /// Reads that held more edges than their window.
    windows_at_limit: usize,
    /// Of those, reads whose first unread edge is proven, or that cannot tell.
    windows_cutting_proven: usize,
    /// Reads widened past [`RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT`] to take counted proven edges.
    windows_widened: usize,
    /// Edges past the windows, when the store counted them.
    edges_cut: Option<usize>,
    /// Proven edges past the windows, when the store counted them.
    proven_edges_cut: Option<usize>,
}

/// The edges into `node_id` that can carry impact, read by type, with the nodes they come from.
///
/// An untyped `neighbors` window around a file node is spent on the file's own outgoing
/// `DEFINES` edges, one per symbol, and in window order those rank with proven edges, ahead of
/// the heuristic inbound edges `possible_impact` is made of: a file with more symbols than the
/// window would report no possible impact at all. (Before window ordering the same window was
/// cut by edge id, and files with 61-65 symbols measured on a real repository reported no derived
/// impact while an 8-symbol file did.) Each impacted type is read inbound and filtered in SQL, so
/// the cap applies per type, in window order, to edges that can actually be impacts.
///
/// `counts`, when the store gave them, skips a type with no edge into the node and says how many
/// edges, and how many proven ones, a full window left out: windows keep proven edges first, so a
/// window of `n` proven edges past its limit cut `n` minus the limit of them. Each read asks for
/// one edge past its window, so a window that is exactly full is not reported as cut, and the
/// first edge left out says whether a proven one was. Where the counts show more proven edges
/// than the window holds, the window is widened to take them (see [`proven_window`]), so a hub
/// symbol's proven dependents are read rather than counted as unread.
///
/// A store without typed reads falls back to the untyped window, which cannot say what it cut:
/// a full one is counted as possibly cutting a proven edge.
fn inbound_impact_edges(
    graph: &dyn GraphStore,
    node_id: &NodeId,
    counts: Option<&BTreeMap<GraphEdgeType, EdgeCount>>,
    files_by_path: &HashMap<String, FileId>,
    policy: &RelationshipUsePolicy,
    widening_budget: &mut usize,
) -> Result<InboundImpactEdges> {
    debug_assert!(IMPACT_EDGE_TYPES.iter().all(is_impacted_by_edge_type));
    let mut edges = Vec::new();
    let mut windows_at_limit = 0;
    let mut windows_cutting_proven = 0;
    let mut windows_widened = 0;
    let mut edges_cut = counts.map(|_| 0usize);
    let mut proven_edges_cut = counts.map(|_| 0usize);
    for edge_type in impact_edge_types_for(node_id) {
        let counted = counts.map(|counts| counts.get(&edge_type).copied().unwrap_or_default());
        if counted.is_some_and(|count| count.total == 0) {
            continue;
        }
        let window = proven_window(
            counted.and_then(|count| count.proven).unwrap_or(0),
            widening_budget,
        );
        windows_widened += usize::from(window > RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT);
        match graph.edges_by_type_for_node(edge_type, &node_id.0, false, window + 1, 0) {
            Ok(mut batch) => {
                if batch.len() > window {
                    windows_at_limit += 1;
                    let first_cut = &batch[window];
                    windows_cutting_proven +=
                        usize::from(policy.classify(first_cut) == RelationshipUseClass::Proven);
                    batch.truncate(window);
                }
                if let (Some(cut), Some(counted)) = (edges_cut.as_mut(), counted) {
                    *cut += counted.total.saturating_sub(batch.len());
                }
                proven_edges_cut = match (proven_edges_cut, counted.and_then(|count| count.proven))
                {
                    (Some(cut), Some(proven)) => Some(cut + proven.saturating_sub(window)),
                    _ => None,
                };
                edges.extend(batch);
            }
            Err(OkError::Unsupported(_)) => {
                // The untyped window is not widened, so what this read took from the budget is
                // left for others.
                *widening_budget += window - RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT;
                let (nodes, edges) =
                    graph.neighbors(&node_id.0, RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT)?;
                let windows_at_limit =
                    usize::from(edges.len() >= RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT);
                return Ok(InboundImpactEdges {
                    nodes,
                    edges,
                    windows_at_limit,
                    windows_cutting_proven: windows_at_limit,
                    windows_widened: 0,
                    edges_cut: None,
                    proven_edges_cut: None,
                });
            }
            Err(err) => return Err(err),
        }
    }
    let sources = edges
        .iter()
        .map(|edge| edge.from.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut nodes = Vec::with_capacity(sources.len());
    for source in sources {
        // A file endpoint is rebuilt from the indexed files, as derived edges always were; a
        // symbol endpoint is read from the graph.
        let node = match file_node_for_id(&source, files_by_path) {
            Some(node) => Some(node),
            None => match graph.node_by_id(&source.0) {
                Ok(node) => node,
                Err(OkError::Unsupported(_)) => None,
                Err(err) => return Err(err),
            },
        };
        if let Some(node) = node {
            nodes.push(node);
        }
    }
    Ok(InboundImpactEdges {
        nodes,
        edges,
        windows_at_limit,
        windows_cutting_proven,
        windows_widened,
        edges_cut,
        proven_edges_cut,
    })
}

/// The window of one read that the store counted `proven` proven edges in: the usual
/// [`RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT`], or, where that would leave proven edges unread, every
/// proven edge up to [`RELATIONSHIP_IMPACT_PROVEN_WINDOW_LIMIT`], as far as `budget` (the edges
/// past the usual limit that the report's reads may still take) allows. Windows keep proven edges
/// first, so a window of `n` proven edges reads exactly those.
fn proven_window(proven: usize, budget: &mut usize) -> usize {
    if proven <= RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT {
        return RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT;
    }
    let widened = proven
        .min(RELATIONSHIP_IMPACT_PROVEN_WINDOW_LIMIT)
        .min(RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT + *budget);
    *budget -= widened - RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT;
    widened
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
        // With no proof to name, the pass that inferred the edge is what a reader can weigh it
        // by: a symbol-registry name match reads differently from a parsed occurrence.
        format!("no structural proof; inferred by {}", edge.evidence.source)
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

/// `match_reason` prefix of a result produced from an indexed symbol occurrence. Prose for
/// readers only: exactness is read from `SearchResult::exact_reference_provenance`.
const EXACT_REFERENCE_MATCH_REASON_PREFIX: &str = "exact symbol reference via ";

/// Direct impacts one report lists. The rest are counted in `ImpactReport::direct_impacts_omitted`.
const MAX_DIRECT_IMPACTS: usize = 25;

/// Indirect impacts one report lists. The rest are counted in `ImpactReport::indirect_impacts_omitted`.
const MAX_INDIRECT_IMPACTS: usize = 15;

/// Further chunks of one path whose evidence lines a grouped direct impact lists. The rest are
/// named by line range on one summary line: a symbol referenced a hundred times in one file
/// would otherwise put a hundred lines in one entry.
const MAX_LISTED_GROUPED_CHUNKS: usize = 4;

/// `match_reason` of a result produced from a local git co-change fact.
const GIT_COCHANGE_MATCH_REASON: &str = "historical git co-change with target file";

/// The edge a direct impact was reached through. Grouping is per path per kind: several
/// chunks of one file found the same way are one impact, while an exact reference and a
/// lexical hit on that file stay separate entries so neither hides the other's authority.
///
/// The derived order is the ranking's authority order: `compare_impact_results` uses it to
/// break an equal score, so the capped lists keep the earlier kind. Reordering the variants or
/// inserting one changes which impacts a report keeps; the test pinning this order must change
/// with it, deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DirectImpactKind {
    ExactReference,
    CrateImport,
    CrateImportUse,
    CoChange,
    Runtime,
    ServiceBoundary,
    Lexical,
}

/// Which tier an impact ranks in before its score is read. Only an exact reference is
/// repository truth. A crate import is a `use` row written through a crate name that the import
/// resolver resolved to the changed file, following the dependency the importer's package
/// declares: a proven file-level import, not a reference to one of the file's symbols, so it ranks
/// below exact references and above everything statistical or lexical, with the name uses in the
/// importing packages it attributes. Co-change is statistical
/// history, and runtime and service-boundary
/// impacts are lexical hits corroborated by a runtime fact or a matching static route or
/// channel string: evidence that a dependency is likely, not that one exists. All four rank
/// together, by score.
fn impact_authority_tier(result: &SearchResult) -> u8 {
    match direct_impact_kind(result) {
        DirectImpactKind::ExactReference => 0,
        DirectImpactKind::CrateImport | DirectImpactKind::CrateImportUse => 1,
        _ => 2,
    }
}

fn direct_impact_kind(result: &SearchResult) -> DirectImpactKind {
    let has_signal = |signal: &str| {
        result
            .score_breakdown
            .iter()
            .any(|component| component.signal == signal)
    };
    if result.is_exact_reference() {
        DirectImpactKind::ExactReference
    } else if has_signal(CRATE_IMPORT_SIGNAL) {
        DirectImpactKind::CrateImport
    } else if has_signal(CRATE_IMPORT_USE_SIGNAL) {
        DirectImpactKind::CrateImportUse
    } else if result.match_reason == GIT_COCHANGE_MATCH_REASON {
        DirectImpactKind::CoChange
    } else if has_signal("runtime_corroboration") {
        DirectImpactKind::Runtime
    } else if has_signal("service_boundary") {
        DirectImpactKind::ServiceBoundary
    } else {
        DirectImpactKind::Lexical
    }
}

/// One entry per (path, edge kind). The best-scoring chunk is the representative; every other
/// chunk's evidence lines are kept under their own ids, prefixed with the chunk's line range
/// and symbol, so the ranges stay visible as evidence while the list bounds files, not chunks.
fn group_direct_impacts(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut groups = BTreeMap::<(std::path::PathBuf, DirectImpactKind), Vec<SearchResult>>::new();
    for result in results {
        groups
            .entry((result.path.clone(), direct_impact_kind(&result)))
            .or_default()
            .push(result);
    }
    groups
        .into_values()
        .filter_map(merge_direct_group)
        .collect()
}

fn merge_direct_group(mut chunks: Vec<SearchResult>) -> Option<SearchResult> {
    let line_start = |result: &SearchResult| result.line_range.as_ref().map(|range| range.start);
    chunks.sort_by(compare_impact_results);
    let mut chunks = chunks.into_iter();
    let mut representative = chunks.next()?;
    let mut others = chunks.collect::<Vec<_>>();
    if others.is_empty() {
        return Some(representative);
    }
    // Only the first few are listed, so this order decides which chunks are named. It falls
    // back to the full ranking order rather than to the order `chunks` happened to be in: that
    // is sorted above today, but the listed subset must not depend on a sort elsewhere.
    others.sort_by(|left, right| {
        line_start(left)
            .cmp(&line_start(right))
            .then_with(|| left.evidence_refs.cmp(&right.evidence_refs))
            .then_with(|| compare_impact_results(left, right))
    });
    let mut evidence_refs = aligned_evidence_refs(&representative);
    let mut evidence = std::mem::take(&mut representative.evidence);
    let mut unlisted = Vec::new();
    for (index, other) in others.into_iter().enumerate() {
        representative.confidence = representative.confidence.max(other.confidence);
        let location = chunk_location(&other);
        if index >= MAX_LISTED_GROUPED_CHUNKS {
            unlisted.push(location);
            continue;
        }
        let listed_lines = evidence.len();
        for (message, id) in other.evidence.iter().zip(aligned_evidence_refs(&other)) {
            if evidence_refs.contains(&id) {
                continue;
            }
            evidence.push(format!("{location}: {message}"));
            evidence_refs.push(id);
        }
        // Every line of this chunk is already cited under the same id. Without naming it here
        // its range would appear in neither the listed lines nor the summary.
        if evidence.len() == listed_lines {
            unlisted.push(location);
        }
    }
    if !unlisted.is_empty() {
        let summary_id = unused_line_id(
            &representative.path,
            &representative.line_range,
            &evidence_refs,
        );
        evidence.push(format!(
            "{} more matching ranges on this path, evidence not listed: {}",
            unlisted.len(),
            unlisted.join("; ")
        ));
        evidence_refs.push(summary_id);
    }
    representative.evidence = evidence;
    representative.evidence_refs = evidence_refs;
    Some(representative)
}

/// One id per evidence line. Refs that are index-aligned with the lines are kept; otherwise
/// every line takes its derived `search:` id, as the context pack's primary evidence does.
/// Pairing refs with lines by position when the counts differ gave a runtime line from one
/// fact a lexical id, and left that fact uncited.
fn aligned_evidence_refs(result: &SearchResult) -> Vec<String> {
    let refs = result.derived_evidence_ids();
    if refs.len() == result.evidence.len() {
        return refs;
    }
    search_result_evidence_ids(&result.path, &result.line_range, result.evidence.len())
        .into_iter()
        .take(result.evidence.len())
        .collect()
}

/// The first derived `search:` id for this path and range that `cited` does not already hold,
/// for a line whose own id another line carries.
fn unused_line_id(
    path: &Path,
    line_range: &Option<open_kioku_core::LineRange>,
    cited: &[String],
) -> String {
    let mut line_count = cited.len() + 1;
    loop {
        if let Some(id) = search_result_evidence_ids(path, line_range, line_count).pop() {
            if !cited.contains(&id) {
                return id;
            }
        }
        line_count += 1;
    }
}

fn chunk_location(result: &SearchResult) -> String {
    let lines = result
        .line_range
        .as_ref()
        .map(|range| format!("lines {}-{}", range.start, range.end))
        .unwrap_or_else(|| "file".into());
    match &result.symbol {
        Some(symbol) => format!("{lines} in `{}`", symbol.qualified_name),
        None => lines,
    }
}

/// Reader-facing name of an exact reference source; `None` for every source that is not one.
fn exact_reference_label(source: &EvidenceSourceType) -> Option<&'static str> {
    match source {
        EvidenceSourceType::Scip => Some("SCIP"),
        EvidenceSourceType::TreeSitter => Some("tree-sitter"),
        EvidenceSourceType::Lsp => Some("LSP"),
        _ => None,
    }
}

/// Which source a merged result or summary record names when exact references from several
/// sources meet: index data from a compiler or language server before tree-sitter occurrences.
fn exact_reference_authority(source: &EvidenceSourceType) -> u8 {
    match source {
        EvidenceSourceType::Scip => 3,
        EvidenceSourceType::Lsp => 2,
        EvidenceSourceType::TreeSitter => 1,
        _ => 0,
    }
}

/// Distinct exact-reference sources among `results`, strongest first.
fn exact_reference_sources_by_authority(results: &[SearchResult]) -> Vec<EvidenceSourceType> {
    let mut sources = Vec::<EvidenceSourceType>::new();
    for source in results
        .iter()
        .filter_map(SearchResult::exact_reference_source)
    {
        if !sources.contains(source) {
            sources.push(source.clone());
        }
    }
    sources.sort_by_key(|source| std::cmp::Reverse(exact_reference_authority(source)));
    sources
}

fn stronger_exact_provenance(
    current: Option<EvidenceSourceType>,
    other: Option<EvidenceSourceType>,
) -> Option<EvidenceSourceType> {
    current
        .into_iter()
        .chain(other)
        .max_by_key(exact_reference_authority)
}

/// Whether a direct impact was reached only through a name match: lexical search, or an
/// identifier in a package that imports the name from the changed file. Not an exact reference,
/// a crate import row, a co-change, a runtime fact or a service-boundary fact. Consumers bound
/// how many of these widen an edit boundary; the structural kinds are admitted without that
/// bound.
pub fn is_lexical_impact_result(result: &SearchResult) -> bool {
    matches!(
        direct_impact_kind(result),
        DirectImpactKind::Lexical | DirectImpactKind::CrateImportUse
    )
}

/// Which Rust files a change to one package can reach through Cargo: that package and the
/// packages that depend on it. A Rust file in another package, or in none, has no Cargo
/// dependency path to the change, so a lexical match there is read as a shared word rather than
/// a dependent. A procedural-macro package is never pruned: the code its macros emit belongs to
/// the crates that use them, whatever direction the dependency runs. Coupling Cargo does not see
/// (a harness that runs a built binary and parses its output) is not followed; the pruned count
/// and a sample of paths stay in the risk reasons.
struct RustReachability<'a> {
    workspace: &'a CargoWorkspace,
    package: usize,
    reachable: std::collections::BTreeSet<usize>,
}

impl RustReachability<'_> {
    /// False only when the manifests prove the path unreachable; an unknown membership and any
    /// file that is not Rust source keep their match.
    fn may_depend(&self, path: &Path) -> bool {
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            return true;
        }
        match self.workspace.membership(path) {
            Membership::Package(package) => {
                self.reachable.contains(&package) || self.workspace.is_proc_macro(package)
            }
            Membership::Outside => false,
            Membership::Unknown => true,
        }
    }
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
    // A lexical or heuristic occurrence is not an exact reference, and it used to be counted
    // as one "via indexed". The lexical searches over the target's symbol names still reach its
    // file, without exact authority.
    let Some(source) = exact_reference_label(&occurrence.provenance) else {
        return Ok(None);
    };
    let chunks = store.chunks_for_file(&occurrence.file_id)?;
    let snippet = best_occurrence_snippet(&chunks, occurrence, &symbol.name);
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
        exact_reference_provenance: Some(occurrence.provenance.clone()),
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

/// Keeps the first `cap` of an already ranked list and returns how many it cut, so the report
/// can count what it does not list.
fn cap_impacts(results: &mut Vec<SearchResult>, cap: usize) -> usize {
    let omitted = results.len().saturating_sub(cap);
    results.truncate(cap);
    omitted
}

/// Authority first, then descending score, then the edge kind, then repository position: path,
/// line range, and the evidence ids the result was published with.
///
/// Impacts are truncated after this sort, and consumers take prefixes of it (indirect impacts
/// are seeded from the first five), so the order itself must carry "exact facts outrank
/// heuristics". Scores cannot: an exact reference scores `1.25 + occurrence confidence` while a
/// lexical hit carries raw BM25 plus boosts, commonly above 5, so ordering by score first let
/// keyword matches fill the 25-entry cap and cut proven references. An exact reference is
/// therefore never ranked below a heuristic entry, and within a tier the score decides.
/// `DirectImpactKind` breaks an equal score; position stays last so the order is total.
fn compare_impact_results(left: &SearchResult, right: &SearchResult) -> std::cmp::Ordering {
    let bounds = |result: &SearchResult| {
        result
            .line_range
            .as_ref()
            .map(|range| (range.start, range.end))
    };
    impact_authority_tier(left)
        .cmp(&impact_authority_tier(right))
        .then_with(|| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .then_with(|| direct_impact_kind(left).cmp(&direct_impact_kind(right)))
        .then_with(|| left.path.cmp(&right.path))
        .then_with(|| bounds(left).cmp(&bounds(right)))
        .then_with(|| left.evidence_refs.cmp(&right.evidence_refs))
}

fn dedupe_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut by_path = BTreeMap::<String, SearchResult>::new();
    for result in results {
        let key = result_key(&result);
        match by_path.get_mut(&key) {
            Some(existing) => merge_duplicate(existing, result),
            None => {
                by_path.insert(key, result);
            }
        }
    }
    by_path.into_values().collect()
}

/// Folds a result on the same path and range into `existing`. The higher score supplies the
/// score and prose, but exact-reference provenance is kept from whichever duplicate carries
/// it: a lexical hit outscoring an exact reference used to erase it. Each evidence line keeps
/// the id it was published with, so a runtime line from a second fact cites that fact.
fn merge_duplicate(existing: &mut SearchResult, duplicate: SearchResult) {
    let mut evidence_refs = aligned_evidence_refs(existing);
    let duplicate_refs = aligned_evidence_refs(&duplicate);
    let duplicate_line_ids = search_result_evidence_ids(
        &duplicate.path,
        &duplicate.line_range,
        duplicate.evidence.len(),
    );
    existing.exact_reference_provenance = stronger_exact_provenance(
        existing.exact_reference_provenance.take(),
        duplicate.exact_reference_provenance,
    );
    if duplicate.score > existing.score {
        existing.score = duplicate.score;
        existing.snippet = duplicate.snippet;
        existing.line_range = duplicate.line_range;
        existing.match_reason = duplicate.match_reason;
        existing.confidence = existing.confidence.max(duplicate.confidence);
        existing.score_breakdown = duplicate.score_breakdown;
    }
    for ((message, id), line_id) in duplicate
        .evidence
        .into_iter()
        .zip(duplicate_refs)
        .zip(duplicate_line_ids)
    {
        let cited = evidence_refs.contains(&id);
        // The same line under its positional id, or under an id already cited, is the same
        // evidence. The same line under another fact's id is that fact's, and stays.
        if existing.evidence.contains(&message) && (cited || id == line_id) {
            continue;
        }
        let id = if cited {
            unused_line_id(&existing.path, &existing.line_range, &evidence_refs)
        } else {
            id
        };
        existing.evidence.push(message);
        evidence_refs.push(id);
    }
    existing.evidence_refs = evidence_refs;
    existing.reconcile_score_breakdown();
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

/// The file's names searched for lexical dependents, longest first; only the first
/// [`MAX_IMPACT_TERMS`] are searched.
///
/// Test code is left out: nothing outside a file depends on its tests, and with names ranked
/// by length, long `#[test]` names and `#[cfg(test)]` helpers took the searched slots, so adding
/// a test reshuffled the file's direct impacts. Which names are test code is read from the
/// index's test targets, see [`TestScope`]. A file whose only non-generic names are tests (a
/// test-path file, or a source file of nothing but tests) keeps them, since they are then the
/// only names it has.
fn impact_terms(
    path: &Path,
    file: &open_kioku_core::File,
    symbols: &[open_kioku_core::Symbol],
    file_tests: &[open_kioku_core::TestTarget],
) -> Vec<String> {
    let file_symbols = symbols
        .iter()
        .filter(|symbol| symbol.file_id == file.id)
        .collect::<Vec<_>>();
    // Built before generic names are dropped: `mod tests` is one.
    let test_scope = TestScope::new(&file_symbols, file_tests);
    let test_file = open_kioku_core::is_test_code_path(&file.path.to_string_lossy());
    let symbols = file_symbols
        .into_iter()
        .filter(|symbol| !is_generic_symbol_name(&symbol.name))
        .collect::<Vec<_>>();
    let production = symbols
        .iter()
        .copied()
        .filter(|symbol| !test_file && !test_scope.contains(symbol))
        .collect::<Vec<_>>();
    let selected = if production.is_empty() {
        symbols
    } else {
        production
    };

    let mut terms = selected
        .iter()
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

/// A file's test code as the index recorded it.
///
/// A symbol is test code when it is one of the file's test targets, matched by name and an
/// overlapping line range (a SCIP definition covers only the name's line, a tree-sitter one the
/// whole item), or when it lies in the innermost Rust inline module enclosing such a target: the
/// `cfg(test)` attribute is not indexed, and this is how a `mod tests` helper is recognised. Only
/// the innermost module, so `pub mod client { pub fn open() {} #[cfg(test)] mod tests { .. } }`
/// keeps `client` and `open`.
///
/// A public symbol matched as a test only by its name or an annotation outside a test path
/// (`TestTargetOrigin::Symbol`) is production API that happens to start with `test`, such as
/// `pub fn test_connection_health`: a `#[test]` function is never public, and a public JUnit
/// method lives in a test path, where its origin is `TestFileSymbol`. It stays a search term.
struct TestScope<'a> {
    targets: Vec<(&'a str, Option<&'a open_kioku_core::LineRange>)>,
    modules: Vec<&'a open_kioku_core::LineRange>,
}

impl<'a> TestScope<'a> {
    fn new(
        file_symbols: &[&'a open_kioku_core::Symbol],
        file_tests: &'a [open_kioku_core::TestTarget],
    ) -> Self {
        let targets = file_tests
            .iter()
            .filter(|test| {
                test.origin != open_kioku_core::TestTargetOrigin::Symbol
                    || !file_symbols.iter().any(|symbol| {
                        symbol.visibility == open_kioku_core::Visibility::Public
                            && names_same_item(symbol, test.name.as_str(), test.range.as_ref())
                    })
            })
            .map(|test| (test.name.as_str(), test.range.as_ref()))
            .collect::<Vec<_>>();
        let rust_modules = file_symbols
            .iter()
            .filter(|symbol| {
                symbol.kind == open_kioku_core::SymbolKind::Module
                    && symbol.language == open_kioku_core::Language::Rust
            })
            .filter_map(|module| module.range.as_ref())
            .collect::<Vec<_>>();
        let mut modules = Vec::<&open_kioku_core::LineRange>::new();
        for test_range in targets.iter().filter_map(|(_, range)| *range) {
            let innermost = rust_modules
                .iter()
                .copied()
                .filter(|module_range| line_range_contains(module_range, test_range))
                .min_by_key(|module_range| {
                    (module_range.end - module_range.start, module_range.start)
                });
            if let Some(module_range) = innermost {
                if !modules.contains(&module_range) {
                    modules.push(module_range);
                }
            }
        }
        Self { targets, modules }
    }

    fn contains(&self, symbol: &open_kioku_core::Symbol) -> bool {
        self.targets
            .iter()
            .any(|(name, range)| names_same_item(symbol, name, *range))
            || symbol.range.as_ref().is_some_and(|range| {
                self.modules
                    .iter()
                    .any(|module_range| line_range_contains(module_range, range))
            })
    }
}

/// Whether `symbol` is the item a test target names: the same name, and line ranges that
/// overlap when both are known.
fn names_same_item(
    symbol: &open_kioku_core::Symbol,
    name: &str,
    range: Option<&open_kioku_core::LineRange>,
) -> bool {
    symbol.name == name
        && match (symbol.range.as_ref(), range) {
            (Some(left), Some(right)) => left.start <= right.end && right.start <= left.end,
            _ => true,
        }
}

fn line_range_contains(
    outer: &open_kioku_core::LineRange,
    inner: &open_kioku_core::LineRange,
) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
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
            snapshot: None,
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

    fn chunk_hit(path: &str, start: u32, score: f32, message: &str) -> SearchResult {
        let line_range = Some(LineRange {
            start,
            end: start + 2,
        });
        SearchResult {
            path: PathBuf::from(path),
            evidence_refs: search_result_evidence_ids(Path::new(path), &line_range, 1),
            line_range,
            snippet: message.into(),
            symbol: None,
            score,
            match_reason: "tantivy hybrid lexical match".into(),
            evidence: vec![message.into()],
            confidence: 0.5,
            score_breakdown: Vec::new(),
            exact_reference_provenance: None,
        }
    }

    #[test]
    fn impact_results_break_equal_scores_on_path_then_line_range() {
        let inputs = vec![
            chunk_hit("src/b.rs", 40, 0.5, "b later"),
            chunk_hit("src/c.rs", 1, 0.9, "c strongest"),
            chunk_hit("src/b.rs", 4, 0.5, "b earlier"),
            chunk_hit("src/a.rs", 90, 0.5, "a"),
        ];
        let mut reversed = inputs.clone();
        reversed.reverse();
        for mut results in [inputs, reversed] {
            results.sort_by(compare_impact_results);
            let order = results
                .iter()
                .map(|result| {
                    (
                        result.path.to_string_lossy().into_owned(),
                        result.line_range.as_ref().map(|range| range.start),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                order,
                vec![
                    ("src/c.rs".to_string(), Some(1)),
                    ("src/a.rs".to_string(), Some(90)),
                    ("src/b.rs".to_string(), Some(4)),
                    ("src/b.rs".to_string(), Some(40)),
                ]
            );
            // The report truncates after this sort, so the kept impacts must not depend on the
            // order the streams produced them in.
            results.truncate(2);
            assert_eq!(
                results
                    .iter()
                    .map(|result| result.path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                vec!["src/c.rs".to_string(), "src/a.rs".to_string()]
            );
        }
    }

    #[test]
    fn a_grouped_impact_keeps_one_representative_whatever_the_chunk_order() {
        let chunks = vec![
            chunk_hit("src/a.rs", 30, 0.4, "later chunk"),
            chunk_hit("src/a.rs", 5, 0.4, "earlier chunk"),
        ];
        let mut reversed = chunks.clone();
        reversed.reverse();
        for group in [chunks, reversed] {
            let merged = merge_direct_group(group).expect("group merges into one impact");
            assert_eq!(
                merged.line_range.as_ref().map(|range| range.start),
                Some(5),
                "the lowest-lined chunk of a tied group represents it"
            );
        }
    }

    #[test]
    fn chunk_hits_on_one_path_group_into_one_direct_impact_per_edge_kind() {
        let mut exact = chunk_hit("src/publisher.rs", 40, 2.0, "exact reference");
        exact.match_reason = format!("{EXACT_REFERENCE_MATCH_REASON_PREFIX}SCIP");
        exact.exact_reference_provenance = Some(EvidenceSourceType::Scip);
        let grouped = group_direct_impacts(vec![
            chunk_hit("src/publisher.rs", 20, 0.4, "matched `rate` at 20"),
            chunk_hit("src/publisher.rs", 1, 0.9, "matched `rate` at 1"),
            chunk_hit("src/publisher.rs", 10, 0.6, "matched `rate` at 10"),
            exact,
        ]);

        assert_eq!(
            grouped.len(),
            2,
            "one lexical and one exact-reference entry"
        );
        let lexical = grouped
            .iter()
            .find(|result| !result.is_exact_reference())
            .unwrap();
        assert_eq!(lexical.line_range, Some(LineRange { start: 1, end: 3 }));
        assert_eq!(
            lexical.evidence,
            vec![
                "matched `rate` at 1".to_string(),
                "lines 10-12: matched `rate` at 10".to_string(),
                "lines 20-22: matched `rate` at 20".to_string(),
            ]
        );
        assert_eq!(
            lexical.evidence_refs,
            vec![
                "search:src/publisher.rs:1-3:0".to_string(),
                "search:src/publisher.rs:10-12:0".to_string(),
                "search:src/publisher.rs:20-22:0".to_string(),
            ]
        );
        assert!(grouped.iter().any(SearchResult::is_exact_reference));
    }

    #[test]
    fn a_grouped_impact_lists_a_bounded_number_of_chunks_and_names_the_rest_by_range() {
        let chunks = (0..7u32)
            .map(|index| {
                chunk_hit(
                    "src/publisher.rs",
                    1 + index * 10,
                    1.0 - index as f32 * 0.1,
                    &format!("matched `rate` at {}", 1 + index * 10),
                )
            })
            .collect::<Vec<_>>();
        let grouped = group_direct_impacts(chunks);

        assert_eq!(grouped.len(), 1);
        let entry = &grouped[0];
        // The representative, four listed chunks, and one line naming the other two ranges.
        assert_eq!(entry.evidence.len(), 1 + MAX_LISTED_GROUPED_CHUNKS + 1);
        assert_eq!(entry.evidence.len(), entry.evidence_refs.len());
        assert_eq!(
            entry.evidence.last().unwrap(),
            "2 more matching ranges on this path, evidence not listed: lines 51-53; lines 61-63"
        );
        let unique = entry
            .evidence_refs
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), entry.evidence_refs.len());
    }

    fn exact_hit(path: &str, start: u32, source: EvidenceSourceType) -> SearchResult {
        let mut hit = chunk_hit(
            path,
            start,
            2.2,
            "exact reference to `rates::RateValidator` from `SCIP` occurrence data",
        );
        hit.match_reason = format!("{EXACT_REFERENCE_MATCH_REASON_PREFIX}SCIP");
        hit.exact_reference_provenance = Some(source);
        hit
    }

    #[test]
    fn a_lexical_duplicate_that_outscores_an_exact_reference_keeps_its_exact_provenance() {
        let exact = exact_hit("src/publisher.rs", 10, EvidenceSourceType::Scip);
        let lexical = chunk_hit(
            "src/publisher.rs",
            10,
            31.0,
            "query variant `RateValidator` matched local index",
        );
        for arrival in [vec![exact.clone(), lexical.clone()], vec![lexical, exact]] {
            let grouped = group_direct_impacts(dedupe_results(arrival));

            assert_eq!(grouped.len(), 1, "{grouped:#?}");
            let entry = &grouped[0];
            assert_eq!(entry.score, 31.0);
            assert_eq!(entry.match_reason, "tantivy hybrid lexical match");
            assert_eq!(
                entry.exact_reference_provenance,
                Some(EvidenceSourceType::Scip)
            );
            assert!(!is_lexical_impact_result(entry));
            assert_eq!(
                grouped
                    .iter()
                    .filter(|result| result.is_exact_reference())
                    .count(),
                1
            );
            assert_eq!(entry.evidence.len(), entry.evidence_refs.len());
            let unique = entry
                .evidence_refs
                .iter()
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(unique.len(), entry.evidence_refs.len());
        }
    }

    fn runtime_fact(id: &str, target: &str) -> AnalysisFact {
        AnalysisFact {
            id: id.into(),
            file_id: FileId::new("target"),
            symbol_id: None,
            target: target.into(),
            target_kind: GraphNodeType::File,
            target_symbol_id: None,
            ambiguity: Vec::new(),
            edge_type: GraphEdgeType::SimilarTo,
            range: None,
            confidence: Confidence::High,
            source: "traces/checkout.json".into(),
            source_type: EvidenceSourceType::Runtime,
            message: "observed endpoint".into(),
        }
    }

    #[test]
    fn deduplicated_results_with_different_runtime_annotations_cite_their_own_facts() {
        let mut orders = chunk_hit("src/checkout.rs", 10, 1.0, "matched `checkout` at 10");
        annotate_runtime_impact(&mut orders, &runtime_fact("runtime:orders", "/v1/orders"));
        let mut payments = chunk_hit("src/checkout.rs", 10, 0.8, "matched `checkout` at 10");
        annotate_runtime_impact(
            &mut payments,
            &runtime_fact("runtime:payments", "/v1/payments"),
        );

        let merged = dedupe_results(vec![orders, payments]);

        assert_eq!(merged.len(), 1);
        let merged = &merged[0];
        assert_eq!(merged.evidence.len(), merged.evidence_refs.len());
        let cited = |fragment: &str| {
            merged
                .evidence
                .iter()
                .zip(&merged.evidence_refs)
                .find(|(message, _)| message.contains(fragment))
                .map(|(_, id)| id.as_str())
        };
        assert_eq!(
            cited("matched `checkout`"),
            Some("search:src/checkout.rs:10-12:0")
        );
        assert_eq!(cited("`/v1/orders`"), Some("runtime:orders"));
        assert_eq!(cited("`/v1/payments`"), Some("runtime:payments"));
    }

    #[test]
    fn evidence_refs_that_do_not_align_with_lines_are_not_paired_by_position() {
        let mut result = chunk_hit("src/publisher.rs", 1, 0.9, "matched `rate` at 1");
        result
            .evidence
            .push("runtime corroboration from local artifact `a` targeting `b`".into());
        result
            .evidence
            .push("service-boundary evidence from `c` targeting `d`".into());
        result.evidence_refs.push("runtime:a".into());

        assert_eq!(
            aligned_evidence_refs(&result),
            vec![
                "search:src/publisher.rs:1-3:0".to_string(),
                "search:src/publisher.rs:1-3:1".to_string(),
                "search:src/publisher.rs:1-3:2".to_string(),
            ]
        );
    }

    #[test]
    fn a_grouped_chunk_whose_lines_are_all_already_cited_is_still_named() {
        let representative = chunk_hit("src/publisher.rs", 1, 0.9, "matched `rate` at 1");
        // Producers put the line range in every id, so this needs a ref two chunks share.
        let mut already_cited = chunk_hit("src/publisher.rs", 20, 0.5, "matched `rate` at 20");
        already_cited.evidence_refs = representative.evidence_refs.clone();

        let grouped = group_direct_impacts(vec![representative, already_cited]);

        assert_eq!(grouped.len(), 1);
        let entry = &grouped[0];
        assert_eq!(entry.evidence.len(), entry.evidence_refs.len());
        assert_eq!(
            entry.evidence.last().unwrap(),
            "1 more matching ranges on this path, evidence not listed: lines 20-22"
        );
    }

    /// A target defining `RateValidator` and a caller holding one reference to it, the
    /// reference occurrence carrying `provenance`.
    fn index_one_reference(provenance: EvidenceSourceType) -> SqliteStore {
        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let file = |id: &str, path: &str| File {
            id: FileId::new(id),
            repository_id: repo_id.clone(),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: id.into(),
            is_generated: false,
            is_vendor: false,
        };
        let source = file("source", "src/rates.rs");
        let caller = file("caller", "src/publisher.rs");
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
            alias_of: None,
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
        let occurrence = SymbolOccurrence {
            symbol_id: symbol.id.clone(),
            file_id: caller.id.clone(),
            range: Some(LineRange { start: 10, end: 10 }),
            source_range: None,
            is_definition: false,
            confidence: Confidence::High,
            provenance,
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
            symbol_count: 1,
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
            snapshot: None,
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[source, caller],
                symbols: &[symbol],
                occurrences: &[occurrence],
                chunks: &chunks,
                imports: &[],
                tests: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();
        store
    }

    fn impact_record(report: &ImpactReport) -> &Evidence {
        report
            .evidence
            .iter()
            .find(|item| item.id.0 == "impact:src/rates.rs")
            .expect("impact evidence record")
    }

    #[test]
    fn impact_evidence_takes_the_source_type_of_its_exact_references() {
        for source in [
            EvidenceSourceType::Scip,
            EvidenceSourceType::TreeSitter,
            EvidenceSourceType::Lsp,
        ] {
            let store = index_one_reference(source.clone());
            let report = ImpactEngine::new(&store)
                .for_file(Path::new("src/rates.rs"))
                .unwrap();

            let record = impact_record(&report);
            assert_eq!(record.source_type, source, "{record:?}");
            let reference = report
                .direct_impacts
                .iter()
                .find(|result| result.is_exact_reference())
                .expect("exact-reference direct impact");
            assert_eq!(reference.exact_reference_provenance.as_ref(), Some(&source));
        }
    }

    #[test]
    fn an_occurrence_from_a_non_exact_source_is_not_an_exact_reference() {
        let store = index_one_reference(EvidenceSourceType::Heuristic);
        let report = ImpactEngine::new(&store)
            .for_file(Path::new("src/rates.rs"))
            .unwrap();

        assert_eq!(
            impact_record(&report).source_type,
            EvidenceSourceType::Lexical
        );
        assert!(!report
            .direct_impacts
            .iter()
            .any(SearchResult::is_exact_reference));
        assert!(!report
            .risk_report
            .reasons
            .iter()
            .any(|reason| reason.contains("exact indexed symbol reference")));
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
            alias_of: None,
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
            // Two more call sites in the same caller file: three references, one impacted file.
            SymbolOccurrence {
                symbol_id: symbol.id.clone(),
                file_id: caller.id.clone(),
                range: Some(LineRange { start: 11, end: 11 }),
                source_range: None,
                is_definition: false,
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
            },
            SymbolOccurrence {
                symbol_id: symbol.id.clone(),
                file_id: caller.id.clone(),
                range: Some(LineRange { start: 12, end: 12 }),
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
            snapshot: None,
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
                && result.exact_reference_provenance == Some(EvidenceSourceType::Scip)));
        assert_eq!(
            report
                .direct_impacts
                .iter()
                .filter(|result| result.is_exact_reference())
                .count(),
            1,
            "three call sites in one file group into one exact-reference entry"
        );
        assert!(
            report
                .risk_report
                .reasons
                .iter()
                .any(|reason| reason == "3 exact indexed symbol reference(s) found in 1 file(s)"),
            "{:?}",
            report.risk_report.reasons
        );
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
            snapshot: None,
        };
        let history_fact = AnalysisFact {
            id: "history:source-neighbor".into(),
            file_id: source.id.clone(),
            symbol_id: None,
            target: historical_neighbor.path.display().to_string(),
            target_kind: GraphNodeType::File,
            target_symbol_id: None,
            ambiguity: Vec::new(),
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
            target_symbol_id: None,
            ambiguity: Vec::new(),
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
            snapshot: None,
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
            target_symbol_id: None,
            ambiguity: Vec::new(),
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
            snapshot: None,
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
            snapshot: None,
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
            alias_of: None,
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
        // A second symbol of the target file, which the proven caller also reaches through a
        // proof-less edge of another type: a second seed reaching the same dependent.
        let target_type = make_symbol("symbol:auth::Token", "Token", "auth::Token", &target_file);

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
            symbol_count: 4,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
            snapshot: None,
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
                    target_type.clone(),
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
            symbol_node(&target_type, &target_file),
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
        let repeated_edge = GraphEdge {
            id: open_kioku_core::EdgeId::new("edge:repeated"),
            from: identity::symbol_node_id(&caller_symbol),
            to: identity::symbol_node_id(&target_type),
            edge_type: GraphEdgeType::Calls,
            evidence: Evidence::default(),
            ..Default::default()
        };
        store
            .replace_graph(&nodes, &[proven_edge, heuristic_edge, repeated_edge])
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
        assert!(
            !report
                .possible_impact
                .iter()
                .any(|impact| impact.path == Path::new("src/session.rs")),
            "a dependent listed as proven is not repeated as a possibility: {:?}",
            report.possible_impact
        );
        // A proof-less entry names the pass that inferred it.
        assert!(report
            .possible_impact
            .iter()
            .all(|impact| impact.reason.contains("no structural proof; inferred by")));
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
            snapshot: None,
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

    /// A file with more parsed definitions than the relationship window still reports the
    /// heuristic edge into it. Its own `DEFINES` edges rank with proven edges, so an untyped
    /// window around the file is spent on them before any inbound heuristic edge.
    #[test]
    fn a_file_with_more_definitions_than_the_window_keeps_its_inbound_possible_impact() {
        use open_kioku_core::{identity, EvidenceSourceType, GraphEdge, GraphNode};

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
        let target = make_file("target", "src/big.rs");
        let caller = make_file("caller", "src/caller.rs");
        let manifest = IndexManifest {
            repository: Repository {
                id: repo_id.clone(),
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
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            quality: IndexQuality::default(),
            snapshot: None,
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[target.clone(), caller.clone()],
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

        let target_node = identity::file_node_id(&target.path);
        let mut nodes = [&target, &caller]
            .into_iter()
            .map(|file| GraphNode {
                id: identity::file_node_id(&file.path),
                node_type: GraphNodeType::File,
                label: file.path.display().to_string(),
                file_id: Some(file.id.clone()),
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let mut edges = Vec::new();
        for index in 0..(RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT + 5) {
            let symbol = open_kioku_core::NodeId::new(format!("symbol:s{index:02}"));
            nodes.push(GraphNode {
                id: symbol.clone(),
                node_type: GraphNodeType::Function,
                label: format!("s{index:02}"),
                ..Default::default()
            });
            let mut defines = GraphEdge {
                id: open_kioku_core::EdgeId::new(format!("a-defines-{index:02}")),
                from: target_node.clone(),
                to: symbol,
                edge_type: GraphEdgeType::Defines,
                ..Default::default()
            };
            defines.evidence.source_type = EvidenceSourceType::TreeSitter;
            defines.evidence.confidence = Confidence::High;
            edges.push(defines);
        }
        // Proofless, so heuristic, and its id sorts after every definition.
        let mut import = GraphEdge {
            id: open_kioku_core::EdgeId::new("z-import"),
            from: identity::file_node_id(&caller.path),
            to: target_node,
            edge_type: GraphEdgeType::Imports,
            ..Default::default()
        };
        import.evidence.confidence = Confidence::Medium;
        edges.push(import);
        store.replace_graph(&nodes, &edges).unwrap();

        let report = ImpactEngine::new(&store)
            .with_graph_store(Some(&store))
            .for_file(Path::new("src/big.rs"))
            .unwrap();
        assert!(
            report
                .possible_impact
                .iter()
                .any(|impact| impact.path == Path::new("src/caller.rs")),
            "{:?}",
            report.possible_impact
        );
    }

    /// Name matches reach real symbols: a changed file's own symbols use each other, and one
    /// dependent file can name the change from many symbols. The cap on possibilities keeps every
    /// other dependent file, even one whose path sorts last behind a file that alone would fill
    /// it, puts the changed file's own last, and counts what it cut.
    #[test]
    fn the_possible_impact_cap_keeps_other_files_first_and_counts_the_rest() {
        use open_kioku_core::{identity, GraphEdge, GraphNode};

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
        let target = make_file("target", "src/a_ledger.rs");
        let caller = make_file("caller", "src/z_books.rs");
        let busy = make_file("busy", "src/b_audit.rs");
        let settle = Symbol {
            id: SymbolId::new("symbol:ledger::settle"),
            name: "settle".into(),
            qualified_name: "ledger::settle".into(),
            kind: SymbolKind::Function,
            file_id: target.id.clone(),
            range: Some(LineRange { start: 1, end: 3 }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
            alias_of: None,
        };
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
            symbol_count: 1,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
            snapshot: None,
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[target.clone(), caller.clone(), busy.clone()],
                symbols: std::slice::from_ref(&settle),
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

        let settle_node = identity::symbol_node_id(&settle);
        let symbol_node = |id: &str, file: &File| GraphNode {
            id: open_kioku_core::NodeId::new(format!("symbol:{id}")),
            node_type: GraphNodeType::Function,
            label: id.into(),
            file_id: Some(file.id.clone()),
            symbol_id: Some(SymbolId::new(id)),
            ..Default::default()
        };
        // A proof-less call, as the symbol registry draws one.
        let guess = |from: &GraphNode| GraphEdge {
            id: open_kioku_core::EdgeId::new(format!("edge:{}", from.label)),
            from: from.id.clone(),
            to: settle_node.clone(),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        };
        let mut nodes = vec![GraphNode {
            id: settle_node.clone(),
            node_type: GraphNodeType::Function,
            label: settle.qualified_name.clone(),
            file_id: Some(target.id.clone()),
            symbol_id: Some(settle.id.clone()),
            ..Default::default()
        }];
        // Every edge is read: the per-type window around `settle` holds them all.
        let (own, busy_callers) = (5, RELATIONSHIP_IMPACT_LIMIT + 5);
        assert!(own + busy_callers < RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT);
        let mut edges = Vec::new();
        for index in 0..own {
            let node = symbol_node(&format!("ledger::entry_{index:02}"), &target);
            edges.push(guess(&node));
            nodes.push(node);
        }
        for index in 0..busy_callers {
            let node = symbol_node(&format!("audit::trace_{index:02}"), &busy);
            edges.push(guess(&node));
            nodes.push(node);
        }
        let close = symbol_node("books::close", &caller);
        edges.push(guess(&close));
        nodes.push(close);
        store.replace_graph(&nodes, &edges).unwrap();

        let report = ImpactEngine::new(&store)
            .with_graph_store(Some(&store))
            .for_file(Path::new("src/a_ledger.rs"))
            .unwrap();
        let paths = report
            .possible_impact
            .iter()
            .map(|impact| impact.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(paths.len(), RELATIONSHIP_IMPACT_LIMIT);
        assert_eq!(
            paths[..2],
            ["src/b_audit.rs", "src/z_books.rs"],
            "{paths:?}"
        );
        assert!(
            !paths.contains(&"src/a_ledger.rs".to_string()),
            "the changed file's own symbols go last: {paths:?}"
        );
        assert_eq!(
            report.possible_impact_omitted,
            own + busy_callers + 1 - RELATIONSHIP_IMPACT_LIMIT
        );
        // Every edge was read, so the count is the whole of what the cap cut.
        assert!(
            report.relationship_impact_caveats.is_empty(),
            "{:?}",
            report.relationship_impact_caveats
        );
    }

    /// A changed symbol with more inbound edges of one type than impact reads: the read stops at
    /// its limit, and the report says so rather than reading as every dependent.
    #[test]
    fn a_full_inbound_read_is_counted_in_the_relationship_reads() {
        use open_kioku_core::{identity, GraphEdge, GraphNode};

        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let file = |id: &str, path: &str| File {
            id: FileId::new(id),
            repository_id: repo_id.clone(),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: id.into(),
            is_generated: false,
            is_vendor: false,
        };
        let target = file("target", "src/ledger.rs");
        let caller = file("caller", "src/books.rs");
        let settle = Symbol {
            id: SymbolId::new("symbol:ledger::settle"),
            name: "settle".into(),
            qualified_name: "ledger::settle".into(),
            kind: SymbolKind::Function,
            file_id: target.id.clone(),
            range: Some(LineRange { start: 1, end: 3 }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
            alias_of: None,
        };
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
            file_count: 2,
            symbol_count: 1,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
            snapshot: None,
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[target.clone(), caller.clone()],
                symbols: std::slice::from_ref(&settle),
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
        let settle_node = identity::symbol_node_id(&settle);
        let mut nodes = vec![GraphNode {
            id: settle_node.clone(),
            node_type: GraphNodeType::Function,
            label: settle.qualified_name.clone(),
            file_id: Some(target.id.clone()),
            symbol_id: Some(settle.id.clone()),
            ..Default::default()
        }];
        let mut edges = Vec::new();
        for index in 0..(RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT + 5) {
            let id = format!("books::close_{index:02}");
            let node = GraphNode {
                id: open_kioku_core::NodeId::new(format!("symbol:{id}")),
                node_type: GraphNodeType::Function,
                label: id.clone(),
                file_id: Some(caller.id.clone()),
                symbol_id: Some(SymbolId::new(id)),
                ..Default::default()
            };
            edges.push(GraphEdge {
                id: open_kioku_core::EdgeId::new(format!("edge:{index:02}")),
                from: node.id.clone(),
                to: settle_node.clone(),
                edge_type: GraphEdgeType::Calls,
                ..Default::default()
            });
            nodes.push(node);
        }
        store.replace_graph(&nodes, &edges).unwrap();

        let report = ImpactEngine::new(&store)
            .with_graph_store(Some(&store))
            .for_file(Path::new("src/ledger.rs"))
            .unwrap();
        let reads = report.relationship_impact_reads.clone().unwrap();
        assert_eq!(reads.windows_at_limit, 1);
        assert_eq!(reads.edges_unread, Some(5));
        // Every edge is a guess, so the cut left possibilities out, and the capped possible
        // list's count already reads as a lower bound.
        assert_eq!(reads.windows_cutting_proven, 0);
        assert!(report.possible_impact_omitted > 0);
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
            snapshot: None,
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

    /// Answers every query with the same fixed results, in the order given.
    struct FixedSearchIndex(Vec<SearchResult>);

    impl SearchIndex for FixedSearchIndex {
        fn rebuild(&mut self, _: &[CodeChunk], _: &[File], _: &[Symbol]) -> Result<()> {
            Ok(())
        }

        fn search(&self, _: &str, _: usize) -> Result<Vec<SearchResult>> {
            Ok(self.0.clone())
        }
    }

    fn store_with_target(path: &str) -> SqliteStore {
        let store = make_store();
        let target = File {
            id: FileId::new("target"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: "target".into(),
            is_generated: false,
            is_vendor: false,
        };
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
            snapshot: None,
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[target],
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
        store
    }

    /// `src/rates.rs` defining `rates::RateValidator`, referenced once from each of
    /// `callers` files through a tree-sitter occurrence: the path `exact_reference_impacts`
    /// reads in production, at production scores.
    fn store_with_exact_callers(callers: usize) -> SqliteStore {
        let store = make_store();
        let repo_id = RepositoryId::new("repo");
        let file = |id: &str, path: String| File {
            id: FileId::new(id),
            repository_id: repo_id.clone(),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: id.into(),
            is_generated: false,
            is_vendor: false,
        };
        let mut files = vec![file("source", "src/rates.rs".into())];
        files.extend((0..callers).map(|index| {
            file(
                &format!("caller-{index:02}"),
                format!("src/callers/caller_{index:02}.rs"),
            )
        }));
        let symbol = Symbol {
            id: SymbolId::new("symbol:rate_validator"),
            name: "RateValidator".into(),
            qualified_name: "rates::RateValidator".into(),
            kind: SymbolKind::Class,
            file_id: FileId::new("source"),
            range: Some(LineRange { start: 1, end: 5 }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
            alias_of: None,
        };
        let occurrences = files[1..]
            .iter()
            .map(|caller| SymbolOccurrence {
                symbol_id: symbol.id.clone(),
                file_id: caller.id.clone(),
                range: Some(LineRange { start: 10, end: 10 }),
                source_range: None,
                is_definition: false,
                confidence: Confidence::High,
                provenance: EvidenceSourceType::TreeSitter,
            })
            .collect::<Vec<_>>();
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
            file_count: files.len(),
            symbol_count: 1,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
            snapshot: None,
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &files,
                symbols: &[symbol],
                occurrences: &occurrences,
                chunks: &[],
                imports: &[],
                tests: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();
        store
    }

    /// Lexical hits at a BM25-scale score, on paths that are not callers.
    fn keyword_hits(count: usize) -> FixedSearchIndex {
        FixedSearchIndex(
            (0..count)
                .map(|index| {
                    chunk_hit(
                        &format!("src/lexical/mention_{index:02}.rs"),
                        1,
                        6.4,
                        "matched `RateValidator` in a comment",
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn keyword_matches_that_outscore_exact_references_do_not_push_them_past_the_cap() {
        let store = store_with_exact_callers(3);
        let index = keyword_hits(MAX_DIRECT_IMPACTS);
        let report = ImpactEngine::new(&store)
            .with_search_index(Some(&index))
            .for_file(Path::new("src/rates.rs"))
            .unwrap();

        let exact = report
            .direct_impacts
            .iter()
            .filter(|result| result.is_exact_reference())
            .collect::<Vec<_>>();
        assert_eq!(
            exact.len(),
            3,
            "every exact reference survives the cap: {:#?}",
            report.direct_impacts
        );
        // The inversion this guards is score-driven, not a tie: the exact references score
        // below every keyword match they must still outrank.
        assert!(exact.iter().all(|result| result.score < 6.4));
        assert!(report.direct_impacts[..3]
            .iter()
            .all(SearchResult::is_exact_reference));
        assert_eq!(report.direct_impacts.len(), MAX_DIRECT_IMPACTS);
        assert_eq!(report.direct_impacts_omitted, 3);
        assert!(
            report.risk_report.reasons.iter().any(|reason| reason.starts_with(
                "3 further direct impact(s) omitted beyond the 25-entry cap, 0 of them exact-reference entries"
            )),
            "{:?}",
            report.risk_report.reasons
        );
    }

    #[test]
    fn exact_references_that_alone_overflow_the_cap_are_counted_as_cut() {
        let store = store_with_exact_callers(MAX_DIRECT_IMPACTS + 2);
        let index = keyword_hits(5);
        let report = ImpactEngine::new(&store)
            .with_search_index(Some(&index))
            .for_file(Path::new("src/rates.rs"))
            .unwrap();

        assert!(report
            .direct_impacts
            .iter()
            .all(SearchResult::is_exact_reference));
        assert_eq!(report.direct_impacts_omitted, 7);
        assert!(
            report.risk_report.reasons.iter().any(|reason| reason.starts_with(
                "7 further direct impact(s) omitted beyond the 25-entry cap, 2 of them exact-reference entries"
            )),
            "{:?}",
            report.risk_report.reasons
        );
    }

    #[test]
    fn heuristic_impacts_at_an_equal_score_break_the_tie_by_kind_then_path() {
        let mut cochange = chunk_hit("z/history.rs", 1, 0.2, "co-changed");
        cochange.match_reason = GIT_COCHANGE_MATCH_REASON.into();
        let lexical = chunk_hit("a/mention.rs", 1, 0.2, "lexical");
        for mut results in [
            vec![lexical.clone(), cochange.clone()],
            vec![cochange.clone(), lexical.clone()],
        ] {
            results.sort_by(compare_impact_results);
            assert_eq!(results[0].path, PathBuf::from("z/history.rs"));
        }
    }

    #[test]
    fn direct_impact_kinds_keep_their_authority_order() {
        // Exhaustive, so a new variant does not compile until it is placed here on purpose.
        fn rank(kind: DirectImpactKind) -> usize {
            match kind {
                DirectImpactKind::ExactReference => 0,
                DirectImpactKind::CrateImport => 1,
                DirectImpactKind::CrateImportUse => 2,
                DirectImpactKind::CoChange => 3,
                DirectImpactKind::Runtime => 4,
                DirectImpactKind::ServiceBoundary => 5,
                DirectImpactKind::Lexical => 6,
            }
        }
        let mut kinds = [
            DirectImpactKind::Lexical,
            DirectImpactKind::ServiceBoundary,
            DirectImpactKind::Runtime,
            DirectImpactKind::CoChange,
            DirectImpactKind::CrateImportUse,
            DirectImpactKind::CrateImport,
            DirectImpactKind::ExactReference,
        ];
        kinds.sort();
        assert_eq!(
            kinds.iter().copied().map(rank).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5, 6]
        );
    }

    #[test]
    fn an_uncapped_report_counts_nothing_omitted_and_serializes_no_count() {
        let store = store_with_target("src/rates.rs");
        let index = FixedSearchIndex(vec![chunk_hit("src/publisher.rs", 1, 1.0, "lexical")]);
        let report = ImpactEngine::new(&store)
            .with_search_index(Some(&index))
            .for_file(Path::new("src/rates.rs"))
            .unwrap();
        assert_eq!(report.direct_impacts_omitted, 0);
        assert_eq!(report.indirect_impacts_omitted, 0);
        assert!(!report
            .risk_report
            .reasons
            .iter()
            .any(|reason| reason.contains("omitted beyond")));
        let json = serde_json::to_value(&report).unwrap();
        assert!(json.get("direct_impacts_omitted").is_none());
        assert!(json.get("indirect_impacts_omitted").is_none());
    }

    /// Answers a query for `stem` with `dependents` and every other query with `direct`.
    struct StemSearchIndex {
        stem: &'static str,
        direct: Vec<SearchResult>,
        dependents: Vec<SearchResult>,
    }

    impl SearchIndex for StemSearchIndex {
        fn rebuild(&mut self, _: &[CodeChunk], _: &[File], _: &[Symbol]) -> Result<()> {
            Ok(())
        }

        fn search(&self, query: &str, _: usize) -> Result<Vec<SearchResult>> {
            Ok(if query == self.stem {
                self.dependents.clone()
            } else {
                self.direct.clone()
            })
        }
    }

    #[test]
    fn an_indirect_path_found_at_two_scores_is_listed_once() {
        let store = store_with_target("src/rates.rs");
        let index = StemSearchIndex {
            stem: "publisher",
            direct: vec![chunk_hit("src/publisher.rs", 1, 1.0, "direct")],
            dependents: vec![
                chunk_hit("src/ledger.rs", 1, 0.9, "ledger strong"),
                chunk_hit("src/billing.rs", 1, 0.5, "billing"),
                chunk_hit("src/ledger.rs", 40, 0.3, "ledger weak"),
            ],
        };
        let report = ImpactEngine::new(&store)
            .with_search_index(Some(&index))
            .for_file(Path::new("src/rates.rs"))
            .unwrap();
        assert_eq!(
            report
                .indirect_impacts
                .iter()
                .map(|result| (
                    result.path.to_string_lossy().into_owned(),
                    result.line_range.as_ref().map(|range| range.start)
                ))
                .collect::<Vec<_>>(),
            vec![
                ("src/ledger.rs".to_string(), Some(1)),
                ("src/billing.rs".to_string(), Some(1)),
            ]
        );
    }

    #[test]
    fn grouped_chunks_listed_under_the_cap_do_not_depend_on_arrival_order() {
        // Chunks of one path that cite one shared runtime fact and start on one line tie on
        // start line and on refs, and the cap sits between them. Which one is listed, and which
        // is folded into the summary line, must come from the chunks, not their arrival order.
        let shared = |start: u32, end: u32, message: &str| {
            let mut hit = chunk_hit("src/publisher.rs", start, 0.4, message);
            hit.line_range = Some(LineRange { start, end });
            hit.evidence_refs = vec!["runtime:fact-1".into()];
            hit
        };
        let chunks = vec![
            chunk_hit("src/publisher.rs", 1, 0.9, "representative"),
            chunk_hit("src/publisher.rs", 10, 0.4, "at 10"),
            chunk_hit("src/publisher.rs", 20, 0.4, "at 20"),
            chunk_hit("src/publisher.rs", 30, 0.4, "at 30"),
            shared(40, 45, "wide at 40"),
            shared(40, 42, "narrow at 40"),
            chunk_hit("src/publisher.rs", 50, 0.4, "at 50"),
        ];
        let mut reversed = chunks.clone();
        reversed.reverse();
        let merged = [chunks, reversed]
            .into_iter()
            .map(|group| merge_direct_group(group).expect("group merges into one impact"))
            .collect::<Vec<_>>();

        assert_eq!(merged[0].evidence, merged[1].evidence);
        assert_eq!(merged[0].evidence_refs, merged[1].evidence_refs);
        assert!(
            merged[0]
                .evidence
                .contains(&"lines 40-42: narrow at 40".to_string()),
            "{:#?}",
            merged[0].evidence
        );
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

    fn term_symbol(
        name: &str,
        kind: SymbolKind,
        lines: (u32, u32),
        visibility: open_kioku_core::Visibility,
    ) -> Symbol {
        Symbol {
            id: SymbolId::new(format!("symbol:{name}:{}", lines.0)),
            name: name.into(),
            qualified_name: name.into(),
            kind,
            file_id: FileId::new("terms"),
            range: Some(LineRange {
                start: lines.0,
                end: lines.1,
            }),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility,
            alias_of: None,
        }
    }

    fn term_test(symbol: &Symbol) -> open_kioku_core::TestTarget {
        open_kioku_core::TestTarget {
            id: format!("test:{}", symbol.name),
            name: symbol.name.clone(),
            file_id: symbol.file_id.clone(),
            range: symbol.range.clone(),
            command: None,
            confidence: Confidence::Medium,
            reason: "test-like path, annotation, or naming convention".into(),
            evidence_refs: Vec::new(),
            score_breakdown: Vec::new(),
            selection_tier: Default::default(),
            tier_justification: Vec::new(),
            origin: open_kioku_core::TestTargetOrigin::Symbol,
        }
    }

    fn term_file(path: &str) -> File {
        File {
            id: FileId::new("terms"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 1,
            content_hash: "terms".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    #[test]
    fn impact_terms_leave_out_test_functions_and_cfg_test_helpers() {
        use open_kioku_core::Visibility::{Private, Public};
        let production = term_symbol("rank", SymbolKind::Function, (1, 3), Public);
        let tests_module = term_symbol("tests", SymbolKind::Module, (5, 40), Private);
        let helper = term_symbol(
            "build_a_fixture_with_a_very_long_name",
            SymbolKind::Function,
            (7, 9),
            Private,
        );
        let test_fn = term_symbol(
            "ranking_breaks_ties_by_path_when_scores_match",
            SymbolKind::Function,
            (11, 14),
            Private,
        );
        let file = term_file("src/scoring.rs");
        let symbols = vec![production, tests_module, helper, test_fn.clone()];

        let terms = impact_terms(&file.path, &file, &symbols, &[term_test(&test_fn)]);

        assert_eq!(terms, vec!["scoring".to_string(), "rank".to_string()]);
    }

    #[test]
    fn adding_a_test_does_not_change_a_files_impact_terms() {
        use open_kioku_core::Visibility::{Private, Public};
        let file = term_file("src/scoring.rs");
        let mut symbols = (0..8)
            .map(|index| {
                term_symbol(
                    &format!("score_{index}"),
                    SymbolKind::Function,
                    (index * 2 + 1, index * 2 + 2),
                    Public,
                )
            })
            .collect::<Vec<_>>();
        let before = impact_terms(&file.path, &file, &symbols, &[]);

        let added = term_symbol(
            "scores_are_rounded_half_up_for_every_supported_precision",
            SymbolKind::Function,
            (100, 104),
            Private,
        );
        symbols.push(added.clone());
        let after = impact_terms(&file.path, &file, &symbols, &[term_test(&added)]);

        assert_eq!(before, after);
    }

    #[test]
    fn a_test_module_nested_in_a_production_module_leaves_the_outer_module_searched() {
        use open_kioku_core::Visibility::{Private, Public};
        let file = term_file("src/net.rs");
        let client = term_symbol("client", SymbolKind::Module, (1, 20), Public);
        let open = term_symbol(
            "open_connection_pool_for_tenant",
            SymbolKind::Function,
            (2, 4),
            Public,
        );
        let tests_module = term_symbol("tests", SymbolKind::Module, (6, 19), Private);
        let helper = term_symbol(
            "build_a_fixture_with_a_very_long_name",
            SymbolKind::Function,
            (8, 10),
            Private,
        );
        let test_fn = term_symbol("opens", SymbolKind::Function, (12, 15), Private);
        let symbols = vec![client, open, tests_module, helper, test_fn.clone()];

        let terms = impact_terms(&file.path, &file, &symbols, &[term_test(&test_fn)]);

        assert_eq!(
            terms,
            vec![
                "open_connection_pool_for_tenant".to_string(),
                "client".to_string(),
                "net".to_string(),
            ]
        );
    }

    #[test]
    fn a_public_production_name_starting_with_test_stays_a_search_term() {
        use open_kioku_core::Visibility::{Private, Public};
        // Rust: recorded as a test target by its `test_` prefix, but public, so production API.
        let rust_file = term_file("src/health.rs");
        let rust_api = term_symbol(
            "test_connection_health",
            SymbolKind::Function,
            (1, 3),
            Public,
        );
        let rust_test = term_symbol("checks_health", SymbolKind::Function, (5, 8), Private);
        let symbols = vec![rust_api.clone(), rust_test.clone()];
        let terms = impact_terms(
            &rust_file.path,
            &rust_file,
            &symbols,
            &[term_test(&rust_api), term_test(&rust_test)],
        );
        assert_eq!(
            terms,
            vec!["test_connection_health".to_string(), "health".to_string()]
        );

        // Java: `public boolean testConnection()` in a main source set.
        let java_file = term_file("src/main/java/app/Db.java");
        let mut java_api = term_symbol("testConnection", SymbolKind::Method, (4, 6), Public);
        java_api.language = Language::Java;
        let terms = impact_terms(
            &java_file.path,
            &java_file,
            &[java_api.clone()],
            &[term_test(&java_api)],
        );
        assert_eq!(terms, vec!["testConnection".to_string(), "Db".to_string()]);
    }

    #[test]
    fn a_scip_definition_of_a_test_function_is_test_code_too() {
        use open_kioku_core::Visibility::{Private, Public, Unknown};
        let file = term_file("src/scoring.rs");
        let production = term_symbol("rank", SymbolKind::Function, (1, 3), Public);
        let test_fn = term_symbol(
            "ranking_breaks_ties_by_path_when_scores_match",
            SymbolKind::Function,
            (10, 14),
            Private,
        );
        // SCIP's definition range is the name's line, not the item's.
        let mut scip_test_fn = term_symbol(
            "ranking_breaks_ties_by_path_when_scores_match",
            SymbolKind::Function,
            (11, 11),
            Unknown,
        );
        scip_test_fn.provenance = EvidenceSourceType::Scip;
        let symbols = vec![production, test_fn.clone(), scip_test_fn];

        let terms = impact_terms(&file.path, &file, &symbols, &[term_test(&test_fn)]);

        assert_eq!(terms, vec!["scoring".to_string(), "rank".to_string()]);
    }

    #[test]
    fn a_file_of_only_tests_keeps_its_test_names_as_impact_terms() {
        use open_kioku_core::Visibility::Private;
        let test_fn = term_symbol("rounds_half_up", SymbolKind::Function, (1, 3), Private);
        let symbols = vec![test_fn.clone()];

        // A test-path file: every name is test code, so none is dropped.
        let test_path = term_file("tests/rounding.rs");
        assert_eq!(
            impact_terms(
                &test_path.path,
                &test_path,
                &symbols,
                &[term_test(&test_fn)]
            ),
            vec!["rounds_half_up".to_string(), "rounding".to_string()]
        );
        // A source file whose only names are tests keeps them too.
        let source = term_file("src/checks.rs");
        assert_eq!(
            impact_terms(&source.path, &source, &symbols, &[term_test(&test_fn)]),
            vec!["rounds_half_up".to_string(), "checks".to_string()]
        );
    }

    /// A changed file `src/ledger.rs`, its symbols, and graph edges into them from other files,
    /// built up by a test and written into a fresh store.
    struct LedgerGraph {
        files: Vec<File>,
        symbols: Vec<Symbol>,
        nodes: Vec<open_kioku_core::GraphNode>,
        edges: Vec<open_kioku_core::GraphEdge>,
    }

    impl LedgerGraph {
        fn new() -> Self {
            let mut graph = Self {
                files: Vec::new(),
                symbols: Vec::new(),
                nodes: Vec::new(),
                edges: Vec::new(),
            };
            graph.file("src/ledger.rs");
            graph
        }

        fn file(&mut self, path: &str) -> FileId {
            let id = FileId::new(path);
            if !self.files.iter().any(|file| file.id == id) {
                self.files.push(File {
                    id: id.clone(),
                    repository_id: RepositoryId::new("repo"),
                    path: PathBuf::from(path),
                    language: Language::Rust,
                    size_bytes: 100,
                    content_hash: path.into(),
                    is_generated: false,
                    is_vendor: false,
                });
            }
            id
        }

        /// A symbol of the changed file, on lines `10 * n + 1 ..= 10 * n + 5` for the `n`-th.
        fn changed_symbol(
            &mut self,
            name: &str,
            visibility: open_kioku_core::Visibility,
        ) -> Symbol {
            let start = 10 * self.symbols.len() as u32 + 1;
            let symbol = Symbol {
                id: SymbolId::new(format!("ledger::{name}")),
                name: name.into(),
                qualified_name: format!("ledger::{name}"),
                kind: SymbolKind::Function,
                file_id: FileId::new("src/ledger.rs"),
                range: Some(LineRange {
                    start,
                    end: start + 4,
                }),
                language: Language::Rust,
                confidence: Confidence::High,
                provenance: EvidenceSourceType::TreeSitter,
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility,
                alias_of: None,
            };
            self.nodes.push(open_kioku_core::GraphNode {
                id: identity::symbol_node_id(&symbol),
                node_type: GraphNodeType::Function,
                label: symbol.qualified_name.clone(),
                file_id: Some(symbol.file_id.clone()),
                symbol_id: Some(symbol.id.clone()),
                ..Default::default()
            });
            self.symbols.push(symbol.clone());
            symbol
        }

        /// `caller` (a symbol named after its path) references `target`, with an exact-reference proof
        /// when `proven`, and with none, as the symbol registry draws one, otherwise.
        fn call(&mut self, caller_path: &str, caller: &str, target: &Symbol, proven: bool) {
            let file_id = self.file(caller_path);
            let caller_id = open_kioku_core::NodeId::new(format!("symbol:{caller}"));
            if !self.nodes.iter().any(|node| node.id == caller_id) {
                self.nodes.push(open_kioku_core::GraphNode {
                    id: caller_id.clone(),
                    node_type: GraphNodeType::Function,
                    label: caller.into(),
                    file_id: Some(file_id),
                    symbol_id: Some(SymbolId::new(caller)),
                    ..Default::default()
                });
            }
            let mut edge = open_kioku_core::GraphEdge {
                id: open_kioku_core::EdgeId::new(format!("edge:{caller}->{}", target.name)),
                from: caller_id,
                to: identity::symbol_node_id(target),
                edge_type: GraphEdgeType::References,
                ..Default::default()
            };
            if proven {
                edge.set_relationship_proofs(vec![open_kioku_core::RelationshipProof::new(
                    open_kioku_core::RelationshipProofKind::ExactReference,
                    "test-exact-reference",
                    1,
                )])
                .unwrap();
            }
            self.edges.push(edge);
        }

        /// The file at `importer_path` imports `src/ledger.rs` by path, with an import-binding proof.
        fn import(&mut self, importer_path: &str) {
            let file_id = self.file(importer_path);
            let from = identity::try_file_node_id(Path::new(importer_path)).unwrap();
            self.nodes.push(open_kioku_core::GraphNode {
                id: from.clone(),
                node_type: GraphNodeType::File,
                label: importer_path.into(),
                file_id: Some(file_id),
                ..Default::default()
            });
            let mut edge = open_kioku_core::GraphEdge {
                id: open_kioku_core::EdgeId::new(format!("edge:{importer_path}->ledger")),
                from,
                to: identity::try_file_node_id(Path::new("src/ledger.rs")).unwrap(),
                edge_type: GraphEdgeType::Imports,
                ..Default::default()
            };
            edge.set_relationship_proofs(vec![open_kioku_core::RelationshipProof::new(
                open_kioku_core::RelationshipProofKind::ImportBinding,
                "test-import-binding",
                1,
            )])
            .unwrap();
            self.edges.push(edge);
        }

        fn report(&self, focus: &ChangeFocus) -> ImpactReport {
            let store = self.store();
            ImpactEngine::new(&store)
                .with_graph_store(Some(&store))
                .for_change(Path::new("src/ledger.rs"), focus)
                .unwrap()
        }

        fn answer(&self, root: &Path, request: ImpactRequest<'_>) -> Result<ImpactAnswer> {
            let store = self.store();
            ImpactEngine::new(&store)
                .with_graph_store(Some(&store))
                .answer(root, request)
        }

        fn store(&self) -> SqliteStore {
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
                file_count: self.files.len(),
                symbol_count: self.symbols.len(),
                chunk_count: 0,
                indexed_at: Utc::now(),
                schema_version: 1,
                index_mode: Default::default(),
                phase_reports: Vec::new(),
                quality: IndexQuality::default(),
                snapshot: None,
            };
            store
                .replace_index(IndexData {
                    manifest: &manifest,
                    files: &self.files,
                    symbols: &self.symbols,
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
            store.replace_graph(&self.nodes, &self.edges).unwrap();
            store
        }
    }

    fn listed(impacts: &[RelationshipImpact]) -> Vec<String> {
        impacts
            .iter()
            .map(|impact| impact.symbol.clone().unwrap_or_default())
            .collect()
    }

    /// Impact read the dependents of the first 16 symbols in file order, so a change deep in a
    /// large file lost its dependents, proven ones included, and the truncation caveat fired for
    /// every file with more than 16 symbols, whether or not the unread ones had any dependent.
    /// A symbol with no inbound edge needs no read, and leaving it unread skips nothing.
    #[test]
    fn a_symbol_late_in_a_large_file_has_its_dependents_read() {
        let mut graph = LedgerGraph::new();
        let mut last = None;
        for index in 0..20 {
            last = Some(graph.changed_symbol(
                &format!("entry_{index:02}"),
                open_kioku_core::Visibility::Private,
            ));
        }
        graph.call("src/books.rs", "books::close", &last.unwrap(), true);

        let report = graph.report(&ChangeFocus::default());
        assert_eq!(listed(&report.proven_impact), ["books::close"]);
        assert!(
            report.relationship_impact_caveats.is_empty(),
            "{:?}",
            report.relationship_impact_caveats
        );
        assert_eq!(
            report.relationship_impact_reads,
            Some(RelationshipImpactReads {
                symbols_total: 20,
                symbols_touched: 0,
                symbols_read: 1,
                symbols_unread_with_dependents: Some(0),
                edges_unread: Some(0),
                proven_edges_unread: Some(0),
                windows_at_limit: 0,
                windows_cutting_proven: 0,
                windows_widened: 0,
            })
        );
    }

    /// Past the read limit, symbols are taken by importance, not file order: public first, then
    /// by inbound edges. What is left unread is counted, and the caveat says so.
    #[test]
    fn symbols_past_the_read_limit_are_taken_by_importance_and_counted() {
        let mut graph = LedgerGraph::new();
        let private = (0..RELATIONSHIP_IMPACT_SYMBOL_SEEDS)
            .map(|index| {
                graph.changed_symbol(
                    &format!("entry_{index:03}"),
                    open_kioku_core::Visibility::Private,
                )
            })
            .collect::<Vec<_>>();
        let busy = graph.changed_symbol("busy", open_kioku_core::Visibility::Private);
        let public = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        for (index, symbol) in private.iter().enumerate() {
            graph.call(
                "src/audit.rs",
                &format!("audit::trace_{index:03}"),
                symbol,
                false,
            );
        }
        // Read only if taken by importance: both come last in the file.
        graph.call("src/books.rs", "books::close", &public, true);
        graph.call("src/books.rs", "books::open", &busy, true);
        graph.call("src/books.rs", "books::reopen", &busy, true);

        let report = graph.report(&ChangeFocus::default());
        assert_eq!(
            listed(&report.proven_impact),
            ["books::close", "books::open", "books::reopen"]
        );
        let reads = report.relationship_impact_reads.unwrap();
        assert_eq!(reads.symbols_total, RELATIONSHIP_IMPACT_SYMBOL_SEEDS + 2);
        assert_eq!(reads.symbols_read, RELATIONSHIP_IMPACT_SYMBOL_SEEDS);
        // Two of the private symbols with one inbound edge each.
        assert_eq!(reads.symbols_unread_with_dependents, Some(2));
        assert_eq!(reads.edges_unread, Some(2));
        assert!(
            report
                .relationship_impact_caveats
                .iter()
                .any(|caveat| caveat.contains(
                    "the 2 others have 2 inbound edge(s), 0 of them proven, that were not read"
                )),
            "{:?}",
            report.relationship_impact_caveats
        );
    }

    /// The symbols a change touches are read first, whatever their visibility or fan-in, so
    /// their dependents are never the ones a bounded read leaves out.
    #[test]
    fn the_symbols_a_change_touches_are_read_first() {
        let mut graph = LedgerGraph::new();
        for index in 0..RELATIONSHIP_IMPACT_SYMBOL_SEEDS {
            let symbol = graph.changed_symbol(
                &format!("entry_{index:03}"),
                open_kioku_core::Visibility::Public,
            );
            graph.call(
                "src/audit.rs",
                &format!("audit::trace_{index:03}"),
                &symbol,
                true,
            );
        }
        let edited = graph.changed_symbol("reopen", open_kioku_core::Visibility::Private);
        graph.call("src/books.rs", "books::close", &edited, true);
        let lists_close = |report: &ImpactReport| {
            listed(&report.proven_impact).contains(&"books::close".to_string())
        };

        // Unfocused, the private symbol ranks last and is left unread.
        let unfocused = graph.report(&ChangeFocus::default());
        assert!(!lists_close(&unfocused), "{:?}", unfocused.proven_impact);
        assert_eq!(
            unfocused
                .relationship_impact_reads
                .as_ref()
                .and_then(|reads| reads.symbols_unread_with_dependents),
            Some(1)
        );

        // A change to its lines reads it first.
        let range = edited.range.clone().unwrap();
        let by_line = graph.report(&ChangeFocus::lines(vec![LineRange {
            start: range.start + 1,
            end: range.start + 1,
        }]));
        assert!(lists_close(&by_line), "{:?}", by_line.proven_impact);
        let reads = by_line.relationship_impact_reads.unwrap();
        assert_eq!(reads.symbols_touched, 1);
        assert_eq!(reads.symbols_unread_with_dependents, Some(1));

        // So does naming it.
        let by_name = graph.report(&ChangeFocus::symbols(vec![edited.id.clone()]));
        assert!(lists_close(&by_name), "{:?}", by_name.proven_impact);
    }

    /// A changed line touches the innermost symbols around it: an edit inside one method of a
    /// large class or `impl` block touches that method, not the class and every member with it.
    #[test]
    fn a_changed_line_touches_the_innermost_symbol_around_it() {
        let mut graph = LedgerGraph::new();
        let mut ledger = graph.changed_symbol("Ledger", open_kioku_core::Visibility::Public);
        let mut settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        let mut reopen = graph.changed_symbol("reopen", open_kioku_core::Visibility::Public);
        ledger.range = Some(LineRange { start: 1, end: 30 });
        settle.range = Some(LineRange { start: 3, end: 10 });
        reopen.range = Some(LineRange { start: 12, end: 20 });
        let symbols = [&ledger, &settle, &reopen];
        let touched =
            |start, end| ChangeFocus::lines(vec![LineRange { start, end }]).touched(&symbols);

        assert_eq!(touched(5, 6), [false, true, false]);
        // A line of the class no member holds touches the class.
        assert_eq!(touched(1, 1), [true, false, false]);
        assert_eq!(touched(9, 11), [true, true, false]);
        assert_eq!(touched(8, 14), [true, true, true]);
        assert_eq!(touched(40, 41), [false, false, false]);
        // A symbol named as changed is touched, whatever lines say.
        assert_eq!(
            ChangeFocus::symbols(vec![ledger.id.clone()]).touched(&symbols),
            [true, false, false]
        );
    }

    /// Past the read limit, a symbol with a proven dependent is read before symbols with only
    /// name matches, however many: a run of guesses never pushes a proof out of the read.
    #[test]
    fn a_proven_dependent_is_read_before_any_number_of_guesses() {
        let mut graph = LedgerGraph::new();
        for index in 0..RELATIONSHIP_IMPACT_SYMBOL_SEEDS + 1 {
            let symbol = graph.changed_symbol(
                &format!("entry_{index:03}"),
                open_kioku_core::Visibility::Public,
            );
            for caller in ["audit", "books"] {
                graph.call(
                    &format!("src/{caller}.rs"),
                    &format!("{caller}::guess_{index:03}"),
                    &symbol,
                    false,
                );
            }
        }
        let proven = graph.changed_symbol("settle", open_kioku_core::Visibility::Private);
        graph.call("src/ledger_test.rs", "ledger_test::settles", &proven, true);

        let report = graph.report(&ChangeFocus::default());
        assert_eq!(listed(&report.proven_impact), ["ledger_test::settles"]);
        let reads = report.relationship_impact_reads.unwrap();
        assert_eq!(reads.symbols_unread_with_dependents, Some(2));
        assert_eq!(reads.edges_unread, Some(4));
        // Every proven edge was read.
        assert_eq!(reads.proven_edges_unread, Some(0));
    }

    /// A read cut after every proven edge left possibilities unread. Where `possible_impact` was
    /// not cut it would read as complete, so the caveat says it is not.
    #[test]
    fn a_heuristic_cut_beside_an_uncut_possible_list_is_a_caveat() {
        let mut graph = LedgerGraph::new();
        let settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        let reopen = graph.changed_symbol("reopen", open_kioku_core::Visibility::Public);
        // Every caller of `reopen` is proven, in a file of its own; each also names `settle`
        // without proof, so the possibilities it would add repeat listed proven dependents.
        for index in 0..RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT {
            let path = format!("src/books_{index:02}.rs");
            let caller = format!("books::close_{index:02}");
            graph.call(&path, &caller, &reopen, true);
            graph.call(&path, &caller, &settle, false);
        }
        graph.call("src/books_99.rs", "books::close_99", &settle, false);

        let report = graph.report(&ChangeFocus::default());
        let reads = report.relationship_impact_reads.clone().unwrap();
        assert_eq!(
            (
                reads.windows_at_limit,
                reads.windows_cutting_proven,
                reads.edges_unread
            ),
            (1, 0, Some(1))
        );
        assert_eq!(report.possible_impact_omitted, 0);
        assert!(
            report
                .relationship_impact_caveats
                .iter()
                .any(|caveat| caveat.contains("after every proven edge")),
            "{:?}",
            report.relationship_impact_caveats
        );
    }

    /// A read that came back exactly full left nothing out. It used to be reported as cut.
    #[test]
    fn an_exactly_full_inbound_read_is_not_reported_as_cut() {
        let mut graph = LedgerGraph::new();
        let settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        for index in 0..RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT {
            graph.call(
                "src/audit.rs",
                &format!("audit::trace_{index:02}"),
                &settle,
                false,
            );
        }

        let report = graph.report(&ChangeFocus::default());
        assert!(
            report.relationship_impact_caveats.is_empty(),
            "{:?}",
            report.relationship_impact_caveats
        );
        let reads = report.relationship_impact_reads.unwrap();
        assert_eq!(reads.windows_at_limit, 0);
        assert_eq!(reads.edges_unread, Some(0));
    }

    /// Windows keep proven edges first, so the first edge a full read leaves out says whether any
    /// proven dependent went unread; the count says how many edges did.
    #[test]
    fn a_cut_inbound_read_says_whether_it_left_proven_edges_unread() {
        let mut graph = LedgerGraph::new();
        let guessed = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        for index in 0..RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT + 5 {
            graph.call(
                "src/audit.rs",
                &format!("audit::trace_{index:02}"),
                &guessed,
                false,
            );
        }
        let heuristic_only = graph.report(&ChangeFocus::default());
        let reads = heuristic_only.relationship_impact_reads.clone().unwrap();
        assert_eq!(
            (
                reads.windows_at_limit,
                reads.windows_cutting_proven,
                reads.edges_unread
            ),
            (1, 0, Some(5))
        );
        // The possible list is already cut and counted, and the count reads as a lower bound;
        // a sentence would only repeat the numbers.
        assert!(heuristic_only.possible_impact_omitted > 0);
        assert!(
            heuristic_only.relationship_impact_caveats.is_empty(),
            "{:?}",
            heuristic_only.relationship_impact_caveats
        );

        // Past the widened window's bound, a read cuts proven edges, and says so.
        let proven = graph.changed_symbol("reopen", open_kioku_core::Visibility::Public);
        for index in 0..RELATIONSHIP_IMPACT_PROVEN_WINDOW_LIMIT + 1 {
            graph.call(
                "src/books.rs",
                &format!("books::close_{index:03}"),
                &proven,
                true,
            );
        }
        let report = graph.report(&ChangeFocus::default());
        let reads = report.relationship_impact_reads.clone().unwrap();
        assert_eq!(
            (
                reads.windows_at_limit,
                reads.windows_cutting_proven,
                reads.windows_widened,
                reads.edges_unread
            ),
            (2, 1, 1, Some(6))
        );
        assert!(
            report
                .relationship_impact_caveats
                .iter()
                .any(|caveat| caveat.contains("1 of them possibly before every proven edge was read (1 proven edge(s) in all were not read)")),
            "{:?}",
            report.relationship_impact_caveats
        );
    }

    /// A hub symbol's window held 40 edges, so its proven dependents past the 40th went unread
    /// though the store had counted them. A window the counts show holds more proven edges is
    /// widened to take them: every proven dependent is read, and only heuristic edges are cut.
    #[test]
    fn a_hub_window_with_more_proven_edges_than_the_limit_reads_every_one() {
        let mut graph = LedgerGraph::new();
        let settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        let proven_callers = RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT + 20;
        for index in 0..proven_callers {
            graph.call(
                &format!("src/caller_{index:02}.rs"),
                &format!("caller_{index:02}::run"),
                &settle,
                true,
            );
        }
        for index in 0..10 {
            graph.call(
                "src/audit.rs",
                &format!("audit::trace_{index:02}"),
                &settle,
                false,
            );
        }

        let report = graph.report(&ChangeFocus::default());
        let reads = report.relationship_impact_reads.clone().unwrap();
        assert_eq!(
            (
                reads.windows_widened,
                reads.windows_at_limit,
                reads.windows_cutting_proven,
                reads.proven_edges_unread,
                reads.edges_unread,
            ),
            (1, 1, 0, Some(0), Some(10))
        );
        assert_eq!(
            report.proven_impact.len() + report.proven_impact_omitted,
            proven_callers
        );
        // An edit to the hub lists every proven dependent, past the 40th.
        let edited = graph.report(&ChangeFocus::symbols(vec![settle.id.clone()]));
        assert_eq!(edited.proven_impact.len(), proven_callers);
        assert!(
            !edited
                .relationship_impact_caveats
                .iter()
                .any(|caveat| caveat.contains("proven dependents through them may be missing")),
            "{:?}",
            edited.relationship_impact_caveats
        );
    }

    /// Widening is bounded per report: the reads of the most important seeds widen first, and a
    /// window past the budget is cut at what is left of it, its unread proven edges counted.
    #[test]
    fn widened_reads_stop_at_the_report_budget_and_count_what_they_left() {
        let mut graph = LedgerGraph::new();
        let per_hub = RELATIONSHIP_IMPACT_PROVEN_WINDOW_LIMIT;
        let extra = per_hub - RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT;
        let hubs = RELATIONSHIP_IMPACT_WIDENING_BUDGET / extra + 1;
        for hub in 0..hubs {
            let symbol =
                graph.changed_symbol(&format!("hub_{hub}"), open_kioku_core::Visibility::Public);
            for index in 0..per_hub {
                graph.call(
                    &format!("src/caller_{hub}.rs"),
                    &format!("caller_{hub}::run_{index:03}"),
                    &symbol,
                    true,
                );
            }
        }

        let report = graph.report(&ChangeFocus::default());
        let reads = report.relationship_impact_reads.clone().unwrap();
        let unread = hubs * extra - RELATIONSHIP_IMPACT_WIDENING_BUDGET;
        assert_eq!(reads.proven_edges_unread, Some(unread));
        assert_eq!(reads.windows_cutting_proven, 1);
        assert!(
            report
                .relationship_impact_caveats
                .iter()
                .any(|caveat| caveat
                    .contains(&format!("({unread} proven edge(s) in all were not read)"))),
            "{:?}",
            report.relationship_impact_caveats
        );
    }

    /// `proven_impact` is cut by the rule `possible_impact` is, and says how many it cut: each
    /// other file's first entry before any file's second, the changed file's own last, except
    /// that a dependent of a symbol the change touches is never cut. Cut by path alone, one busy
    /// file filled the list, and a capped list read as every proven dependent.
    #[test]
    fn the_proven_impact_cap_keeps_touched_and_other_files_first_and_counts_the_rest() {
        let mut graph = LedgerGraph::new();
        let settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        let reopen = graph.changed_symbol("reopen", open_kioku_core::Visibility::Public);
        let busy_callers = RELATIONSHIP_IMPACT_LIMIT + 5;
        assert!(busy_callers < RELATIONSHIP_IMPACT_NEIGHBOR_LIMIT);
        for index in 0..busy_callers {
            graph.call(
                "src/b_audit.rs",
                &format!("audit::trace_{index:02}"),
                &settle,
                true,
            );
        }
        graph.call("src/ledger.rs", "ledger::entry", &settle, true);
        graph.call("src/z_books.rs", "books::close", &settle, true);
        // Sorts after every other caller in its file.
        graph.call("src/b_audit.rs", "audit::zz_reopen", &reopen, true);

        let report = graph.report(&ChangeFocus::default());
        assert_eq!(report.proven_impact.len(), RELATIONSHIP_IMPACT_LIMIT);
        assert_eq!(
            report.proven_impact_omitted,
            busy_callers + 3 - RELATIONSHIP_IMPACT_LIMIT
        );
        // The cut entries are in the busy file, which is named, and in the changed file, which
        // is no dependent of itself.
        assert_eq!(report.proven_impact_omitted_files, 0);
        let kept = listed(&report.proven_impact);
        assert!(kept.contains(&"books::close".to_string()), "{kept:?}");
        assert!(!kept.contains(&"ledger::entry".to_string()), "{kept:?}");
        assert!(!kept.contains(&"audit::zz_reopen".to_string()), "{kept:?}");
        // Still listed by path, as proven entries always were.
        let mut sorted = report.proven_impact.clone();
        sorted.sort_by(|a, b| (&a.path, &a.symbol).cmp(&(&b.path, &b.symbol)));
        assert_eq!(report.proven_impact, sorted);
        // Every dependent of the edited symbol is kept, whatever the cap.
        let focused = graph.report(&ChangeFocus::symbols(vec![reopen.id.clone()]));
        assert!(
            listed(&focused.proven_impact).contains(&"audit::zz_reopen".to_string()),
            "{:?}",
            focused.proven_impact
        );
        assert_eq!(focused.proven_impact_omitted, report.proven_impact_omitted);
        // Even past the cap: an edit to `settle` lists all of its proven dependents, and the cap
        // cuts only the dependent of the symbol it did not touch.
        let busy_edit = graph.report(&ChangeFocus::symbols(vec![settle.id.clone()]));
        assert_eq!(busy_edit.proven_impact.len(), busy_callers + 2);
        assert_eq!(busy_edit.proven_impact_omitted, 1);
        assert!(!listed(&busy_edit.proven_impact).contains(&"audit::zz_reopen".to_string()));
        // A cut proven list is counted, not a caveat, as for possible_impact.
        assert!(
            report.relationship_impact_caveats.is_empty(),
            "{:?}",
            report.relationship_impact_caveats
        );
    }

    /// A registry match the registry flags as speculative reaches impact as ambiguous.
    #[test]
    fn a_speculative_registry_edge_is_an_ambiguous_possibility() {
        let mut graph = LedgerGraph::new();
        let settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        graph.call("src/books.rs", "books::close", &settle, false);
        graph.call("src/audit.rs", "audit::trace", &settle, false);
        graph.edges[0]
            .ambiguity
            .push("name-only match via unique-project-name".into());

        let report = graph.report(&ChangeFocus::default());
        let ambiguous = report
            .possible_impact
            .iter()
            .map(|impact| (impact.symbol.clone().unwrap_or_default(), impact.ambiguous))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            ambiguous,
            BTreeMap::from([
                ("audit::trace".to_string(), false),
                ("books::close".to_string(), true),
            ])
        );
    }

    /// The graph of [`LedgerGraph`] where the read limit leaves one private symbol, `reopen`
    /// (proven caller `books::close`), unread unless the change focuses on it.
    fn ledger_with_an_unread_symbol() -> (LedgerGraph, Symbol) {
        let mut graph = LedgerGraph::new();
        for index in 0..RELATIONSHIP_IMPACT_SYMBOL_SEEDS {
            let symbol = graph.changed_symbol(
                &format!("entry_{index:03}"),
                open_kioku_core::Visibility::Public,
            );
            graph.call(
                "src/audit.rs",
                &format!("audit::trace_{index:03}"),
                &symbol,
                true,
            );
        }
        let edited = graph.changed_symbol("reopen", open_kioku_core::Visibility::Private);
        graph.call("src/books.rs", "books::close", &edited, true);
        (graph, edited)
    }

    fn diff_changing(path: &str, lines: Option<LineRange>) -> open_kioku_git::DiffFile {
        open_kioku_git::DiffFile {
            old_path: Some(PathBuf::from(path)),
            new_path: Some(PathBuf::from(path)),
            status: GitChangeKind::Modified,
            rename_score: None,
            hunks: vec![open_kioku_git::DiffHunk {
                old_range: lines.clone(),
                new_range: lines,
            }],
        }
    }

    fn one_report(answer: ImpactAnswer) -> ImpactReport {
        match answer {
            ImpactAnswer::File(report) => *report,
            ImpactAnswer::Diff(diff) => panic!("expected one report, got {}", diff.reports.len()),
        }
    }

    /// `ok impact` and MCP `impact_analysis` both answer through `ImpactEngine::answer`. A diff
    /// focuses a file on the symbols its changed lines touch, but only while the index holds the
    /// file as it is on disk: otherwise the lines could name other symbols, and the report says
    /// why the focus is missing where it would have changed the answer.
    #[test]
    fn a_diff_focuses_a_file_on_its_changed_lines_while_the_index_holds_it() {
        let (mut graph, edited) = ledger_with_an_unread_symbol();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/ledger.rs"), "pub fn reopen() {}\n").unwrap();
        graph.files[0].content_hash = format!("{:x}", Sha256::digest(b"pub fn reopen() {}\n"));
        let range = edited.range.clone().unwrap();
        let line = LineRange::single(range.start + 1);
        let diff = [diff_changing("src/ledger.rs", Some(line.clone()))];
        let path = Path::new("src/ledger.rs");
        let lists_close = |report: &ImpactReport| {
            listed(&report.proven_impact).contains(&"books::close".to_string())
        };

        let focused = one_report(
            graph
                .answer(
                    root.path(),
                    ImpactRequest {
                        path: Some(path),
                        diff: Some(&diff),
                        ..Default::default()
                    },
                )
                .unwrap(),
        );
        assert!(lists_close(&focused), "{:?}", focused.proven_impact);
        assert_eq!(
            focused
                .relationship_impact_reads
                .as_ref()
                .map(|reads| reads.symbols_touched),
            Some(1)
        );

        // The same diff over every file it changes gives the same report.
        let ImpactAnswer::Diff(DiffImpact { reports, .. }) = graph
            .answer(
                root.path(),
                ImpactRequest {
                    diff: Some(&diff),
                    ..Default::default()
                },
            )
            .unwrap()
        else {
            panic!("a diff alone answers per changed file");
        };
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].proven_impact, focused.proven_impact);

        // Edited since it was indexed: the lines are dropped, and the report says why.
        std::fs::write(root.path().join("src/ledger.rs"), "pub fn reopen() { 1 }\n").unwrap();
        let stale = one_report(
            graph
                .answer(
                    root.path(),
                    ImpactRequest {
                        path: Some(path),
                        diff: Some(&diff),
                        ..Default::default()
                    },
                )
                .unwrap(),
        );
        assert!(!lists_close(&stale), "{:?}", stale.proven_impact);
        assert!(
            stale
                .relationship_impact_caveats
                .iter()
                .any(|caveat| caveat.contains("differs from the indexed copy")),
            "{:?}",
            stale.relationship_impact_caveats
        );

        // A diff that changes no line of the file says so.
        let elsewhere = [diff_changing("src/books.rs", Some(line))];
        let untouched = one_report(
            graph
                .answer(
                    root.path(),
                    ImpactRequest {
                        path: Some(path),
                        diff: Some(&elsewhere),
                        ..Default::default()
                    },
                )
                .unwrap(),
        );
        assert!(
            untouched
                .relationship_impact_caveats
                .iter()
                .any(|caveat| caveat.contains("changes no line of `src/ledger.rs`")),
            "{:?}",
            untouched.relationship_impact_caveats
        );

        // A symbol alone analyzes the file that defines it, focused on the symbol.
        let by_symbol = one_report(
            graph
                .answer(
                    root.path(),
                    ImpactRequest {
                        symbol: Some(&edited),
                        ..Default::default()
                    },
                )
                .unwrap(),
        );
        assert_eq!(by_symbol.target, "src/ledger.rs");
        assert!(lists_close(&by_symbol), "{:?}", by_symbol.proven_impact);

        // Nothing to analyze is the caller's mistake.
        assert!(matches!(
            graph.answer(root.path(), ImpactRequest::default()),
            Err(OkError::InvalidInput(_))
        ));
    }

    /// An omitted count says how many entries the cap cut; the file count says how many whole
    /// dependent files the list does not name, which the entry count cannot tell apart.
    #[test]
    fn omitted_entries_count_the_dependent_files_the_list_does_not_name() {
        let mut graph = LedgerGraph::new();
        let settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        // One busy file with more callers than the cap, then three files of one caller each,
        // and the changed file's own callers, which go last.
        for index in 0..RELATIONSHIP_IMPACT_LIMIT + 3 {
            graph.call(
                "src/b_audit.rs",
                &format!("audit::trace_{index:02}"),
                &settle,
                false,
            );
        }
        for name in ["c_books", "d_close", "e_open"] {
            graph.call(
                &format!("src/{name}.rs"),
                &format!("{name}::run"),
                &settle,
                false,
            );
        }
        let report = graph.report(&ChangeFocus::default());
        assert_eq!(report.possible_impact.len(), RELATIONSHIP_IMPACT_LIMIT);
        assert_eq!(report.possible_impact_omitted, 6);
        // Every other file's first entry is kept, so every file is named.
        assert_eq!(report.possible_impact_omitted_files, 0);

        // More files than slots, every edge inside one 40-edge read: three entries in one file
        // and one in each of 28 others.
        let mut graph = LedgerGraph::new();
        let settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        for index in 0..3 {
            graph.call(
                "src/b_audit.rs",
                &format!("audit::trace_{index:02}"),
                &settle,
                false,
            );
        }
        for index in 0..RELATIONSHIP_IMPACT_LIMIT + 3 {
            graph.call(
                &format!("src/z_{index:02}.rs"),
                &format!("z_{index:02}::run"),
                &settle,
                false,
            );
        }
        let report = graph.report(&ChangeFocus::default());
        assert_eq!(report.possible_impact.len(), RELATIONSHIP_IMPACT_LIMIT);
        assert_eq!(report.possible_impact_omitted, 6);
        // 29 files with a first entry for 25 slots: four whole files are not named.
        assert_eq!(report.possible_impact_omitted_files, 4);
    }

    /// 26 proven callers in other files and one in the changed file: the cap keeps 25 other
    /// files, so one whole dependent file is unnamed. The changed file's own cut entry is not a
    /// dependent file, and counting it read as two.
    #[test]
    fn the_changed_file_is_not_counted_as_an_omitted_dependent_file() {
        let mut graph = LedgerGraph::new();
        let settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        for index in 0..RELATIONSHIP_IMPACT_LIMIT + 1 {
            graph.call(
                &format!("src/caller_{index:02}.rs"),
                &format!("caller_{index:02}::run"),
                &settle,
                true,
            );
        }
        graph.call("src/ledger.rs", "ledger::entry", &settle, true);
        let report = graph.report(&ChangeFocus::default());
        assert_eq!(report.proven_impact_omitted, 2);
        assert_eq!(report.proven_impact_omitted_files, 1);
    }

    /// A diff of an old revision can change hundreds of files, each a full impact read. A `since`
    /// request reports at most `DIFF_REPORT_LIMIT` of them, those whose touched symbols have the
    /// most proven dependents first, starts none after its deadline, and counts the rest.
    #[test]
    fn a_diff_reports_a_bounded_number_of_paths_ranked_by_proven_dependents() {
        let (graph, edited) = ledger_with_an_unread_symbol();
        let root = tempfile::tempdir().unwrap();
        let range = edited.range.clone().unwrap();
        let mut diff = (0..DIFF_REPORT_LIMIT + 4)
            .map(|index| diff_changing(&format!("src/other_{index:02}.rs"), None))
            .collect::<Vec<_>>();
        // Last in diff order, but its edited line touches `reopen`, which has a proven caller.
        diff.push(diff_changing(
            "src/ledger.rs",
            Some(LineRange::single(range.start)),
        ));
        // The index holds `src/ledger.rs` as it is on disk.
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/ledger.rs"), "ledger").unwrap();
        let mut graph = graph;
        graph.files[0].content_hash = format!("{:x}", Sha256::digest(b"ledger"));
        let request = |deadline| ImpactRequest {
            diff: Some(&diff),
            deadline,
            ..Default::default()
        };

        let ImpactAnswer::Diff(bounded) = graph.answer(root.path(), request(None)).unwrap() else {
            panic!("a diff alone answers per changed path");
        };
        assert_eq!(bounded.reports.len(), DIFF_REPORT_LIMIT);
        assert_eq!(bounded.reports_omitted, 5);
        assert_eq!(bounded.reports[0].target, "src/ledger.rs");
        assert!(
            bounded.caveats[0].contains(&format!("at most {DIFF_REPORT_LIMIT} are reported")),
            "{:?}",
            bounded.caveats
        );
        let shape = bounded.to_json("HEAD~9", &diff);
        assert_eq!(shape["impact_reports_omitted"], 5);
        assert_eq!(shape["changed_files"].as_array().unwrap().len(), diff.len());

        // A deadline already past: no path is ranked or started, and the answer says so.
        let ImpactAnswer::Diff(late) = graph
            .answer(root.path(), request(Some(std::time::Instant::now())))
            .unwrap()
        else {
            panic!("a diff alone answers per changed path");
        };
        assert!(late.reports.is_empty());
        assert_eq!(late.reports_omitted, diff.len());
        assert!(
            late.caveats[0].contains("not started within the time"),
            "{:?}",
            late.caveats
        );
        assert!(
            late.caveats[1].contains(&format!("{} changed path(s) were not ranked", diff.len())),
            "{:?}",
            late.caveats
        );
    }

    fn diff_deleting(path: &str) -> open_kioku_git::DiffFile {
        open_kioku_git::DiffFile {
            old_path: Some(PathBuf::from(path)),
            new_path: None,
            status: GitChangeKind::Deleted,
            rename_score: None,
            hunks: vec![open_kioku_git::DiffHunk {
                old_range: Some(LineRange { start: 1, end: 40 }),
                new_range: None,
            }],
        }
    }

    fn diff_renaming(old: &str, new: &str) -> open_kioku_git::DiffFile {
        open_kioku_git::DiffFile {
            old_path: Some(PathBuf::from(old)),
            new_path: Some(PathBuf::from(new)),
            status: GitChangeKind::Renamed,
            rename_score: Some(100),
            hunks: Vec::new(),
        }
    }

    fn diff_answer(
        graph: &LedgerGraph,
        root: &Path,
        diff: &[open_kioku_git::DiffFile],
    ) -> DiffImpact {
        let ImpactAnswer::Diff(answer) = graph
            .answer(
                root,
                ImpactRequest {
                    diff: Some(diff),
                    ..Default::default()
                },
            )
            .unwrap()
        else {
            panic!("a diff alone answers per changed path");
        };
        answer
    }

    /// A file the diff deletes was skipped: no report, and not counted among the paths left out,
    /// though its dependents are the ones most certain to break. It is reported from the
    /// dependents the index last held for it, every symbol it defined touched, so its proven
    /// dependents are listed whatever the cap, and ranked among the other changed paths.
    #[test]
    fn a_deleted_file_is_reported_from_its_last_indexed_dependents() {
        let mut graph = LedgerGraph::new();
        let settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Private);
        let reopen = graph.changed_symbol("reopen", open_kioku_core::Visibility::Public);
        graph.call("src/books.rs", "books::close", &settle, true);
        graph.call("src/books.rs", "books::reopen_all", &reopen, true);
        graph.call("src/audit.rs", "audit::trace", &settle, false);
        let root = tempfile::tempdir().unwrap();
        // Modified, with no dependents: first in diff order, ranked after the deletion.
        let diff = [
            diff_changing("src/audit.rs", Some(LineRange::single(1))),
            diff_deleting("src/ledger.rs"),
        ];

        let answer = diff_answer(&graph, root.path(), &diff);
        assert_eq!(answer.reports.len(), 2);
        assert_eq!(answer.reports_omitted, 0);
        assert!(answer.removed_paths_not_indexed.is_empty());
        let deleted = &answer.reports[0];
        assert_eq!(deleted.target, "src/ledger.rs");
        assert_eq!(
            listed(&deleted.proven_impact),
            ["books::close", "books::reopen_all"]
        );
        assert_eq!(listed(&deleted.possible_impact), ["audit::trace"]);
        let reads = deleted.relationship_impact_reads.clone().unwrap();
        assert_eq!((reads.symbols_total, reads.symbols_touched), (2, 2));
        assert!(
            deleted.relationship_impact_caveats[0].contains("the diff deletes `src/ledger.rs`"),
            "{:?}",
            deleted.relationship_impact_caveats
        );
        assert!(deleted.risk_report.reasons[0].contains("the diff deletes"));

        // `path` with the same diff gives the same report.
        let by_path = one_report(
            graph
                .answer(
                    root.path(),
                    ImpactRequest {
                        path: Some(Path::new("src/ledger.rs")),
                        diff: Some(&diff),
                        ..Default::default()
                    },
                )
                .unwrap(),
        );
        assert_eq!(by_path.proven_impact, deleted.proven_impact);
        assert_eq!(
            by_path.relationship_impact_caveats,
            deleted.relationship_impact_caveats
        );
    }

    /// Deleting a file removes the file itself, so what imports it by path breaks as surely as a
    /// caller of one of its symbols. Its importers are kept past the list cap, as the symbols'
    /// proven dependents are; cut at 25, a module imported by 30 files named only 25 of them.
    #[test]
    fn a_deleted_file_keeps_every_proven_importer() {
        let mut graph = LedgerGraph::new();
        graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        let importers = RELATIONSHIP_IMPACT_LIMIT + 5;
        for index in 0..importers {
            graph.import(&format!("src/importer_{index:02}.rs"));
        }
        let root = tempfile::tempdir().unwrap();
        let diff = [diff_deleting("src/ledger.rs")];

        let answer = diff_answer(&graph, root.path(), &diff);
        let deleted = &answer.reports[0];
        assert_eq!(deleted.proven_impact.len(), importers, "{deleted:#?}");
        assert_eq!(deleted.proven_impact_omitted, 0);

        // The same report for the path as a caller may spell it.
        let by_path = one_report(
            graph
                .answer(
                    root.path(),
                    ImpactRequest {
                        path: Some(Path::new("./src/ledger.rs")),
                        diff: Some(&diff),
                        ..Default::default()
                    },
                )
                .unwrap(),
        );
        assert_eq!(by_path.proven_impact, deleted.proven_impact);
        assert!(by_path.relationship_impact_caveats[0].contains("the diff deletes"));

        // An edit to the file's lines does not touch its importers: the cap cuts them.
        let edited = graph.report(&ChangeFocus::lines(vec![LineRange::single(1)]));
        assert_eq!(edited.proven_impact.len(), RELATIONSHIP_IMPACT_LIMIT);
        assert_eq!(edited.proven_impact_omitted, 5);
    }

    /// Once the index no longer holds a deleted path, it has no dependents to read. It is not a
    /// report, nor one left out by the cap: it is listed and counted apart, with its own caveat,
    /// so every changed path is accounted for.
    #[test]
    fn a_deleted_file_the_index_no_longer_holds_is_listed_apart() {
        let (graph, _) = ledger_with_an_unread_symbol();
        let root = tempfile::tempdir().unwrap();
        let mut diff = (0..DIFF_REPORT_LIMIT + 2)
            .map(|index| diff_changing(&format!("src/other_{index:02}.rs"), None))
            .collect::<Vec<_>>();
        diff.push(diff_deleting("src/gone.rs"));
        diff.push(diff_deleting("src/ledger.rs"));

        let answer = diff_answer(&graph, root.path(), &diff);
        assert_eq!(
            answer.removed_paths_not_indexed,
            [PathBuf::from("src/gone.rs")]
        );
        // The deletion the index still holds ranks first, by its proven dependents.
        assert_eq!(answer.reports[0].target, "src/ledger.rs");
        assert_eq!(answer.reports.len(), DIFF_REPORT_LIMIT);
        // Every changed path is a report, one left out, or one the index no longer holds.
        assert_eq!(
            answer.reports.len() + answer.reports_omitted + answer.removed_paths_not_indexed.len(),
            diff.len()
        );
        assert!(
            answer.caveats[0].contains(&format!(
                "cover {DIFF_REPORT_LIMIT} of the {} changed paths",
                diff.len()
            )),
            "{:?}",
            answer.caveats
        );
        assert!(
            answer.caveats[1]
                .contains("1 path(s) the diff deletes or renames away are not in the index"),
            "{:?}",
            answer.caveats
        );
        let shape = answer.to_json("HEAD~3", &diff);
        assert_eq!(
            shape["removed_paths_not_indexed"],
            serde_json::json!(["src/gone.rs"])
        );

        // `path` with the same diff says why the report has no dependents.
        let by_path = one_report(
            graph
                .answer(
                    root.path(),
                    ImpactRequest {
                        path: Some(Path::new("src/gone.rs")),
                        diff: Some(&diff),
                        ..Default::default()
                    },
                )
                .unwrap(),
        );
        assert!(
            by_path.relationship_impact_caveats[0]
                .contains("the diff deletes `src/gone.rs` and the index does not hold it"),
            "{:?}",
            by_path.relationship_impact_caveats
        );
    }

    /// A rename is a removal of its previous path beside a change to its new one. The previous
    /// path is reported from the dependents the index last held for it; the new one, which the
    /// index does not hold yet, is a changed path of its own.
    #[test]
    fn a_rename_reports_its_previous_path_as_a_removal() {
        let mut graph = LedgerGraph::new();
        let settle = graph.changed_symbol("settle", open_kioku_core::Visibility::Public);
        graph.call("src/books.rs", "books::close", &settle, true);
        let root = tempfile::tempdir().unwrap();
        let diff = [diff_renaming("src/ledger.rs", "src/journal.rs")];

        let answer = diff_answer(&graph, root.path(), &diff);
        let targets = answer
            .reports
            .iter()
            .map(|report| report.target.as_str())
            .collect::<Vec<_>>();
        assert_eq!(targets, ["src/ledger.rs", "src/journal.rs"]);
        assert_eq!(listed(&answer.reports[0].proven_impact), ["books::close"]);
        assert!(
            answer.reports[0].relationship_impact_caveats[0]
                .contains("the diff renames `src/ledger.rs` to `src/journal.rs`"),
            "{:?}",
            answer.reports[0].relationship_impact_caveats
        );
        assert_eq!(answer.reports[1].risk_report.level, "unknown");

        // Two files swapping names remove neither path.
        let swap = [
            diff_renaming("src/ledger.rs", "src/books.rs"),
            diff_renaming("src/books.rs", "src/ledger.rs"),
        ];
        assert!(removals(&swap).is_empty());
    }

    /// `since` compares against git history; outside a git work tree there is none, and an empty
    /// diff would read as a repository where nothing changed.
    #[test]
    fn a_revision_outside_a_git_repository_is_invalid_input() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            ImpactEngine::changes_since(root.path(), "HEAD"),
            Err(OkError::InvalidInput(message)) if message.contains("not a git repository")
        ));
    }
}
