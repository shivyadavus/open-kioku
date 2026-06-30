use open_kioku_core::{
    AnalysisFact, ChurnSummary, CodeChunk, EvidenceSourceType, File, FileId, FileProvenance,
    GitCochangeEdge, GitCommitRecord, GraphEdge, GraphEdgeType, GraphNode, GraphNodeType,
    HistorySignalQuery, HistorySignalSummary, HistorySnapshot, HistorySummary, ImpactReport,
    Import, IndexManifest, ScoreComponent, SearchResult, SimilarChangeQuery, SimilarChangeReport,
    Symbol, SymbolId, SymbolOccurrence, SymbolProvenance, TestTarget,
};
use open_kioku_errors::{OkError, Result};
use std::collections::BTreeSet;
use std::path::Path;

pub trait MetadataStore: Send + Sync {
    fn initialize(&self) -> Result<()>;
    fn put_manifest(&self, manifest: &IndexManifest) -> Result<()>;
    fn manifest(&self) -> Result<Option<IndexManifest>>;
    fn replace_index(&self, data: IndexData<'_>) -> Result<()>;
    fn replace_files_index(&self, _update: PartialIndexUpdate<'_>) -> Result<()> {
        Err(OkError::Unsupported(
            "partial index replacement is not implemented by this metadata store".into(),
        ))
    }
    fn list_files(&self, limit: usize, offset: usize) -> Result<Vec<File>>;
    fn get_file_by_path(&self, path: &Path) -> Result<Option<File>>;
    fn list_symbols(&self, query: Option<&str>, limit: usize, offset: usize)
        -> Result<Vec<Symbol>>;
    fn symbol_by_id(&self, id: &SymbolId) -> Result<Option<Symbol>>;
    fn chunks_for_file(&self, file_id: &FileId) -> Result<Vec<CodeChunk>>;
    fn all_chunks(&self) -> Result<Vec<CodeChunk>>;
    fn tests(&self) -> Result<Vec<TestTarget>>;
    fn imports(&self) -> Result<Vec<Import>>;
    fn analysis_facts(
        &self,
        _source_type: Option<EvidenceSourceType>,
        _limit: usize,
    ) -> Result<Vec<AnalysisFact>> {
        Ok(Vec::new())
    }
    fn references_for_symbol(&self, id: &SymbolId, limit: usize) -> Result<Vec<SymbolOccurrence>>;
    fn occurrences_for_file(&self, file_id: &FileId) -> Result<Vec<SymbolOccurrence>>;
    fn symbols_for_file(&self, _file_id: &FileId) -> Result<Vec<Symbol>> {
        Ok(Vec::new())
    }
    fn find_chunks_containing(&self, query: &str, limit: usize) -> Result<Vec<CodeChunk>> {
        let chunks = self.all_chunks()?;
        let mut results = Vec::new();
        for chunk in chunks {
            if chunk.text.contains(query) {
                results.push(chunk);
                if results.len() >= limit {
                    break;
                }
            }
        }
        Ok(results)
    }
    fn find_files_by_path_pattern(&self, pattern: &str) -> Result<Vec<File>> {
        let files = self.list_files(usize::MAX, 0)?;
        let lower_pattern = pattern.to_ascii_lowercase();
        Ok(files
            .into_iter()
            .filter(|f| {
                f.path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .contains(&lower_pattern)
            })
            .collect())
    }
    fn tests_for_files(&self, file_ids: &[FileId]) -> Result<Vec<TestTarget>> {
        let tests = self.tests()?;
        let set = file_ids.iter().collect::<std::collections::HashSet<_>>();
        Ok(tests
            .into_iter()
            .filter(|t| set.contains(&t.file_id))
            .collect())
    }
}

pub struct IndexData<'a> {
    pub manifest: &'a IndexManifest,
    pub files: &'a [File],
    pub symbols: &'a [Symbol],
    pub chunks: &'a [CodeChunk],
    pub tests: &'a [TestTarget],
    pub imports: &'a [Import],
    pub occurrences: &'a [SymbolOccurrence],
    pub analysis_facts: &'a [AnalysisFact],
}

pub struct PartialIndexUpdate<'a> {
    pub manifest: &'a IndexManifest,
    pub changed_files: &'a [File],
    pub deleted_file_ids: &'a [FileId],
    pub symbols: &'a [Symbol],
    pub chunks: &'a [CodeChunk],
    pub tests: &'a [TestTarget],
    pub imports: &'a [Import],
    pub occurrences: &'a [SymbolOccurrence],
    pub analysis_facts: &'a [AnalysisFact],
    pub graph_nodes: &'a [GraphNode],
    pub graph_edges: &'a [GraphEdge],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexChangeKind {
    Unchanged,
    Modified,
    Added,
    Deleted,
    Renamed,
    ModeSkipped,
    ParserVersionStale,
    SchemaVersionStale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexChange {
    pub old_path: Option<std::path::PathBuf>,
    pub new_path: Option<std::path::PathBuf>,
    pub file_id: Option<FileId>,
    pub kind: IndexChangeKind,
}

pub fn classify_file_changes(
    previous_manifest: Option<&IndexManifest>,
    next_manifest: &IndexManifest,
    previous_files: &[File],
    next_files: &[File],
) -> Vec<IndexChange> {
    classify_file_changes_with_parser_version(
        previous_manifest,
        next_manifest,
        previous_files,
        next_files,
        None,
        None,
    )
}

pub fn classify_file_changes_with_parser_version(
    previous_manifest: Option<&IndexManifest>,
    next_manifest: &IndexManifest,
    previous_files: &[File],
    next_files: &[File],
    previous_parser_version: Option<&str>,
    next_parser_version: Option<&str>,
) -> Vec<IndexChange> {
    if previous_manifest
        .is_some_and(|manifest| manifest.schema_version != next_manifest.schema_version)
    {
        return next_files
            .iter()
            .map(|file| IndexChange {
                old_path: Some(file.path.clone()),
                new_path: Some(file.path.clone()),
                file_id: Some(file.id.clone()),
                kind: IndexChangeKind::SchemaVersionStale,
            })
            .collect();
    }
    if previous_parser_version
        .zip(next_parser_version)
        .is_some_and(|(previous, next)| previous != next)
    {
        return next_files
            .iter()
            .map(|file| IndexChange {
                old_path: Some(file.path.clone()),
                new_path: Some(file.path.clone()),
                file_id: Some(file.id.clone()),
                kind: IndexChangeKind::ParserVersionStale,
            })
            .collect();
    }
    if previous_manifest.is_some_and(|manifest| manifest.index_mode != next_manifest.index_mode) {
        return next_files
            .iter()
            .map(|file| IndexChange {
                old_path: Some(file.path.clone()),
                new_path: Some(file.path.clone()),
                file_id: Some(file.id.clone()),
                kind: IndexChangeKind::ModeSkipped,
            })
            .collect();
    }

    let previous_by_id = previous_files
        .iter()
        .map(|file| (&file.id, file))
        .collect::<std::collections::BTreeMap<_, _>>();
    let next_by_id = next_files
        .iter()
        .map(|file| (&file.id, file))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut changes = Vec::new();
    for file in next_files {
        let kind = match previous_by_id.get(&file.id) {
            None => IndexChangeKind::Added,
            Some(previous) if previous.path != file.path => IndexChangeKind::Renamed,
            Some(previous) if previous.content_hash != file.content_hash => {
                IndexChangeKind::Modified
            }
            Some(_) => IndexChangeKind::Unchanged,
        };
        let old_path = previous_by_id.get(&file.id).map(|file| file.path.clone());
        changes.push(IndexChange {
            old_path,
            new_path: Some(file.path.clone()),
            file_id: Some(file.id.clone()),
            kind,
        });
    }
    for file in previous_files {
        if !next_by_id.contains_key(&file.id) {
            changes.push(IndexChange {
                old_path: Some(file.path.clone()),
                new_path: None,
                file_id: Some(file.id.clone()),
                kind: IndexChangeKind::Deleted,
            });
        }
    }
    changes.sort_by(|left, right| {
        left.new_path
            .as_ref()
            .or(left.old_path.as_ref())
            .cmp(&right.new_path.as_ref().or(right.old_path.as_ref()))
    });
    changes
}

pub fn partial_index_supported(previous: Option<&IndexManifest>, next: &IndexManifest) -> bool {
    previous.is_some_and(|previous| {
        previous.schema_version == next.schema_version && previous.index_mode == next.index_mode
    })
}

pub fn partial_index_supported_for_versions(
    previous: Option<&IndexManifest>,
    next: &IndexManifest,
    previous_parser_version: Option<&str>,
    next_parser_version: Option<&str>,
) -> bool {
    partial_index_supported(previous, next)
        && previous_parser_version
            .zip(next_parser_version)
            .map(|(previous, next)| previous == next)
            .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::{
        classify_file_changes, classify_file_changes_with_parser_version, IndexChangeKind,
    };
    use chrono::Utc;
    use open_kioku_core::{
        File, FileId, IndexManifest, IndexQuality, Language, Repository, RepositoryId,
    };
    use std::path::PathBuf;

    #[test]
    fn classifies_added_modified_deleted_and_renamed_files() {
        let previous = vec![
            file("stable", "src/stable.rs", "a"),
            file("modified", "src/modified.rs", "a"),
            file("renamed", "src/old.rs", "a"),
            file("deleted", "src/deleted.rs", "a"),
        ];
        let next = vec![
            file("stable", "src/stable.rs", "a"),
            file("modified", "src/modified.rs", "b"),
            file("renamed", "src/new.rs", "a"),
            file("added", "src/added.rs", "a"),
        ];

        let changes = classify_file_changes(Some(&manifest(1)), &manifest(1), &previous, &next);

        assert!(changes
            .iter()
            .any(|change| change.kind == IndexChangeKind::Unchanged
                && change.new_path.as_deref() == Some(std::path::Path::new("src/stable.rs"))));
        assert!(changes
            .iter()
            .any(|change| change.kind == IndexChangeKind::Modified
                && change.new_path.as_deref() == Some(std::path::Path::new("src/modified.rs"))));
        assert!(changes
            .iter()
            .any(|change| change.kind == IndexChangeKind::Renamed
                && change.old_path.as_deref() == Some(std::path::Path::new("src/old.rs"))
                && change.new_path.as_deref() == Some(std::path::Path::new("src/new.rs"))));
        assert!(changes
            .iter()
            .any(|change| change.kind == IndexChangeKind::Added
                && change.new_path.as_deref() == Some(std::path::Path::new("src/added.rs"))));
        assert!(changes
            .iter()
            .any(|change| change.kind == IndexChangeKind::Deleted
                && change.old_path.as_deref() == Some(std::path::Path::new("src/deleted.rs"))));
    }

    #[test]
    fn schema_and_parser_version_changes_force_stale_classification() {
        let previous = vec![file("f1", "src/lib.rs", "a")];
        let next = vec![file("f1", "src/lib.rs", "b")];

        let schema_changes =
            classify_file_changes(Some(&manifest(1)), &manifest(2), &previous, &next);
        assert_eq!(schema_changes[0].kind, IndexChangeKind::SchemaVersionStale);

        let parser_changes = classify_file_changes_with_parser_version(
            Some(&manifest(1)),
            &manifest(1),
            &previous,
            &next,
            Some("parser-a"),
            Some("parser-b"),
        );
        assert_eq!(parser_changes[0].kind, IndexChangeKind::ParserVersionStale);
    }

    fn manifest(schema_version: u32) -> IndexManifest {
        IndexManifest {
            repository: Repository {
                id: RepositoryId::new("repo"),
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: Some(Utc::now()),
            },
            file_count: 0,
            symbol_count: 0,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        }
    }

    fn file(id: &str, path: &str, hash: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 10,
            content_hash: hash.into(),
            is_generated: false,
            is_vendor: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphCounts {
    pub nodes: usize,
    pub edges: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphSchemaCounts {
    pub node_types: std::collections::BTreeMap<String, usize>,
    pub edge_types: std::collections::BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Default)]
pub struct TypeStats {
    pub count: usize,
    pub evidence_available: bool,
    pub freshness: Option<u64>,
}

pub trait GraphStore: Send + Sync {
    fn replace_graph(&self, nodes: &[GraphNode], edges: &[GraphEdge]) -> Result<()>;
    fn node_by_id(&self, _id: &str) -> Result<Option<GraphNode>> {
        Err(OkError::Unsupported(
            "node_by_id is not implemented by this graph store".into(),
        ))
    }
    fn neighbors(&self, node: &str, limit: usize) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)>;
    fn shortest_path(&self, from: &str, to: &str, max_depth: usize) -> Result<Vec<GraphEdge>>;

    fn node_type_stats(&self) -> Result<std::collections::HashMap<String, TypeStats>> {
        Ok(std::collections::HashMap::new())
    }

    fn edge_type_stats(&self) -> Result<std::collections::HashMap<String, TypeStats>> {
        Ok(std::collections::HashMap::new())
    }

    fn nodes_by_type(
        &self,
        _node_type: GraphNodeType,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<GraphNode>> {
        Err(OkError::Unsupported(
            "nodes_by_type is not implemented by this graph store".into(),
        ))
    }

    fn all_graph_nodes(&self) -> Result<Vec<GraphNode>> {
        Err(OkError::Unsupported(
            "all_graph_nodes is not implemented by this graph store".into(),
        ))
    }

    fn edges_by_type(
        &self,
        _edge_type: GraphEdgeType,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<GraphEdge>> {
        Err(OkError::Unsupported(
            "edges_by_type is not implemented by this graph store".into(),
        ))
    }

    fn graph_counts(&self) -> Result<GraphCounts> {
        Err(OkError::Unsupported(
            "graph_counts is not implemented by this graph store".into(),
        ))
    }

    fn graph_schema_counts(&self) -> Result<GraphSchemaCounts> {
        Err(OkError::Unsupported(
            "graph_schema_counts is not implemented by this graph store".into(),
        ))
    }

    fn graph_edges_between(&self, _from: &str, _to: &str, _limit: usize) -> Result<Vec<GraphEdge>> {
        Err(OkError::Unsupported(
            "graph_edges_between is not implemented by this graph store".into(),
        ))
    }
}

pub trait HistoryStore: Send + Sync {
    fn put_history_snapshot(&self, snapshot: &HistorySnapshot) -> Result<()>;
    fn history_for_file(&self, path: &Path, limit: usize) -> Result<HistorySummary>;
    fn churn_for_file(&self, _path: &Path) -> Result<ChurnSummary> {
        Err(OkError::Unsupported(
            "file churn lookup is not implemented by this history store".into(),
        ))
    }
    fn churn_for_module(&self, _module: &Path) -> Result<ChurnSummary> {
        Err(OkError::Unsupported(
            "module churn lookup is not implemented by this history store".into(),
        ))
    }
    fn churn_for_symbol(&self, _symbol_id: &SymbolId) -> Result<ChurnSummary> {
        Err(OkError::Unsupported(
            "symbol churn lookup is not implemented by this history store".into(),
        ))
    }
    fn provenance_for_path(&self, _path: &Path, _limit: usize) -> Result<FileProvenance> {
        Err(OkError::Unsupported(
            "file provenance lookup is not implemented by this history store".into(),
        ))
    }
    fn provenance_for_symbol(
        &self,
        _symbol_id: &SymbolId,
        _limit: usize,
    ) -> Result<SymbolProvenance> {
        Err(OkError::Unsupported(
            "symbol provenance lookup is not implemented by this history store".into(),
        ))
    }
    fn similar_changes(
        &self,
        _query: &SimilarChangeQuery,
        _limit: usize,
    ) -> Result<SimilarChangeReport> {
        Err(OkError::Unsupported(
            "similar historical change lookup is not implemented by this history store".into(),
        ))
    }
    fn history_score_components(
        &self,
        query: &HistorySignalQuery,
        limit: usize,
    ) -> Result<HistorySignalSummary> {
        Ok(history_signal_summary(self, query, limit))
    }
    fn cochange_neighbors(&self, path: &Path, limit: usize) -> Result<Vec<GitCochangeEdge>>;
    fn recent_commits(&self, limit: usize) -> Result<Vec<GitCommitRecord>>;
}

fn history_signal_summary<T: HistoryStore + ?Sized>(
    store: &T,
    query: &HistorySignalQuery,
    limit: usize,
) -> HistorySignalSummary {
    let limit = limit.clamp(1, 25);
    let mut summary = HistorySignalSummary::empty(query.path.clone());
    summary.uncertainty.clear();

    match store.churn_for_file(&query.path) {
        Ok(churn) => add_churn_signal(&mut summary, &churn),
        Err(err) => summary
            .uncertainty
            .push(format!("history_churn unavailable: {err}")),
    }

    match store.history_for_file(&query.path, limit) {
        Ok(history) => add_history_summary_signals(&mut summary, &history),
        Err(err) => summary
            .uncertainty
            .push(format!("history summary unavailable: {err}")),
    }

    let similar_query = SimilarChangeQuery {
        task: query.task.clone(),
        paths: vec![query.path.clone()],
        symbols: query.symbols.clone(),
    };
    match store.similar_changes(&similar_query, 5.min(limit)) {
        Ok(report) => add_similar_change_signal(&mut summary, &report),
        Err(err) => summary
            .uncertainty
            .push(format!("similar_change_overlap unavailable: {err}")),
    }

    summary.evidence_refs.sort();
    summary.evidence_refs.dedup();
    summary.reasons.sort();
    summary.reasons.dedup();
    summary.uncertainty.sort();
    summary.uncertainty.dedup();
    if summary.components.is_empty() && summary.uncertainty.is_empty() {
        summary
            .uncertainty
            .push("no bounded history signals were available for this path".into());
    }
    summary
}

fn add_churn_signal(summary: &mut HistorySignalSummary, churn: &ChurnSummary) {
    if churn.stats.touch_count == 0 {
        summary.uncertainty.extend(churn.uncertainty.clone());
        return;
    }
    let contribution = if churn.stats.hotspot_score >= 3.0 {
        0.12
    } else if churn.stats.hotspot_score >= 1.5 {
        0.07
    } else {
        0.03
    };
    let evidence_id = format!("history-churn:{}", churn.key);
    summary.evidence_refs.push(evidence_id.clone());
    summary.reasons.push(format!(
        "history churn: hotspot {:.2} from {} touch(es)",
        churn.stats.hotspot_score, churn.stats.touch_count
    ));
    summary.components.push(ScoreComponent::adjustment(
        "history_churn",
        contribution,
        vec![evidence_id],
        "bounded churn/hotspot signal from persisted local git history",
    ));
    summary.uncertainty.extend(churn.uncertainty.clone());
}

fn add_history_summary_signals(summary: &mut HistorySignalSummary, history: &HistorySummary) {
    let author_count = history
        .recent_commits
        .iter()
        .map(|commit| owner_identity(&commit.author))
        .filter(|identity| !identity.is_empty())
        .collect::<BTreeSet<_>>()
        .len();
    let reviewer_count = history
        .reviewer_evidence
        .iter()
        .map(|reviewer| owner_identity(&reviewer.reviewer))
        .filter(|identity| !identity.is_empty())
        .collect::<BTreeSet<_>>()
        .len();
    summary.distinct_author_count = author_count;
    summary.reviewer_count = reviewer_count;

    if author_count > 0 {
        let contribution = if author_count >= 4 {
            0.12
        } else if author_count >= 2 {
            0.07
        } else if reviewer_count == 0 && !history.file_touches.is_empty() {
            0.04
        } else {
            0.0
        };
        if contribution > 0.0 {
            let evidence_ids = history
                .recent_commits
                .iter()
                .take(5)
                .map(|commit| format!("history-author:{}", commit.id.0))
                .collect::<Vec<_>>();
            summary.evidence_refs.extend(evidence_ids.clone());
            summary.reasons.push(format!(
                "ownership risk: {} distinct historical author(s), {} reviewer(s)",
                author_count, reviewer_count
            ));
            summary.components.push(ScoreComponent::adjustment(
                "ownership_risk",
                contribution,
                evidence_ids,
                "bounded ownership risk from dispersed local author history",
            ));
        }
    }

    if reviewer_count > 0 {
        let contribution = (0.04 + reviewer_count.min(3) as f32 * 0.02).min(0.10);
        let evidence_ids = history
            .reviewer_evidence
            .iter()
            .take(5)
            .map(|review| format!("history-reviewer:{}", review.id.0))
            .collect::<Vec<_>>();
        summary.evidence_refs.extend(evidence_ids.clone());
        summary.reasons.push(format!(
            "reviewer affinity: {} historical reviewer signal(s)",
            reviewer_count
        ));
        summary.components.push(ScoreComponent::adjustment(
            "reviewer_affinity",
            contribution,
            evidence_ids,
            "bounded reviewer affinity from local review and owner history",
        ));
    }

    if !history.cochange_neighbors.is_empty() {
        let contribution =
            (history.cochange_neighbors.iter().take(3).count() as f32 * 0.04).min(0.12);
        let evidence_ids = history
            .cochange_neighbors
            .iter()
            .take(5)
            .map(|edge| format!("history-cochange:{}", edge.id.0))
            .collect::<Vec<_>>();
        summary.evidence_refs.extend(evidence_ids.clone());
        summary.reasons.push(format!(
            "similar change overlap: {} persisted co-change neighbor(s)",
            history.cochange_neighbors.len()
        ));
        summary.components.push(ScoreComponent::adjustment(
            "similar_change_overlap",
            contribution,
            evidence_ids,
            "bounded co-change overlap from persisted local history",
        ));
    }

    summary.uncertainty.extend(history.uncertainty.clone());
    if history.truncated {
        summary
            .uncertainty
            .push("history signal inputs were truncated".into());
    }
}

fn add_similar_change_signal(summary: &mut HistorySignalSummary, report: &SimilarChangeReport) {
    summary.similar_change_count = report.hits.len();
    if let Some(top_hit) = report.hits.first() {
        let contribution = (top_hit.score.clamp(0.0, 1.0) * 0.18).max(0.05);
        let evidence_ids = report
            .hits
            .iter()
            .take(5)
            .map(|hit| format!("history-similar:{}", hit.change.commit.id.0))
            .collect::<Vec<_>>();
        summary.evidence_refs.extend(evidence_ids.clone());
        summary.reasons.push(format!(
            "similar change overlap: {} similar historical change(s), top `{}`",
            report.hits.len(),
            top_hit.change.commit.summary
        ));
        summary.components.push(ScoreComponent::adjustment(
            "similar_change_overlap",
            contribution.min(0.18),
            evidence_ids,
            "bounded similar-change overlap from persisted local history",
        ));
    }
    summary.uncertainty.extend(report.uncertainty.clone());
    if report.truncated {
        summary
            .uncertainty
            .push("similar change signal inputs were truncated".into());
    }
}

fn owner_identity(owner: &open_kioku_core::Owner) -> String {
    owner
        .email
        .as_deref()
        .filter(|email| !email.trim().is_empty())
        .unwrap_or(&owner.name)
        .trim()
        .to_ascii_lowercase()
}

pub trait SearchIndex: Send + Sync {
    fn rebuild(&mut self, chunks: &[CodeChunk], files: &[File], symbols: &[Symbol]) -> Result<()>;
    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>>;
}

pub trait ImpactStore: Send + Sync {
    fn impact_for_file(&self, path: &Path) -> Result<ImpactReport>;
}

/// Combined store trait for types that implement both metadata and graph storage.
pub trait OkStore: MetadataStore + GraphStore {}
impl<T: MetadataStore + GraphStore> OkStore for T {}
