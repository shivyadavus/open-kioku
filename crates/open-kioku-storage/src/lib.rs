use open_kioku_core::{
    AnalysisFact, CodeChunk, EvidenceSourceType, File, FileId, FileProvenance, GitCochangeEdge,
    GitCommitRecord, GraphEdge, GraphNode, HistorySnapshot, HistorySummary, ImpactReport, Import,
    IndexManifest, SearchResult, Symbol, SymbolId, SymbolOccurrence, SymbolProvenance, TestTarget,
};
use open_kioku_errors::{OkError, Result};
use std::path::Path;

pub trait MetadataStore: Send + Sync {
    fn initialize(&self) -> Result<()>;
    fn put_manifest(&self, manifest: &IndexManifest) -> Result<()>;
    fn manifest(&self) -> Result<Option<IndexManifest>>;
    fn replace_index(&self, data: IndexData<'_>) -> Result<()>;
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

pub trait GraphStore: Send + Sync {
    fn replace_graph(&self, nodes: &[GraphNode], edges: &[GraphEdge]) -> Result<()>;
    fn neighbors(&self, node: &str, limit: usize) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)>;
    fn shortest_path(&self, from: &str, to: &str, max_depth: usize) -> Result<Vec<GraphEdge>>;
}

pub trait HistoryStore: Send + Sync {
    fn put_history_snapshot(&self, snapshot: &HistorySnapshot) -> Result<()>;
    fn history_for_file(&self, path: &Path, limit: usize) -> Result<HistorySummary>;
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
    fn cochange_neighbors(&self, path: &Path, limit: usize) -> Result<Vec<GitCochangeEdge>>;
    fn recent_commits(&self, limit: usize) -> Result<Vec<GitCommitRecord>>;
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
