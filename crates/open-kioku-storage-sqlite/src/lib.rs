use chrono::{DateTime, Utc};
use open_kioku_core::{
    AnalysisFact, ChurnEntityKind, ChurnStats, ChurnSummary, CodeChunk, Confidence,
    EvidenceSourceType, File, FileId, FileProvenance, GitCochangeEdge, GitCommitId,
    GitCommitRecord, GitFileTouch, GitSymbolTouch, GraphEdge, GraphEdgeType, GraphNode,
    GraphNodeType, HistoricalChangeSummary, HistoryRecordId, HistorySnapshot, HistorySummary,
    Import, IndexManifest, ProvenanceTouch, SimilarChangeHit, SimilarChangeQuery,
    SimilarChangeReport, SimilarityEvidence, SimilarityEvidenceSource, Symbol, SymbolId,
    SymbolOccurrence, SymbolProvenance, TestTarget, HISTORY_SCHEMA_VERSION,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_storage::{
    GraphCounts, GraphSchemaCounts, GraphStore, HistoryStore, IndexData, MetadataStore,
    PartialIndexUpdate,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SQLITE_HISTORY_SCHEMA_VERSION: i64 = 1;
pub const SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION: i64 = 2;
const SQLITE_GRAPH_SCHEMA_VERSION: i64 = SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION;
const SQLITE_SUPPORTED_SCHEMA_VERSION: i64 = SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION;

const HISTORY_SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS git_commits (
  id TEXT PRIMARY KEY,
  authored_at TEXT NOT NULL,
  committed_at TEXT NOT NULL,
  author_email TEXT,
  json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_git_commits_committed_at
  ON git_commits(committed_at DESC, id);
CREATE INDEX IF NOT EXISTS idx_git_commits_author_email
  ON git_commits(author_email);

CREATE TABLE IF NOT EXISTS git_file_touches (
  id TEXT PRIMARY KEY,
  commit_id TEXT NOT NULL,
  path TEXT NOT NULL,
  previous_path TEXT,
  touched_at TEXT NOT NULL,
  json TEXT NOT NULL,
  FOREIGN KEY(commit_id) REFERENCES git_commits(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_git_file_touches_path
  ON git_file_touches(path, touched_at DESC);
CREATE INDEX IF NOT EXISTS idx_git_file_touches_previous_path
  ON git_file_touches(previous_path, touched_at DESC);
CREATE INDEX IF NOT EXISTS idx_git_file_touches_commit
  ON git_file_touches(commit_id);

CREATE TABLE IF NOT EXISTS git_symbol_touches (
  id TEXT PRIMARY KEY,
  commit_id TEXT NOT NULL,
  symbol_id TEXT,
  qualified_name TEXT NOT NULL,
  file_path TEXT NOT NULL,
  touched_at TEXT NOT NULL,
  json TEXT NOT NULL,
  FOREIGN KEY(commit_id) REFERENCES git_commits(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_git_symbol_touches_file
  ON git_symbol_touches(file_path, touched_at DESC);
CREATE INDEX IF NOT EXISTS idx_git_symbol_touches_symbol
  ON git_symbol_touches(symbol_id, touched_at DESC);
CREATE INDEX IF NOT EXISTS idx_git_symbol_touches_commit
  ON git_symbol_touches(commit_id);

CREATE TABLE IF NOT EXISTS git_cochange_edges (
  id TEXT PRIMARY KEY,
  path TEXT NOT NULL,
  cochanged_path TEXT NOT NULL,
  commit_count INTEGER NOT NULL,
  recency_weight REAL NOT NULL,
  last_changed_at TEXT,
  json TEXT NOT NULL,
  UNIQUE(path, cochanged_path)
);
CREATE INDEX IF NOT EXISTS idx_git_cochange_edges_path
  ON git_cochange_edges(path, recency_weight DESC, commit_count DESC);

CREATE TABLE IF NOT EXISTS git_review_events (
  id TEXT PRIMARY KEY,
  commit_id TEXT,
  path TEXT,
  reviewer_identity TEXT NOT NULL,
  observed_at TEXT NOT NULL,
  json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_git_review_events_path
  ON git_review_events(path, observed_at DESC);
CREATE INDEX IF NOT EXISTS idx_git_review_events_commit
  ON git_review_events(commit_id, observed_at DESC);
CREATE INDEX IF NOT EXISTS idx_git_review_events_reviewer
  ON git_review_events(reviewer_identity, observed_at DESC);

CREATE TABLE IF NOT EXISTS history_hotspots (
  entity_kind TEXT NOT NULL,
  entity_key TEXT NOT NULL,
  path TEXT,
  symbol_id TEXT,
  qualified_name TEXT,
  hotspot_score REAL NOT NULL,
  touch_count INTEGER NOT NULL,
  generated_at TEXT NOT NULL,
  json TEXT NOT NULL,
  PRIMARY KEY(entity_kind, entity_key)
);
CREATE INDEX IF NOT EXISTS idx_history_hotspots_kind_score
  ON history_hotspots(entity_kind, hotspot_score DESC, touch_count DESC, entity_key);
CREATE INDEX IF NOT EXISTS idx_history_hotspots_path
  ON history_hotspots(path);
CREATE INDEX IF NOT EXISTS idx_history_hotspots_symbol
  ON history_hotspots(symbol_id);
"#;

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

    fn churn_by_kind_and_key<F>(
        &self,
        kind: ChurnEntityKind,
        key: &str,
        missing: F,
    ) -> Result<ChurnSummary>
    where
        F: FnOnce() -> ChurnSummary,
    {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let raw = conn
            .query_row(
                "SELECT json FROM history_hotspots WHERE entity_kind = ?1 AND entity_key = ?2",
                params![churn_entity_kind_key(kind), key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_err)?;
        match raw {
            Some(raw) => Ok(serde_json::from_str(&raw)?),
            None => Ok(missing()),
        }
    }
}

impl MetadataStore for SqliteStore {
    fn initialize(&self) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        ensure_supported_sqlite_schema(&conn)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
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
              node_type TEXT DEFAULT '',
              file_id TEXT DEFAULT '',
              symbol_id TEXT DEFAULT '',
              json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS graph_edges (
              id TEXT PRIMARY KEY,
              from_id TEXT NOT NULL,
              to_id TEXT NOT NULL,
              edge_type TEXT NOT NULL,
              confidence TEXT DEFAULT '',
              source_type TEXT DEFAULT '',
              source_file TEXT DEFAULT '',
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_graph_edges_from ON graph_edges(from_id);
            CREATE INDEX IF NOT EXISTS idx_graph_edges_to ON graph_edges(to_id);
            "#,
        )
        .map_err(storage_err)?;
        migrate_history_schema(&mut conn)?;
        migrate_graph_schema(&mut conn)?;
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
        insert_index_rows(
            &tx,
            IndexRows {
                files: data.files,
                symbols: data.symbols,
                chunks: data.chunks,
                tests: data.tests,
                imports: data.imports,
                occurrences: data.occurrences,
                analysis_facts: data.analysis_facts,
            },
        )?;
        tx.commit().map_err(storage_err)?;
        Ok(())
    }

    fn replace_files_index(&self, update: PartialIndexUpdate<'_>) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        let affected_file_ids = update
            .changed_files
            .iter()
            .map(|file| file.id.clone())
            .chain(update.deleted_file_ids.iter().cloned())
            .collect::<BTreeSet<_>>();
        let mut affected_file_paths = update
            .changed_files
            .iter()
            .map(|file| file.path.to_string_lossy().to_string())
            .collect::<BTreeSet<_>>();
        for file_id in &affected_file_ids {
            let path: Option<String> = tx
                .query_row(
                    "SELECT path FROM files WHERE id = ?1",
                    params![&file_id.0],
                    |row| row.get(0),
                )
                .optional()
                .map_err(storage_err)?;
            if let Some(path) = path {
                affected_file_paths.insert(path);
            }
        }

        let mut affected_symbol_ids = update
            .symbols
            .iter()
            .map(|symbol| symbol.id.clone())
            .collect::<BTreeSet<_>>();
        for file_id in &affected_file_ids {
            let mut stmt = tx
                .prepare("SELECT id FROM symbols WHERE file_id = ?1")
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![&file_id.0], |row| row.get::<_, String>(0))
                .map_err(storage_err)?;
            for row in rows {
                affected_symbol_ids.insert(SymbolId::new(row.map_err(storage_err)?));
            }
        }

        let mut affected_node_ids = update
            .graph_nodes
            .iter()
            .map(|node| node.id.0.clone())
            .collect::<BTreeSet<_>>();
        for file_id in &affected_file_ids {
            let mut stmt = tx
                .prepare("SELECT id FROM graph_nodes WHERE file_id = ?1")
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![&file_id.0], |row| row.get::<_, String>(0))
                .map_err(storage_err)?;
            for row in rows {
                affected_node_ids.insert(row.map_err(storage_err)?);
            }
        }
        for symbol_id in &affected_symbol_ids {
            let mut stmt = tx
                .prepare("SELECT id FROM graph_nodes WHERE symbol_id = ?1")
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![&symbol_id.0], |row| row.get::<_, String>(0))
                .map_err(storage_err)?;
            for row in rows {
                affected_node_ids.insert(row.map_err(storage_err)?);
            }
        }

        tx.execute(
            "INSERT INTO manifests(id, json) VALUES(1, ?1)
             ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![serde_json::to_string(update.manifest)?],
        )
        .map_err(storage_err)?;

        for node_id in &affected_node_ids {
            tx.execute(
                "DELETE FROM graph_edges WHERE from_id = ?1 OR to_id = ?1",
                params![node_id],
            )
            .map_err(storage_err)?;
        }
        for path in &affected_file_paths {
            tx.execute(
                "DELETE FROM graph_edges WHERE source_file = ?1",
                params![path],
            )
            .map_err(storage_err)?;
        }
        for node_id in &affected_node_ids {
            tx.execute("DELETE FROM graph_nodes WHERE id = ?1", params![node_id])
                .map_err(storage_err)?;
        }
        for file_id in &affected_file_ids {
            tx.execute(
                "DELETE FROM graph_nodes WHERE file_id = ?1",
                params![&file_id.0],
            )
            .map_err(storage_err)?;
        }
        for symbol_id in &affected_symbol_ids {
            tx.execute(
                "DELETE FROM graph_nodes WHERE symbol_id = ?1",
                params![&symbol_id.0],
            )
            .map_err(storage_err)?;
        }

        for symbol_id in &affected_symbol_ids {
            tx.execute(
                "DELETE FROM occurrences WHERE symbol_id = ?1",
                params![&symbol_id.0],
            )
            .map_err(storage_err)?;
        }
        for file_id in &affected_file_ids {
            tx.execute(
                "DELETE FROM occurrences WHERE file_id = ?1",
                params![&file_id.0],
            )
            .map_err(storage_err)?;
            tx.execute(
                "DELETE FROM analysis_facts WHERE file_id = ?1",
                params![&file_id.0],
            )
            .map_err(storage_err)?;
            tx.execute(
                "DELETE FROM imports WHERE file_id = ?1",
                params![&file_id.0],
            )
            .map_err(storage_err)?;
            tx.execute("DELETE FROM tests WHERE file_id = ?1", params![&file_id.0])
                .map_err(storage_err)?;
            tx.execute("DELETE FROM chunks WHERE file_id = ?1", params![&file_id.0])
                .map_err(storage_err)?;
            tx.execute(
                "DELETE FROM symbols WHERE file_id = ?1",
                params![&file_id.0],
            )
            .map_err(storage_err)?;
            tx.execute("DELETE FROM files WHERE id = ?1", params![&file_id.0])
                .map_err(storage_err)?;
        }

        insert_index_rows(
            &tx,
            IndexRows {
                files: update.changed_files,
                symbols: update.symbols,
                chunks: update.chunks,
                tests: update.tests,
                imports: update.imports,
                occurrences: update.occurrences,
                analysis_facts: update.analysis_facts,
            },
        )?;
        insert_graph_rows(&tx, update.graph_nodes, update.graph_edges)?;
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

impl HistoryStore for SqliteStore {
    fn put_history_snapshot(&self, snapshot: &HistorySnapshot) -> Result<()> {
        validate_history_snapshot(snapshot)?;
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;

        tx.execute("DELETE FROM git_review_events", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM history_hotspots", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM git_cochange_edges", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM git_symbol_touches", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM git_file_touches", [])
            .map_err(storage_err)?;
        tx.execute("DELETE FROM git_commits", [])
            .map_err(storage_err)?;

        for commit in &snapshot.commits {
            tx.execute(
                "INSERT INTO git_commits(id, authored_at, committed_at, author_email, json) VALUES(?1, ?2, ?3, ?4, ?5)",
                params![
                    &commit.id.0,
                    commit.authored_at.to_rfc3339(),
                    commit.committed_at.to_rfc3339(),
                    commit.author.email.as_deref(),
                    serde_json::to_string(commit)?,
                ],
            )
            .map_err(storage_err)?;
        }
        for touch in &snapshot.file_touches {
            tx.execute(
                "INSERT INTO git_file_touches(id, commit_id, path, previous_path, touched_at, json) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    &touch.id.0,
                    &touch.commit_id.0,
                    history_path(&touch.path)?,
                    touch
                        .previous_path
                        .as_deref()
                        .map(history_path)
                        .transpose()?,
                    touch.touched_at.to_rfc3339(),
                    serde_json::to_string(touch)?,
                ],
            )
            .map_err(storage_err)?;
        }
        for touch in &snapshot.symbol_touches {
            tx.execute(
                "INSERT INTO git_symbol_touches(id, commit_id, symbol_id, qualified_name, file_path, touched_at, json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    &touch.id.0,
                    &touch.commit_id.0,
                    touch.symbol_id.as_ref().map(|id| id.0.as_str()),
                    &touch.qualified_name,
                    history_path(&touch.file_path)?,
                    touch.touched_at.to_rfc3339(),
                    serde_json::to_string(touch)?,
                ],
            )
            .map_err(storage_err)?;
        }
        for edge in &snapshot.cochange_edges {
            tx.execute(
                "INSERT INTO git_cochange_edges(id, path, cochanged_path, commit_count, recency_weight, last_changed_at, json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    &edge.id.0,
                    history_path(&edge.path)?,
                    history_path(&edge.cochanged_path)?,
                    usize_to_i64(edge.commit_count, "co-change commit count")?,
                    edge.recency_weight,
                    edge.last_changed_at.map(|value| value.to_rfc3339()),
                    serde_json::to_string(edge)?,
                ],
            )
            .map_err(storage_err)?;
        }
        for evidence in &snapshot.reviewer_evidence {
            let reviewer_identity = evidence
                .reviewer
                .email
                .as_deref()
                .unwrap_or(&evidence.reviewer.name);
            tx.execute(
                "INSERT INTO git_review_events(id, commit_id, path, reviewer_identity, observed_at, json) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    &evidence.id.0,
                    evidence.commit_id.as_ref().map(|id| id.0.as_str()),
                    evidence.path.as_deref().map(history_path).transpose()?,
                    reviewer_identity,
                    evidence.observed_at.to_rfc3339(),
                    serde_json::to_string(evidence)?,
                ],
            )
            .map_err(storage_err)?;
        }
        for summary in materialize_churn_summaries(snapshot)? {
            tx.execute(
                "INSERT INTO history_hotspots(entity_kind, entity_key, path, symbol_id, qualified_name, hotspot_score, touch_count, generated_at, json)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    churn_entity_kind_key(summary.entity_kind),
                    &summary.key,
                    summary.path.as_deref().map(history_path).transpose()?,
                    summary.symbol_id.as_ref().map(|id| id.0.as_str()),
                    summary.qualified_name.as_deref(),
                    summary.stats.hotspot_score,
                    usize_to_i64(summary.stats.touch_count, "history hotspot touch count")?,
                    summary.generated_at.to_rfc3339(),
                    serde_json::to_string(&summary)?,
                ],
            )
            .map_err(storage_err)?;
        }

        tx.commit().map_err(storage_err)?;
        Ok(())
    }

    fn history_for_file(&self, path: &Path, limit: usize) -> Result<HistorySummary> {
        let normalized_path = history_path(path)?;
        if limit == 0 {
            return Ok(HistorySummary {
                path: path.to_path_buf(),
                recent_commits: Vec::new(),
                file_touches: Vec::new(),
                symbol_touches: Vec::new(),
                cochange_neighbors: Vec::new(),
                reviewer_evidence: Vec::new(),
                truncated: false,
                uncertainty: vec!["history query limit is zero".into()],
            });
        }

        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let query_limit = history_query_limit(limit);

        let mut commit_stmt = conn
            .prepare(
                "SELECT c.json FROM git_commits c
                 WHERE EXISTS (
                   SELECT 1 FROM git_file_touches t
                   WHERE t.commit_id = c.id AND (t.path = ?1 OR t.previous_path = ?1)
                 )
                 ORDER BY c.committed_at DESC, c.id
                 LIMIT ?2",
            )
            .map_err(storage_err)?;
        let commit_rows = commit_stmt
            .query_map(params![&normalized_path, query_limit], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        let (recent_commits, commits_truncated) = collect_limited_json(commit_rows, limit)?;

        let mut file_touch_stmt = conn
            .prepare(
                "SELECT json FROM git_file_touches
                 WHERE path = ?1 OR previous_path = ?1
                 ORDER BY touched_at DESC, id
                 LIMIT ?2",
            )
            .map_err(storage_err)?;
        let file_touch_rows = file_touch_stmt
            .query_map(params![&normalized_path, query_limit], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        let (file_touches, file_touches_truncated) = collect_limited_json(file_touch_rows, limit)?;

        let mut symbol_touch_stmt = conn
            .prepare(
                "SELECT json FROM git_symbol_touches
                 WHERE file_path = ?1
                 ORDER BY touched_at DESC, id
                 LIMIT ?2",
            )
            .map_err(storage_err)?;
        let symbol_touch_rows = symbol_touch_stmt
            .query_map(params![&normalized_path, query_limit], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        let (symbol_touches, symbol_touches_truncated) =
            collect_limited_json(symbol_touch_rows, limit)?;

        let mut cochange_stmt = conn
            .prepare(
                "SELECT json FROM git_cochange_edges
                 WHERE path = ?1
                 ORDER BY recency_weight DESC, commit_count DESC, cochanged_path
                 LIMIT ?2",
            )
            .map_err(storage_err)?;
        let cochange_rows = cochange_stmt
            .query_map(params![&normalized_path, query_limit], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        let (cochange_neighbors, cochange_truncated) = collect_limited_json(cochange_rows, limit)?;

        let mut reviewer_stmt = conn
            .prepare(
                "SELECT e.json FROM git_review_events e
                 WHERE e.path = ?1
                    OR (
                      e.path IS NULL
                      AND e.commit_id IN (
                        SELECT t.commit_id FROM git_file_touches t
                        WHERE t.path = ?1 OR t.previous_path = ?1
                      )
                    )
                 ORDER BY e.observed_at DESC, e.id
                 LIMIT ?2",
            )
            .map_err(storage_err)?;
        let reviewer_rows = reviewer_stmt
            .query_map(params![&normalized_path, query_limit], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        let (reviewer_evidence, reviewers_truncated) = collect_limited_json(reviewer_rows, limit)?;

        let truncated = commits_truncated
            || file_touches_truncated
            || symbol_touches_truncated
            || cochange_truncated
            || reviewers_truncated;
        let mut uncertainty = Vec::new();
        if recent_commits.is_empty()
            && file_touches.is_empty()
            && symbol_touches.is_empty()
            && cochange_neighbors.is_empty()
            && reviewer_evidence.is_empty()
        {
            uncertainty.push("no persisted history evidence is available for this path".into());
        } else {
            if symbol_touches.is_empty() {
                uncertainty.push("no symbol-level history is stored for this path".into());
            }
            if reviewer_evidence.is_empty() {
                uncertainty.push("no reviewer or owner evidence is stored for this path".into());
            }
        }
        if truncated {
            uncertainty.push(format!(
                "history results are truncated to {limit} records per category"
            ));
        }

        Ok(HistorySummary {
            path: path.to_path_buf(),
            recent_commits,
            file_touches,
            symbol_touches,
            cochange_neighbors,
            reviewer_evidence,
            truncated,
            uncertainty,
        })
    }

    fn provenance_for_path(&self, path: &Path, limit: usize) -> Result<FileProvenance> {
        let normalized_path = history_path(path)?;
        if limit == 0 {
            return Ok(FileProvenance {
                path: path.to_path_buf(),
                first_seen: None,
                last_touched: None,
                recent_touches: Vec::new(),
                confidence: Confidence::Low,
                truncated: false,
                uncertainty: vec!["provenance query limit is zero".into()],
            });
        }

        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let query_limit = history_query_limit(limit);
        let aliases = "
            WITH RECURSIVE aliases(path) AS (
              SELECT ?1
              UNION
              SELECT t.previous_path
              FROM git_file_touches t JOIN aliases a ON t.path = a.path
              WHERE t.previous_path IS NOT NULL
              UNION
              SELECT t.path
              FROM git_file_touches t JOIN aliases a ON t.previous_path = a.path
            )";
        let recent_sql = format!(
            "{aliases}
             SELECT DISTINCT t.json, c.json
             FROM git_file_touches t
             JOIN git_commits c ON c.id = t.commit_id
             WHERE t.path IN aliases OR t.previous_path IN aliases
             ORDER BY t.touched_at DESC, t.id
             LIMIT ?2"
        );
        let mut recent_stmt = conn.prepare(&recent_sql).map_err(storage_err)?;
        let rows = recent_stmt
            .query_map(params![&normalized_path, query_limit], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(storage_err)?;
        let mut recent_touches = collect_provenance_rows(rows, file_provenance_touch)?;
        let truncated = recent_touches.len() > limit;
        recent_touches.truncate(limit);

        let first_sql = format!(
            "{aliases}
             SELECT DISTINCT t.json, c.json
             FROM git_file_touches t
             JOIN git_commits c ON c.id = t.commit_id
             WHERE t.path IN aliases OR t.previous_path IN aliases
             ORDER BY t.touched_at ASC, t.id
             LIMIT 1"
        );
        let first_seen = conn
            .query_row(&first_sql, params![&normalized_path], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .optional()
            .map_err(storage_err)?
            .map(|(touch, commit)| file_provenance_touch(&touch, &commit))
            .transpose()?;
        let last_touched = recent_touches.first().cloned();
        let mut uncertainty = Vec::new();
        if first_seen.is_none() {
            uncertainty.push("no persisted commit provenance is available for this path".into());
        } else if first_seen
            .as_ref()
            .is_some_and(|touch| touch.change_kind != open_kioku_core::GitChangeKind::Added)
        {
            uncertainty.push(
                "first_seen is the earliest persisted touch in the configured local history window, not a proven file-creation commit"
                    .into(),
            );
        }
        if truncated {
            uncertainty.push(format!(
                "recent provenance is truncated to {limit} touch records"
            ));
        }

        let confidence = if uncertainty.is_empty() {
            Confidence::Exact
        } else if last_touched.is_some() {
            Confidence::High
        } else {
            Confidence::Low
        };
        Ok(FileProvenance {
            path: path.to_path_buf(),
            first_seen,
            last_touched,
            recent_touches,
            confidence,
            truncated,
            uncertainty,
        })
    }

    fn churn_for_file(&self, path: &Path) -> Result<ChurnSummary> {
        let normalized_path = history_path(path)?;
        self.churn_by_kind_and_key(ChurnEntityKind::File, &normalized_path, || {
            ChurnSummary::missing(ChurnEntityKind::File, normalized_path.clone())
        })
    }

    fn churn_for_module(&self, module: &Path) -> Result<ChurnSummary> {
        let normalized_module = if module == Path::new(".") || module.as_os_str().is_empty() {
            "__root__".to_string()
        } else {
            history_path(module)?
        };
        self.churn_by_kind_and_key(ChurnEntityKind::Module, &normalized_module, || {
            ChurnSummary::missing(ChurnEntityKind::Module, normalized_module.clone())
        })
    }

    fn churn_for_symbol(&self, symbol_id: &SymbolId) -> Result<ChurnSummary> {
        self.churn_by_kind_and_key(ChurnEntityKind::Symbol, &symbol_id.0, || {
            let mut summary = ChurnSummary::missing(ChurnEntityKind::Symbol, symbol_id.0.clone());
            summary.symbol_id = Some(symbol_id.clone());
            summary.uncertainty =
                vec!["no persisted symbol-level churn is available for this symbol".into()];
            summary
        })
    }

    fn provenance_for_symbol(
        &self,
        symbol_id: &SymbolId,
        limit: usize,
    ) -> Result<SymbolProvenance> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let symbol_json: Option<String> = conn
            .query_row(
                "SELECT json FROM symbols WHERE id = ?1",
                params![&symbol_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage_err)?;
        let Some(symbol_json) = symbol_json else {
            return Err(OkError::SymbolNotFound(symbol_id.0.clone()));
        };
        let symbol: Symbol = serde_json::from_str(&symbol_json)?;
        let file_path: String = conn
            .query_row(
                "SELECT path FROM files WHERE id = ?1",
                params![&symbol.file_id.0],
                |row| row.get(0),
            )
            .map_err(storage_err)?;
        if limit == 0 {
            return Ok(SymbolProvenance {
                symbol_id: symbol.id,
                qualified_name: symbol.qualified_name,
                file_path: PathBuf::from(file_path),
                range: symbol.range,
                first_seen: None,
                last_touched: None,
                recent_touches: Vec::new(),
                confidence: Confidence::Low,
                truncated: false,
                uncertainty: vec!["provenance query limit is zero".into()],
            });
        }

        let query_limit = history_query_limit(limit);
        let mut recent_stmt = conn
            .prepare(
                "SELECT t.json, c.json
                 FROM git_symbol_touches t
                 JOIN git_commits c ON c.id = t.commit_id
                 WHERE t.symbol_id = ?1
                 ORDER BY t.touched_at DESC, t.id
                 LIMIT ?2",
            )
            .map_err(storage_err)?;
        let rows = recent_stmt
            .query_map(params![&symbol_id.0, query_limit], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(storage_err)?;
        let mut recent_touches = collect_provenance_rows(rows, symbol_provenance_touch)?;
        let truncated = recent_touches.len() > limit;
        recent_touches.truncate(limit);
        let first_seen = conn
            .query_row(
                "SELECT t.json, c.json
                 FROM git_symbol_touches t
                 JOIN git_commits c ON c.id = t.commit_id
                 WHERE t.symbol_id = ?1
                 ORDER BY t.touched_at ASC, t.id
                 LIMIT 1",
                params![&symbol_id.0],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(storage_err)?
            .map(|(touch, commit)| symbol_provenance_touch(&touch, &commit))
            .transpose()?;
        let last_touched = recent_touches.first().cloned();
        let mut uncertainty = recent_touches
            .iter()
            .flat_map(|touch| touch.uncertainty.clone())
            .collect::<Vec<_>>();
        if let Some(first_seen) = &first_seen {
            uncertainty.extend(first_seen.uncertainty.clone());
            uncertainty.push(
                "first_seen is the earliest line-mapped touch in the configured local history window; it may not be the symbol-introduction commit"
                    .into(),
            );
        } else {
            uncertainty
                .push("no persisted line-level commit mapping is available for this symbol".into());
        }
        if symbol.range.is_none() {
            uncertainty.push(
                "the indexed symbol has no line range, so commit hunks cannot be mapped".into(),
            );
        }
        if truncated {
            uncertainty.push(format!(
                "recent provenance is truncated to {limit} touch records"
            ));
        }
        uncertainty.sort();
        uncertainty.dedup();
        let confidence = recent_touches
            .iter()
            .map(|touch| touch.confidence)
            .chain(first_seen.iter().map(|touch| touch.confidence))
            .reduce(lower_history_confidence)
            .unwrap_or(Confidence::Low);

        Ok(SymbolProvenance {
            symbol_id: symbol.id,
            qualified_name: symbol.qualified_name,
            file_path: PathBuf::from(file_path),
            range: symbol.range,
            first_seen,
            last_touched,
            recent_touches,
            confidence,
            truncated,
            uncertainty,
        })
    }

    fn similar_changes(
        &self,
        query: &SimilarChangeQuery,
        limit: usize,
    ) -> Result<SimilarChangeReport> {
        let normalized_query = normalize_similar_change_query(query)?;
        if limit == 0 {
            return Ok(SimilarChangeReport {
                query: normalized_query,
                generated_at: Utc::now(),
                hits: Vec::new(),
                truncated: false,
                uncertainty: vec!["similar-change query limit is zero".into()],
            });
        }

        let task_tokens = normalized_query
            .task
            .as_deref()
            .map(tokenize_similarity_text)
            .unwrap_or_default();
        let query_paths = normalized_query
            .paths
            .iter()
            .map(|path| history_path(path))
            .collect::<Result<BTreeSet<_>>>()?;
        let symbol_queries = normalized_query
            .symbols
            .iter()
            .map(|symbol| symbol.to_lowercase())
            .collect::<BTreeSet<_>>();

        if task_tokens.is_empty() && query_paths.is_empty() && symbol_queries.is_empty() {
            return Ok(SimilarChangeReport {
                query: normalized_query,
                generated_at: Utc::now(),
                hits: Vec::new(),
                truncated: false,
                uncertainty: vec![
                    "provide at least one task, path, or symbol similarity signal".into(),
                ],
            });
        }

        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let scan_limit = similar_history_scan_limit(limit);

        let commits = load_similarity_commits(&conn, scan_limit)?;
        if commits.is_empty() {
            return Ok(SimilarChangeReport {
                query: normalized_query,
                generated_at: Utc::now(),
                hits: Vec::new(),
                truncated: false,
                uncertainty: vec!["no persisted commit history is available".into()],
            });
        }
        let file_touches = load_similarity_file_touches(&conn, scan_limit)?;
        let symbol_touches = load_similarity_symbol_touches(&conn, scan_limit)?;
        let cochange_edges = load_similarity_cochange_edges(&conn)?;
        let hotspots = load_similarity_file_hotspots(&conn)?;

        let mut file_touches_by_commit: BTreeMap<String, Vec<GitFileTouch>> = BTreeMap::new();
        for touch in file_touches {
            file_touches_by_commit
                .entry(touch.commit_id.0.clone())
                .or_default()
                .push(touch);
        }

        let mut symbol_touches_by_commit: BTreeMap<String, Vec<GitSymbolTouch>> = BTreeMap::new();
        for touch in symbol_touches {
            symbol_touches_by_commit
                .entry(touch.commit_id.0.clone())
                .or_default()
                .push(touch);
        }

        let mut query_neighbors: BTreeMap<String, Vec<GitCochangeEdge>> = BTreeMap::new();
        let mut sample_edges_by_commit: BTreeMap<String, Vec<GitCochangeEdge>> = BTreeMap::new();
        for edge in cochange_edges {
            let path = history_path(&edge.path)?;
            let cochanged_path = history_path(&edge.cochanged_path)?;
            let touches_query_path =
                query_paths.contains(&path) || query_paths.contains(&cochanged_path);
            if query_paths.contains(&path) {
                query_neighbors
                    .entry(cochanged_path.clone())
                    .or_default()
                    .push(edge.clone());
            }
            if query_paths.contains(&cochanged_path) {
                query_neighbors
                    .entry(path.clone())
                    .or_default()
                    .push(edge.clone());
            }
            if touches_query_path {
                for commit_id in &edge.sample_commits {
                    sample_edges_by_commit
                        .entry(commit_id.0.clone())
                        .or_default()
                        .push(edge.clone());
                }
            }
        }

        let query_related_paths = query_paths
            .iter()
            .cloned()
            .chain(query_neighbors.keys().cloned())
            .collect::<BTreeSet<_>>();

        let mut hits = Vec::new();
        for commit in commits {
            let file_touches = file_touches_by_commit
                .get(&commit.id.0)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let symbol_touches = symbol_touches_by_commit
                .get(&commit.id.0)
                .map(Vec::as_slice)
                .unwrap_or(&[]);

            let candidate = score_similar_commit(
                &normalized_query,
                &task_tokens,
                &query_paths,
                &symbol_queries,
                &query_neighbors,
                &query_related_paths,
                &sample_edges_by_commit,
                &hotspots,
                &commit,
                file_touches,
                symbol_touches,
            )?;
            if candidate.score > 0.0 {
                hits.push(candidate);
            }
        }

        hits.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| {
                    history_confidence_rank(right.confidence)
                        .cmp(&history_confidence_rank(left.confidence))
                })
                .then_with(|| {
                    right
                        .change
                        .commit
                        .committed_at
                        .cmp(&left.change.commit.committed_at)
                })
                .then_with(|| left.change.commit.id.0.cmp(&right.change.commit.id.0))
        });
        let truncated = hits.len() > limit;
        hits.truncate(limit);

        let mut uncertainty = Vec::new();
        if hits.is_empty() {
            uncertainty.push("no similar historical changes matched the query signals".into());
        }
        if truncated {
            uncertainty.push(format!(
                "similar-change results are truncated to {limit} hits"
            ));
        }

        Ok(SimilarChangeReport {
            query: normalized_query,
            generated_at: Utc::now(),
            hits,
            truncated,
            uncertainty,
        })
    }

    fn cochange_neighbors(&self, path: &Path, limit: usize) -> Result<Vec<GitCochangeEdge>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let normalized_path = history_path(path)?;
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT json FROM git_cochange_edges
                 WHERE path = ?1
                 ORDER BY recency_weight DESC, commit_count DESC, cochanged_path
                 LIMIT ?2",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(
                params![normalized_path, limit.min(i64::MAX as usize) as i64],
                |row| row.get::<_, String>(0),
            )
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn recent_commits(&self, limit: usize) -> Result<Vec<GitCommitRecord>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM git_commits ORDER BY committed_at DESC, id LIMIT ?1")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![limit.min(i64::MAX as usize) as i64], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        collect_json(rows)
    }
}

fn collect_provenance_rows<F>(
    rows: rusqlite::MappedRows<'_, F>,
    decode: fn(&str, &str) -> Result<ProvenanceTouch>,
) -> Result<Vec<ProvenanceTouch>>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<(String, String)>,
{
    let mut touches = Vec::new();
    for row in rows {
        let (touch, commit) = row.map_err(storage_err)?;
        touches.push(decode(&touch, &commit)?);
    }
    Ok(touches)
}

fn file_provenance_touch(touch: &str, commit: &str) -> Result<ProvenanceTouch> {
    let touch: GitFileTouch = serde_json::from_str(touch)?;
    let commit: GitCommitRecord = serde_json::from_str(commit)?;
    Ok(ProvenanceTouch {
        commit,
        path: touch.path,
        previous_path: touch.previous_path,
        symbol_id: None,
        qualified_name: None,
        change_kind: touch.change_kind,
        line_ranges: Vec::new(),
        confidence: Confidence::Exact,
        uncertainty: Vec::new(),
    })
}

fn symbol_provenance_touch(touch: &str, commit: &str) -> Result<ProvenanceTouch> {
    let touch: GitSymbolTouch = serde_json::from_str(touch)?;
    let commit: GitCommitRecord = serde_json::from_str(commit)?;
    Ok(ProvenanceTouch {
        commit,
        path: touch.file_path,
        previous_path: None,
        symbol_id: touch.symbol_id,
        qualified_name: Some(touch.qualified_name),
        change_kind: touch.change_kind,
        line_ranges: touch.line_ranges,
        confidence: touch.confidence,
        uncertainty: touch.uncertainty,
    })
}

fn normalize_similar_change_query(query: &SimilarChangeQuery) -> Result<SimilarChangeQuery> {
    let task = query
        .task
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let mut paths = BTreeSet::new();
    for path in &query.paths {
        paths.insert(PathBuf::from(history_path(path)?));
    }

    let mut symbols = BTreeSet::new();
    for symbol in &query.symbols {
        let symbol = symbol.trim();
        if !symbol.is_empty() {
            symbols.insert(symbol.to_string());
        }
    }

    Ok(SimilarChangeQuery {
        task,
        paths: paths.into_iter().collect(),
        symbols: symbols.into_iter().collect(),
    })
}

fn similar_history_scan_limit(limit: usize) -> i64 {
    limit
        .saturating_mul(80)
        .clamp(500, 5_000)
        .min(i64::MAX as usize) as i64
}

fn load_similarity_commits(conn: &Connection, scan_limit: i64) -> Result<Vec<GitCommitRecord>> {
    let mut stmt = conn
        .prepare("SELECT json FROM git_commits ORDER BY committed_at DESC, id LIMIT ?1")
        .map_err(storage_err)?;
    let rows = stmt
        .query_map(params![scan_limit], |row| row.get::<_, String>(0))
        .map_err(storage_err)?;
    collect_json(rows)
}

fn load_similarity_file_touches(conn: &Connection, scan_limit: i64) -> Result<Vec<GitFileTouch>> {
    let mut stmt = conn
        .prepare(
            "SELECT t.json
             FROM git_file_touches t
             JOIN (
               SELECT id FROM git_commits ORDER BY committed_at DESC, id LIMIT ?1
             ) recent ON recent.id = t.commit_id
             ORDER BY t.touched_at DESC, t.id",
        )
        .map_err(storage_err)?;
    let rows = stmt
        .query_map(params![scan_limit], |row| row.get::<_, String>(0))
        .map_err(storage_err)?;
    collect_json(rows)
}

fn load_similarity_symbol_touches(
    conn: &Connection,
    scan_limit: i64,
) -> Result<Vec<GitSymbolTouch>> {
    let mut stmt = conn
        .prepare(
            "SELECT t.json
             FROM git_symbol_touches t
             JOIN (
               SELECT id FROM git_commits ORDER BY committed_at DESC, id LIMIT ?1
             ) recent ON recent.id = t.commit_id
             ORDER BY t.touched_at DESC, t.id",
        )
        .map_err(storage_err)?;
    let rows = stmt
        .query_map(params![scan_limit], |row| row.get::<_, String>(0))
        .map_err(storage_err)?;
    collect_json(rows)
}

fn load_similarity_cochange_edges(conn: &Connection) -> Result<Vec<GitCochangeEdge>> {
    let mut stmt = conn
        .prepare(
            "SELECT json FROM git_cochange_edges
             ORDER BY recency_weight DESC, commit_count DESC, path, cochanged_path
             LIMIT 5000",
        )
        .map_err(storage_err)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(storage_err)?;
    collect_json(rows)
}

fn load_similarity_file_hotspots(conn: &Connection) -> Result<BTreeMap<String, ChurnSummary>> {
    let mut stmt = conn
        .prepare(
            "SELECT entity_key, json FROM history_hotspots
             WHERE entity_kind = 'file'
             ORDER BY hotspot_score DESC, touch_count DESC, entity_key
             LIMIT 5000",
        )
        .map_err(storage_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(storage_err)?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (key, json) = row.map_err(storage_err)?;
        out.insert(key, serde_json::from_str(&json)?);
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn score_similar_commit(
    query: &SimilarChangeQuery,
    task_tokens: &BTreeSet<String>,
    query_paths: &BTreeSet<String>,
    symbol_queries: &BTreeSet<String>,
    query_neighbors: &BTreeMap<String, Vec<GitCochangeEdge>>,
    query_related_paths: &BTreeSet<String>,
    sample_edges_by_commit: &BTreeMap<String, Vec<GitCochangeEdge>>,
    hotspots: &BTreeMap<String, ChurnSummary>,
    commit: &GitCommitRecord,
    file_touches: &[GitFileTouch],
    symbol_touches: &[GitSymbolTouch],
) -> Result<SimilarChangeHit> {
    let mut score = 0.0_f32;
    let mut evidence = Vec::new();
    let mut source_types = BTreeSet::new();
    let mut touched_paths = BTreeSet::new();
    let mut touched_symbols = BTreeSet::new();
    let mut cochange_paths = BTreeSet::new();
    let mut max_hotspot_score = 0.0_f32;

    for touch in file_touches {
        touched_paths.insert(touch.path.clone());
        if let Some(previous_path) = &touch.previous_path {
            touched_paths.insert(previous_path.clone());
        }
    }
    for touch in symbol_touches {
        touched_symbols.insert(touch.qualified_name.clone());
    }

    if let Some(task) = &query.task {
        let commit_tokens =
            tokenize_similarity_text(&format!("{} {}", commit.summary, commit.message));
        let overlaps = task_tokens
            .intersection(&commit_tokens)
            .cloned()
            .collect::<Vec<_>>();
        if !overlaps.is_empty() {
            let contribution = (overlaps.len() as f32 * 0.08).min(0.32);
            let task_score = round_similarity_score(contribution * 0.75);
            let metadata_score = round_similarity_score(contribution * 0.25);
            evidence.push(SimilarityEvidence {
                source_type: SimilarityEvidenceSource::TaskText,
                score: task_score,
                message: format!(
                    "task text matched commit summary/message token(s): {}",
                    overlaps.join(", ")
                ),
                query: Some(task.clone()),
                path: None,
                symbol: None,
                commit_id: Some(commit.id.clone()),
            });
            evidence.push(SimilarityEvidence {
                source_type: SimilarityEvidenceSource::CommitMetadata,
                score: metadata_score,
                message: "commit summary and message metadata overlap the requested task".into(),
                query: Some(task.clone()),
                path: None,
                symbol: None,
                commit_id: Some(commit.id.clone()),
            });
            score += contribution;
            source_types.insert(SimilarityEvidenceSource::TaskText);
            source_types.insert(SimilarityEvidenceSource::CommitMetadata);
        }
    }

    let mut path_score = 0.0_f32;
    let mut matched_paths = BTreeSet::new();
    for touch in file_touches {
        let path = history_path(&touch.path)?;
        if query_paths.contains(&path) && matched_paths.insert(path.clone()) {
            path_score += 0.42;
            evidence.push(SimilarityEvidence {
                source_type: SimilarityEvidenceSource::Path,
                score: 0.42,
                message: "commit touched an exact query path".into(),
                query: Some(path.clone()),
                path: Some(PathBuf::from(path)),
                symbol: None,
                commit_id: Some(commit.id.clone()),
            });
        }
        if let Some(previous_path) = &touch.previous_path {
            let previous_path = history_path(previous_path)?;
            if query_paths.contains(&previous_path) && matched_paths.insert(previous_path.clone()) {
                path_score += 0.32;
                evidence.push(SimilarityEvidence {
                    source_type: SimilarityEvidenceSource::Path,
                    score: 0.32,
                    message: "commit touched a previous name for a query path".into(),
                    query: Some(previous_path.clone()),
                    path: Some(PathBuf::from(previous_path)),
                    symbol: None,
                    commit_id: Some(commit.id.clone()),
                });
            }
        }
    }
    if path_score > 0.0 {
        score += path_score.min(0.50);
        source_types.insert(SimilarityEvidenceSource::Path);
    }

    let mut symbol_score = 0.0_f32;
    let mut matched_symbols = BTreeSet::new();
    for touch in symbol_touches {
        for query_symbol in symbol_queries {
            let Some((matched_symbol, contribution)) = similarity_symbol_match(query_symbol, touch)
            else {
                continue;
            };
            if matched_symbols.insert((query_symbol.clone(), matched_symbol.clone())) {
                symbol_score += contribution;
                evidence.push(SimilarityEvidence {
                    source_type: SimilarityEvidenceSource::Symbol,
                    score: contribution,
                    message: "commit touched a symbol matching the query".into(),
                    query: Some(query_symbol.clone()),
                    path: Some(touch.file_path.clone()),
                    symbol: Some(matched_symbol),
                    commit_id: Some(commit.id.clone()),
                });
            }
        }
    }
    if symbol_score > 0.0 {
        score += symbol_score.min(0.45);
        source_types.insert(SimilarityEvidenceSource::Symbol);
    }

    let mut cochange_score = 0.0_f32;
    let mut matched_cochanges = BTreeSet::new();
    for touch in file_touches {
        let path = history_path(&touch.path)?;
        if let Some(edges) = query_neighbors.get(&path) {
            for edge in edges {
                let edge_path = history_path(&edge.path)?;
                let edge_cochanged = history_path(&edge.cochanged_path)?;
                let neighbor = if query_paths.contains(&edge_path) {
                    edge_cochanged
                } else {
                    edge_path
                };
                if matched_cochanges.insert(neighbor.clone()) {
                    let contribution = (0.16 + edge.recency_weight.min(2.5) * 0.03).min(0.26);
                    cochange_score += contribution;
                    cochange_paths.insert(PathBuf::from(neighbor.clone()));
                    evidence.push(SimilarityEvidence {
                        source_type: SimilarityEvidenceSource::Cochange,
                        score: round_similarity_score(contribution),
                        message: "commit touched a co-change neighbor of a query path".into(),
                        query: query_paths.iter().next().cloned(),
                        path: Some(PathBuf::from(neighbor)),
                        symbol: None,
                        commit_id: Some(commit.id.clone()),
                    });
                }
            }
        }
    }
    if let Some(edges) = sample_edges_by_commit.get(&commit.id.0) {
        for edge in edges {
            let sample_key = format!(
                "sample:{}:{}",
                edge.path.display(),
                edge.cochanged_path.display()
            );
            if matched_cochanges.insert(sample_key) {
                let contribution = 0.10_f32;
                cochange_score += contribution;
                cochange_paths.insert(edge.path.clone());
                cochange_paths.insert(edge.cochanged_path.clone());
                evidence.push(SimilarityEvidence {
                    source_type: SimilarityEvidenceSource::Cochange,
                    score: contribution,
                    message: "commit is a persisted sample for a query path co-change edge".into(),
                    query: query_paths.iter().next().cloned(),
                    path: Some(edge.cochanged_path.clone()),
                    symbol: None,
                    commit_id: Some(commit.id.clone()),
                });
            }
        }
    }
    if cochange_score > 0.0 {
        score += cochange_score.min(0.35);
        source_types.insert(SimilarityEvidenceSource::Cochange);
    }

    let mut churn_score = 0.0_f32;
    let mut matched_hotspots = BTreeSet::new();
    for touch in file_touches {
        let path = history_path(&touch.path)?;
        if !query_related_paths.contains(&path) {
            continue;
        }
        let Some(summary) = hotspots.get(&path) else {
            continue;
        };
        if summary.stats.hotspot_score <= 0.0 || !matched_hotspots.insert(path.clone()) {
            continue;
        }
        let contribution = (summary.stats.hotspot_score.ln_1p() * 0.08).min(0.14);
        churn_score += contribution;
        max_hotspot_score = max_hotspot_score.max(summary.stats.hotspot_score);
        evidence.push(SimilarityEvidence {
            source_type: SimilarityEvidenceSource::Churn,
            score: round_similarity_score(contribution),
            message: "commit touched a query-related historical churn hotspot".into(),
            query: Some(path.clone()),
            path: Some(PathBuf::from(path)),
            symbol: None,
            commit_id: Some(commit.id.clone()),
        });
    }
    if churn_score > 0.0 {
        score += churn_score.min(0.18);
        source_types.insert(SimilarityEvidenceSource::Churn);
    }

    let rounded_score = round_similarity_score(score.min(1.0));
    let confidence = similar_change_confidence(rounded_score, &source_types);
    let mut uncertainty = Vec::new();
    if source_types == BTreeSet::from([SimilarityEvidenceSource::Path]) {
        uncertainty.push("similarity is based only on exact path overlap".into());
    }
    if confidence == Confidence::Low {
        uncertainty
            .push("low-confidence historical similarity; inspect the commit before reuse".into());
    }
    if query.task.is_some() && !source_types.contains(&SimilarityEvidenceSource::TaskText) {
        uncertainty.push("task text did not match this commit's summary or message".into());
    }
    uncertainty.sort();
    uncertainty.dedup();

    Ok(SimilarChangeHit {
        change: HistoricalChangeSummary {
            commit: commit.clone(),
            touched_paths: touched_paths.into_iter().collect(),
            touched_symbols: touched_symbols.into_iter().collect(),
            cochange_paths: cochange_paths.into_iter().collect(),
            churn_hotspot_score: round_similarity_score(max_hotspot_score),
        },
        score: rounded_score,
        confidence,
        evidence,
        uncertainty,
    })
}

fn tokenize_similarity_text(text: &str) -> BTreeSet<String> {
    const STOP_WORDS: &[&str] = &[
        "and", "are", "but", "for", "from", "into", "the", "this", "that", "with", "your", "you",
        "fix", "add", "use", "using",
    ];
    let stop_words = STOP_WORDS.iter().copied().collect::<BTreeSet<_>>();
    let mut tokens = BTreeSet::new();
    let mut current = String::new();
    for ch in text.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            current.push(ch);
        } else if !current.is_empty() {
            if current.len() >= 3 && !stop_words.contains(current.as_str()) {
                tokens.insert(std::mem::take(&mut current));
            } else {
                current.clear();
            }
        }
    }
    if current.len() >= 3 && !stop_words.contains(current.as_str()) {
        tokens.insert(current);
    }
    tokens
}

fn similarity_symbol_match(query_symbol: &str, touch: &GitSymbolTouch) -> Option<(String, f32)> {
    let qualified = touch.qualified_name.to_lowercase();
    let symbol_id = touch
        .symbol_id
        .as_ref()
        .map(|id| id.0.to_lowercase())
        .unwrap_or_default();
    let namespace_tail = qualified.rsplit("::").next().unwrap_or(&qualified);
    let short_name = namespace_tail.rsplit('.').next().unwrap_or(namespace_tail);
    if query_symbol == qualified || query_symbol == symbol_id || query_symbol == short_name {
        Some((touch.qualified_name.clone(), 0.35))
    } else if qualified.contains(query_symbol) {
        Some((touch.qualified_name.clone(), 0.18))
    } else {
        None
    }
}

fn similar_change_confidence(
    score: f32,
    source_types: &BTreeSet<SimilarityEvidenceSource>,
) -> Confidence {
    let source_count = source_types.len();
    if (source_count >= 4 && score >= 0.75) || (source_count >= 3 && score >= 0.55) {
        Confidence::High
    } else if source_count >= 2 && score >= 0.35 {
        Confidence::Medium
    } else {
        Confidence::Low
    }
}

fn round_similarity_score(score: f32) -> f32 {
    (score * 1000.0).round() / 1000.0
}

fn lower_history_confidence(left: Confidence, right: Confidence) -> Confidence {
    if history_confidence_rank(left) <= history_confidence_rank(right) {
        left
    } else {
        right
    }
}

fn history_confidence_rank(confidence: Confidence) -> u8 {
    match confidence {
        Confidence::Low => 0,
        Confidence::Medium => 1,
        Confidence::High => 2,
        Confidence::Exact => 3,
    }
}
const DEFAULT_GRAPH_QUERY_LIMIT: usize = 100;
const MAX_GRAPH_QUERY_LIMIT: usize = 1_000;

struct IndexRows<'a> {
    files: &'a [File],
    symbols: &'a [Symbol],
    chunks: &'a [CodeChunk],
    tests: &'a [TestTarget],
    imports: &'a [Import],
    occurrences: &'a [SymbolOccurrence],
    analysis_facts: &'a [AnalysisFact],
}

fn insert_index_rows(tx: &Transaction<'_>, rows: IndexRows<'_>) -> Result<()> {
    for file in rows.files {
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
    for symbol in rows.symbols {
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
    for chunk in rows.chunks {
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
    for test in rows.tests {
        tx.execute(
            "INSERT INTO tests(id, file_id, json) VALUES(?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![&test.id, &test.file_id.0, serde_json::to_string(test)?],
        )
        .map_err(storage_err)?;
    }
    for import in rows.imports {
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
    for occurrence in rows.occurrences {
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
    for fact in rows.analysis_facts {
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
    Ok(())
}

fn insert_graph_rows(tx: &Transaction<'_>, nodes: &[GraphNode], edges: &[GraphEdge]) -> Result<()> {
    for node in nodes {
        let evidence_available = node.file_id.is_some() || node.symbol_id.is_some();
        tx.execute(
            "INSERT INTO graph_nodes(id, label, node_type, file_id, symbol_id, evidence_available, freshness, json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &node.id.0,
                &node.label,
                format!("{:?}", node.node_type),
                node.file_id.as_ref().map(|f| &f.0),
                node.symbol_id.as_ref().map(|s| &s.0),
                evidence_available,
                0_i64,
                serde_json::to_string(node)?
            ],
        )
        .map_err(storage_err)?;
    }
    for edge in edges {
        let freshness = edge.evidence.indexed_at.timestamp();
        tx.execute(
            "INSERT INTO graph_edges(id, from_id, to_id, edge_type, confidence, source_type, source_file, evidence_available, freshness, json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                &edge.id.0,
                &edge.from.0,
                &edge.to.0,
                format!("{:?}", edge.edge_type),
                format!("{:?}", edge.evidence.confidence),
                format!("{:?}", edge.evidence.source_type),
                &edge.evidence.source,
                true,
                freshness,
                serde_json::to_string(edge)?
            ],
        )
        .map_err(storage_err)?;
    }
    Ok(())
}

fn clamp_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_GRAPH_QUERY_LIMIT
    } else {
        limit.min(MAX_GRAPH_QUERY_LIMIT)
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
        insert_graph_rows(&tx, nodes, edges)?;
        tx.commit().map_err(storage_err)?;
        Ok(())
    }

    fn node_type_stats(
        &self,
    ) -> Result<std::collections::HashMap<String, open_kioku_storage::TypeStats>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT node_type, COUNT(*), MAX(evidence_available), MAX(freshness) FROM graph_nodes GROUP BY node_type")
            .map_err(storage_err)?;
        let mut rows = stmt.query([]).map_err(storage_err)?;
        let mut map = std::collections::HashMap::new();
        while let Some(row) = rows.next().map_err(storage_err)? {
            let t: String = row.get(0).map_err(storage_err)?;
            let c: i64 = row.get(1).map_err(storage_err)?;
            let ev: bool = row.get(2).unwrap_or(false);
            let fr: Option<i64> = row.get(3).unwrap_or(None);
            map.insert(
                t,
                open_kioku_storage::TypeStats {
                    count: c as usize,
                    evidence_available: ev,
                    freshness: fr.map(|v| v as u64),
                },
            );
        }
        Ok(map)
    }

    fn edge_type_stats(
        &self,
    ) -> Result<std::collections::HashMap<String, open_kioku_storage::TypeStats>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT edge_type, COUNT(*), MAX(evidence_available), MAX(freshness) FROM graph_edges GROUP BY edge_type")
            .map_err(storage_err)?;
        let mut rows = stmt.query([]).map_err(storage_err)?;
        let mut map = std::collections::HashMap::new();
        while let Some(row) = rows.next().map_err(storage_err)? {
            let t: String = row.get(0).map_err(storage_err)?;
            let c: i64 = row.get(1).map_err(storage_err)?;
            let ev: bool = row.get(2).unwrap_or(false);
            let fr: Option<i64> = row.get(3).unwrap_or(None);
            map.insert(
                t,
                open_kioku_storage::TypeStats {
                    count: c as usize,
                    evidence_available: ev,
                    freshness: fr.map(|v| v as u64),
                },
            );
        }
        Ok(map)
    }

    fn node_by_id(&self, id: &str) -> Result<Option<GraphNode>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        graph_node_by_id(&conn, id)
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
    fn nodes_by_type(
        &self,
        node_type: GraphNodeType,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GraphNode>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let limit = clamp_limit(limit) as i64;
        let offset = offset as i64;
        let type_str = format!("{:?}", node_type);
        let mut stmt = conn
            .prepare(
                "SELECT json FROM graph_nodes WHERE node_type = ?1 ORDER BY id LIMIT ?2 OFFSET ?3",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![type_str, limit, offset], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn all_graph_nodes(&self) -> Result<Vec<GraphNode>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM graph_nodes ORDER BY id")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn edges_by_type(
        &self,
        edge_type: GraphEdgeType,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GraphEdge>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let limit = clamp_limit(limit) as i64;
        let offset = offset as i64;
        let type_str = format!("{:?}", edge_type);
        let mut stmt = conn
            .prepare(
                "SELECT json FROM graph_edges WHERE edge_type = ?1 ORDER BY id LIMIT ?2 OFFSET ?3",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![type_str, limit, offset], |row| {
                row.get::<_, String>(0)
            })
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn graph_counts(&self) -> Result<GraphCounts> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let nodes: usize = conn
            .query_row("SELECT COUNT(*) FROM graph_nodes", [], |row| row.get(0))
            .map_err(storage_err)?;
        let edges: usize = conn
            .query_row("SELECT COUNT(*) FROM graph_edges", [], |row| row.get(0))
            .map_err(storage_err)?;
        Ok(GraphCounts { nodes, edges })
    }

    fn graph_schema_counts(&self) -> Result<GraphSchemaCounts> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;

        let mut node_types = std::collections::BTreeMap::new();
        let mut stmt = conn
            .prepare("SELECT node_type, COUNT(*) FROM graph_nodes GROUP BY node_type")
            .map_err(storage_err)?;
        let mut rows = stmt.query([]).map_err(storage_err)?;
        while let Some(row) = rows.next().map_err(storage_err)? {
            let ntype: String = row.get(0).map_err(storage_err)?;
            let count: usize = row.get(1).map_err(storage_err)?;
            if !ntype.is_empty() {
                node_types.insert(ntype, count);
            }
        }

        let mut edge_types = std::collections::BTreeMap::new();
        let mut stmt = conn
            .prepare("SELECT edge_type, COUNT(*) FROM graph_edges GROUP BY edge_type")
            .map_err(storage_err)?;
        let mut rows = stmt.query([]).map_err(storage_err)?;
        while let Some(row) = rows.next().map_err(storage_err)? {
            let etype: String = row.get(0).map_err(storage_err)?;
            let count: usize = row.get(1).map_err(storage_err)?;
            if !etype.is_empty() {
                edge_types.insert(etype, count);
            }
        }

        Ok(GraphSchemaCounts {
            node_types,
            edge_types,
        })
    }

    fn graph_edges_between(&self, from: &str, to: &str, limit: usize) -> Result<Vec<GraphEdge>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let limit = clamp_limit(limit) as i64;
        let mut stmt = conn
            .prepare("SELECT json FROM graph_edges WHERE from_id = ?1 AND to_id = ?2 ORDER BY id LIMIT ?3")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![from, to, limit], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }
}

fn is_duplicate_column(err: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(_, Some(msg)) = err {
        msg.contains("duplicate column name")
    } else {
        false
    }
}

fn add_column_if_not_exists(conn: &mut Connection, stmt: &str) -> Result<()> {
    match conn.execute(stmt, []) {
        Ok(_) => Ok(()),
        Err(err) if is_duplicate_column(&err) => Ok(()),
        Err(err) => Err(storage_err(err)),
    }
}

fn migrate_graph_schema(conn: &mut Connection) -> Result<()> {
    // Add columns to graph_nodes
    add_column_if_not_exists(
        conn,
        "ALTER TABLE graph_nodes ADD COLUMN node_type TEXT DEFAULT ''",
    )?;
    add_column_if_not_exists(
        conn,
        "ALTER TABLE graph_nodes ADD COLUMN file_id TEXT DEFAULT ''",
    )?;
    add_column_if_not_exists(
        conn,
        "ALTER TABLE graph_nodes ADD COLUMN symbol_id TEXT DEFAULT ''",
    )?;
    add_column_if_not_exists(
        conn,
        "ALTER TABLE graph_nodes ADD COLUMN evidence_available BOOLEAN DEFAULT 0",
    )?;
    add_column_if_not_exists(
        conn,
        "ALTER TABLE graph_nodes ADD COLUMN freshness INTEGER DEFAULT 0",
    )?;

    // Add columns to graph_edges
    add_column_if_not_exists(
        conn,
        "ALTER TABLE graph_edges ADD COLUMN confidence TEXT DEFAULT ''",
    )?;
    add_column_if_not_exists(
        conn,
        "ALTER TABLE graph_edges ADD COLUMN source_type TEXT DEFAULT ''",
    )?;
    add_column_if_not_exists(
        conn,
        "ALTER TABLE graph_edges ADD COLUMN source_file TEXT DEFAULT ''",
    )?;
    add_column_if_not_exists(
        conn,
        "ALTER TABLE graph_edges ADD COLUMN evidence_available BOOLEAN DEFAULT 0",
    )?;
    add_column_if_not_exists(
        conn,
        "ALTER TABLE graph_edges ADD COLUMN freshness INTEGER DEFAULT 0",
    )?;

    backfill_graph_query_columns(conn)?;

    // Add indexes (these are idempotent via IF NOT EXISTS)
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_graph_nodes_type ON graph_nodes(node_type)",
        [],
    )
    .map_err(storage_err)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_graph_nodes_file ON graph_nodes(file_id)",
        [],
    )
    .map_err(storage_err)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_graph_nodes_symbol ON graph_nodes(symbol_id)",
        [],
    )
    .map_err(storage_err)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_graph_edges_type ON graph_edges(edge_type)",
        [],
    )
    .map_err(storage_err)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_graph_edges_from_type ON graph_edges(from_id, edge_type)",
        [],
    )
    .map_err(storage_err)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_graph_edges_to_type ON graph_edges(to_id, edge_type)",
        [],
    )
    .map_err(storage_err)?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_graph_edges_source_type ON graph_edges(source_type)",
        [],
    )
    .map_err(storage_err)?;

    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(storage_err)?;
    if version < SQLITE_GRAPH_SCHEMA_VERSION {
        conn.pragma_update(None, "user_version", SQLITE_GRAPH_SCHEMA_VERSION)
            .map_err(storage_err)?;
    }

    Ok(())
}

fn backfill_graph_query_columns(conn: &mut Connection) -> Result<()> {
    let node_rows = {
        let mut stmt = conn
            .prepare(
                "SELECT id, json FROM graph_nodes
                 WHERE COALESCE(node_type, '') = ''
                    OR COALESCE(file_id, '') = ''
                    OR COALESCE(symbol_id, '') = ''",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(storage_err)?;
        let mut rows_out = Vec::new();
        for row in rows {
            rows_out.push(row.map_err(storage_err)?);
        }
        rows_out
    };
    if !node_rows.is_empty() {
        let tx = conn.transaction().map_err(storage_err)?;
        for (id, json) in node_rows {
            let Ok(node) = serde_json::from_str::<GraphNode>(&json) else {
                continue;
            };
            tx.execute(
                "UPDATE graph_nodes
                 SET node_type = ?1,
                     file_id = ?2,
                     symbol_id = ?3,
                     evidence_available = ?4
                 WHERE id = ?5",
                params![
                    format!("{:?}", node.node_type),
                    node.file_id.as_ref().map(|file_id| file_id.0.as_str()),
                    node.symbol_id
                        .as_ref()
                        .map(|symbol_id| symbol_id.0.as_str()),
                    node.file_id.is_some() || node.symbol_id.is_some(),
                    id,
                ],
            )
            .map_err(storage_err)?;
        }
        tx.commit().map_err(storage_err)?;
    }

    let edge_rows = {
        let mut stmt = conn
            .prepare(
                "SELECT id, json FROM graph_edges
                 WHERE COALESCE(edge_type, '') = ''
                    OR COALESCE(confidence, '') = ''
                    OR COALESCE(source_type, '') = ''
                    OR COALESCE(source_file, '') = ''",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(storage_err)?;
        let mut rows_out = Vec::new();
        for row in rows {
            rows_out.push(row.map_err(storage_err)?);
        }
        rows_out
    };
    if !edge_rows.is_empty() {
        let tx = conn.transaction().map_err(storage_err)?;
        for (id, json) in edge_rows {
            let Ok(edge) = serde_json::from_str::<GraphEdge>(&json) else {
                continue;
            };
            tx.execute(
                "UPDATE graph_edges
                 SET from_id = ?1,
                     to_id = ?2,
                     edge_type = ?3,
                     confidence = ?4,
                     source_type = ?5,
                     source_file = ?6,
                     evidence_available = ?7,
                     freshness = ?8
                 WHERE id = ?9",
                params![
                    edge.from.0.as_str(),
                    edge.to.0.as_str(),
                    format!("{:?}", edge.edge_type),
                    format!("{:?}", edge.evidence.confidence),
                    format!("{:?}", edge.evidence.source_type),
                    edge.evidence.source.as_str(),
                    true,
                    edge.evidence.indexed_at.timestamp(),
                    id,
                ],
            )
            .map_err(storage_err)?;
        }
        tx.commit().map_err(storage_err)?;
    }

    Ok(())
}

fn migrate_history_schema(conn: &mut Connection) -> Result<()> {
    ensure_supported_sqlite_schema(conn)?;
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(storage_err)?;
    let tx = conn.transaction().map_err(storage_err)?;
    tx.execute_batch(HISTORY_SCHEMA_V1).map_err(storage_err)?;
    if version < SQLITE_HISTORY_SCHEMA_VERSION {
        tx.pragma_update(None, "user_version", SQLITE_HISTORY_SCHEMA_VERSION)
            .map_err(storage_err)?;
    }
    tx.commit().map_err(storage_err)?;
    Ok(())
}

fn ensure_supported_sqlite_schema(conn: &Connection) -> Result<()> {
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(storage_err)?;
    if version > SQLITE_SUPPORTED_SCHEMA_VERSION {
        return Err(OkError::Storage(format!(
            "sqlite schema version {version} is newer than supported version {SQLITE_SUPPORTED_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

fn validate_history_snapshot(snapshot: &HistorySnapshot) -> Result<()> {
    if snapshot.schema_version != HISTORY_SCHEMA_VERSION {
        return Err(OkError::Storage(format!(
            "unsupported history snapshot schema version {}; expected {}",
            snapshot.schema_version, HISTORY_SCHEMA_VERSION
        )));
    }

    let mut commit_ids = BTreeSet::new();
    for commit in &snapshot.commits {
        validate_text("commit id", &commit.id.0)?;
        if !commit_ids.insert(commit.id.0.clone()) {
            return Err(OkError::Storage(format!(
                "duplicate history commit id `{}`",
                commit.id
            )));
        }
        validate_text("commit author name", &commit.author.name)?;
        if let Some(committer) = &commit.committer {
            validate_text("commit committer name", &committer.name)?;
        }
        let mut parent_ids = BTreeSet::new();
        for parent_id in &commit.parent_ids {
            validate_text("parent commit id", &parent_id.0)?;
            if !parent_ids.insert(parent_id.0.as_str()) {
                return Err(OkError::Storage(format!(
                    "commit `{}` contains duplicate parent `{parent_id}`",
                    commit.id
                )));
            }
        }
    }

    let mut file_touch_ids = BTreeSet::new();
    for touch in &snapshot.file_touches {
        validate_history_record_id(&touch.id, "file touch", &mut file_touch_ids)?;
        validate_commit_reference(&touch.commit_id, &commit_ids, "file touch")?;
        history_path(&touch.path)?;
        if let Some(previous_path) = &touch.previous_path {
            history_path(previous_path)?;
        }
    }

    let mut symbol_touch_ids = BTreeSet::new();
    for touch in &snapshot.symbol_touches {
        validate_history_record_id(&touch.id, "symbol touch", &mut symbol_touch_ids)?;
        validate_commit_reference(&touch.commit_id, &commit_ids, "symbol touch")?;
        validate_text("symbol qualified name", &touch.qualified_name)?;
        history_path(&touch.file_path)?;
    }

    let mut cochange_ids = BTreeSet::new();
    let mut cochange_pairs = BTreeSet::new();
    for edge in &snapshot.cochange_edges {
        validate_history_record_id(&edge.id, "co-change edge", &mut cochange_ids)?;
        let path = history_path(&edge.path)?;
        let cochanged_path = history_path(&edge.cochanged_path)?;
        if path == cochanged_path {
            return Err(OkError::Storage(format!(
                "co-change edge `{}` must connect two different paths",
                edge.id
            )));
        }
        if !cochange_pairs.insert((path.clone(), cochanged_path.clone())) {
            return Err(OkError::Storage(format!(
                "duplicate co-change edge `{path}` -> `{cochanged_path}`"
            )));
        }
        if edge.commit_count == 0 {
            return Err(OkError::Storage(format!(
                "co-change edge `{}` must have a positive commit count",
                edge.id
            )));
        }
        if !edge.recency_weight.is_finite() || edge.recency_weight < 0.0 {
            return Err(OkError::Storage(format!(
                "co-change edge `{}` has invalid recency weight {}",
                edge.id, edge.recency_weight
            )));
        }
        let mut sample_commits = BTreeSet::new();
        for commit_id in &edge.sample_commits {
            validate_text("sample commit id", &commit_id.0)?;
            if !sample_commits.insert(commit_id.0.as_str()) {
                return Err(OkError::Storage(format!(
                    "co-change edge `{}` contains duplicate sample commit `{commit_id}`",
                    edge.id
                )));
            }
        }
    }

    let mut reviewer_ids = BTreeSet::new();
    for evidence in &snapshot.reviewer_evidence {
        validate_history_record_id(&evidence.id, "review event", &mut reviewer_ids)?;
        validate_text("reviewer name", &evidence.reviewer.name)?;
        validate_text("review evidence source", &evidence.source)?;
        if let Some(commit_id) = &evidence.commit_id {
            validate_text("review commit id", &commit_id.0)?;
        }
        if let Some(path) = &evidence.path {
            history_path(path)?;
        }
    }

    Ok(())
}

fn validate_history_record_id(
    id: &HistoryRecordId,
    kind: &str,
    ids: &mut BTreeSet<String>,
) -> Result<()> {
    validate_text(&format!("{kind} id"), &id.0)?;
    if !ids.insert(id.0.clone()) {
        return Err(OkError::Storage(format!("duplicate {kind} id `{id}`")));
    }
    Ok(())
}

fn validate_commit_reference(
    commit_id: &GitCommitId,
    commit_ids: &BTreeSet<String>,
    kind: &str,
) -> Result<()> {
    validate_text("commit id", &commit_id.0)?;
    if !commit_ids.contains(&commit_id.0) {
        return Err(OkError::Storage(format!(
            "{kind} references missing commit `{commit_id}`"
        )));
    }
    Ok(())
}

fn validate_text(field: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(OkError::Storage(format!("{field} must not be empty")));
    }
    Ok(())
}

fn history_path(path: &Path) -> Result<String> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(OkError::Storage(format!(
            "history path must be a normalized repository-relative path: {}",
            path.display()
        )));
    }
    let value = path.to_str().ok_or_else(|| {
        OkError::Storage(format!(
            "history path must be valid UTF-8: {}",
            path.display()
        ))
    })?;
    if value.contains('\\') {
        return Err(OkError::Storage(format!(
            "history path must use `/` separators: {}",
            path.display()
        )));
    }
    Ok(value.to_string())
}

#[derive(Debug, Clone)]
struct ChurnTouchSample {
    id: String,
    touched_at: DateTime<Utc>,
    additions: u32,
    deletions: u32,
    confidence: Confidence,
    uncertainty: Vec<String>,
}

fn materialize_churn_summaries(snapshot: &HistorySnapshot) -> Result<Vec<ChurnSummary>> {
    let Some(reference_at) = newest_history_touch(snapshot) else {
        return Ok(Vec::new());
    };

    let mut file_samples = BTreeMap::<String, Vec<ChurnTouchSample>>::new();
    let mut file_aliases = Vec::<(String, String)>::new();
    let mut module_samples = BTreeMap::<String, BTreeMap<String, ChurnTouchSample>>::new();
    let mut symbol_samples = BTreeMap::<String, SymbolChurnAccumulator>::new();

    for touch in &snapshot.file_touches {
        let path = history_path(&touch.path)?;
        let sample = ChurnTouchSample {
            id: touch.id.0.clone(),
            touched_at: touch.touched_at,
            additions: touch.additions.unwrap_or_default(),
            deletions: touch.deletions.unwrap_or_default(),
            confidence: Confidence::Exact,
            uncertainty: Vec::new(),
        };
        file_samples
            .entry(path.clone())
            .or_default()
            .push(sample.clone());
        if let Some(previous_path) = &touch.previous_path {
            file_aliases.push((path, history_path(previous_path)?));
        }
    }
    let file_samples = expand_file_churn_aliases(file_samples, file_aliases);
    for (path, samples) in &file_samples {
        for module in churn_modules_for_path(Path::new(path)) {
            let module_entry = module_samples.entry(module).or_default();
            for sample in samples {
                module_entry.insert(sample.id.clone(), sample.clone());
            }
        }
    }

    for touch in &snapshot.symbol_touches {
        let Some(symbol_id) = &touch.symbol_id else {
            continue;
        };
        let file_path = history_path(&touch.file_path)?;
        let entry = symbol_samples
            .entry(symbol_id.0.clone())
            .or_insert_with(|| SymbolChurnAccumulator {
                file_path: PathBuf::from(&file_path),
                symbol_id: symbol_id.clone(),
                qualified_name: touch.qualified_name.clone(),
                samples: Vec::new(),
                saw_uncertainty: false,
            });
        entry.samples.push(ChurnTouchSample {
            id: touch.id.0.clone(),
            touched_at: touch.touched_at,
            additions: 0,
            deletions: 0,
            confidence: touch.confidence,
            uncertainty: touch.uncertainty.clone(),
        });
        if !touch.uncertainty.is_empty() {
            entry.saw_uncertainty = true;
        }
    }

    let mut summaries = Vec::new();
    for (path, samples) in file_samples {
        summaries.push(ChurnSummary {
            entity_kind: ChurnEntityKind::File,
            key: path.clone(),
            path: Some(PathBuf::from(path)),
            symbol_id: None,
            qualified_name: None,
            generated_at: reference_at,
            stats: churn_stats(&samples, reference_at),
            confidence: Confidence::Exact,
            uncertainty: Vec::new(),
        });
    }
    for (module, samples) in module_samples {
        let samples = samples.into_values().collect::<Vec<_>>();
        summaries.push(ChurnSummary {
            entity_kind: ChurnEntityKind::Module,
            key: module.clone(),
            path: Some(PathBuf::from(module)),
            symbol_id: None,
            qualified_name: None,
            generated_at: reference_at,
            stats: churn_stats(&samples, reference_at),
            confidence: Confidence::Medium,
            uncertainty: vec![
                "module churn is aggregated from persisted file touches in this directory tree"
                    .into(),
            ],
        });
    }
    for (key, entry) in symbol_samples {
        let mut uncertainty = entry
            .samples
            .iter()
            .flat_map(|sample| sample.uncertainty.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if entry.saw_uncertainty {
            uncertainty.push("symbol churn inherits uncertainty from line-level history".into());
        }
        summaries.push(ChurnSummary {
            entity_kind: ChurnEntityKind::Symbol,
            key,
            path: Some(entry.file_path),
            symbol_id: Some(entry.symbol_id),
            qualified_name: Some(entry.qualified_name),
            generated_at: reference_at,
            stats: churn_stats(&entry.samples, reference_at),
            confidence: minimum_churn_confidence(&entry.samples),
            uncertainty,
        });
    }

    summaries.sort_by(|left, right| {
        left.entity_kind
            .cmp(&right.entity_kind)
            .then_with(|| {
                right
                    .stats
                    .hotspot_score
                    .total_cmp(&left.stats.hotspot_score)
            })
            .then_with(|| right.stats.touch_count.cmp(&left.stats.touch_count))
            .then_with(|| left.key.cmp(&right.key))
    });
    Ok(summaries)
}

#[derive(Debug, Clone)]
struct SymbolChurnAccumulator {
    file_path: PathBuf,
    symbol_id: SymbolId,
    qualified_name: String,
    samples: Vec<ChurnTouchSample>,
    saw_uncertainty: bool,
}

fn newest_history_touch(snapshot: &HistorySnapshot) -> Option<DateTime<Utc>> {
    snapshot
        .file_touches
        .iter()
        .map(|touch| touch.touched_at)
        .chain(snapshot.symbol_touches.iter().map(|touch| touch.touched_at))
        .max()
}

fn churn_modules_for_path(path: &Path) -> Vec<String> {
    let mut modules = Vec::new();
    let mut current = path.parent();
    while let Some(parent) = current {
        let key = if parent.as_os_str().is_empty() {
            "__root__".to_string()
        } else {
            parent.to_string_lossy().to_string()
        };
        modules.push(key);
        current = parent.parent();
    }
    if modules.is_empty() {
        modules.push("__root__".to_string());
    }
    modules
}

fn expand_file_churn_aliases(
    samples: BTreeMap<String, Vec<ChurnTouchSample>>,
    aliases: Vec<(String, String)>,
) -> BTreeMap<String, Vec<ChurnTouchSample>> {
    if aliases.is_empty() {
        return samples;
    }

    let mut groups = samples
        .keys()
        .map(|path| BTreeSet::from([path.clone()]))
        .collect::<Vec<_>>();
    for (path, previous_path) in aliases {
        merge_file_alias_group(&mut groups, path, previous_path);
    }

    let mut expanded = BTreeMap::new();
    for group in groups {
        let mut combined = Vec::new();
        for path in &group {
            if let Some(path_samples) = samples.get(path) {
                combined.extend(path_samples.clone());
            }
        }
        if combined.is_empty() {
            continue;
        }
        for path in group {
            expanded.insert(path, combined.clone());
        }
    }
    expanded
}

fn merge_file_alias_group(groups: &mut Vec<BTreeSet<String>>, path: String, previous_path: String) {
    let left = groups.iter().position(|group| group.contains(&path));
    let right = groups
        .iter()
        .position(|group| group.contains(&previous_path));
    match (left, right) {
        (Some(left), Some(right)) if left == right => {}
        (Some(left), Some(right)) => {
            let (keep, remove) = if left < right {
                (left, right)
            } else {
                (right, left)
            };
            let removed = groups.remove(remove);
            groups[keep].extend(removed);
        }
        (Some(index), None) => {
            groups[index].insert(previous_path);
        }
        (None, Some(index)) => {
            groups[index].insert(path);
        }
        (None, None) => {
            groups.push(BTreeSet::from([path, previous_path]));
        }
    }
}

fn churn_stats(samples: &[ChurnTouchSample], reference_at: DateTime<Utc>) -> ChurnStats {
    let mut last_30d = 0;
    let mut last_90d = 0;
    let mut recency_weighted = 0.0_f32;
    let mut churn_volume = 0_u64;

    for sample in samples {
        let age_seconds = reference_at
            .signed_duration_since(sample.touched_at)
            .num_seconds()
            .max(0) as f32;
        let age_days = age_seconds / 86_400.0;
        if age_days <= 30.0 {
            last_30d += 1;
        }
        if age_days <= 90.0 {
            last_90d += 1;
        }
        recency_weighted += 1.0 / (1.0 + age_days / 30.0);
        churn_volume += u64::from(sample.additions) + u64::from(sample.deletions);
    }

    let touch_count = samples.len();
    let hotspot_score =
        recency_weighted * (touch_count as f32).ln_1p() + (churn_volume as f32).ln_1p() / 10.0;
    ChurnStats {
        all_time: touch_count,
        last_30d,
        last_90d,
        recency_weighted,
        touch_count,
        hotspot_score,
    }
}

fn minimum_churn_confidence(samples: &[ChurnTouchSample]) -> Confidence {
    samples
        .iter()
        .map(|sample| sample.confidence)
        .min_by_key(|confidence| confidence_rank(*confidence))
        .unwrap_or(Confidence::Low)
}

fn confidence_rank(confidence: Confidence) -> u8 {
    match confidence {
        Confidence::Low => 0,
        Confidence::Medium => 1,
        Confidence::High => 2,
        Confidence::Exact => 3,
    }
}

fn churn_entity_kind_key(kind: ChurnEntityKind) -> &'static str {
    match kind {
        ChurnEntityKind::File => "file",
        ChurnEntityKind::Module => "module",
        ChurnEntityKind::Symbol => "symbol",
    }
}

fn usize_to_i64(value: usize, field: &str) -> Result<i64> {
    i64::try_from(value)
        .map_err(|_| OkError::Storage(format!("{field} exceeds SQLite integer range")))
}

fn history_query_limit(limit: usize) -> i64 {
    limit.saturating_add(1).min(i64::MAX as usize) as i64
}

fn collect_limited_json<T, F>(
    rows: rusqlite::MappedRows<'_, F>,
    limit: usize,
) -> Result<(Vec<T>, bool)>
where
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<String>,
    T: serde::de::DeserializeOwned,
{
    let mut values = collect_json(rows)?;
    let truncated = values.len() > limit;
    values.truncate(limit);
    Ok((values, truncated))
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
    use super::{SqliteStore, SQLITE_GRAPH_SCHEMA_VERSION};
    use chrono::{TimeZone, Utc};
    use open_kioku_core::{
        AnalysisFact, ChurnEntityKind, CodeChunk, Confidence, EdgeId, Evidence, EvidenceId,
        EvidenceSourceType, File, FileId, GitChangeKind, GitCochangeEdge, GitCommitId,
        GitCommitRecord, GitFileTouch, GitSymbolTouch, GraphEdge, GraphEdgeType, GraphNode,
        GraphNodeType, HistoryRecordId, HistorySignalQuery, HistorySnapshot, IndexManifest,
        IndexQuality, Language, LineRange, NodeId, Owner, Repository, RepositoryId,
        ReviewerEvidence, ReviewerRole, SimilarChangeQuery, SimilarityEvidenceSource, Symbol,
        SymbolId, SymbolKind, SymbolOccurrence, HISTORY_SCHEMA_VERSION,
    };
    use open_kioku_storage::{
        GraphStore, HistoryStore, IndexData, MetadataStore, PartialIndexUpdate,
    };
    use rusqlite::{params, Connection};
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

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
            ..Default::default()
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
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: IndexQuality::default(),
        }
    }

    fn history_snapshot() -> HistorySnapshot {
        let older_at = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
        let newer_at = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        let older_id = GitCommitId::new("older");
        let newer_id = GitCommitId::new("newer");
        HistorySnapshot {
            schema_version: HISTORY_SCHEMA_VERSION,
            commits: vec![
                GitCommitRecord {
                    id: older_id.clone(),
                    parent_ids: Vec::new(),
                    author: Owner {
                        name: "Older Author".into(),
                        email: Some("older@example.com".into()),
                    },
                    committer: None,
                    authored_at: older_at,
                    committed_at: older_at,
                    summary: "Introduce library".into(),
                    message: "Introduce library".into(),
                    file_count: 2,
                },
                GitCommitRecord {
                    id: newer_id.clone(),
                    parent_ids: vec![older_id.clone()],
                    author: Owner {
                        name: "Newer Author".into(),
                        email: Some("newer@example.com".into()),
                    },
                    committer: None,
                    authored_at: newer_at,
                    committed_at: newer_at,
                    summary: "Refine library".into(),
                    message: "Refine library and tests".into(),
                    file_count: 3,
                },
            ],
            file_touches: vec![
                GitFileTouch {
                    id: HistoryRecordId::new("file-touch-older"),
                    commit_id: older_id.clone(),
                    path: "src/lib.rs".into(),
                    previous_path: None,
                    change_kind: GitChangeKind::Added,
                    additions: Some(20),
                    deletions: Some(0),
                    touched_at: older_at,
                },
                GitFileTouch {
                    id: HistoryRecordId::new("file-touch-newer"),
                    commit_id: newer_id.clone(),
                    path: "src/lib.rs".into(),
                    previous_path: None,
                    change_kind: GitChangeKind::Modified,
                    additions: Some(5),
                    deletions: Some(2),
                    touched_at: newer_at,
                },
            ],
            symbol_touches: vec![GitSymbolTouch {
                id: HistoryRecordId::new("symbol-touch-newer"),
                commit_id: newer_id.clone(),
                symbol_id: Some(SymbolId::new("symbol-1")),
                qualified_name: "crate::history_for_file".into(),
                file_path: "src/lib.rs".into(),
                change_kind: GitChangeKind::Modified,
                line_ranges: vec![LineRange { start: 4, end: 8 }],
                confidence: Confidence::Medium,
                uncertainty: vec!["historical coordinates may have shifted".into()],
                touched_at: newer_at,
            }],
            cochange_edges: vec![
                GitCochangeEdge {
                    id: HistoryRecordId::new("cochange-test"),
                    path: "src/lib.rs".into(),
                    cochanged_path: "tests/lib_test.rs".into(),
                    commit_count: 2,
                    recency_weight: 1.8,
                    last_changed_at: Some(newer_at),
                    sample_commits: vec![newer_id.clone(), older_id.clone()],
                    test_corun: true,
                },
                GitCochangeEdge {
                    id: HistoryRecordId::new("cochange-docs"),
                    path: "src/lib.rs".into(),
                    cochanged_path: "docs/library.md".into(),
                    commit_count: 1,
                    recency_weight: 0.5,
                    last_changed_at: Some(older_at),
                    sample_commits: vec![older_id],
                    test_corun: false,
                },
            ],
            reviewer_evidence: vec![ReviewerEvidence {
                id: HistoryRecordId::new("review-newer"),
                commit_id: Some(newer_id),
                path: None,
                reviewer: Owner {
                    name: "Reviewer".into(),
                    email: Some("reviewer@example.com".into()),
                },
                role: ReviewerRole::Reviewer,
                observed_at: newer_at,
                source: "git-trailer:reviewed-by".into(),
                confidence: Confidence::High,
            }],
        }
    }

    fn similar_history_snapshot() -> HistorySnapshot {
        let intro_at = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        let target_at = Utc.with_ymd_and_hms(2026, 6, 2, 12, 0, 0).unwrap();
        let move_at = Utc.with_ymd_and_hms(2026, 6, 3, 12, 0, 0).unwrap();
        let docs_at = Utc.with_ymd_and_hms(2026, 6, 4, 12, 0, 0).unwrap();
        let intro_id = GitCommitId::new("auth-intro");
        let target_id = GitCommitId::new("auth-expiry-fix");
        let move_id = GitCommitId::new("auth-module-move");
        let docs_id = GitCommitId::new("token-docs");

        HistorySnapshot {
            schema_version: HISTORY_SCHEMA_VERSION,
            commits: vec![
                GitCommitRecord {
                    id: intro_id.clone(),
                    parent_ids: Vec::new(),
                    author: Owner {
                        name: "Auth Dev".into(),
                        email: Some("auth@example.com".into()),
                    },
                    committer: None,
                    authored_at: intro_at,
                    committed_at: intro_at,
                    summary: "Add login token validation".into(),
                    message: "Add token validation for login requests".into(),
                    file_count: 1,
                },
                GitCommitRecord {
                    id: target_id.clone(),
                    parent_ids: vec![intro_id.clone()],
                    author: Owner {
                        name: "Auth Dev".into(),
                        email: Some("auth@example.com".into()),
                    },
                    committer: None,
                    authored_at: target_at,
                    committed_at: target_at,
                    summary: "Fix token expiration in login flow".into(),
                    message:
                        "Fix login token expiration by updating auth validation and auth tests"
                            .into(),
                    file_count: 2,
                },
                GitCommitRecord {
                    id: move_id.clone(),
                    parent_ids: vec![target_id.clone()],
                    author: Owner {
                        name: "Platform Dev".into(),
                        email: Some("platform@example.com".into()),
                    },
                    committer: None,
                    authored_at: move_at,
                    committed_at: move_at,
                    summary: "Move auth module".into(),
                    message: "Move auth module without behavior changes".into(),
                    file_count: 1,
                },
                GitCommitRecord {
                    id: docs_id.clone(),
                    parent_ids: vec![move_id.clone()],
                    author: Owner {
                        name: "Docs Dev".into(),
                        email: Some("docs@example.com".into()),
                    },
                    committer: None,
                    authored_at: docs_at,
                    committed_at: docs_at,
                    summary: "Update token glossary".into(),
                    message: "Refresh token wording in docs".into(),
                    file_count: 1,
                },
            ],
            file_touches: vec![
                GitFileTouch {
                    id: HistoryRecordId::new("intro-auth"),
                    commit_id: intro_id.clone(),
                    path: "src/auth.rs".into(),
                    previous_path: None,
                    change_kind: GitChangeKind::Added,
                    additions: Some(40),
                    deletions: Some(0),
                    touched_at: intro_at,
                },
                GitFileTouch {
                    id: HistoryRecordId::new("target-auth"),
                    commit_id: target_id.clone(),
                    path: "src/auth.rs".into(),
                    previous_path: None,
                    change_kind: GitChangeKind::Modified,
                    additions: Some(12),
                    deletions: Some(3),
                    touched_at: target_at,
                },
                GitFileTouch {
                    id: HistoryRecordId::new("target-tests"),
                    commit_id: target_id.clone(),
                    path: "tests/auth_flow.rs".into(),
                    previous_path: None,
                    change_kind: GitChangeKind::Modified,
                    additions: Some(18),
                    deletions: Some(1),
                    touched_at: target_at,
                },
                GitFileTouch {
                    id: HistoryRecordId::new("move-auth"),
                    commit_id: move_id.clone(),
                    path: "src/auth.rs".into(),
                    previous_path: None,
                    change_kind: GitChangeKind::Modified,
                    additions: Some(3),
                    deletions: Some(3),
                    touched_at: move_at,
                },
                GitFileTouch {
                    id: HistoryRecordId::new("docs-token"),
                    commit_id: docs_id.clone(),
                    path: "docs/tokens.md".into(),
                    previous_path: None,
                    change_kind: GitChangeKind::Modified,
                    additions: Some(5),
                    deletions: Some(1),
                    touched_at: docs_at,
                },
            ],
            symbol_touches: vec![GitSymbolTouch {
                id: HistoryRecordId::new("target-symbol"),
                commit_id: target_id.clone(),
                symbol_id: Some(SymbolId::new("auth-validate-token")),
                qualified_name: "crate::auth::validate_token".into(),
                file_path: "src/auth.rs".into(),
                change_kind: GitChangeKind::Modified,
                line_ranges: vec![LineRange { start: 10, end: 18 }],
                confidence: Confidence::Medium,
                uncertainty: Vec::new(),
                touched_at: target_at,
            }],
            cochange_edges: vec![GitCochangeEdge {
                id: HistoryRecordId::new("auth-tests-cochange"),
                path: "src/auth.rs".into(),
                cochanged_path: "tests/auth_flow.rs".into(),
                commit_count: 2,
                recency_weight: 1.9,
                last_changed_at: Some(target_at),
                sample_commits: vec![target_id],
                test_corun: true,
            }],
            reviewer_evidence: Vec::new(),
        }
    }

    #[test]
    fn history_migration_upgrades_legacy_database_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite");
        let legacy = Connection::open(&path).unwrap();
        legacy
            .execute_batch(
                r#"
                PRAGMA user_version = 0;
                CREATE TABLE analysis_facts (
                  id TEXT PRIMARY KEY,
                  file_id TEXT NOT NULL,
                  source_type TEXT NOT NULL,
                  target TEXT NOT NULL,
                  json TEXT NOT NULL
                );
                INSERT INTO analysis_facts(id, file_id, source_type, target, json)
                VALUES('legacy-git', 'f1', 'git_history', 'tests/lib_test.rs', '{}');
                "#,
            )
            .unwrap();
        drop(legacy);

        let store = SqliteStore::open(&path).unwrap();
        store.initialize().unwrap();

        let conn = store.connection.lock().unwrap();
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SQLITE_GRAPH_SCHEMA_VERSION);
        let history_table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table'
                   AND name IN (
                     'git_commits',
                     'git_file_touches',
                     'git_symbol_touches',
                     'git_cochange_edges',
                     'git_review_events',
                     'history_hotspots'
                   )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history_table_count, 6);
        let legacy_fact_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM analysis_facts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(legacy_fact_count, 1);
    }

    #[test]
    fn newer_sqlite_schema_is_rejected_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.sqlite");
        let future = Connection::open(&path).unwrap();
        future
            .execute_batch(
                r#"
                PRAGMA user_version = 3;
                CREATE TABLE future_history_marker (id INTEGER PRIMARY KEY);
                "#,
            )
            .unwrap();
        drop(future);

        let error = match SqliteStore::open(&path) {
            Ok(_) => panic!("newer schema should be rejected"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("newer than supported version 2"));

        let conn = Connection::open(&path).unwrap();
        let current_table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'manifests'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current_table_count, 0);
        let future_marker_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'future_history_marker'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(future_marker_count, 1);
    }

    #[test]
    fn history_snapshot_queries_return_typed_evidence() {
        let store = make_store();
        store.put_history_snapshot(&history_snapshot()).unwrap();

        let recent = store.recent_commits(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id.0, "newer");

        let neighbors = store
            .cochange_neighbors(std::path::Path::new("src/lib.rs"), 10)
            .unwrap();
        assert_eq!(neighbors.len(), 2);
        assert_eq!(
            neighbors[0].cochanged_path,
            std::path::Path::new("tests/lib_test.rs")
        );

        let summary = store
            .history_for_file(std::path::Path::new("src/lib.rs"), 10)
            .unwrap();
        assert_eq!(summary.recent_commits.len(), 2);
        assert_eq!(summary.file_touches.len(), 2);
        assert_eq!(summary.symbol_touches.len(), 1);
        assert_eq!(summary.cochange_neighbors.len(), 2);
        assert_eq!(summary.reviewer_evidence.len(), 1);
        assert!(!summary.truncated);
        assert!(summary.uncertainty.is_empty());

        let truncated = store
            .history_for_file(std::path::Path::new("src/lib.rs"), 1)
            .unwrap();
        assert!(truncated.truncated);
        assert!(truncated
            .uncertainty
            .iter()
            .any(|note| note.contains("truncated")));
    }

    #[test]
    fn similar_changes_rank_and_explain_multi_signal_history() {
        let store = make_store();
        store
            .put_history_snapshot(&similar_history_snapshot())
            .unwrap();

        let report = store
            .similar_changes(
                &SimilarChangeQuery {
                    task: Some("fix token expiration".into()),
                    paths: vec!["src/auth.rs".into()],
                    symbols: vec!["validate_token".into()],
                },
                5,
            )
            .unwrap();

        assert!(!report.truncated);
        assert_eq!(report.hits[0].change.commit.id.0, "auth-expiry-fix");
        assert!(report.hits[0].score > 0.90, "{:#?}", report.hits[0]);
        assert_eq!(report.hits[0].confidence, Confidence::High);
        let source_types = report.hits[0]
            .evidence
            .iter()
            .map(|evidence| evidence.source_type)
            .collect::<BTreeSet<_>>();
        assert!(source_types.contains(&SimilarityEvidenceSource::TaskText));
        assert!(source_types.contains(&SimilarityEvidenceSource::CommitMetadata));
        assert!(source_types.contains(&SimilarityEvidenceSource::Path));
        assert!(source_types.contains(&SimilarityEvidenceSource::Symbol));
        assert!(source_types.contains(&SimilarityEvidenceSource::Cochange));
        assert!(source_types.contains(&SimilarityEvidenceSource::Churn));

        let weak = report
            .hits
            .iter()
            .find(|hit| hit.change.commit.id.0 == "token-docs")
            .expect("weak task-text hit should still be visible");
        assert_eq!(weak.confidence, Confidence::Low);
        assert!(weak
            .uncertainty
            .iter()
            .any(|note| note.contains("low-confidence")));
    }

    #[test]
    fn history_score_components_are_bounded_and_named() {
        let store = make_store();
        store.put_history_snapshot(&history_snapshot()).unwrap();

        let summary = store
            .history_score_components(
                &HistorySignalQuery {
                    path: "src/lib.rs".into(),
                    task: Some("update lib history behavior".into()),
                    symbols: vec!["crate::history_for_file".into()],
                },
                10,
            )
            .unwrap();

        let signals = summary
            .components
            .iter()
            .map(|component| component.signal.as_str())
            .collect::<BTreeSet<_>>();
        assert!(signals.contains("history_churn"), "{summary:#?}");
        assert!(signals.contains("similar_change_overlap"), "{summary:#?}");
        assert!(signals.contains("reviewer_affinity"), "{summary:#?}");
        assert!(summary
            .components
            .iter()
            .all(|component| component.contribution <= 0.18));
        assert!(!summary.evidence_refs.is_empty());
        assert!(summary.reasons.iter().any(|reason| {
            reason.contains("history churn") || reason.contains("similar change")
        }));
    }

    #[test]
    fn similar_changes_limit_is_deterministic_and_reports_truncation() {
        let store = make_store();
        store
            .put_history_snapshot(&similar_history_snapshot())
            .unwrap();

        let report = store
            .similar_changes(
                &SimilarChangeQuery {
                    task: Some("fix token expiration".into()),
                    paths: vec!["src/auth.rs".into()],
                    symbols: vec!["validate_token".into()],
                },
                1,
            )
            .unwrap();

        assert!(report.truncated);
        assert_eq!(report.hits.len(), 1);
        assert_eq!(report.hits[0].change.commit.id.0, "auth-expiry-fix");
        assert!(report
            .uncertainty
            .iter()
            .any(|note| note.contains("truncated to 1")));
    }

    #[test]
    fn churn_summaries_are_materialized_with_deterministic_windows() {
        let store = make_store();
        store.put_history_snapshot(&history_snapshot()).unwrap();

        let file = store
            .churn_for_file(std::path::Path::new("src/lib.rs"))
            .unwrap();
        assert_eq!(file.entity_kind, ChurnEntityKind::File);
        assert_eq!(file.stats.all_time, 2);
        assert_eq!(file.stats.last_30d, 1);
        assert_eq!(file.stats.last_90d, 2);
        assert_eq!(file.stats.touch_count, 2);
        assert!(file.stats.recency_weighted > 1.4);
        assert!(file.stats.hotspot_score > file.stats.recency_weighted);
        assert_eq!(
            file.generated_at,
            Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
        );
        assert_eq!(file.confidence, Confidence::Exact);

        let module = store.churn_for_module(std::path::Path::new("src")).unwrap();
        assert_eq!(module.entity_kind, ChurnEntityKind::Module);
        assert_eq!(module.stats.all_time, 2);
        assert_eq!(module.stats.last_30d, 1);
        assert_eq!(module.confidence, Confidence::Medium);
        assert!(module
            .uncertainty
            .iter()
            .any(|note| note.contains("aggregated from persisted file touches")));

        let symbol_id = SymbolId::new("symbol-1");
        let symbol = store.churn_for_symbol(&symbol_id).unwrap();
        assert_eq!(symbol.entity_kind, ChurnEntityKind::Symbol);
        assert_eq!(symbol.stats.all_time, 1);
        assert_eq!(symbol.stats.last_30d, 1);
        assert_eq!(symbol.stats.last_90d, 1);
        assert_eq!(symbol.confidence, Confidence::Medium);
        assert_eq!(
            symbol.qualified_name.as_deref(),
            Some("crate::history_for_file")
        );
        assert!(symbol
            .uncertainty
            .iter()
            .any(|note| note.contains("historical coordinates may have shifted")));

        let missing = store
            .churn_for_symbol(&SymbolId::new("missing-symbol"))
            .unwrap();
        assert_eq!(missing.stats.touch_count, 0);
        assert_eq!(missing.confidence, Confidence::Low);
        assert!(missing
            .uncertainty
            .iter()
            .any(|note| note.contains("no persisted symbol-level churn")));
    }

    #[test]
    fn hotspot_ordering_and_lookup_use_persisted_summary_table() {
        let store = make_store();
        let mut snapshot = history_snapshot();
        snapshot.file_touches.push(GitFileTouch {
            id: HistoryRecordId::new("file-touch-docs"),
            commit_id: GitCommitId::new("older"),
            path: "docs/readme.md".into(),
            previous_path: None,
            change_kind: GitChangeKind::Modified,
            additions: Some(1),
            deletions: Some(0),
            touched_at: Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap(),
        });
        store.put_history_snapshot(&snapshot).unwrap();

        let conn = store.connection.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT entity_key FROM history_hotspots
                 WHERE entity_kind = 'file'
                 ORDER BY hotspot_score DESC, touch_count DESC, entity_key
                 LIMIT 2",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        drop(stmt);
        drop(conn);
        assert_eq!(rows, vec!["src/lib.rs", "docs/readme.md"]);

        let mut elapsed = Vec::new();
        for _ in 0..40 {
            let started = std::time::Instant::now();
            let summary = store
                .churn_for_file(std::path::Path::new("src/lib.rs"))
                .unwrap();
            assert_eq!(summary.stats.touch_count, 2);
            elapsed.push(started.elapsed());
        }
        elapsed.sort();
        let p95 = elapsed[(elapsed.len() * 95 / 100).min(elapsed.len() - 1)];
        assert!(
            p95 < Duration::from_millis(200),
            "persisted churn lookup p95 was {p95:?}"
        );
    }

    #[test]
    fn churn_summaries_follow_rename_aliases_without_module_double_counting() {
        let store = make_store();
        let mut snapshot = history_snapshot();
        snapshot.file_touches[0].path = "src/old.rs".into();
        snapshot.file_touches[1].previous_path = Some("src/old.rs".into());
        snapshot.file_touches[1].change_kind = GitChangeKind::Renamed;
        store.put_history_snapshot(&snapshot).unwrap();

        let current = store
            .churn_for_file(std::path::Path::new("src/lib.rs"))
            .unwrap();
        let historical = store
            .churn_for_file(std::path::Path::new("src/old.rs"))
            .unwrap();
        assert_eq!(current.stats.all_time, 2);
        assert_eq!(historical.stats.all_time, 2);
        assert_eq!(current.stats.last_30d, 1);
        assert_eq!(historical.stats.last_30d, 1);

        let module = store.churn_for_module(std::path::Path::new("src")).unwrap();
        assert_eq!(module.stats.all_time, 2);
        assert_eq!(module.stats.last_90d, 2);

        let root = store.churn_for_module(std::path::Path::new(".")).unwrap();
        assert_eq!(root.key, "__root__");
        assert_eq!(root.stats.all_time, 2);
    }

    #[test]
    fn provenance_queries_return_first_last_and_explicit_symbol_uncertainty() {
        let store = make_store();
        let file = make_file("file-1", "src/lib.rs");
        let symbol = make_symbol("symbol-1", "history_for_file", "file-1");
        let mut unmapped_symbol = make_symbol("symbol-2", "unmapped", "file-1");
        unmapped_symbol.range = None;
        let manifest = make_manifest();
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: std::slice::from_ref(&file),
                symbols: &[symbol.clone(), unmapped_symbol.clone()],
                chunks: &[],
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
            })
            .unwrap();
        store.put_history_snapshot(&history_snapshot()).unwrap();

        let file_provenance = store
            .provenance_for_path(std::path::Path::new("src/lib.rs"), 10)
            .unwrap();
        assert_eq!(
            file_provenance
                .first_seen
                .as_ref()
                .map(|touch| touch.commit.id.0.as_str()),
            Some("older")
        );
        assert_eq!(
            file_provenance
                .last_touched
                .as_ref()
                .map(|touch| touch.commit.id.0.as_str()),
            Some("newer")
        );
        assert_eq!(file_provenance.recent_touches.len(), 2);
        assert_eq!(file_provenance.confidence, Confidence::Exact);

        let symbol_provenance = store.provenance_for_symbol(&symbol.id, 10).unwrap();
        assert_eq!(symbol_provenance.recent_touches.len(), 1);
        assert_eq!(symbol_provenance.confidence, Confidence::Medium);
        assert_eq!(
            symbol_provenance.recent_touches[0].commit.author.name,
            "Newer Author"
        );
        assert_eq!(
            symbol_provenance.recent_touches[0].line_ranges,
            vec![LineRange { start: 4, end: 8 }]
        );
        assert!(symbol_provenance
            .uncertainty
            .iter()
            .any(|note| note.contains("earliest line-mapped touch")));

        let unmapped = store
            .provenance_for_symbol(&unmapped_symbol.id, 10)
            .unwrap();
        assert!(unmapped.first_seen.is_none());
        assert!(unmapped.last_touched.is_none());
        assert!(unmapped.recent_touches.is_empty());
        assert_eq!(unmapped.confidence, Confidence::Low);
        assert!(unmapped
            .uncertainty
            .iter()
            .any(|note| note.contains("no persisted line-level commit mapping")));
        assert!(unmapped
            .uncertainty
            .iter()
            .any(|note| note.contains("has no line range")));
    }

    #[test]
    fn path_provenance_follows_rename_aliases_in_both_directions() {
        let store = make_store();
        let mut snapshot = history_snapshot();
        snapshot.file_touches[0].path = "src/old.rs".into();
        snapshot.file_touches[1].previous_path = Some("src/old.rs".into());
        snapshot.file_touches[1].change_kind = GitChangeKind::Renamed;
        store.put_history_snapshot(&snapshot).unwrap();

        let current = store
            .provenance_for_path(std::path::Path::new("src/lib.rs"), 10)
            .unwrap();
        let historical = store
            .provenance_for_path(std::path::Path::new("src/old.rs"), 10)
            .unwrap();

        assert_eq!(current.recent_touches.len(), 2);
        assert_eq!(historical.recent_touches.len(), 2);
        assert_eq!(
            current
                .first_seen
                .as_ref()
                .map(|touch| touch.path.as_path()),
            Some(std::path::Path::new("src/old.rs"))
        );
    }

    #[test]
    fn invalid_snapshot_does_not_replace_existing_history() {
        let store = make_store();
        let snapshot = history_snapshot();
        store.put_history_snapshot(&snapshot).unwrap();

        let mut invalid = snapshot;
        invalid.file_touches[0].commit_id = GitCommitId::new("missing");
        let error = store
            .put_history_snapshot(&invalid)
            .unwrap_err()
            .to_string();
        assert!(error.contains("references missing commit `missing`"));

        let recent = store.recent_commits(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id.0, "newer");

        store
            .put_history_snapshot(&HistorySnapshot::empty())
            .unwrap();
        assert!(store.recent_commits(10).unwrap().is_empty());
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
    fn partial_replace_updates_changed_files_and_cleans_deleted_graph_edges() {
        let store = make_store();
        let manifest = make_manifest();
        let file1 = make_file("f1", "src/main.rs");
        let file2 = make_file("f2", "src/lib.rs");
        let sym1 = make_symbol("s1", "main_fn", "f1");
        let sym2 = make_symbol("s2", "lib_fn", "f2");
        let old_chunk = CodeChunk {
            id: "c1".into(),
            file_id: file1.id.clone(),
            range: LineRange { start: 1, end: 1 },
            language: Language::Rust,
            text: "fn main_fn() {}".into(),
            symbol_id: Some(sym1.id.clone()),
        };
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[file1.clone(), file2.clone()],
                symbols: &[sym1.clone(), sym2.clone()],
                chunks: std::slice::from_ref(&old_chunk),
                tests: &[],
                imports: &[],
                occurrences: &[SymbolOccurrence {
                    symbol_id: sym1.id.clone(),
                    file_id: file1.id.clone(),
                    range: Some(LineRange::single(1)),
                    is_definition: true,
                    confidence: Confidence::Exact,
                    provenance: EvidenceSourceType::StaticAnalysis,
                }],
                analysis_facts: &[],
            })
            .unwrap();
        let node1 = GraphNode {
            id: NodeId::new("symbol:s1"),
            node_type: GraphNodeType::Function,
            label: "main_fn".into(),
            file_id: Some(file1.id.clone()),
            symbol_id: Some(sym1.id.clone()),
            ..Default::default()
        };
        let node2 = GraphNode {
            id: NodeId::new("symbol:s2"),
            node_type: GraphNodeType::Function,
            label: "lib_fn".into(),
            file_id: Some(file2.id.clone()),
            symbol_id: Some(sym2.id.clone()),
            ..Default::default()
        };
        let edge = GraphEdge {
            id: EdgeId::new("edge:s1-s2"),
            from: node1.id.clone(),
            to: node2.id.clone(),
            edge_type: GraphEdgeType::References,
            evidence: evidence(),
            ..Default::default()
        };
        let node3 = GraphNode {
            id: NodeId::new("external:a"),
            node_type: GraphNodeType::Module,
            label: "external a".into(),
            ..Default::default()
        };
        let node4 = GraphNode {
            id: NodeId::new("external:b"),
            node_type: GraphNodeType::Module,
            label: "external b".into(),
            ..Default::default()
        };
        let mut source_evidence = evidence();
        source_evidence.source = "src/main.rs".into();
        let source_edge = GraphEdge {
            id: EdgeId::new("edge:source-file"),
            from: node3.id.clone(),
            to: node4.id.clone(),
            edge_type: GraphEdgeType::RelatedToTicket,
            evidence: source_evidence,
            ..Default::default()
        };
        store
            .replace_graph(
                &[node1, node2.clone(), node3.clone(), node4.clone()],
                &[edge.clone(), source_edge],
            )
            .unwrap();

        let mut updated_file2 = file2.clone();
        updated_file2.content_hash = "new-hash".into();
        let updated_sym2 = make_symbol("s2b", "lib_fn_new", "f2");
        let updated_chunk = CodeChunk {
            id: "c2".into(),
            file_id: updated_file2.id.clone(),
            range: LineRange { start: 2, end: 2 },
            language: Language::Rust,
            text: "fn lib_fn_new() {}".into(),
            symbol_id: Some(updated_sym2.id.clone()),
        };
        let updated_node2 = GraphNode {
            id: NodeId::new("symbol:s2b"),
            node_type: GraphNodeType::Function,
            label: "lib_fn_new".into(),
            file_id: Some(updated_file2.id.clone()),
            symbol_id: Some(updated_sym2.id.clone()),
            ..Default::default()
        };
        store
            .replace_files_index(PartialIndexUpdate {
                manifest: &manifest,
                changed_files: std::slice::from_ref(&updated_file2),
                deleted_file_ids: std::slice::from_ref(&file1.id),
                symbols: std::slice::from_ref(&updated_sym2),
                chunks: std::slice::from_ref(&updated_chunk),
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
                graph_nodes: std::slice::from_ref(&updated_node2),
                graph_edges: &[],
            })
            .unwrap();

        assert!(store
            .get_file_by_path(std::path::Path::new("src/main.rs"))
            .unwrap()
            .is_none());
        assert_eq!(
            store
                .get_file_by_path(std::path::Path::new("src/lib.rs"))
                .unwrap()
                .unwrap()
                .content_hash,
            "new-hash"
        );
        assert!(store.symbol_by_id(&sym1.id).unwrap().is_none());
        assert!(store.symbol_by_id(&updated_sym2.id).unwrap().is_some());
        assert!(store.chunks_for_file(&file1.id).unwrap().is_empty());
        assert_eq!(store.chunks_for_file(&file2.id).unwrap()[0].id, "c2");
        let edge_count: i64 = store
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM graph_edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(edge_count, 0);
        assert!(store.node_by_id("symbol:s1").unwrap().is_none());
        assert!(store.node_by_id("symbol:s2b").unwrap().is_some());
    }

    #[test]
    fn partial_replace_rolls_back_on_insert_failure() {
        let store = make_store();
        let manifest = make_manifest();
        let file = make_file("f1", "src/lib.rs");
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: std::slice::from_ref(&file),
                symbols: &[],
                chunks: &[],
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
            })
            .unwrap();

        let duplicate_a = make_file("f2", "src/dup.rs");
        let mut duplicate_b = make_file("f3", "src/dup.rs");
        duplicate_b.content_hash = "other".into();
        let error = store
            .replace_files_index(PartialIndexUpdate {
                manifest: &manifest,
                changed_files: &[duplicate_a, duplicate_b],
                deleted_file_ids: std::slice::from_ref(&file.id),
                symbols: &[],
                chunks: &[],
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
                graph_nodes: &[],
                graph_edges: &[],
            })
            .unwrap_err()
            .to_string();
        assert!(error.contains("UNIQUE") || error.contains("constraint"));
        assert!(store
            .get_file_by_path(std::path::Path::new("src/lib.rs"))
            .unwrap()
            .is_some());
        assert!(store
            .get_file_by_path(std::path::Path::new("src/dup.rs"))
            .unwrap()
            .is_none());
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
    fn replace_index_preserves_typed_and_legacy_history() {
        let store = make_store();
        store.put_history_snapshot(&history_snapshot()).unwrap();

        let file = make_file("f1", "src/lib.rs");
        let manifest = make_manifest();
        let git_fact = AnalysisFact {
            id: "legacy-git-1".into(),
            file_id: file.id.clone(),
            symbol_id: None,
            target: "tests/lib_test.rs".into(),
            target_kind: GraphNodeType::Test,
            edge_type: GraphEdgeType::ChangedBy,
            range: None,
            confidence: Confidence::High,
            source: "git-history:newer".into(),
            source_type: EvidenceSourceType::GitHistory,
            message: "legacy co-change compatibility fact".into(),
        };

        for _ in 0..2 {
            store
                .replace_index(IndexData {
                    manifest: &manifest,
                    files: std::slice::from_ref(&file),
                    symbols: &[],
                    occurrences: &[],
                    chunks: &[],
                    imports: &[],
                    tests: &[],
                    analysis_facts: std::slice::from_ref(&git_fact),
                })
                .unwrap();
        }

        assert_eq!(store.recent_commits(10).unwrap().len(), 2);
        let summary = store
            .history_for_file(std::path::Path::new("src/lib.rs"), 10)
            .unwrap();
        assert_eq!(summary.file_touches.len(), 2);
        let legacy = store
            .analysis_facts(Some(EvidenceSourceType::GitHistory), 10)
            .unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].id, git_fact.id);
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
            ..Default::default()
        };
        let node_b = GraphNode {
            id: NodeId::new("symbol:s1"),
            node_type: GraphNodeType::Function,
            label: "worker".into(),
            file_id: Some(FileId::new("f1")),
            symbol_id: Some(SymbolId::new("s1")),
            ..Default::default()
        };
        let edge = GraphEdge {
            id: EdgeId::new("e1"),
            from: node_a.id.clone(),
            to: node_b.id.clone(),
            edge_type: GraphEdgeType::Defines,
            evidence: evidence(),
            ..Default::default()
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
    fn graph_facts_with_properties_and_confidence_metadata_round_trip() {
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
            id: NodeId::new("file:src/lib.rs"),
            node_type: GraphNodeType::File,
            label: "src/lib.rs".into(),
            file_id: Some(FileId::new("f1")),
            properties: BTreeMap::from([("package".into(), serde_json::json!("open-kioku"))]),
            schema_version: Some("graph-v1".into()),
            source_pass: Some("tree_sitter".into()),
            index_mode: Some("full".into()),
            extractor_version: Some("test-extractor".into()),
            ambiguity: vec!["generated file status unknown".into()],
            quality_notes: vec!["file path verified".into()],
            ..Default::default()
        };
        let node_b = GraphNode {
            id: NodeId::new("symbol:s1"),
            node_type: GraphNodeType::Function,
            label: "worker".into(),
            file_id: Some(FileId::new("f1")),
            symbol_id: Some(SymbolId::new("s1")),
            ..Default::default()
        };
        let mut edge_evidence = evidence();
        edge_evidence.confidence_score = Some(0.98);
        edge_evidence.confidence_reason = Some("exact symbol occurrence".into());
        edge_evidence.freshness = Some("fresh".into());
        let edge = GraphEdge {
            id: EdgeId::new("e1"),
            from: node_a.id.clone(),
            to: node_b.id.clone(),
            edge_type: GraphEdgeType::Defines,
            evidence: edge_evidence,
            properties: BTreeMap::from([("relation".into(), serde_json::json!("definition"))]),
            schema_version: Some("graph-v1".into()),
            source_pass: Some("scip".into()),
            index_mode: Some("full".into()),
            extractor_version: Some("test-scip".into()),
            ambiguity: vec!["macro expansion not modeled".into()],
            quality_notes: vec!["exact definition edge".into()],
        };

        store
            .replace_graph(
                &[node_a.clone(), node_b.clone()],
                std::slice::from_ref(&edge),
            )
            .unwrap();

        let (nodes, edges) = store.neighbors("file:src/lib.rs", 10).unwrap();
        let stored_node = nodes.iter().find(|node| node.id == node_a.id).unwrap();
        assert_eq!(stored_node.properties, node_a.properties);
        assert_eq!(stored_node.schema_version.as_deref(), Some("graph-v1"));
        assert_eq!(stored_node.source_pass.as_deref(), Some("tree_sitter"));
        assert_eq!(stored_node.quality_notes, vec!["file path verified"]);

        assert_eq!(edges.len(), 1);
        let stored_edge = &edges[0];
        assert_eq!(stored_edge.properties, edge.properties);
        assert_eq!(stored_edge.schema_version.as_deref(), Some("graph-v1"));
        assert_eq!(stored_edge.evidence.confidence_score, Some(0.98));
        assert_eq!(
            stored_edge.evidence.confidence_reason.as_deref(),
            Some("exact symbol occurrence")
        );
        assert_eq!(stored_edge.evidence.freshness.as_deref(), Some("fresh"));

        let indexed_confidence: String = store
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT confidence FROM graph_edges WHERE id = 'e1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(indexed_confidence, "Medium");
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
            ..Default::default()
        };
        let node_b = GraphNode {
            id: NodeId::new("b"),
            node_type: GraphNodeType::File,
            label: "b".into(),
            file_id: None,
            symbol_id: None,
            ..Default::default()
        };
        let edge = GraphEdge {
            id: EdgeId::new("a-b"),
            from: node_a.id.clone(),
            to: node_b.id.clone(),
            edge_type: GraphEdgeType::Defines,
            evidence: evidence(),
            ..Default::default()
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

    #[test]
    fn test_old_graph_tables_migrate_and_replace_graph_backfills_columns() {
        let store = make_store();
        let legacy_file = GraphNode {
            id: NodeId::new("legacy_file"),
            node_type: GraphNodeType::File,
            label: "legacy.rs".into(),
            file_id: Some(FileId::new("f1")),
            ..Default::default()
        };
        let legacy_symbol = GraphNode {
            id: NodeId::new("legacy_symbol"),
            node_type: GraphNodeType::Function,
            label: "legacy_fn".into(),
            symbol_id: Some(SymbolId::new("s1")),
            ..Default::default()
        };
        let mut legacy_evidence = evidence();
        legacy_evidence.source_type = EvidenceSourceType::Scip;
        legacy_evidence.source = "index.scip".into();
        let legacy_edge = GraphEdge {
            id: EdgeId::new("legacy_edge"),
            from: legacy_file.id.clone(),
            to: legacy_symbol.id.clone(),
            edge_type: GraphEdgeType::Defines,
            evidence: legacy_evidence,
            ..Default::default()
        };
        {
            let conn = store.connection.lock().unwrap();
            conn.execute("DROP TABLE graph_nodes", []).unwrap();
            conn.execute("DROP TABLE graph_edges", []).unwrap();
            conn.execute(
                "CREATE TABLE graph_nodes(id TEXT PRIMARY KEY, label TEXT, json TEXT)",
                [],
            )
            .unwrap();
            conn.execute("CREATE TABLE graph_edges(id TEXT PRIMARY KEY, from_id TEXT, to_id TEXT, edge_type TEXT, json TEXT)", []).unwrap();
            conn.execute(
                "INSERT INTO graph_nodes(id, label, json) VALUES(?1, ?2, ?3)",
                params![
                    legacy_file.id.0.as_str(),
                    legacy_file.label.as_str(),
                    serde_json::to_string(&legacy_file).unwrap(),
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO graph_nodes(id, label, json) VALUES(?1, ?2, ?3)",
                params![
                    legacy_symbol.id.0.as_str(),
                    legacy_symbol.label.as_str(),
                    serde_json::to_string(&legacy_symbol).unwrap(),
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO graph_edges(id, from_id, to_id, edge_type, json)
                 VALUES(?1, ?2, ?3, '', ?4)",
                params![
                    legacy_edge.id.0.as_str(),
                    legacy_edge.from.0.as_str(),
                    legacy_edge.to.0.as_str(),
                    serde_json::to_string(&legacy_edge).unwrap(),
                ],
            )
            .unwrap();
        }
        store.initialize().unwrap();
        store.initialize().unwrap();

        let migrated_nodes = store.nodes_by_type(GraphNodeType::File, 10, 0).unwrap();
        assert_eq!(migrated_nodes.len(), 1);
        assert_eq!(migrated_nodes[0].id.0, "legacy_file");

        let migrated_edges = store.edges_by_type(GraphEdgeType::Defines, 10, 0).unwrap();
        assert_eq!(migrated_edges.len(), 1);
        assert_eq!(migrated_edges[0].id.0, "legacy_edge");
        let migrated_between = store
            .graph_edges_between("legacy_file", "legacy_symbol", 10)
            .unwrap();
        assert_eq!(migrated_between.len(), 1);

        let migrated_counts = store.graph_schema_counts().unwrap();
        assert_eq!(migrated_counts.node_types.get("File"), Some(&1));
        assert_eq!(migrated_counts.edge_types.get("Defines"), Some(&1));

        let node = GraphNode {
            id: NodeId::new("test_node"),
            node_type: GraphNodeType::File,
            label: "test".into(),
            ..Default::default()
        };
        store.replace_graph(&[node], &[]).unwrap();

        let count: i64 = store
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM graph_nodes WHERE node_type = 'File'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        let version: i64 = store
            .connection
            .lock()
            .unwrap()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SQLITE_GRAPH_SCHEMA_VERSION);

        let index_count: i64 = store
            .connection
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index'
                   AND name IN (
                     'idx_graph_nodes_type',
                     'idx_graph_nodes_file',
                     'idx_graph_nodes_symbol',
                     'idx_graph_edges_type',
                     'idx_graph_edges_from_type',
                     'idx_graph_edges_to_type',
                     'idx_graph_edges_source_type'
                   )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 7);
    }

    #[test]
    fn test_nodes_by_type_uses_indexed_column() {
        let store = make_store();
        let node1 = GraphNode {
            id: NodeId::new("n1"),
            node_type: GraphNodeType::File,
            ..Default::default()
        };
        let node2 = GraphNode {
            id: NodeId::new("n2"),
            node_type: GraphNodeType::File,
            ..Default::default()
        };
        let node3 = GraphNode {
            id: NodeId::new("n3"),
            node_type: GraphNodeType::Function,
            ..Default::default()
        };
        store
            .replace_graph(&[node2.clone(), node3.clone(), node1.clone()], &[])
            .unwrap();

        let nodes = store.nodes_by_type(GraphNodeType::File, 10, 0).unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].id.0, "n1");
        assert_eq!(nodes[1].id.0, "n2");
    }

    #[test]
    fn test_edges_by_type_uses_indexed_column() {
        let store = make_store();
        let node1 = GraphNode {
            id: NodeId::new("n1"),
            ..Default::default()
        };
        let node2 = GraphNode {
            id: NodeId::new("n2"),
            ..Default::default()
        };
        let edge1 = GraphEdge {
            id: EdgeId::new("e1"),
            from: NodeId::new("n1"),
            to: NodeId::new("n2"),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        };
        let edge2 = GraphEdge {
            id: EdgeId::new("e2"),
            from: NodeId::new("n1"),
            to: NodeId::new("n2"),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        };
        let edge3 = GraphEdge {
            id: EdgeId::new("e3"),
            from: NodeId::new("n1"),
            to: NodeId::new("n2"),
            edge_type: GraphEdgeType::Defines,
            ..Default::default()
        };
        store
            .replace_graph(
                &[node1, node2],
                &[edge2.clone(), edge3.clone(), edge1.clone()],
            )
            .unwrap();

        let edges = store.edges_by_type(GraphEdgeType::Calls, 10, 0).unwrap();
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].id.0, "e1");
        assert_eq!(edges[1].id.0, "e2");
    }

    #[test]
    fn test_graph_edges_between_respects_limit() {
        let store = make_store();
        let node1 = GraphNode {
            id: NodeId::new("n1"),
            ..Default::default()
        };
        let node2 = GraphNode {
            id: NodeId::new("n2"),
            ..Default::default()
        };
        let edge1 = GraphEdge {
            id: EdgeId::new("e1"),
            from: NodeId::new("n1"),
            to: NodeId::new("n2"),
            ..Default::default()
        };
        let edge2 = GraphEdge {
            id: EdgeId::new("e2"),
            from: NodeId::new("n1"),
            to: NodeId::new("n2"),
            ..Default::default()
        };
        store
            .replace_graph(&[node1, node2], &[edge2.clone(), edge1.clone()])
            .unwrap();

        let edges = store.graph_edges_between("n1", "n2", 1).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].id.0, "e1");
    }

    #[test]
    fn test_query_limit_is_capped() {
        assert_eq!(super::clamp_limit(0), 100);
        assert_eq!(super::clamp_limit(5), 5);
        assert_eq!(super::clamp_limit(5000), 1000);
    }

    #[test]
    fn test_graph_schema_counts_returns_sorted_type_counts() {
        let store = make_store();
        let node1 = GraphNode {
            id: NodeId::new("n1"),
            node_type: GraphNodeType::File,
            ..Default::default()
        };
        let node2 = GraphNode {
            id: NodeId::new("n2"),
            node_type: GraphNodeType::File,
            ..Default::default()
        };
        let node3 = GraphNode {
            id: NodeId::new("n3"),
            node_type: GraphNodeType::Function,
            ..Default::default()
        };
        let edge1 = GraphEdge {
            id: EdgeId::new("e1"),
            from: NodeId::new("n1"),
            to: NodeId::new("n2"),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        };
        store
            .replace_graph(&[node1, node2, node3], &[edge1])
            .unwrap();

        let counts = store.graph_schema_counts().unwrap();
        assert_eq!(counts.node_types.get("File"), Some(&2));
        assert_eq!(counts.node_types.get("Function"), Some(&1));
        assert_eq!(counts.edge_types.get("Calls"), Some(&1));
    }

    #[test]
    fn test_graph_counts_returns_total_nodes_and_edges() {
        let store = make_store();
        let node1 = GraphNode {
            id: NodeId::new("n1"),
            node_type: GraphNodeType::File,
            ..Default::default()
        };
        let node2 = GraphNode {
            id: NodeId::new("n2"),
            node_type: GraphNodeType::File,
            ..Default::default()
        };
        let edge1 = GraphEdge {
            id: EdgeId::new("e1"),
            from: NodeId::new("n1"),
            to: NodeId::new("n2"),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        };
        store.replace_graph(&[node1, node2], &[edge1]).unwrap();

        let overall = store.graph_counts().unwrap();
        assert_eq!(overall.nodes, 2);
        assert_eq!(overall.edges, 1);
    }
}
