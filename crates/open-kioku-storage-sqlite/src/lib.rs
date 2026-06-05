use open_kioku_core::{
    AnalysisFact, CodeChunk, EvidenceSourceType, File, FileId, GraphEdge, GraphNode, Import,
    IndexManifest, Symbol, SymbolId, SymbolOccurrence, TestTarget,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_storage::{GraphStore, IndexData, MetadataStore};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct SqliteStore {
    path: PathBuf,
    connection: Mutex<Connection>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(storage_err)?;
        let store = Self {
            path,
            connection: Mutex::new(connection),
        };
        store.initialize()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl MetadataStore for SqliteStore {
    fn initialize(&self) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS manifests (
              id INTEGER PRIMARY KEY CHECK (id = 1),
              json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS files (
              id TEXT PRIMARY KEY,
              path TEXT NOT NULL UNIQUE,
              json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS symbols (
              id TEXT PRIMARY KEY,
              name TEXT NOT NULL,
              qualified_name TEXT NOT NULL,
              file_id TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
            CREATE TABLE IF NOT EXISTS chunks (
              id TEXT PRIMARY KEY,
              file_id TEXT NOT NULL,
              start_line INTEGER NOT NULL,
              end_line INTEGER NOT NULL,
              text TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_id);
            CREATE TABLE IF NOT EXISTS tests (
              id TEXT PRIMARY KEY,
              file_id TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tests_file ON tests(file_id);
            CREATE TABLE IF NOT EXISTS imports (
              id TEXT PRIMARY KEY,
              file_id TEXT NOT NULL,
              imported TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_imports_file ON imports(file_id);
            CREATE TABLE IF NOT EXISTS occurrences (
              id TEXT PRIMARY KEY,
              symbol_id TEXT NOT NULL,
              file_id TEXT NOT NULL,
              is_definition INTEGER NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_occurrences_symbol ON occurrences(symbol_id);
            CREATE INDEX IF NOT EXISTS idx_occurrences_file ON occurrences(file_id);
            CREATE TABLE IF NOT EXISTS analysis_facts (
              id TEXT PRIMARY KEY,
              file_id TEXT NOT NULL,
              source_type TEXT NOT NULL,
              target TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_analysis_facts_file ON analysis_facts(file_id);
            CREATE INDEX IF NOT EXISTS idx_analysis_facts_source ON analysis_facts(source_type);
            CREATE TABLE IF NOT EXISTS vector_targets (
              id TEXT PRIMARY KEY,
              file_id TEXT NOT NULL,
              target_kind TEXT NOT NULL,
              content_hash TEXT NOT NULL,
              vector_id INTEGER NOT NULL,
              model TEXT NOT NULL,
              dimensions INTEGER NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_vector_targets_file ON vector_targets(file_id);
            CREATE TABLE IF NOT EXISTS embedding_cache (
              cache_key TEXT PRIMARY KEY,
              target_id TEXT NOT NULL,
              content_hash TEXT NOT NULL,
              model TEXT NOT NULL,
              dimensions INTEGER NOT NULL,
              json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS semantic_index_runs (
              id TEXT PRIMARY KEY,
              status TEXT NOT NULL,
              model TEXT NOT NULL,
              dimensions INTEGER NOT NULL,
              vector_count INTEGER NOT NULL,
              created_at TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS semantic_coverage (
              id TEXT PRIMARY KEY,
              target_kind TEXT NOT NULL,
              indexed_count INTEGER NOT NULL,
              stale_count INTEGER NOT NULL,
              failed_count INTEGER NOT NULL,
              json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS graph_nodes (
              id TEXT PRIMARY KEY,
              label TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS graph_edges (
              id TEXT PRIMARY KEY,
              from_id TEXT NOT NULL,
              to_id TEXT NOT NULL,
              edge_type TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_graph_edges_from ON graph_edges(from_id);
            CREATE INDEX IF NOT EXISTS idx_graph_edges_to ON graph_edges(to_id);
            "#,
        )
        .map_err(storage_err)?;
        Ok(())
    }

    fn put_manifest(&self, manifest: &IndexManifest) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let json = serde_json::to_string(manifest)?;
        conn.execute(
            "INSERT INTO manifests(id, json) VALUES(1, ?1) ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![json],
        )
        .map_err(storage_err)?;
        Ok(())
    }

    fn manifest(&self) -> Result<Option<IndexManifest>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let raw: Option<String> = conn
            .query_row("SELECT json FROM manifests WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(storage_err)?;
        raw.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    fn replace_index(&self, data: IndexData<'_>) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        tx.execute("DELETE FROM occurrences", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM analysis_facts", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM imports", []).map_err(storage_err)?;
        tx.execute("DELETE FROM tests", []).map_err(storage_err)?;
        tx.execute("DELETE FROM chunks", []).map_err(storage_err)?;
        tx.execute("DELETE FROM symbols", []).map_err(storage_err)?;
        tx.execute("DELETE FROM files", []).map_err(storage_err)?;
        tx.execute("DELETE FROM manifests", [])
            .map_err(storage_err)?;
        tx.execute(
            "INSERT INTO manifests(id, json) VALUES(1, ?1)",
            params![serde_json::to_string(data.manifest)?],
        )
        .map_err(storage_err)?;
        for file in data.files {
            tx.execute(
                "INSERT INTO files(id, path, json) VALUES(?1, ?2, ?3)",
                params![
                    &file.id.0,
                    file.path.to_string_lossy().as_ref(),
                    serde_json::to_string(file)?
                ],
            )
            .map_err(storage_err)?;
        }
        for symbol in data.symbols {
            tx.execute(
                "INSERT INTO symbols(id, name, qualified_name, file_id, json) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    &symbol.id.0,
                    &symbol.name,
                    &symbol.qualified_name,
                    &symbol.file_id.0,
                    serde_json::to_string(symbol)?
                ],
            )
            .map_err(storage_err)?;
        }
        for chunk in data.chunks {
            tx.execute(
                "INSERT INTO chunks(id, file_id, start_line, end_line, text, json) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    &chunk.id,
                    &chunk.file_id.0,
                    chunk.range.start,
                    chunk.range.end,
                    &chunk.text,
                    serde_json::to_string(chunk)?
                ],
            )
            .map_err(storage_err)?;
        }
        for test in data.tests {
            tx.execute(
                "INSERT INTO tests(id, file_id, json) VALUES(?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET json = excluded.json",
                params![&test.id, &test.file_id.0, serde_json::to_string(test)?],
            )
            .map_err(storage_err)?;
        }
        for import in data.imports {
            tx.execute(
                "INSERT INTO imports(id, file_id, imported, json) VALUES(?1, ?2, ?3, ?4)",
                params![
                    occurrence_id(
                        &import.file_id.0,
                        &import.imported,
                        import.range.as_ref().map(|range| range.start),
                        true
                    ),
                    &import.file_id.0,
                    &import.imported,
                    serde_json::to_string(import)?
                ],
            )
            .map_err(storage_err)?;
        }
        for occurrence in data.occurrences {
            tx.execute(
                "INSERT INTO occurrences(id, symbol_id, file_id, is_definition, json) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    occurrence_id(
                        &occurrence.file_id.0,
                        &occurrence.symbol_id.0,
                        occurrence.range.as_ref().map(|range| range.start),
                        occurrence.is_definition,
                    ),
                    &occurrence.symbol_id.0,
                    &occurrence.file_id.0,
                    if occurrence.is_definition { 1 } else { 0 },
                    serde_json::to_string(occurrence)?
                ],
            )
            .map_err(storage_err)?;
        }
        for fact in data.analysis_facts {
            tx.execute(
                "INSERT INTO analysis_facts(id, file_id, source_type, target, json) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    &fact.id,
                    &fact.file_id.0,
                    source_type_name(&fact.source_type),
                    &fact.target,
                    serde_json::to_string(fact)?
                ],
            )
            .map_err(storage_err)?;
        }
        tx.commit().map_err(storage_err)?;
        Ok(())
    }

    fn list_files(&self, limit: usize, offset: usize) -> Result<Vec<File>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM files ORDER BY path LIMIT ?1 OFFSET ?2")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn get_file_by_path(&self, path: &Path) -> Result<Option<File>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT json FROM files WHERE path = ?1",
                params![path.to_string_lossy().as_ref()],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err)?;
        raw.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    fn list_symbols(
        &self,
        query: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Symbol>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let pattern = format!("%{}%", query.unwrap_or_default());
        let mut stmt = conn
            .prepare(
                "SELECT json FROM symbols WHERE (?1 = '%%' OR name LIKE ?1 COLLATE NOCASE OR qualified_name LIKE ?1 COLLATE NOCASE) ORDER BY qualified_name LIMIT ?2 OFFSET ?3",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![pattern, limit as i64, offset as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn symbol_by_id(&self, id: &SymbolId) -> Result<Option<Symbol>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT json FROM symbols WHERE id = ?1",
                params![&id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err)?;
        raw.map(|json| serde_json::from_str(&json).map_err(Into::into))
            .transpose()
    }

    fn chunks_for_file(&self, file_id: &FileId) -> Result<Vec<CodeChunk>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM chunks WHERE file_id = ?1 ORDER BY start_line")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![&file_id.0], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn all_chunks(&self) -> Result<Vec<CodeChunk>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM chunks ORDER BY file_id, start_line")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn tests(&self) -> Result<Vec<TestTarget>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM tests ORDER BY file_id")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn imports(&self) -> Result<Vec<Import>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM imports ORDER BY file_id")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn analysis_facts(
        &self,
        source_type: Option<EvidenceSourceType>,
        limit: usize,
    ) -> Result<Vec<AnalysisFact>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let limit = limit.min(i64::MAX as usize) as i64;
        let rows = if let Some(source_type) = source_type {
            let mut stmt = conn
                .prepare(
                    "SELECT json FROM analysis_facts WHERE source_type = ?1 ORDER BY file_id, target LIMIT ?2",
                )
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![source_type_name(&source_type), limit], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(storage_err)?;
            collect_json(rows)?
        } else {
            let mut stmt = conn
                .prepare("SELECT json FROM analysis_facts ORDER BY file_id, target LIMIT ?1")
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![limit], |row| row.get::<_, String>(0))
                .map_err(storage_err)?;
            collect_json(rows)?
        };
        Ok(rows)
    }

    fn references_for_symbol(&self, id: &SymbolId, limit: usize) -> Result<Vec<SymbolOccurrence>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT json FROM occurrences WHERE symbol_id = ?1 AND is_definition = 0 ORDER BY file_id LIMIT ?2",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![&id.0, limit as i64], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn occurrences_for_file(&self, file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM occurrences WHERE file_id = ?1 ORDER BY symbol_id")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![&file_id.0], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn symbols_for_file(&self, file_id: &FileId) -> Result<Vec<Symbol>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM symbols WHERE file_id = ?1 ORDER BY name")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![&file_id.0], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn find_chunks_containing(&self, query: &str, limit: usize) -> Result<Vec<CodeChunk>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let pattern = format!("%{}%", query);
        let mut stmt = conn
            .prepare("SELECT json FROM chunks WHERE text LIKE ?1 LIMIT ?2")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![pattern, limit as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn find_files_by_path_pattern(&self, pattern: &str) -> Result<Vec<File>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let match_pat = format!("%{}%", pattern);
        let mut stmt = conn
            .prepare("SELECT json FROM files WHERE path LIKE ?1 COLLATE NOCASE")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![match_pat], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn tests_for_files(&self, file_ids: &[FileId]) -> Result<Vec<TestTarget>> {
        if file_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;

        let placeholders = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!("SELECT json FROM tests WHERE file_id IN ({})", placeholders);
        let mut stmt = conn.prepare(&sql).map_err(storage_err)?;

        let params = rusqlite::params_from_iter(file_ids.iter().map(|id| &id.0));
        let rows = stmt
            .query_map(params, |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }
}

impl GraphStore for SqliteStore {
    fn replace_graph(&self, nodes: &[GraphNode], edges: &[GraphEdge]) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        tx.execute("DELETE FROM graph_edges", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM graph_nodes", [])
            .map_err(storage_err)?;
        for node in nodes {
            tx.execute(
                "INSERT INTO graph_nodes(id, label, json) VALUES(?1, ?2, ?3)",
                params![&node.id.0, &node.label, serde_json::to_string(node)?],
            )
            .map_err(storage_err)?;
        }
        for edge in edges {
            tx.execute(
                "INSERT INTO graph_edges(id, from_id, to_id, edge_type, json) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    &edge.id.0,
                    &edge.from.0,
                    &edge.to.0,
                    format!("{:?}", edge.edge_type),
                    serde_json::to_string(edge)?
                ],
            )
            .map_err(storage_err)?;
        }
        tx.commit().map_err(storage_err)?;
        Ok(())
    }

    fn neighbors(&self, node: &str, limit: usize) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM graph_edges WHERE from_id = ?1 OR to_id = ?1 LIMIT ?2")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![node, limit as i64], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        let edges: Vec<GraphEdge> = collect_json(rows)?;
        let mut ids = edges
            .iter()
            .flat_map(|edge| [edge.from.0.clone(), edge.to.0.clone()])
            .collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        let mut nodes = Vec::new();
        for id in ids {
            if let Some(node) = graph_node_by_id(&conn, &id)? {
                nodes.push(node);
            }
        }
        Ok((nodes, edges))
    }

    fn shortest_path(&self, from: &str, to: &str, max_depth: usize) -> Result<Vec<GraphEdge>> {
        use std::collections::{HashSet, VecDeque};

        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;

        // Prepare the statement once outside the BFS loop to avoid
        // O(N) statement recompilation on large graphs.
        let mut edge_stmt = conn
            .prepare("SELECT json FROM graph_edges WHERE from_id = ?1")
            .map_err(storage_err)?;

        let mut queue = VecDeque::from([(from.to_string(), Vec::<GraphEdge>::new())]);
        let mut seen = HashSet::new();
        while let Some((node, path)) = queue.pop_front() {
            if node == to {
                return Ok(path);
            }
            if path.len() >= max_depth || !seen.insert(node.clone()) {
                continue;
            }
            let rows = edge_stmt
                .query_map(params![&node], |row| row.get::<_, String>(0))
                .map_err(storage_err)?;
            let edges: Vec<GraphEdge> = collect_json(rows)?;
            for edge in edges {
                let mut next_path = path.clone();
                next_path.push(edge.clone());
                queue.push_back((edge.to.0.clone(), next_path));
            }
        }
        Ok(Vec::new())
    }
}

fn collect_json<T, F>(rows: rusqlite::MappedRows<'_, F>) -> Result<Vec<T>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<String>,
    T: serde::de::DeserializeOwned,
{
    let mut out = Vec::new();
    for row in rows {
        let raw = row.map_err(storage_err)?;
        out.push(serde_json::from_str(&raw)?);
    }
    Ok(out)
}

fn graph_node_by_id(conn: &Connection, id: &str) -> Result<Option<GraphNode>> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT json FROM graph_nodes WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_err)?;
    raw.map(|json| serde_json::from_str(&json).map_err(Into::into))
        .transpose()
}

fn storage_err(err: rusqlite::Error) -> OkError {
    OkError::Storage(err.to_string())
}

fn occurrence_id(file_id: &str, value: &str, line: Option<u32>, flag: bool) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(file_id.as_bytes());
    hasher.update(b":");
    hasher.update(value.as_bytes());
    hasher.update(b":");
    hasher.update(line.unwrap_or_default().to_string().as_bytes());
    hasher.update(b":");
    hasher.update(if flag { b"1" } else { b"0" });
    format!("{:x}", hasher.finalize())
}

fn source_type_name(source_type: &EvidenceSourceType) -> &'static str {
    match source_type {
        EvidenceSourceType::TreeSitter => "tree_sitter",
        EvidenceSourceType::Scip => "scip",
        EvidenceSourceType::Lsp => "lsp",
        EvidenceSourceType::Regex => "regex",
        EvidenceSourceType::Lexical => "lexical",
        EvidenceSourceType::Semantic => "semantic",
        EvidenceSourceType::Runtime => "runtime",
        EvidenceSourceType::GitHistory => "git_history",
        EvidenceSourceType::StaticAnalysis => "static_analysis",
        EvidenceSourceType::ExternalIntegration => "external_integration",
        EvidenceSourceType::Heuristic => "heuristic",
    }
}

#[cfg(test)]
mod tests {
    use super::SqliteStore;
    use chrono::Utc;
    use open_kioku_core::{
        AnalysisFact, Confidence, EdgeId, Evidence, EvidenceId, EvidenceSourceType, File, FileId,
        GraphEdge, GraphEdgeType, GraphNode, GraphNodeType, IndexManifest, IndexQuality, Language,
        LineRange, NodeId, Repository, RepositoryId, Symbol, SymbolId, SymbolKind,
    };
    use open_kioku_storage::{GraphStore, IndexData, MetadataStore};

    fn make_store() -> SqliteStore {
        SqliteStore::open(":memory:").expect("in-memory store")
    }

    fn make_file(id: &str, path: &str) -> File {
        File {
            id: FileId::new(id),
            repository_id: RepositoryId::new("repo"),
            path: path.into(),
            language: Language::Rust,
            size_bytes: 100,
            content_hash: format!("hash-{id}"),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn make_symbol(id: &str, name: &str, file_id: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("module::{name}"),
            kind: SymbolKind::Function,
            file_id: FileId::new(file_id),
            range: Some(LineRange::single(1)),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        }
    }

    fn evidence() -> Evidence {
        Evidence {
            id: EvidenceId::new("ev-1"),
            source: "test".into(),
            source_type: EvidenceSourceType::Lexical,
            file_range: None,
            symbol_id: None,
            confidence: Confidence::Medium,
            message: "test evidence".into(),
            indexed_at: Utc::now(),
        }
    }

    fn make_manifest() -> IndexManifest {
        IndexManifest {
            repository: Repository {
                id: RepositoryId::new("repo"),
                name: "repo".into(),
                root: std::path::PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 2,
            symbol_count: 2,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 1,
            quality: IndexQuality::default(),
        }
    }

    #[test]
    fn replace_index_and_list_files() {
        let store = make_store();
        let file1 = make_file("f1", "src/main.rs");
        let file2 = make_file("f2", "src/lib.rs");
        let sym1 = make_symbol("s1", "main_fn", "f1");

        let manifest = make_manifest();
        let files = vec![file1.clone(), file2.clone()];
        let symbols = vec![sym1.clone()];

        let data = IndexData {
            manifest: &manifest,
            files: &files,
            symbols: &symbols,
            occurrences: &[],
            chunks: &[],
            imports: &[],
            tests: &[],
            analysis_facts: &[],
        };
        store.replace_index(data).unwrap();

        let files_list = store.list_files(100, 0).unwrap();
        assert_eq!(files_list.len(), 2);

        let by_path = store
            .get_file_by_path(&std::path::PathBuf::from("src/main.rs"))
            .unwrap();
        assert!(by_path.is_some());
        assert_eq!(by_path.unwrap().id, file1.id);
    }

    #[test]
    fn replace_index_persists_analysis_facts() {
        let store = make_store();
        let file = make_file("f1", "src/handler.rs");
        let manifest = make_manifest();
        let runtime_fact = AnalysisFact {
            id: "runtime-1".into(),
            file_id: file.id.clone(),
            symbol_id: None,
            target: "GET /api/orders".into(),
            target_kind: GraphNodeType::Endpoint,
            edge_type: GraphEdgeType::ExposesEndpoint,
            range: Some(LineRange::single(12)),
            confidence: Confidence::High,
            source: "open-kioku-runtime:.ok/runtime/spans.jsonl".into(),
            source_type: EvidenceSourceType::Runtime,
            message: "runtime endpoint observed in local trace artifact".into(),
        };
        let static_fact = AnalysisFact {
            id: "static-1".into(),
            file_id: file.id.clone(),
            symbol_id: None,
            target: "orders".into(),
            target_kind: GraphNodeType::DatabaseTable,
            edge_type: GraphEdgeType::ReadsTable,
            range: None,
            confidence: Confidence::Medium,
            source: "open-kioku-static".into(),
            source_type: EvidenceSourceType::StaticAnalysis,
            message: "static fact".into(),
        };
        let git_fact = AnalysisFact {
            id: "git-1".into(),
            file_id: file.id.clone(),
            symbol_id: None,
            target: "tests/handler_test.rs".into(),
            target_kind: GraphNodeType::Test,
            edge_type: GraphEdgeType::ChangedBy,
            range: None,
            confidence: Confidence::High,
            source: "git-history:abc123".into(),
            source_type: EvidenceSourceType::GitHistory,
            message: "git co-change observed in 1 commit(s), recency weight 1.00".into(),
        };

        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[file],
                symbols: &[],
                occurrences: &[],
                chunks: &[],
                imports: &[],
                tests: &[],
                analysis_facts: &[runtime_fact.clone(), static_fact, git_fact.clone()],
            })
            .unwrap();

        let runtime = store
            .analysis_facts(Some(EvidenceSourceType::Runtime), 10)
            .unwrap();
        assert_eq!(runtime.len(), 1);
        assert_eq!(runtime[0].id, runtime_fact.id);
        assert_eq!(runtime[0].target, runtime_fact.target);
        let git = store
            .analysis_facts(Some(EvidenceSourceType::GitHistory), 10)
            .unwrap();
        assert_eq!(git.len(), 1);
        assert_eq!(git[0].id, git_fact.id);
        assert_eq!(git[0].target, git_fact.target);
        let all = store.analysis_facts(None, 10).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn list_symbols_with_filter() {
        let store = make_store();
        let file = make_file("f1", "src/lib.rs");
        let sym_a = make_symbol("s1", "alpha_handler", "f1");
        let sym_b = make_symbol("s2", "beta_worker", "f1");
        let manifest = make_manifest();
        let files = vec![file];
        let symbols = vec![sym_a, sym_b];
        let data = IndexData {
            manifest: &manifest,
            files: &files,
            symbols: &symbols,
            occurrences: &[],
            chunks: &[],
            imports: &[],
            tests: &[],
            analysis_facts: &[],
        };
        store.replace_index(data).unwrap();

        let all = store.list_symbols(None, 100, 0).unwrap();
        assert_eq!(all.len(), 2);

        let filtered = store.list_symbols(Some("alpha"), 10, 0).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "alpha_handler");
    }

    #[test]
    fn replace_graph_and_neighbors() {
        let store = make_store();
        // First we need an index so that the graph tables exist.
        let file = make_file("f1", "src/lib.rs");
        let manifest = make_manifest();
        let files = vec![file];
        let data = IndexData {
            manifest: &manifest,
            files: &files,
            symbols: &[],
            occurrences: &[],
            chunks: &[],
            imports: &[],
            tests: &[],
            analysis_facts: &[],
        };
        store.replace_index(data).unwrap();

        let node_a = GraphNode {
            id: NodeId::new("file:src/lib.rs"),
            node_type: GraphNodeType::File,
            label: "src/lib.rs".into(),
            file_id: Some(FileId::new("f1")),
            symbol_id: None,
        };
        let node_b = GraphNode {
            id: NodeId::new("symbol:s1"),
            node_type: GraphNodeType::Function,
            label: "worker".into(),
            file_id: Some(FileId::new("f1")),
            symbol_id: Some(SymbolId::new("s1")),
        };
        let edge = GraphEdge {
            id: EdgeId::new("e1"),
            from: node_a.id.clone(),
            to: node_b.id.clone(),
            edge_type: GraphEdgeType::Defines,
            evidence: evidence(),
        };

        store
            .replace_graph(
                &[node_a.clone(), node_b.clone()],
                std::slice::from_ref(&edge),
            )
            .unwrap();

        let (nodes, edges) = store.neighbors("file:src/lib.rs", 10).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].id.0, "e1");
        assert!(nodes.iter().any(|n| n.id == node_a.id));
    }

    #[test]
    fn shortest_path_finds_direct_route() {
        let store = make_store();
        let file = make_file("f1", "src/lib.rs");
        let manifest = make_manifest();
        let files = vec![file];
        let data = IndexData {
            manifest: &manifest,
            files: &files,
            symbols: &[],
            occurrences: &[],
            chunks: &[],
            imports: &[],
            tests: &[],
            analysis_facts: &[],
        };
        store.replace_index(data).unwrap();

        let node_a = GraphNode {
            id: NodeId::new("a"),
            node_type: GraphNodeType::File,
            label: "a".into(),
            file_id: None,
            symbol_id: None,
        };
        let node_b = GraphNode {
            id: NodeId::new("b"),
            node_type: GraphNodeType::File,
            label: "b".into(),
            file_id: None,
            symbol_id: None,
        };
        let edge = GraphEdge {
            id: EdgeId::new("a-b"),
            from: node_a.id.clone(),
            to: node_b.id.clone(),
            edge_type: GraphEdgeType::Defines,
            evidence: evidence(),
        };
        store.replace_graph(&[node_a, node_b], &[edge]).unwrap();

        let path = store.shortest_path("a", "b", 5).unwrap();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].id.0, "a-b");
    }

    #[test]
    fn shortest_path_returns_empty_when_no_route() {
        let store = make_store();
        let file = make_file("f1", "src/lib.rs");
        let manifest = make_manifest();
        let files = vec![file];
        let data = IndexData {
            manifest: &manifest,
            files: &files,
            symbols: &[],
            occurrences: &[],
            chunks: &[],
            imports: &[],
            tests: &[],
            analysis_facts: &[],
        };
        store.replace_index(data).unwrap();
        store.replace_graph(&[], &[]).unwrap();

        let path = store.shortest_path("x", "y", 5).unwrap();
        assert!(path.is_empty());
    }
}
