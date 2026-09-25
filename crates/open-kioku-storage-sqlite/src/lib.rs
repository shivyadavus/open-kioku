mod compact;

use chrono::{DateTime, Utc};
use open_kioku_core::{
    AnalysisFact, ChurnEntityKind, ChurnStats, ChurnSummary, CodeChunk, Confidence,
    DocumentSection, EvidenceSourceType, File, FileId, FileProvenance, GitCochangeEdge,
    GitCommitId, GitCommitRecord, GitFileTouch, GitSymbolTouch, GraphEdge, GraphEdgeType,
    GraphNode, GraphNodeType, HistoricalChangeSummary, HistoryRecordId, HistorySnapshot,
    HistorySummary, Import, IndexManifest, ProvenanceTouch, SimilarChangeHit, SimilarChangeQuery,
    SimilarChangeReport, SimilarityEvidence, SimilarityEvidenceSource, Symbol, SymbolId,
    SymbolOccurrence, SymbolProvenance, TestTarget, HISTORY_SCHEMA_VERSION,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_storage::{
    generations::{IndexRefusalState, NotIndexedStatus},
    GraphCounts, GraphSchemaCounts, GraphStore, HistoryStore, IndexData, MetadataStore,
    PartialIndexUpdate,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const SQLITE_HISTORY_SCHEMA_VERSION: i64 = 1;
pub const SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION: i64 = 4;
const SQLITE_GRAPH_SCHEMA_VERSION: i64 = SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION;
const SQLITE_SUPPORTED_SCHEMA_VERSION: i64 = SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION;
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Cached verdict of `require_authoritative_relationship_semantics`, keyed by SQLite's
    /// `data_version` so writes from other connections invalidate it. The manifest is several
    /// megabytes of JSON on a large repository and was re-parsed on every relationship query.
    semantics_verdict: Mutex<Option<(i64, std::result::Result<(), String>)>>,
    /// Co-change edges and file hotspots for similar-change scoring, keyed by `data_version`.
    /// `similar_changes` runs once per primary result while a context pack is annotated (about
    /// twenty times per pack) and each call re-read and re-parsed both tables; on a 10k-file
    /// repository with history that was ~5 s of a 10 s pack.
    similarity_statics: Mutex<Option<(i64, std::sync::Arc<SimilarityStatics>)>>,
}

struct SimilarityStatics {
    cochange_edges: Vec<GitCochangeEdge>,
    hotspots: BTreeMap<String, ChurnSummary>,
}

impl SqliteStore {
    /// Open the index at `path`, creating the file and its directory when absent. This is the
    /// writer's open: `ok index`, `ok init`, snapshot import and the watcher go through it.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    }

    /// Open an index that already exists, creating neither the file nor its directory.
    ///
    /// Every read surface opens through here (or through `open_repo_index`), so a repository
    /// that has never been indexed stays untouched on disk. When reads went through `open`, a
    /// read-only MCP session or `ok search` left an empty `.ok/index.sqlite` behind, which
    /// every later read reported as a legacy index awaiting rebuild rather than as a
    /// repository nobody had indexed. The connection is still read-write: `initialize` runs
    /// the idempotent schema statements and the legacy-layout reset records its marker.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.is_file() {
            return Err(OkError::Index(format!(
                "no index database at {}",
                path.display()
            )));
        }
        Self::open_with_flags(
            path.to_path_buf(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    }

    /// The repository's active index for reading, or `None` when the repository has never
    /// been indexed: no database, or a database without a manifest, which is what a 4.0.0
    /// read surface left behind. `Err` is reserved for an index that cannot be served: one
    /// that exists and cannot be opened, one written by a newer Open Kioku, or one a live
    /// writer is building right now (it holds the `.ok/index.lock` advisory lock and no
    /// manifest is published), so a caller can tell "not indexed" from each of those and say
    /// the right thing.
    pub fn open_repo_index(repo: &Path) -> Result<Option<Self>> {
        Self::probe_repo_index(repo).map_err(|refusal| refusal.error)
    }

    /// [`open_repo_index`](Self::open_repo_index) with the refusal classified: the same store,
    /// `None`, or error, plus the [`IndexRefusalState`] a client branches on. The state is
    /// decided from the index itself (the lock, the database's `user_version`, the manifest's
    /// `schema_version`), never from the error text.
    pub fn probe_repo_index(repo: &Path) -> std::result::Result<Option<Self>, IndexOpenRefusal> {
        let path = open_kioku_storage::generations::resolve_index_location(repo).sqlite_path();
        if !path.is_file() {
            return Self::unindexed(repo);
        }
        let store = match Self::open_for_probe(&path) {
            Ok(store) => store,
            Err(error) => return Err(Self::unservable(repo, &path, None, error)),
        };
        // `initialize` is what refuses a database this binary cannot read, and the probe does
        // not run it. Serving one anyway would answer from a schema we do not understand, so
        // the probe applies the same check, with the same sentence.
        if let Some(version) = newer_sqlite_schema(&path) {
            return Err(IndexOpenRefusal {
                state: IndexRefusalState::IndexNewerThanBinary,
                error: OkError::Storage(newer_sqlite_schema_message(version)),
            });
        }
        match store.manifest() {
            Ok(Some(_)) => Ok(Some(store)),
            Ok(None) => Self::unindexed(repo),
            Err(error) => Err(Self::unservable(repo, &path, Some(&store), error)),
        }
    }

    /// Why an index that exists cannot be served, for either the open or the manifest read.
    ///
    /// A newer schema is decided by the index itself. Otherwise a live writer explains it:
    /// `ok index` and `ok watch` hold the lock across the write transaction a reader can fail
    /// on, so reporting an unavailable index there would tell the agent to rebuild while
    /// `ok doctor` on the same state says wait. The error text is the one every read surface
    /// prints either way; only the state and the next step differ.
    fn unservable(
        repo: &Path,
        path: &Path,
        store: Option<&Self>,
        error: OkError,
    ) -> IndexOpenRefusal {
        let newer = newer_sqlite_schema(path).is_some()
            || store.is_some_and(|store| store.stored_manifest_is_newer());
        let state = if newer {
            IndexRefusalState::IndexNewerThanBinary
        } else if open_kioku_storage::generations::index_write_in_progress(repo) {
            IndexRefusalState::IndexingInProgress
        } else {
            IndexRefusalState::IndexUnavailable
        };
        IndexOpenRefusal { state, error }
    }

    /// No manifest is "not indexed" unless a writer holds the lock: the manifest is the last
    /// thing an index run writes, so its absence under the lock is an index being built.
    fn unindexed(repo: &Path) -> std::result::Result<Option<Self>, IndexOpenRefusal> {
        if open_kioku_storage::generations::index_write_in_progress(repo) {
            return Err(IndexOpenRefusal {
                state: IndexRefusalState::IndexingInProgress,
                error: OkError::Index(
                    open_kioku_storage::generations::indexing_in_progress_message(repo),
                ),
            });
        }
        Ok(None)
    }

    /// Whether the stored manifest row declares a schema newer than this binary reads. Only
    /// consulted to classify a manifest read that already failed, so an unreadable row is
    /// `false` and the failure is reported as an unavailable index with its own error.
    fn stored_manifest_is_newer(&self) -> bool {
        let Ok(conn) = self.connection.lock() else {
            return false;
        };
        conn.query_row("SELECT json FROM manifests WHERE id = 1", [], |row| {
            row.get::<_, String>(0)
        })
        .ok()
        .and_then(|json| manifest_schema_version(&json).ok())
        .is_some_and(|version| version > open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION)
    }

    /// The not-indexed status `repo_status` and `ok --json status` return, with the withdrawal
    /// reason when the repository's database has rows whose manifest was withdrawn. Creates
    /// nothing: without a database it is the plain not-indexed status.
    pub fn repo_not_indexed_status(repo: &Path) -> Result<NotIndexedStatus> {
        let status = open_kioku_storage::generations::not_indexed_status(repo);
        if !status.index_path.is_file() {
            return Ok(status);
        }
        let reason = Self::open_for_probe(&status.index_path)?.manifest_withdrawal()?;
        Ok(NotIndexedStatus { reason, ..status })
    }

    /// [`repo_not_indexed_status`](Self::repo_not_indexed_status) for a store already open.
    pub fn not_indexed_status(&self, repo: &Path) -> Result<NotIndexedStatus> {
        Ok(NotIndexedStatus {
            reason: self.manifest_withdrawal()?,
            ..open_kioku_storage::generations::not_indexed_status(repo)
        })
    }

    fn open_with_flags(path: PathBuf, flags: rusqlite::OpenFlags) -> Result<Self> {
        let store = Self::connect(path, flags)?;
        store.initialize()?;
        Ok(store)
    }

    /// A connection and nothing else: no `initialize`, so no DDL and no write transaction.
    fn connect(path: PathBuf, flags: rusqlite::OpenFlags) -> Result<Self> {
        let connection = Connection::open_with_flags(&path, flags).map_err(storage_err)?;
        connection
            .busy_timeout(SQLITE_BUSY_TIMEOUT)
            .map_err(storage_err)?;
        Ok(Self {
            path,
            connection: Mutex::new(connection),
            semantics_verdict: Mutex::new(None),
            similarity_statics: Mutex::new(None),
        })
    }

    /// The open every probe uses: it never creates the file and never runs `initialize`, so
    /// answering "can this index be served" adds no table, index or trigger to someone's
    /// database and takes no write transaction. Read-only first, because a read-only surface
    /// must not write to answer a question; a read-only open of a WAL database whose `-shm` is
    /// gone (the usual state once the writer has exited) fails, and that case falls back to a
    /// read-write connection which still runs no schema statement.
    fn open_for_probe(path: &Path) -> Result<Self> {
        let read_only =
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        match Self::connect(path.to_path_buf(), read_only) {
            Ok(store) => Ok(store),
            Err(_) => Self::connect(
                path.to_path_buf(),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            ),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this store's graph edges were discarded on open and are waiting on `ok index`.
    ///
    /// Every relationship read already refuses such a store; this is for the status surfaces
    /// (`ok doctor`, `ok status`, `repo_status`) and the pre-checks in front of impact and plan,
    /// which need to report the marker rather than discover it one failed read at a time.
    /// Rewrites the database without free pages, then truncates the WAL. SQLite keeps the
    /// bytes of deleted rows in free pages until they are reused, so after an index written
    /// before secret-value redaction is replaced, this is what removes the values it held
    /// (#379). The cost is one rewrite of the database, with free disk space about its size
    /// while it runs. A checkpoint blocked by another reader completes at a later checkpoint.
    pub fn vacuum(&self) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        conn.execute_batch("VACUUM;").map_err(storage_err)?;
        // `wal_checkpoint` does not fail when it is blocked: it returns `(busy, log,
        // checkpointed)` with `busy = 1` and leaves the log in place. Discarding that row
        // reported a compaction that had not happened, with the values still in the WAL.
        let (busy, _log, _checkpointed): (i64, i64, i64) = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(storage_err)?;
        if busy != 0 {
            return Err(OkError::Storage(
                "the write-ahead log could not be truncated because another connection is \
                 reading the database; bytes stored before secret-value redaction may remain in \
                 it"
                .into(),
            ));
        }
        Ok(())
    }

    pub fn graph_rebuild_required(&self) -> Result<bool> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        graph_rebuild_required(&conn)
    }

    /// `replace_index_with_documents` with the manifest withheld: every row is replaced and
    /// the previous manifest row is removed, so the index stays unpublished until
    /// `put_manifest`. Indexing writes the graph and the search index after the rows;
    /// publishing the manifest with the rows let a concurrent reader open a manifest whose
    /// graph was still the previous index's, or empty. While unpublished, readers report the
    /// index as being built when the writer holds `.ok/index.lock` and as unindexed otherwise.
    ///
    /// `data.manifest` is not written; the caller publishes it once the graph and search
    /// components are in place.
    pub fn stage_index_with_documents(
        &self,
        data: IndexData<'_>,
        document_sections: &[DocumentSection],
    ) -> Result<()> {
        self.replace_index_with_documents_and_manifest(
            data,
            document_sections,
            ManifestWrite::Withhold,
        )
    }

    /// The incremental writer's whole update in one transaction, with the manifest withheld
    /// as in [`stage_index_with_documents`](Self::stage_index_with_documents): the changed
    /// files' rows replace their predecessors, and the stored graph is reconciled with
    /// `nodes` and `edges`, the complete graph of the new snapshot.
    ///
    /// A file's edges are the ones anchored at a node the file owns (its file node and its
    /// symbols' nodes, in either direction) and the ones whose evidence range lies in the
    /// file; every one of them is removed and rebuilt from the new graph, so a renamed symbol
    /// loses its old callers and a moved call site gets its new range. Edges that end at a
    /// node derived from the file's content but start in an unchanged file (a resolved call
    /// to the renamed symbol, a name reference from a config file) are not anchored at a node
    /// the file owns, so they are reconciled by identity instead: every stored node and edge
    /// absent from the new graph is removed, and every node and edge of the new graph absent
    /// from the store is added. That pass reads every stored edge id once, which is
    /// proportional to the graph rather than to the change, and is what makes the stored graph
    /// match a clean rebuild rather than approximate it. Unchanged edges keep their stored
    /// evidence, including its `indexed_at`.
    ///
    /// The previous manifest stays published: readers are not told the repository is
    /// unindexed, and they read this update's rows and graph, under the previous manifest, as
    /// soon as the transaction commits. The caller publishes the new manifest once the search
    /// index is rebuilt, or withdraws the previous one (`withdraw_manifest`) if a later step
    /// fails. `update.graph_nodes` and `update.graph_edges` are inserted before the
    /// reconciliation and are normally empty here.
    pub fn stage_files_index_with_graph(
        &self,
        update: PartialIndexUpdate<'_>,
        nodes: &[GraphNode],
        edges: &[GraphEdge],
    ) -> Result<GraphReconciliation> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        let report =
            replace_files_rows(&tx, &update, ManifestWrite::Withhold, Some((nodes, edges)))?;
        tx.commit().map_err(storage_err)?;
        self.invalidate_semantics_verdict();
        Ok(report)
    }

    /// Every repository-relative path the store's rows name, split by what governs them:
    /// indexed content (files and document sections), which the scan policy decides, and Git
    /// history, which is recorded for every path a commit touched whatever that policy says.
    pub fn stored_paths(&self) -> Result<StoredPaths> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let read = |queries: &[&str]| -> Result<BTreeSet<PathBuf>> {
            let mut paths = BTreeSet::new();
            for sql in queries {
                let mut stmt = conn.prepare(sql).map_err(storage_err)?;
                let rows = stmt
                    .query_map([], |row| row.get::<_, Option<String>>(0))
                    .map_err(storage_err)?;
                for row in rows {
                    if let Some(path) = row.map_err(storage_err)? {
                        paths.insert(PathBuf::from(path));
                    }
                }
            }
            Ok(paths)
        };
        let mut unanchored_nodes = Vec::new();
        let mut stmt = conn
            .prepare(
                "SELECT id, label FROM graph_nodes WHERE file_id IS NULL OR file_id = '' \
                 ORDER BY id",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(storage_err)?;
        for row in rows {
            unanchored_nodes.push(row.map_err(storage_err)?);
        }
        drop(stmt);
        Ok(StoredPaths {
            indexed: read(INDEXED_PATH_QUERIES)?,
            history: read(HISTORY_PATH_QUERIES)?,
            unanchored_nodes,
        })
    }

    /// Every way the store's rows disagree with each other where readers rely on them to
    /// agree, one line per check that failed; empty for a store `ok index` wrote.
    ///
    /// The path policy an import applies is decided on the path columns, and readers serve
    /// the path inside each row's JSON, resolve content through `file_id`, and find graph
    /// strings by their hash. A row whose column says `src/ok.rs` and whose JSON says `.env`
    /// would pass the policy and be served as `.env`; an orphaned chunk or a dictionary entry
    /// under the wrong hash would escape the purge. None of these states can be produced by
    /// the writers, so any of them means the database was not written by Open Kioku as it
    /// stands, and it is refused rather than repaired.
    pub fn consistency_violations(&self) -> Result<Vec<String>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut violations = Vec::new();
        for (check, sql) in CONSISTENCY_CHECKS {
            let count: i64 = conn
                .query_row(sql, [], |row| row.get(0))
                .map_err(storage_err)?;
            if count > 0 {
                violations.push(format!("{count} row(s): {check}"));
            }
        }
        let mut stmt = conn
            .prepare("SELECT vhash, value FROM graph_strings")
            .map_err(storage_err)?;
        let mut rows = stmt.query([]).map_err(storage_err)?;
        let mut mismatched = 0usize;
        while let Some(row) = rows.next().map_err(storage_err)? {
            let vhash: i64 = row.get(0).map_err(storage_err)?;
            let value: String = row.get(1).map_err(storage_err)?;
            if compact::fnv1a64(&value) != vhash {
                mismatched += 1;
            }
        }
        if mismatched > 0 {
            violations.push(format!(
                "{mismatched} row(s): graph dictionary entries whose hash does not match their value"
            ));
        }
        Ok(violations)
    }

    /// Remove, in one transaction, every row derived from indexing one of `indexed`: each
    /// file's rows and graph nodes, the edges anchored at them or evidenced in the file, its
    /// vector targets and document sections, the facts other files hold about it, and the
    /// symbol-level history derived from its symbols. Its file-level Git history is kept, as
    /// `ok index` keeps it for a path the scan policy excludes. Every history row naming one
    /// of `history` is removed as well. The manifest is not written; `manifest` only
    /// satisfies the shared row writer, as in
    /// [`stage_files_index_with_graph`](Self::stage_files_index_with_graph).
    pub fn purge_paths(
        &self,
        indexed: &BTreeSet<PathBuf>,
        history: &BTreeSet<PathBuf>,
        graph_nodes: &BTreeSet<String>,
        manifest: &IndexManifest,
    ) -> Result<PathPurge> {
        let mut report = PathPurge::default();
        if indexed.is_empty() && history.is_empty() && graph_nodes.is_empty() {
            return Ok(report);
        }
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        let mut file_ids = Vec::new();
        for path in indexed {
            let path = path.to_string_lossy();
            let id: Option<String> = tx
                .query_row(
                    "SELECT id FROM files WHERE path = ?1",
                    params![path],
                    |row| row.get(0),
                )
                .optional()
                .map_err(storage_err)?;
            if let Some(id) = id {
                file_ids.push(FileId::new(id));
            }
        }
        let count_for = |sql: &str, id: &FileId| -> Result<usize> {
            tx.query_row(sql, params![&id.0], |row| row.get::<_, i64>(0))
                .map(|count| count as usize)
                .map_err(storage_err)
        };
        for id in &file_ids {
            report.symbols_removed +=
                count_for("SELECT COUNT(*) FROM symbols WHERE file_id = ?1", id)?;
            report.chunks_removed +=
                count_for("SELECT COUNT(*) FROM chunks WHERE file_id = ?1", id)?;
            tx.execute(
                "DELETE FROM vector_targets WHERE file_id = ?1",
                params![&id.0],
            )
            .map_err(storage_err)?;
        }
        report.files_removed = file_ids.len();
        let update = PartialIndexUpdate {
            manifest,
            changed_files: &[],
            deleted_file_ids: &file_ids,
            symbols: &[],
            chunks: &[],
            tests: &[],
            imports: &[],
            occurrences: &[],
            analysis_facts: &[],
            graph_nodes: &[],
            graph_edges: &[],
            scopes: &[],
            bindings: &[],
            call_sites: &[],
        };
        replace_files_rows(&tx, &update, ManifestWrite::Withhold, None)?;
        // Nodes no file owns (a test named by history, a resource) go with every edge at them.
        let mut orphan_candidates = HashSet::new();
        for node_id in graph_nodes {
            if let Some(sid) = compact::lookup_sid(&tx, compact::GRAPH_STRINGS, node_id)? {
                delete_edges_at_node(&tx, sid, &mut orphan_candidates)?;
                orphan_candidates.insert(sid);
            }
            report.graph_nodes_removed += tx
                .execute("DELETE FROM graph_nodes WHERE id = ?1", params![node_id])
                .map_err(storage_err)?;
        }
        remove_orphan_graph_strings(&tx, orphan_candidates)?;
        for (paths, statements) in [
            (indexed, INDEXED_PATH_PURGE_STATEMENTS),
            (history, HISTORY_PATH_PURGE_STATEMENTS),
        ] {
            for path in paths {
                let path = path.to_string_lossy();
                for sql in statements {
                    report.other_rows_removed +=
                        tx.execute(sql, params![path]).map_err(storage_err)?;
                }
            }
        }
        tx.commit().map_err(storage_err)?;
        self.invalidate_semantics_verdict();
        Ok(report)
    }

    fn replace_index_with_documents_and_manifest(
        &self,
        data: IndexData<'_>,
        document_sections: &[DocumentSection],
        manifest: ManifestWrite,
    ) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        replace_index_rows(&tx, data, manifest)?;
        tx.execute("DELETE FROM document_sections", [])
            .map_err(storage_err)?;
        insert_document_sections(&tx, document_sections)?;
        tx.commit().map_err(storage_err)?;
        self.invalidate_semantics_verdict();
        Ok(())
    }

    /// Whether a manifest is published, without deserializing it. A long-lived reader (the
    /// MCP server) checks this before each request: a full `ok index` removes the manifest
    /// when it stages its rows, and a store opened before that must not keep answering from
    /// rows whose graph and search index are being rewritten.
    pub fn has_manifest(&self) -> Result<bool> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        conn.query_row("SELECT 1 FROM manifests WHERE id = 1", [], |_| Ok(()))
            .optional()
            .map(|row| row.is_some())
            .map_err(storage_err)
    }

    /// Remove the published manifest, leaving every other row in place. For a writer whose
    /// committed rows no longer match the previous manifest and which failed before it could
    /// publish its own: the repository then reads as unindexed rather than as the previous
    /// index over this run's rows.
    ///
    /// `reason` is recorded in the same transaction and reported as the not-indexed status's
    /// `reason` while no manifest is published, so the withdrawn index is not described as one
    /// nobody built. Publishing a manifest, by any binary, removes it (a trigger on
    /// `manifests`), and so does replacing the rows.
    pub fn withdraw_manifest(&self, reason: &str) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        tx.execute("DELETE FROM manifests", [])
            .map_err(storage_err)?;
        tx.execute(
            "INSERT INTO manifest_withdrawals(id, reason, withdrawn_at) VALUES(1, ?1, ?2)
             ON CONFLICT(id) DO UPDATE SET reason = excluded.reason, withdrawn_at = excluded.withdrawn_at",
            params![reason, Utc::now().to_rfc3339()],
        )
        .map_err(storage_err)?;
        tx.commit().map_err(storage_err)?;
        self.invalidate_semantics_verdict();
        Ok(())
    }

    /// The reason recorded by the last [`withdraw_manifest`](Self::withdraw_manifest), and only
    /// while the index is withdrawn: `None` whenever a manifest is published, whatever row is
    /// left in the table.
    ///
    /// A database without the table has no withdrawal to report, and a probe must not create
    /// one to find that out, so the table is checked rather than assumed.
    pub fn manifest_withdrawal(&self) -> Result<Option<String>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        if !table_exists(&conn, "manifest_withdrawals")? {
            return Ok(None);
        }
        conn.query_row(
            "SELECT reason FROM manifest_withdrawals
             WHERE id = 1 AND NOT EXISTS (SELECT 1 FROM manifests)",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage_err)
    }

    /// SQLite's `PRAGMA data_version`: changes when another connection commits. Our own
    /// writes do not move it, so manifest writers call `invalidate_semantics_verdict`.
    fn data_version(&self) -> Result<i64> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        conn.query_row("PRAGMA data_version", [], |row| row.get(0))
            .map_err(storage_err)
    }

    fn invalidate_semantics_verdict(&self) {
        if let Ok(mut verdict) = self.semantics_verdict.lock() {
            *verdict = None;
        }
    }

    fn invalidate_similarity_statics(&self) {
        if let Ok(mut statics) = self.similarity_statics.lock() {
            *statics = None;
        }
    }

    /// The caller already holds the connection lock; `data_version` is read through it.
    fn similarity_statics(&self, conn: &Connection) -> Result<std::sync::Arc<SimilarityStatics>> {
        let version: i64 = conn
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .map_err(storage_err)?;
        let mut cache = self
            .similarity_statics
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        if let Some((cached_version, statics)) = cache.as_ref() {
            if *cached_version == version {
                return Ok(statics.clone());
            }
        }
        let statics = std::sync::Arc::new(SimilarityStatics {
            cochange_edges: load_similarity_cochange_edges(conn)?,
            hotspots: load_similarity_file_hotspots(conn)?,
        });
        *cache = Some((version, statics.clone()));
        Ok(statics)
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
        reset_legacy_graph_storage(&mut conn)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            CREATE TABLE IF NOT EXISTS manifests (
              id INTEGER PRIMARY KEY CHECK (id = 1),
              json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS manifest_withdrawals (
              id INTEGER PRIMARY KEY CHECK (id = 1),
              reason TEXT NOT NULL,
              withdrawn_at TEXT NOT NULL
            );
            -- Publishing a manifest ends a withdrawal whichever binary publishes it, including
            -- one that does not know this table, so a reason can never outlive its index.
            CREATE TRIGGER IF NOT EXISTS manifest_insert_ends_withdrawal
              AFTER INSERT ON manifests BEGIN DELETE FROM manifest_withdrawals; END;
            CREATE TRIGGER IF NOT EXISTS manifest_update_ends_withdrawal
              AFTER UPDATE ON manifests BEGIN DELETE FROM manifest_withdrawals; END;
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
            CREATE INDEX IF NOT EXISTS idx_symbols_name_nocase ON symbols(name COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS idx_symbols_qualified_name ON symbols(qualified_name);
            CREATE TABLE IF NOT EXISTS chunks (
              id TEXT PRIMARY KEY,
              file_id TEXT NOT NULL,
              start_line INTEGER NOT NULL,
              end_line INTEGER NOT NULL,
              text TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_id);
            CREATE TABLE IF NOT EXISTS document_sections (
              path TEXT NOT NULL,
              start_line INTEGER NOT NULL,
              end_line INTEGER NOT NULL,
              content_hash TEXT NOT NULL,
              json TEXT NOT NULL,
              PRIMARY KEY(path, start_line, end_line)
            );
            CREATE INDEX IF NOT EXISTS idx_document_sections_path
              ON document_sections(path, start_line);
            CREATE INDEX IF NOT EXISTS idx_document_sections_hash
              ON document_sections(content_hash);
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
            CREATE INDEX IF NOT EXISTS idx_analysis_facts_target ON analysis_facts(target);
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
              evidence_available BOOLEAN DEFAULT 0,
              freshness INTEGER DEFAULT 0,
              json TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS graph_strings (
              sid INTEGER PRIMARY KEY,
              vhash INTEGER NOT NULL,
              value TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_graph_strings_vhash ON graph_strings(vhash);
            CREATE TABLE IF NOT EXISTS graph_edges (
              id TEXT PRIMARY KEY,
              from_sid INTEGER NOT NULL,
              to_sid INTEGER NOT NULL,
              edge_type TEXT NOT NULL,
              confidence TEXT NOT NULL DEFAULT '',
              source_type TEXT NOT NULL DEFAULT '',
              source_sid INTEGER,
              freshness INTEGER NOT NULL DEFAULT 0,
              ev_id TEXT NOT NULL DEFAULT '',
              ev_path_sid INTEGER,
              ev_line_start INTEGER,
              ev_line_end INTEGER,
              ev_symbol_sid INTEGER,
              ev_message_sid INTEGER,
              ev_indexed_at_sid INTEGER,
              extra_sid INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_graph_edges_from ON graph_edges(from_sid);
            CREATE INDEX IF NOT EXISTS idx_graph_edges_to ON graph_edges(to_sid);

            CREATE TABLE IF NOT EXISTS scopes (
              id TEXT PRIMARY KEY,
              file_id TEXT NOT NULL,
              parent_id TEXT,
              owner_symbol_id TEXT,
              kind TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_scopes_file ON scopes(file_id);

            CREATE TABLE IF NOT EXISTS bindings (
              id TEXT PRIMARY KEY,
              file_id TEXT NOT NULL,
              scope_id TEXT NOT NULL,
              name TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_bindings_lookup ON bindings(file_id, scope_id, name);

            CREATE TABLE IF NOT EXISTS call_site_strings (
              sid INTEGER PRIMARY KEY,
              vhash INTEGER NOT NULL,
              value TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_call_site_strings_vhash ON call_site_strings(vhash);
            CREATE TABLE IF NOT EXISTS call_sites (
              id_sid INTEGER PRIMARY KEY,
              file_sid INTEGER NOT NULL,
              scope_sid INTEGER NOT NULL,
              caller_sid INTEGER,
              callee_sid INTEGER NOT NULL,
              receiver_sid INTEGER,
              receiver_kind TEXT NOT NULL DEFAULT 'Unknown',
              start_line INTEGER NOT NULL,
              start_column INTEGER NOT NULL,
              end_line INTEGER NOT NULL DEFAULT 0,
              end_column INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_call_sites_caller ON call_sites(caller_sid);
            CREATE INDEX IF NOT EXISTS idx_call_sites_name ON call_sites(callee_sid);
            CREATE INDEX IF NOT EXISTS idx_call_sites_file ON call_sites(file_sid);

            CREATE TABLE IF NOT EXISTS relationship_evidence (
              id TEXT PRIMARY KEY,
              edge_id TEXT NOT NULL,
              source_type TEXT NOT NULL,
              json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_relationship_evidence_edge ON relationship_evidence(edge_id);
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
        // The manifest triggers end any outstanding withdrawal in the same statement.
        conn.execute(
            "INSERT INTO manifests(id, json) VALUES(1, ?1) ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![json],
        )
        .map_err(storage_err)?;
        self.invalidate_semantics_verdict();
        Ok(())
    }

    /// Reads `$.quality.coverage` through SQLite's JSON support instead of decoding the
    /// whole manifest. `json_extract` yields SQL NULL both when the path is absent and when
    /// its value is JSON null, and both mean the same thing here: no coverage record.
    /// `index_coverage_matches_the_full_manifest_decode` holds this equal to the default.
    fn index_coverage(&self) -> Result<Option<open_kioku_core::IndexCoverage>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let raw: Option<Option<String>> = conn
            .query_row(
                "SELECT json_extract(json, '$.quality.coverage') FROM manifests WHERE id = 1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(storage_err)?;
        match raw.flatten() {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    fn snapshot_provenance(&self) -> Result<Option<open_kioku_core::SnapshotProvenance>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let raw: Option<Option<String>> = conn
            .query_row(
                "SELECT json_extract(json, '$.snapshot') FROM manifests WHERE id = 1",
                [],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(storage_err)?;
        match raw.flatten() {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
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
        raw.as_deref().map(decode_index_manifest).transpose()
    }

    fn replace_index(&self, data: IndexData<'_>) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        replace_index_rows(&tx, data, ManifestWrite::Publish)?;
        tx.execute("DELETE FROM document_sections", [])
            .map_err(storage_err)?;
        tx.commit().map_err(storage_err)?;
        self.invalidate_semantics_verdict();
        Ok(())
    }

    fn replace_index_with_documents(
        &self,
        data: IndexData<'_>,
        document_sections: &[DocumentSection],
    ) -> Result<()> {
        self.replace_index_with_documents_and_manifest(
            data,
            document_sections,
            ManifestWrite::Publish,
        )
    }

    fn replace_files_index(&self, update: PartialIndexUpdate<'_>) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        replace_files_rows(&tx, &update, ManifestWrite::Publish, None)?;
        tx.commit().map_err(storage_err)?;
        self.invalidate_semantics_verdict();
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

    fn file_by_id(&self, id: &FileId) -> Result<Option<File>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT json FROM files WHERE id = ?1",
                params![&id.0],
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

    fn symbols_named(&self, name: &str, limit: usize) -> Result<Vec<Symbol>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        // Both branches are satisfied from dedicated indexes (name COLLATE NOCASE and
        // qualified_name), so exact lookups never scan the symbols table.
        let mut stmt = conn
            .prepare(
                "SELECT json FROM symbols WHERE name = ?1 COLLATE NOCASE OR qualified_name = ?1 ORDER BY qualified_name LIMIT ?2",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![name, limit as i64], |row| row.get::<_, String>(0))
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
            .prepare("SELECT json FROM chunks WHERE file_id = ?1 ORDER BY start_line, end_line, id")
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
            .prepare("SELECT json FROM chunks ORDER BY file_id, start_line, end_line, id")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn document_sections(&self) -> Result<Vec<DocumentSection>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM document_sections ORDER BY path, start_line, end_line")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn replace_document_corpus(&self, sections: &[DocumentSection]) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        tx.execute("DELETE FROM document_sections", [])
            .map_err(storage_err)?;
        insert_document_sections(&tx, sections)?;
        tx.commit().map_err(storage_err)?;
        Ok(())
    }

    fn replace_document_sections_for_paths(
        &self,
        paths: &[PathBuf],
        sections: &[DocumentSection],
    ) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let changed = paths
            .iter()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect::<BTreeSet<_>>();
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let tx = conn.transaction().map_err(storage_err)?;
        for path in &changed {
            tx.execute(
                "DELETE FROM document_sections WHERE path = ?1",
                params![path],
            )
            .map_err(storage_err)?;
        }
        let replacements = sections
            .iter()
            .filter(|section| changed.contains(&section.path.to_string_lossy().replace('\\', "/")))
            .cloned()
            .collect::<Vec<_>>();
        insert_document_sections(&tx, &replacements)?;
        tx.commit().map_err(storage_err)?;
        Ok(())
    }

    fn tests(&self) -> Result<Vec<TestTarget>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare("SELECT json FROM tests ORDER BY file_id, id")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn imports(&self) -> Result<Vec<Import>> {
        require_authoritative_relationship_semantics(self)?;
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
                    "SELECT json FROM analysis_facts WHERE source_type = ?1 ORDER BY file_id, target, id LIMIT ?2",
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
                .prepare("SELECT json FROM analysis_facts ORDER BY file_id, target, id LIMIT ?1")
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![limit], |row| row.get::<_, String>(0))
                .map_err(storage_err)?;
            collect_json(rows)?
        };
        Ok(rows)
    }

    fn analysis_facts_for_file(
        &self,
        file_id: &FileId,
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
                    "SELECT json FROM analysis_facts WHERE file_id = ?1 AND source_type = ?2 ORDER BY target, id LIMIT ?3",
                )
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(
                    params![&file_id.0, source_type_name(&source_type), limit],
                    |row| row.get::<_, String>(0),
                )
                .map_err(storage_err)?;
            collect_json(rows)?
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT json FROM analysis_facts WHERE file_id = ?1 ORDER BY target, id LIMIT ?2",
                )
                .map_err(storage_err)?;
            let rows = stmt
                .query_map(params![&file_id.0, limit], |row| row.get::<_, String>(0))
                .map_err(storage_err)?;
            collect_json(rows)?
        };
        Ok(rows)
    }

    fn implementation_facts_for_target(
        &self,
        target: &str,
        limit: usize,
    ) -> Result<Vec<AnalysisFact>> {
        require_authoritative_relationship_semantics(self)?;
        let target = target.trim();
        if target.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let limit = limit.min(i64::MAX as usize) as i64;
        let mut stmt = conn
            .prepare(
                "SELECT json FROM analysis_facts
                 WHERE json_extract(json, '$.edge_type') = 'IMPLEMENTS'
                   AND (
                     target = ?1
                     OR (
                       length(target) > length(?1)
                       AND substr(target, -length(?1)) = ?1
                       AND (
                         substr(target, -length(?1) - 1, 1) = '.'
                         OR substr(target, -length(?1) - 1, 1) = ':'
                       )
                     )
                   )
                 ORDER BY file_id, target, id
                 LIMIT ?2",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![target, limit], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn references_for_symbol(&self, id: &SymbolId, limit: usize) -> Result<Vec<SymbolOccurrence>> {
        require_authoritative_relationship_semantics(self)?;
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let mut stmt = conn
            .prepare(
                "SELECT json FROM occurrences WHERE symbol_id = ?1 AND is_definition = 0 ORDER BY file_id, id LIMIT ?2",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![&id.0, limit as i64], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        collect_json(rows)
    }

    fn occurrences_for_file(&self, file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {
        require_authoritative_relationship_semantics(self)?;
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
        self.invalidate_similarity_statics();
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
        let statics = self.similarity_statics(&conn)?;
        let hotspots: &BTreeMap<String, ChurnSummary> = &statics.hotspots;

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
        for edge in statics.cochange_edges.iter().cloned() {
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
                hotspots,
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
    scopes: &'a [open_kioku_core::Scope],
    bindings: &'a [open_kioku_core::Binding],
    call_sites: &'a [open_kioku_core::CallSite],
}

/// Whether a row replacement also writes the manifest that publishes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManifestWrite {
    Publish,
    Withhold,
}

/// The indexed-content path columns [`SqliteStore::stored_paths`] reads.
const INDEXED_PATH_QUERIES: &[&str] = &[
    "SELECT path FROM files",
    "SELECT DISTINCT path FROM document_sections",
];

/// The Git history path columns [`SqliteStore::stored_paths`] reads.
const HISTORY_PATH_QUERIES: &[&str] = &[
    "SELECT DISTINCT path FROM git_file_touches",
    "SELECT DISTINCT previous_path FROM git_file_touches",
    "SELECT DISTINCT file_path FROM git_symbol_touches",
    "SELECT DISTINCT path FROM git_cochange_edges",
    "SELECT DISTINCT cochanged_path FROM git_cochange_edges",
    "SELECT DISTINCT path FROM git_review_events",
    "SELECT DISTINCT path FROM history_hotspots",
];

/// Rows outside the file tables that exist only because a file was indexed: its document
/// sections, the facts other indexed files hold about it (co-change is recorded between
/// indexed files only), and its symbols' history. See [`SqliteStore::purge_paths`].
const INDEXED_PATH_PURGE_STATEMENTS: &[&str] = &[
    "DELETE FROM document_sections WHERE path = ?1",
    "DELETE FROM analysis_facts WHERE target = ?1",
    "DELETE FROM git_symbol_touches WHERE file_path = ?1",
    "DELETE FROM history_hotspots WHERE path = ?1 AND entity_kind = 'symbol'",
];

/// Every history row that names a path. See [`SqliteStore::purge_paths`].
const HISTORY_PATH_PURGE_STATEMENTS: &[&str] = &[
    "DELETE FROM git_file_touches WHERE path = ?1 OR previous_path = ?1",
    "DELETE FROM git_symbol_touches WHERE file_path = ?1",
    "DELETE FROM git_cochange_edges WHERE path = ?1 OR cochanged_path = ?1",
    "DELETE FROM git_review_events WHERE path = ?1",
    "DELETE FROM history_hotspots WHERE path = ?1",
    "DELETE FROM analysis_facts WHERE target = ?1",
];

/// What [`SqliteStore::stored_paths`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StoredPaths {
    pub indexed: BTreeSet<PathBuf>,
    pub history: BTreeSet<PathBuf>,
    /// `(id, label)` of every graph node no file owns. Such a node's label can be a path (a
    /// test named by history), and no file purge reaches it.
    pub unanchored_nodes: Vec<(String, String)>,
}

/// [`SqliteStore::consistency_violations`]: each query counts the rows that break one
/// invariant every Open Kioku writer keeps. JSON and column values are compared with `IS NOT`
/// so a NULL on one side and a value on the other counts.
const CONSISTENCY_CHECKS: &[(&str, &str)] = &[
    (
        "files whose path or id column disagrees with their JSON",
        "SELECT COUNT(*) FROM files WHERE json_extract(json, '$.path') IS NOT path \
         OR json_extract(json, '$.id') IS NOT id",
    ),
    (
        "document sections whose path column disagrees with their JSON",
        // The column is written with `/` separators and the JSON as the platform spells it.
        "SELECT COUNT(*) FROM document_sections \
         WHERE replace(json_extract(json, '$.path'), '\\', '/') IS NOT path",
    ),
    (
        "file history rows whose path columns disagree with their JSON",
        "SELECT COUNT(*) FROM git_file_touches WHERE json_extract(json, '$.path') IS NOT path \
         OR json_extract(json, '$.previous_path') IS NOT previous_path",
    ),
    (
        "symbol history rows whose file path column disagrees with their JSON",
        "SELECT COUNT(*) FROM git_symbol_touches \
         WHERE json_extract(json, '$.file_path') IS NOT file_path",
    ),
    (
        "co-change rows whose path columns disagree with their JSON",
        "SELECT COUNT(*) FROM git_cochange_edges WHERE json_extract(json, '$.path') IS NOT path \
         OR json_extract(json, '$.cochanged_path') IS NOT cochanged_path",
    ),
    (
        "review events whose path column disagrees with their JSON",
        "SELECT COUNT(*) FROM git_review_events WHERE json_extract(json, '$.path') IS NOT path",
    ),
    (
        "hotspots whose path column disagrees with their JSON",
        "SELECT COUNT(*) FROM history_hotspots WHERE json_extract(json, '$.path') IS NOT path",
    ),
    (
        "analysis facts whose file or target column disagrees with their JSON",
        "SELECT COUNT(*) FROM analysis_facts WHERE json_extract(json, '$.file_id') IS NOT file_id \
         OR json_extract(json, '$.target') IS NOT target",
    ),
    (
        "rows that belong to no indexed file",
        "SELECT (SELECT COUNT(*) FROM symbols WHERE file_id NOT IN (SELECT id FROM files) \
                 OR json_extract(json, '$.file_id') IS NOT file_id) \
              + (SELECT COUNT(*) FROM chunks WHERE file_id NOT IN (SELECT id FROM files) \
                 OR json_extract(json, '$.file_id') IS NOT file_id) \
              + (SELECT COUNT(*) FROM occurrences WHERE file_id NOT IN (SELECT id FROM files) \
                 OR json_extract(json, '$.file_id') IS NOT file_id) \
              + (SELECT COUNT(*) FROM tests WHERE file_id NOT IN (SELECT id FROM files) \
                 OR json_extract(json, '$.file_id') IS NOT file_id) \
              + (SELECT COUNT(*) FROM imports WHERE file_id NOT IN (SELECT id FROM files) \
                 OR json_extract(json, '$.file_id') IS NOT file_id) \
              + (SELECT COUNT(*) FROM analysis_facts WHERE file_id NOT IN (SELECT id FROM files)) \
              + (SELECT COUNT(*) FROM scopes WHERE file_id NOT IN (SELECT id FROM files)) \
              + (SELECT COUNT(*) FROM bindings WHERE file_id NOT IN (SELECT id FROM files)) \
              + (SELECT COUNT(*) FROM vector_targets WHERE file_id NOT IN (SELECT id FROM files)) \
              + (SELECT COUNT(*) FROM call_sites c JOIN call_site_strings s ON s.sid = c.file_sid \
                 WHERE s.value NOT IN (SELECT id FROM files))",
    ),
    (
        "history facts about a path that is not an indexed file",
        "SELECT COUNT(*) FROM analysis_facts WHERE source_type = 'git_history' \
         AND target NOT IN (SELECT path FROM files)",
    ),
    (
        "graph nodes whose columns disagree with their JSON",
        "SELECT COUNT(*) FROM graph_nodes WHERE json_extract(json, '$.id') IS NOT id \
         OR json_extract(json, '$.label') IS NOT label \
         OR COALESCE(json_extract(json, '$.file_id'), '') IS NOT COALESCE(file_id, '') \
         OR COALESCE(json_extract(json, '$.symbol_id'), '') IS NOT COALESCE(symbol_id, '')",
    ),
    (
        "graph nodes owned by no indexed file",
        "SELECT COUNT(*) FROM graph_nodes WHERE COALESCE(file_id, '') <> '' \
         AND file_id NOT IN (SELECT id FROM files)",
    ),
    (
        "file nodes not named for the file that owns them",
        "SELECT COUNT(*) FROM graph_nodes n LEFT JOIN files f ON f.id = n.file_id \
         WHERE n.node_type = 'File' AND (f.id IS NULL OR n.id IS NOT 'file:' || f.path \
         OR n.label IS NOT f.path)",
    ),
    (
        "graph edges whose evidence names a path that is not an indexed file",
        "SELECT COUNT(*) FROM graph_edges e JOIN graph_strings s ON s.sid = e.ev_path_sid \
         WHERE s.value NOT IN (SELECT path FROM files)",
    ),
];

/// What [`SqliteStore::purge_paths`] removed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PathPurge {
    pub files_removed: usize,
    /// Graph nodes no file owned, removed with their edges.
    pub graph_nodes_removed: usize,
    pub symbols_removed: usize,
    pub chunks_removed: usize,
    /// Document sections, facts about removed files, and history rows.
    pub other_rows_removed: usize,
}

/// What reconciling the stored graph with a snapshot's graph changed; see
/// [`SqliteStore::stage_files_index_with_graph`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GraphReconciliation {
    pub nodes_removed: usize,
    pub nodes_added: usize,
    pub edges_removed: usize,
    pub edges_added: usize,
    /// Dictionary entries no surviving edge referenced.
    pub strings_removed: usize,
}

/// The message every read surface prints for an index whose manifest was written by a newer
/// Open Kioku than this one. It is the same shape as the graph-rebuild message: what the
/// index is, and the two ways out.
pub fn newer_index_message(found: u32) -> String {
    format!(
        "index was written by a newer Open Kioku (manifest schema {found}; this version reads \
         up to {}): upgrade Open Kioku or run `ok index` to rebuild it with this version",
        open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION
    )
}

/// Deserialize a stored manifest. A manifest from a newer Open Kioku is refused with
/// [`newer_index_message`] rather than left to fail on whichever field it added: the version
/// is checked before the body is read, so the message names the situation instead of a serde
/// error, and a manifest with an older version reads through serde defaults as before.
pub fn decode_index_manifest(json: &str) -> Result<IndexManifest> {
    let schema_version = manifest_schema_version(json)?;
    if schema_version > open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION {
        return Err(OkError::Index(newer_index_message(schema_version)));
    }
    Ok(serde_json::from_str(json)?)
}

/// A stored manifest's `schema_version`, read without the body.
fn manifest_schema_version(json: &str) -> Result<u32> {
    #[derive(serde::Deserialize)]
    struct Header {
        #[serde(default)]
        schema_version: u32,
    }
    Ok(serde_json::from_str::<Header>(json)?.schema_version)
}

/// Why [`SqliteStore::probe_repo_index`] could not serve an index that exists.
#[derive(Debug)]
pub struct IndexOpenRefusal {
    pub state: IndexRefusalState,
    /// The error every read surface prints for it, unchanged by the classification.
    pub error: OkError,
}

/// Whether the database at `path` declares a `user_version` newer than this binary reads.
/// Only consulted to classify an open that already failed, through a read-only connection, so
/// a file that cannot be read at all is `false` and stays an unavailable index.
fn newer_sqlite_schema(path: &Path) -> Option<i64> {
    let conn =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .ok()?;
    (version > SQLITE_SUPPORTED_SCHEMA_VERSION).then_some(version)
}

/// What every surface says about a database whose schema this binary does not read. The probe
/// runs no `initialize`, so it applies the check itself and must say the same sentence.
fn newer_sqlite_schema_message(version: i64) -> String {
    format!(
        "sqlite schema version {version} is newer than supported version {SQLITE_SUPPORTED_SCHEMA_VERSION}"
    )
}

fn replace_index_rows(
    tx: &Transaction<'_>,
    data: IndexData<'_>,
    manifest: ManifestWrite,
) -> Result<()> {
    tx.execute("DELETE FROM call_sites", [])
        .map_err(storage_err)?;
    // The dictionary is owned by `call_sites`, so it is emptied with the rows that reference
    // it rather than being allowed to accumulate strings from earlier index runs.
    tx.execute("DELETE FROM call_site_strings", [])
        .map_err(storage_err)?;
    tx.execute("DELETE FROM bindings", [])
        .map_err(storage_err)?;
    tx.execute("DELETE FROM scopes", []).map_err(storage_err)?;
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
    // The rows a withdrawal described are gone, published or staged.
    tx.execute("DELETE FROM manifest_withdrawals", [])
        .map_err(storage_err)?;
    if manifest == ManifestWrite::Publish {
        tx.execute(
            "INSERT INTO manifests(id, json) VALUES(1, ?1)",
            params![serde_json::to_string(data.manifest)?],
        )
        .map_err(storage_err)?;
    }
    let mut call_site_strings = compact::StringWriter::bulk(compact::CALL_SITE_STRINGS);
    insert_index_rows(
        tx,
        &mut call_site_strings,
        IndexRows {
            files: data.files,
            symbols: data.symbols,
            chunks: data.chunks,
            tests: data.tests,
            imports: data.imports,
            occurrences: data.occurrences,
            analysis_facts: data.analysis_facts,
            scopes: data.scopes,
            bindings: data.bindings,
            call_sites: data.call_sites,
        },
    )
}

/// Everything `replace_files_index` does inside its transaction. With `full_graph`, the
/// stored graph is also reconciled with it; see `SqliteStore::stage_files_index_with_graph`.
fn replace_files_rows(
    tx: &Transaction<'_>,
    update: &PartialIndexUpdate<'_>,
    manifest: ManifestWrite,
    full_graph: Option<(&[GraphNode], &[GraphEdge])>,
) -> Result<GraphReconciliation> {
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

    if manifest == ManifestWrite::Publish {
        tx.execute(
            "INSERT INTO manifests(id, json) VALUES(1, ?1)
             ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![serde_json::to_string(update.manifest)?],
        )
        .map_err(storage_err)?;
    }

    let mut report = GraphReconciliation::default();
    // Dictionary entries the removed edges referenced; whichever of them no surviving edge
    // references is dropped at the end, so an incremental run cannot leave strings behind.
    let mut orphan_candidates = HashSet::<i64>::new();

    // A file's edges, part one: those anchored at a node it owns, in either direction.
    for node_id in &affected_node_ids {
        if let Some(sid) = compact::lookup_sid(tx, compact::GRAPH_STRINGS, node_id)? {
            report.edges_removed += delete_edges_at_node(tx, sid, &mut orphan_candidates)?;
        }
    }

    // Part two, in one pass over the stored edges: those whose evidence range lies in an
    // affected file, whatever their endpoints. With a full graph the same pass finds the
    // stored edges the new graph no longer has, and records which of the new graph's edges
    // are already stored so only the missing ones are inserted below.
    let changed_path_sids = affected_file_paths
        .iter()
        .filter_map(|path| compact::lookup_sid(tx, compact::GRAPH_STRINGS, path).transpose())
        .collect::<Result<HashSet<i64>>>()?;
    let new_edges = full_graph
        .map(|(_, edges)| {
            edges
                .iter()
                .enumerate()
                .map(|(index, edge)| (edge.id.0.as_str(), index))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut edge_stored = vec![false; new_edges.len()];
    let mut removed_edge_ids = Vec::new();
    if !changed_path_sids.is_empty() || full_graph.is_some() {
        let mut stmt = tx
            .prepare(&format!("SELECT id, {EDGE_SID_COLUMNS} FROM graph_edges"))
            .map_err(storage_err)?;
        let mut rows = stmt.query([]).map_err(storage_err)?;
        while let Some(row) = rows.next().map_err(storage_err)? {
            let id: String = row.get(0).map_err(storage_err)?;
            let sids = edge_sids(row, 1)?;
            let refreshed =
                sids[EDGE_SID_EV_PATH].is_some_and(|sid| changed_path_sids.contains(&sid));
            let retained = full_graph.is_none() || new_edges.contains_key(id.as_str());
            if refreshed || !retained {
                orphan_candidates.extend(sids.iter().flatten());
                removed_edge_ids.push(id);
            } else if let Some(&index) = new_edges.get(id.as_str()) {
                edge_stored[index] = true;
            }
        }
    }
    for id in &removed_edge_ids {
        tx.execute("DELETE FROM graph_edges WHERE id = ?1", params![id])
            .map_err(storage_err)?;
    }
    report.edges_removed += removed_edge_ids.len();

    for node_id in &affected_node_ids {
        report.nodes_removed += tx
            .execute("DELETE FROM graph_nodes WHERE id = ?1", params![node_id])
            .map_err(storage_err)?;
    }
    for file_id in &affected_file_ids {
        report.nodes_removed += tx
            .execute(
                "DELETE FROM graph_nodes WHERE file_id = ?1",
                params![&file_id.0],
            )
            .map_err(storage_err)?;
    }
    for symbol_id in &affected_symbol_ids {
        report.nodes_removed += tx
            .execute(
                "DELETE FROM graph_nodes WHERE symbol_id = ?1",
                params![&symbol_id.0],
            )
            .map_err(storage_err)?;
    }
    // Nodes the new graph no longer has. Their edges are gone already: an edge of the new
    // graph cannot end at a node the new graph does not hold, so every edge at such a node
    // failed the identity check above.
    let new_nodes = full_graph
        .map(|(nodes, _)| {
            nodes
                .iter()
                .enumerate()
                .map(|(index, node)| (node.id.0.as_str(), index))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut node_stored = vec![false; new_nodes.len()];
    if full_graph.is_some() {
        let mut removed_node_ids = Vec::new();
        let mut stmt = tx
            .prepare("SELECT id FROM graph_nodes")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_err)?;
        for row in rows {
            let id = row.map_err(storage_err)?;
            match new_nodes.get(id.as_str()) {
                Some(&index) => node_stored[index] = true,
                None => removed_node_ids.push(id),
            }
        }
        drop(stmt);
        for id in &removed_node_ids {
            tx.execute("DELETE FROM graph_nodes WHERE id = ?1", params![id])
                .map_err(storage_err)?;
        }
        report.nodes_removed += removed_node_ids.len();
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
        tx.execute("DELETE FROM scopes WHERE file_id = ?1", params![&file_id.0])
            .map_err(storage_err)?;
        tx.execute(
            "DELETE FROM bindings WHERE file_id = ?1",
            params![&file_id.0],
        )
        .map_err(storage_err)?;
        if let Some(sid) = compact::lookup_sid(tx, compact::CALL_SITE_STRINGS, &file_id.0)? {
            tx.execute("DELETE FROM call_sites WHERE file_sid = ?1", params![sid])
                .map_err(storage_err)?;
        }
    }

    let mut call_site_strings = compact::StringWriter::incremental(tx, compact::CALL_SITE_STRINGS)?;
    insert_index_rows(
        tx,
        &mut call_site_strings,
        IndexRows {
            files: update.changed_files,
            symbols: update.symbols,
            chunks: update.chunks,
            tests: update.tests,
            imports: update.imports,
            occurrences: update.occurrences,
            analysis_facts: update.analysis_facts,
            scopes: update.scopes,
            bindings: update.bindings,
            call_sites: update.call_sites,
        },
    )?;
    let mut graph_strings = compact::StringWriter::incremental(tx, compact::GRAPH_STRINGS)?;
    insert_graph_rows(
        tx,
        &mut graph_strings,
        update.graph_nodes,
        update.graph_edges,
    )?;
    report.nodes_added += update.graph_nodes.len();
    report.edges_added += update.graph_edges.len();
    if let Some((nodes, edges)) = full_graph {
        for node in update.graph_nodes {
            if let Some(&index) = new_nodes.get(node.id.0.as_str()) {
                node_stored[index] = true;
            }
        }
        for edge in update.graph_edges {
            if let Some(&index) = new_edges.get(edge.id.0.as_str()) {
                edge_stored[index] = true;
            }
        }
        let missing_nodes = nodes
            .iter()
            .zip(&node_stored)
            .filter_map(|(node, stored)| (!stored).then_some(node))
            .collect::<Vec<_>>();
        let missing_edges = edges
            .iter()
            .zip(&edge_stored)
            .filter_map(|(edge, stored)| (!stored).then_some(edge))
            .collect::<Vec<_>>();
        report.nodes_added += missing_nodes.len();
        report.edges_added += missing_edges.len();
        insert_graph_rows(tx, &mut graph_strings, missing_nodes, missing_edges)?;
    }
    report.strings_removed = remove_orphan_graph_strings(tx, orphan_candidates)?;
    Ok(report)
}

/// The dictionary-backed columns of `graph_edges`, in the order [`edge_sids`] reads them.
const EDGE_SID_COLUMNS: &str = "from_sid, to_sid, source_sid, ev_path_sid, ev_symbol_sid, \
                                ev_message_sid, ev_indexed_at_sid, extra_sid";
const EDGE_SID_COUNT: usize = 8;
const EDGE_SID_EV_PATH: usize = 3;

fn edge_sids(row: &rusqlite::Row<'_>, first: usize) -> Result<[Option<i64>; EDGE_SID_COUNT]> {
    let mut sids = [None; EDGE_SID_COUNT];
    for (offset, sid) in sids.iter_mut().enumerate() {
        *sid = row.get(first + offset).map_err(storage_err)?;
    }
    Ok(sids)
}

/// Remove every edge at `node_sid`, remembering the dictionary entries the removed rows
/// referenced. Returns the number of edges removed.
fn delete_edges_at_node(
    tx: &Transaction<'_>,
    node_sid: i64,
    orphan_candidates: &mut HashSet<i64>,
) -> Result<usize> {
    let mut stmt = tx
        .prepare_cached(&format!(
            "SELECT {EDGE_SID_COLUMNS} FROM graph_edges WHERE from_sid = ?1 OR to_sid = ?1"
        ))
        .map_err(storage_err)?;
    let mut rows = stmt.query(params![node_sid]).map_err(storage_err)?;
    while let Some(row) = rows.next().map_err(storage_err)? {
        orphan_candidates.extend(edge_sids(row, 0)?.iter().flatten());
    }
    drop(rows);
    drop(stmt);
    tx.execute(
        "DELETE FROM graph_edges WHERE from_sid = ?1 OR to_sid = ?1",
        params![node_sid],
    )
    .map_err(storage_err)
}

/// Drop the `candidates` no edge references any more. One pass over the surviving rows
/// decides; it stops as soon as every candidate has been seen in use.
fn remove_orphan_graph_strings(
    tx: &Transaction<'_>,
    mut candidates: HashSet<i64>,
) -> Result<usize> {
    if candidates.is_empty() {
        return Ok(0);
    }
    let mut stmt = tx
        .prepare(&format!("SELECT {EDGE_SID_COLUMNS} FROM graph_edges"))
        .map_err(storage_err)?;
    let mut rows = stmt.query([]).map_err(storage_err)?;
    while let Some(row) = rows.next().map_err(storage_err)? {
        for sid in edge_sids(row, 0)?.iter().flatten() {
            candidates.remove(sid);
        }
        if candidates.is_empty() {
            return Ok(0);
        }
    }
    drop(rows);
    drop(stmt);
    for sid in &candidates {
        tx.execute("DELETE FROM graph_strings WHERE sid = ?1", params![sid])
            .map_err(storage_err)?;
    }
    Ok(candidates.len())
}

fn insert_document_sections(tx: &Transaction<'_>, sections: &[DocumentSection]) -> Result<()> {
    let mut stmt = tx
        .prepare(
            "INSERT INTO document_sections(path, start_line, end_line, content_hash, json) \
             VALUES(?1, ?2, ?3, ?4, ?5)",
        )
        .map_err(storage_err)?;
    for section in sections {
        let path = section.path.to_string_lossy().replace('\\', "/");
        stmt.execute(params![
            path,
            i64::from(section.line_range.start),
            i64::from(section.line_range.end),
            &section.content_hash,
            serde_json::to_string(section)?,
        ])
        .map_err(storage_err)?;
    }
    Ok(())
}

fn insert_index_rows(
    tx: &Transaction<'_>,
    call_site_strings: &mut compact::StringWriter,
    rows: IndexRows<'_>,
) -> Result<()> {
    {
        let mut stmt = tx
            .prepare_cached("INSERT INTO files(id, path, json) VALUES(?1, ?2, ?3)")
            .map_err(storage_err)?;
        for file in rows.files {
            stmt.execute(params![
                &file.id.0,
                file.path.to_string_lossy().as_ref(),
                serde_json::to_string(file)?
            ])
            .map_err(storage_err)?;
        }
    }
    {
        let mut stmt = tx.prepare_cached("INSERT INTO symbols(id, name, qualified_name, file_id, json) VALUES(?1, ?2, ?3, ?4, ?5)").map_err(storage_err)?;
        for symbol in rows.symbols {
            stmt.execute(params![
                &symbol.id.0,
                &symbol.name,
                &symbol.qualified_name,
                &symbol.file_id.0,
                serde_json::to_string(symbol)?
            ])
            .map_err(storage_err)?;
        }
    }
    {
        let mut stmt = tx.prepare_cached("INSERT INTO chunks(id, file_id, start_line, end_line, text, json) VALUES(?1, ?2, ?3, ?4, ?5, ?6)").map_err(storage_err)?;
        for chunk in rows.chunks {
            stmt.execute(params![
                &chunk.id,
                &chunk.file_id.0,
                chunk.range.start,
                chunk.range.end,
                &chunk.text,
                serde_json::to_string(chunk)?
            ])
            .map_err(storage_err)?;
        }
    }
    for test in rows.tests {
        tx.execute(
            "INSERT INTO tests(id, file_id, json) VALUES(?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET json = excluded.json",
            params![&test.id, &test.file_id.0, serde_json::to_string(test)?],
        )
        .map_err(storage_err)?;
    }
    {
        let mut stmt = tx
            .prepare_cached(
                "INSERT INTO imports(id, file_id, imported, json) VALUES(?1, ?2, ?3, ?4)",
            )
            .map_err(storage_err)?;
        for import in rows.imports {
            stmt.execute(params![
                occurrence_id(
                    &import.file_id.0,
                    &import.imported,
                    import.range.as_ref().map(|range| range.start),
                    true
                ),
                &import.file_id.0,
                &import.imported,
                serde_json::to_string(import)?
            ])
            .map_err(storage_err)?;
        }
    }
    {
        let mut stmt = tx.prepare_cached("INSERT INTO occurrences(id, symbol_id, file_id, is_definition, json) VALUES(?1, ?2, ?3, ?4, ?5)").map_err(storage_err)?;
        for occurrence in rows.occurrences {
            stmt.execute(params![
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
            ])
            .map_err(storage_err)?;
        }
    }
    {
        let mut stmt = tx.prepare_cached("INSERT INTO analysis_facts(id, file_id, source_type, target, json) VALUES(?1, ?2, ?3, ?4, ?5)").map_err(storage_err)?;
        for fact in rows.analysis_facts {
            stmt.execute(params![
                &fact.id,
                &fact.file_id.0,
                source_type_name(&fact.source_type),
                &fact.target,
                serde_json::to_string(fact)?
            ])
            .map_err(storage_err)?;
        }
    }
    {
        let mut stmt = tx.prepare_cached("INSERT INTO scopes(id, file_id, parent_id, owner_symbol_id, kind, json) VALUES(?1, ?2, ?3, ?4, ?5, ?6)").map_err(storage_err)?;
        for scope in rows.scopes {
            stmt.execute(params![
                &scope.id.0,
                &scope.file_id.0,
                scope.parent_id.as_ref().map(|id| &id.0),
                scope.owner_symbol_id.as_ref().map(|id| &id.0),
                format!("{:?}", scope.kind),
                serde_json::to_string(scope)?
            ])
            .map_err(storage_err)?;
        }
    }
    {
        let mut stmt = tx.prepare_cached("INSERT INTO bindings(id, file_id, scope_id, name, json) VALUES(?1, ?2, ?3, ?4, ?5)").map_err(storage_err)?;
        for binding in rows.bindings {
            stmt.execute(params![
                &binding.id.0,
                &binding.file_id.0,
                &binding.scope_id.0,
                &binding.name,
                serde_json::to_string(binding)?
            ])
            .map_err(storage_err)?;
        }
    }
    for call_site in rows.call_sites {
        let row = compact::encode_call_site(tx, call_site_strings, call_site)?;
        tx.prepare_cached("INSERT INTO call_sites(id_sid, file_sid, scope_sid, caller_sid, callee_sid, receiver_sid, receiver_kind, start_line, start_column, end_line, end_column) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)")
            .map_err(storage_err)?
            .execute(params![
                row.id_sid,
                row.file_sid,
                row.scope_sid,
                row.caller_sid,
                row.callee_sid,
                row.receiver_sid,
                row.receiver_kind,
                row.start_line,
                row.start_column,
                row.end_line,
                row.end_column,
            ])
            .map_err(storage_err)?;
    }
    Ok(())
}

/// Secondary indexes over the graph tables. Bulk graph replacement drops and rebuilds them:
/// a sorted post-load CREATE INDEX is dramatically cheaper than maintaining seven B-trees
/// through millions of random-order inserts.
const GRAPH_INDEXES: &[(&str, &str)] = &[
    (
        "idx_graph_nodes_type",
        "CREATE INDEX IF NOT EXISTS idx_graph_nodes_type ON graph_nodes(node_type)",
    ),
    (
        "idx_graph_nodes_label",
        "CREATE INDEX IF NOT EXISTS idx_graph_nodes_label ON graph_nodes(label)",
    ),
    (
        "idx_graph_nodes_file",
        "CREATE INDEX IF NOT EXISTS idx_graph_nodes_file ON graph_nodes(file_id)",
    ),
    (
        "idx_graph_nodes_symbol",
        "CREATE INDEX IF NOT EXISTS idx_graph_nodes_symbol ON graph_nodes(symbol_id)",
    ),
    (
        "idx_graph_edges_from",
        "CREATE INDEX IF NOT EXISTS idx_graph_edges_from ON graph_edges(from_sid)",
    ),
    (
        "idx_graph_edges_to",
        "CREATE INDEX IF NOT EXISTS idx_graph_edges_to ON graph_edges(to_sid)",
    ),
    (
        "idx_graph_edges_type",
        "CREATE INDEX IF NOT EXISTS idx_graph_edges_type ON graph_edges(edge_type)",
    ),
    (
        "idx_graph_edges_from_type",
        "CREATE INDEX IF NOT EXISTS idx_graph_edges_from_type ON graph_edges(from_sid, edge_type)",
    ),
    (
        "idx_graph_edges_to_type",
        "CREATE INDEX IF NOT EXISTS idx_graph_edges_to_type ON graph_edges(to_sid, edge_type)",
    ),
    (
        "idx_graph_edges_source_type",
        "CREATE INDEX IF NOT EXISTS idx_graph_edges_source_type ON graph_edges(source_type)",
    ),
    (
        "idx_graph_strings_vhash",
        "CREATE INDEX IF NOT EXISTS idx_graph_strings_vhash ON graph_strings(vhash)",
    ),
];

fn insert_graph_rows<'a>(
    tx: &Transaction<'_>,
    strings: &mut compact::StringWriter,
    nodes: impl IntoIterator<Item = &'a GraphNode>,
    edges: impl IntoIterator<Item = &'a GraphEdge>,
) -> Result<()> {
    // Insert in primary-key order so the id B-tree fills mostly append-only instead of taking
    // millions of random-page inserts (ids are content hashes, i.e. uniformly random).
    let mut nodes = nodes.into_iter().collect::<Vec<_>>();
    nodes.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    let mut edges = edges.into_iter().collect::<Vec<_>>();
    edges.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    {
        let mut stmt = tx.prepare_cached("INSERT INTO graph_nodes(id, label, node_type, file_id, symbol_id, evidence_available, freshness, json) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)").map_err(storage_err)?;
        for node in nodes {
            stmt.execute(params![
                &node.id.0,
                &node.label,
                format!("{:?}", node.node_type),
                node.file_id.as_ref().map(|id| &id.0),
                node.symbol_id.as_ref().map(|id| &id.0),
                false,
                0,
                serde_json::to_string(node)?
            ])
            .map_err(storage_err)?;
        }
    }
    for edge in edges {
        // Interning writes into the same transaction, so the row encode has to finish before
        // the edge statement is prepared; `prepare_cached` makes the reborrow free.
        let row = compact::encode_edge(tx, strings, edge)?;
        tx.prepare_cached(
            "INSERT INTO graph_edges(id, from_sid, to_sid, edge_type, confidence, source_type, source_sid, freshness, ev_id, ev_path_sid, ev_line_start, ev_line_end, ev_symbol_sid, ev_message_sid, ev_indexed_at_sid, extra_sid) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        )
        .map_err(storage_err)?
        .execute(params![
            row.id,
            row.from_sid,
            row.to_sid,
            row.edge_type,
            row.confidence,
            row.source_type,
            row.source_sid,
            row.freshness,
            row.ev_id,
            row.ev_path_sid,
            row.ev_line_start,
            row.ev_line_end,
            row.ev_symbol_sid,
            row.ev_message_sid,
            row.ev_indexed_at_sid,
            row.extra_sid,
        ])
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

fn require_authoritative_relationship_semantics(store: &SqliteStore) -> Result<()> {
    let data_version = store.data_version()?;
    if let Some((cached_version, verdict)) = store
        .semantics_verdict
        .lock()
        .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?
        .as_ref()
    {
        if *cached_version == data_version {
            return verdict.clone().map_err(OkError::Index);
        }
    }
    let verdict = compute_relationship_semantics_verdict(store);
    *store
        .semantics_verdict
        .lock()
        .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))? =
        Some((data_version, verdict.clone()));
    verdict.map_err(OkError::Index)
}

fn compute_relationship_semantics_verdict(store: &SqliteStore) -> std::result::Result<(), String> {
    // A store whose pre-4.0 edge rows were discarded has an empty graph, not a graph with no
    // relationships. Reporting that difference is the point: an empty answer here would read
    // as "no such relationship exists".
    let rebuild_required = {
        let conn = store
            .connection
            .lock()
            .map_err(|_| "sqlite mutex poisoned".to_string())?;
        schema_meta_flag(&conn, GRAPH_REBUILD_REQUIRED_FLAG).map_err(|err| err.to_string())?
    };
    if rebuild_required {
        return Err(
            "graph edges were built by an older index format and were discarded on open; \
             run `ok index` to rebuild them"
                .to_string(),
        );
    }
    let manifest = MetadataStore::manifest(store).map_err(|err| err.to_string())?;
    let compatibility = open_kioku_core::classify_analysis_semantics(
        manifest
            .as_ref()
            .and_then(|manifest| manifest.analysis_semantics.as_ref()),
        &open_kioku_core::AnalysisSemanticsState::current(),
    );
    if compatibility.status.allows_authoritative_relationships() {
        return Ok(());
    }
    Err(format!(
        "authoritative relationship evidence unavailable: analysis semantics {:?}: {}; stored={}, current={}; {}",
        compatibility.status,
        compatibility.reasons.join("; "),
        compatibility
            .stored_fingerprint
            .as_deref()
            .unwrap_or("missing"),
        compatibility.current_fingerprint,
        compatibility.recommended_action
    ))
}

impl GraphStore for SqliteStore {
    fn replace_graph(&self, nodes: &[GraphNode], edges: &[GraphEdge]) -> Result<()> {
        let mut conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        // Bulk load: give SQLite a large page cache for the duration so random B-tree page
        // touches stay in memory instead of thrashing a multi-gigabyte file.
        let _ = conn.pragma_update(None, "cache_size", -524_288);
        let result = (|| -> Result<()> {
            let tx = conn.transaction().map_err(storage_err)?;
            // Drop secondary indexes first: deleting and reinserting millions of rows through
            // seven live B-trees dominated large-repository indexing time. CREATE INDEX after
            // the load builds each index with a sort instead.
            for (name, _) in GRAPH_INDEXES {
                tx.execute(&format!("DROP INDEX IF EXISTS {name}"), [])
                    .map_err(storage_err)?;
            }
            tx.execute("DELETE FROM graph_edges", [])
                .map_err(storage_err)?;
            tx.execute("DELETE FROM graph_nodes", [])
                .map_err(storage_err)?;
            // The dictionary is owned by `graph_edges`: emptying it here is what keeps a
            // re-index from accumulating strings no surviving row references.
            tx.execute("DELETE FROM graph_strings", [])
                .map_err(storage_err)?;
            let mut strings = compact::StringWriter::bulk(compact::GRAPH_STRINGS);
            insert_graph_rows(&tx, &mut strings, nodes, edges)?;
            for (_, ddl) in GRAPH_INDEXES {
                tx.execute(ddl, []).map_err(storage_err)?;
            }
            clear_schema_meta_flag(&tx, GRAPH_REBUILD_REQUIRED_FLAG)?;
            tx.commit().map_err(storage_err)?;
            Ok(())
        })();
        // The verdict cache is keyed on `data_version`, which this connection's own writes do
        // not advance, so a long-lived process would keep reporting "run `ok index`" after the
        // rebuild that cleared the marker. `replace_files_index` already invalidates here.
        self.invalidate_semantics_verdict();
        let _ = conn.pragma_update(None, "cache_size", -2_000);
        result
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
        // Zero of every edge type with `evidence_available: false` reads as a measurement.
        // On a store awaiting a rebuild it is a missing-evidence state, and the callers that
        // treat a failure as "unknown" render it as absent rather than as a counted zero.
        require_rebuilt_graph(&conn, "graph edge statistics")?;
        let mut stmt = conn
            // Every edge carries evidence by construction — `GraphEdge.evidence` is not an
            // `Option` — so the column this used to aggregate was a stored constant.
            .prepare(
                "SELECT edge_type, COUNT(*), 1, MAX(freshness) FROM graph_edges GROUP BY edge_type",
            )
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
        require_authoritative_relationship_semantics(self)?;
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let Some(node_sid) = compact::lookup_sid(&conn, compact::GRAPH_STRINGS, node)? else {
            return Ok((Vec::new(), Vec::new()));
        };
        // `DERIVED_FROM` is excluded from every untyped read. It is a sibling relation, not a
        // dependency — a test does not depend on the module it is named after — and `neighbors`
        // backs `module_dependencies`, which callers read as imports and dependents. Consumers
        // that want it ask for it by type through `edges_by_type_for_node`.
        // Ordered by edge id before the limit: row order is insertion order, which an incremental
        // `ok watch` update changes, so a high-degree node kept a different set of edges than a
        // fresh index of the same tree.
        let mut stmt = conn
            .prepare(&format!(
                "{} WHERE (e.from_sid = ?1 OR e.to_sid = ?1) AND e.edge_type != 'DerivedFrom' ORDER BY e.id LIMIT ?2",
                compact::EDGE_SELECT
            ))
            .map_err(storage_err)?;
        let mut rows = stmt
            .query(params![node_sid, limit as i64])
            .map_err(storage_err)?;
        let edges = collect_edges(&mut rows)?;
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
        require_authoritative_relationship_semantics(self)?;
        use std::collections::{HashSet, VecDeque};

        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;

        // Prepare the statement once outside the BFS loop to avoid
        // O(N) statement recompilation on large graphs.
        // `DERIVED_FROM` is a sibling relation, not a dependency: a test does not depend on the
        // module it is named after, and a generated file does not depend on its banner's origin.
        // It is the first file-to-file edge in the graph that is not a real dependency, so it is
        // excluded here rather than becoming a traversable hop in a path a caller reads as one.
        let mut edge_stmt = conn
            .prepare(&format!(
                "{} WHERE e.from_sid = ?1 AND e.edge_type != 'DerivedFrom'",
                compact::EDGE_SELECT
            ))
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
            let Some(node_sid) = compact::lookup_sid(&conn, compact::GRAPH_STRINGS, &node)? else {
                continue;
            };
            let mut rows = edge_stmt.query(params![node_sid]).map_err(storage_err)?;
            let edges = collect_edges(&mut rows)?;
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

    fn nodes_by_label(
        &self,
        label: &str,
        node_type: Option<GraphNodeType>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GraphNode>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let limit = clamp_limit(limit) as i64;
        let offset = offset as i64;
        let node_type = node_type.map(|value| format!("{value:?}"));
        let mut stmt = conn
            .prepare(
                r#"
                SELECT graph_nodes.json
                FROM graph_nodes
                WHERE (
                    graph_nodes.label = ?1
                    OR graph_nodes.symbol_id IN (
                        SELECT symbols.id FROM symbols WHERE symbols.name = ?1
                    )
                )
                AND (?2 IS NULL OR graph_nodes.node_type = ?2)
                ORDER BY graph_nodes.id
                LIMIT ?3 OFFSET ?4
                "#,
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![label, node_type, limit, offset], |row| {
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
        require_authoritative_relationship_semantics(self)?;
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let limit = clamp_limit(limit) as i64;
        let offset = offset as i64;
        let type_str = format!("{:?}", edge_type);
        let mut stmt = conn
            .prepare(&format!(
                "{} WHERE e.edge_type = ?1 ORDER BY e.id LIMIT ?2 OFFSET ?3",
                compact::EDGE_SELECT
            ))
            .map_err(storage_err)?;
        let mut rows = stmt
            .query(params![type_str, limit, offset])
            .map_err(storage_err)?;
        collect_edges(&mut rows)
    }

    fn edges_by_type_for_node(
        &self,
        edge_type: GraphEdgeType,
        node_id: &str,
        outgoing: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GraphEdge>> {
        require_authoritative_relationship_semantics(self)?;
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let limit = clamp_limit(limit) as i64;
        let offset = offset as i64;
        let edge_type = format!("{edge_type:?}");
        let Some(node_sid) = compact::lookup_sid(&conn, compact::GRAPH_STRINGS, node_id)? else {
            return Ok(Vec::new());
        };
        let endpoint_column = if outgoing { "from_sid" } else { "to_sid" };
        let sql = format!(
            "{} WHERE e.{endpoint_column} = ?1 AND e.edge_type = ?2 ORDER BY e.id LIMIT ?3 OFFSET ?4",
            compact::EDGE_SELECT
        );
        let mut stmt = conn.prepare(&sql).map_err(storage_err)?;
        let mut rows = stmt
            .query(params![node_sid, edge_type, limit, offset])
            .map_err(storage_err)?;
        collect_edges(&mut rows)
    }

    fn edges_by_type_for_nodes(
        &self,
        edge_type: GraphEdgeType,
        node_ids: &[&str],
        outgoing: bool,
    ) -> Result<Vec<GraphEdge>> {
        require_authoritative_relationship_semantics(self)?;
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let edge_type = format!("{edge_type:?}");
        let mut node_sids = Vec::with_capacity(node_ids.len());
        for node_id in node_ids {
            // A node id that was never stored has no edges.
            if let Some(sid) = compact::lookup_sid(&conn, compact::GRAPH_STRINGS, node_id)? {
                node_sids.push(sid);
            }
        }
        node_sids.sort_unstable();
        node_sids.dedup();
        let endpoint_column = if outgoing { "from_sid" } else { "to_sid" };
        let mut edges = Vec::new();
        // Chunks stay under SQLite's historical limit of 999 bound parameters per statement.
        for chunk in node_sids.chunks(900) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "{} WHERE e.edge_type = ? AND e.{endpoint_column} IN ({placeholders}) ORDER BY e.id",
                compact::EDGE_SELECT
            );
            let mut stmt = conn.prepare(&sql).map_err(storage_err)?;
            let params = std::iter::once(rusqlite::types::Value::Text(edge_type.clone())).chain(
                chunk
                    .iter()
                    .map(|sid| rusqlite::types::Value::Integer(*sid)),
            );
            let mut rows = stmt
                .query(rusqlite::params_from_iter(params))
                .map_err(storage_err)?;
            edges.extend(collect_edges(&mut rows)?);
        }
        // Each chunk is ordered by edge id, so the accumulation is ordered by (chunk, id) until it
        // is sorted here. Chunk membership follows string-interning order, which is not meaningful
        // to a caller, and a caller that truncates this list would otherwise get that order.
        edges.sort_by(|left, right| left.id.0.cmp(&right.id.0));
        Ok(edges)
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
        require_rebuilt_graph(&conn, "graph schema counts")?;

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
        require_authoritative_relationship_semantics(self)?;
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("sqlite mutex poisoned".into()))?;
        let limit = clamp_limit(limit) as i64;
        let (Some(from_sid), Some(to_sid)) = (
            compact::lookup_sid(&conn, compact::GRAPH_STRINGS, from)?,
            compact::lookup_sid(&conn, compact::GRAPH_STRINGS, to)?,
        ) else {
            return Ok(Vec::new());
        };
        let mut stmt = conn
            .prepare(&format!(
                "{} WHERE e.from_sid = ?1 AND e.to_sid = ?2 ORDER BY e.id LIMIT ?3",
                compact::EDGE_SELECT
            ))
            .map_err(storage_err)?;
        let mut rows = stmt
            .query(params![from_sid, to_sid, limit])
            .map_err(storage_err)?;
        collect_edges(&mut rows)
    }
}

/// Materialize [`GraphEdge`]s from a statement projecting [`compact::EDGE_SELECT`].
fn collect_edges(rows: &mut rusqlite::Rows<'_>) -> Result<Vec<GraphEdge>> {
    let mut edges = Vec::new();
    while let Some(row) = rows.next().map_err(storage_err)? {
        edges.push(compact::edge_from_row(row)?);
    }
    Ok(edges)
}

/// Whether this index file's graph edges were discarded and are waiting on `ok index`.
///
/// Read-only on purpose: callers such as the cross-project workspace linker open member
/// indexes under `PRAGMA query_only = ON`, where the `CREATE TABLE IF NOT EXISTS` that
/// [`schema_meta_flag`] performs would fail. A store with no `schema_meta` table has never
/// been reset, so a missing table reports `false`.
pub fn graph_rebuild_required(conn: &Connection) -> Result<bool> {
    if !table_exists(conn, "schema_meta")? {
        return Ok(false);
    }
    conn.query_row(
        "SELECT 1 FROM schema_meta WHERE key = ?1",
        params![GRAPH_REBUILD_REQUIRED_FLAG],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
    .map_err(storage_err)
}

/// The error every read-only graph reader returns for an index awaiting a rebuild.
fn rebuild_required_error(context: &str) -> OkError {
    OkError::Index(format!(
        "{context}: graph edges were built by an older index format and were discarded on \
         open; run `ok index` in that repository to rebuild them"
    ))
}

/// Turn the "this index still has the pre-4.0 edge layout" failure into an instruction.
///
/// A read-only connection never runs the reset, so it meets the old table shape directly and
/// SQLite reports a missing internal column. That is a rebuild instruction, not a bug report.
fn map_edge_read_error(err: rusqlite::Error, context: &str) -> OkError {
    let message = err.to_string();
    if message.contains("no such column") || message.contains("no such table") {
        return rebuild_required_error(context);
    }
    storage_err(err)
}

/// Refuse to read edges out of an index whose graph is known to be missing.
fn require_rebuilt_graph(conn: &Connection, context: &str) -> Result<()> {
    if graph_rebuild_required(conn)? {
        return Err(rebuild_required_error(context));
    }
    Ok(())
}

/// Every edge of one type, read straight from an open index connection.
///
/// The cross-project workspace linker opens each member repository's index file directly
/// rather than through a [`SqliteStore`], and edges are no longer a self-describing JSON
/// column it can decode on its own. Because that path never constructs a store, it never
/// runs the rebuild gate either — so these readers carry it themselves. An index awaiting a
/// rebuild must not answer "no such relationship exists" to a workspace linker that would
/// then persist the empty result.
pub fn read_graph_edges_by_type(
    conn: &Connection,
    edge_type: GraphEdgeType,
) -> Result<Vec<GraphEdge>> {
    let context = "reading graph edges";
    require_rebuilt_graph(conn, context)?;
    let mut stmt = conn
        .prepare(&format!(
            "{} WHERE e.edge_type = ?1 ORDER BY e.id",
            compact::EDGE_SELECT
        ))
        .map_err(|err| map_edge_read_error(err, context))?;
    let mut rows = stmt
        .query(params![format!("{edge_type:?}")])
        .map_err(|err| map_edge_read_error(err, context))?;
    collect_edges(&mut rows)
}

/// Every edge whose evidence carries `source_type`, read from an open index connection.
pub fn read_graph_edges_by_source_type(
    conn: &Connection,
    source_type: EvidenceSourceType,
) -> Result<Vec<GraphEdge>> {
    let context = "reading graph edges";
    require_rebuilt_graph(conn, context)?;
    let mut stmt = conn
        .prepare(&format!(
            "{} WHERE e.source_type = ?1 ORDER BY e.id",
            compact::EDGE_SELECT
        ))
        .map_err(|err| map_edge_read_error(err, context))?;
    let mut rows = stmt
        .query(params![format!("{source_type:?}")])
        .map_err(|err| map_edge_read_error(err, context))?;
    collect_edges(&mut rows)
}

fn is_duplicate_column(err: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(_, Some(msg)) = err {
        msg.contains("duplicate column name")
    } else {
        false
    }
}

/// Returns whether the column was actually added, so callers can detect a genuinely
/// pre-migration table shape.
fn add_column_if_not_exists(conn: &mut Connection, stmt: &str) -> Result<bool> {
    match conn.execute(stmt, []) {
        Ok(_) => Ok(true),
        Err(err) if is_duplicate_column(&err) => Ok(false),
        Err(err) => Err(storage_err(err)),
    }
}

/// Marker recording that the graph query-column backfill has completed for this store, so
/// store open never rescans the graph tables once they are migrated.
const GRAPH_QUERY_COLUMNS_FLAG: &str = "graph_query_columns_v2";

fn schema_meta_flag(conn: &Connection, key: &str) -> Result<bool> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .map_err(storage_err)?;
    let found = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(storage_err)?;
    Ok(found.is_some())
}

fn set_schema_meta_flag(conn: &Connection, key: &str) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .map_err(storage_err)?;
    conn.execute(
        "INSERT OR REPLACE INTO schema_meta(key, value) VALUES(?1, '1')",
        params![key],
    )
    .map_err(storage_err)?;
    Ok(())
}

fn clear_schema_meta_flag(conn: &Connection, key: &str) -> Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .map_err(storage_err)?;
    conn.execute("DELETE FROM schema_meta WHERE key = ?1", params![key])
        .map_err(storage_err)?;
    Ok(())
}

fn migrate_graph_schema(conn: &mut Connection) -> Result<()> {
    // Add columns to graph_nodes. If any column was actually added, the table was genuinely
    // pre-migration and the backfill must run regardless of the marker.
    let mut columns_added = false;
    for stmt in [
        "ALTER TABLE graph_nodes ADD COLUMN node_type TEXT DEFAULT ''",
        "ALTER TABLE graph_nodes ADD COLUMN file_id TEXT DEFAULT ''",
        "ALTER TABLE graph_nodes ADD COLUMN symbol_id TEXT DEFAULT ''",
        "ALTER TABLE graph_nodes ADD COLUMN evidence_available BOOLEAN DEFAULT 0",
        "ALTER TABLE graph_nodes ADD COLUMN freshness INTEGER DEFAULT 0",
    ] {
        columns_added |= add_column_if_not_exists(conn, stmt)?;
    }

    // The backfill full-scans both graph tables, so it must run once per store, not on every
    // open. Before the marker existed it also re-matched rows whose optional columns are
    // legitimately empty (file nodes have no symbol_id), rewriting them on every open.
    if columns_added || !schema_meta_flag(conn, GRAPH_QUERY_COLUMNS_FLAG)? {
        backfill_graph_query_columns(conn)?;
        set_schema_meta_flag(conn, GRAPH_QUERY_COLUMNS_FLAG)?;
    }

    // Add indexes (idempotent via IF NOT EXISTS; shared with bulk replace_graph rebuilds)
    for (_, ddl) in GRAPH_INDEXES {
        conn.execute(ddl, []).map_err(storage_err)?;
    }

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
                // node_type is populated by every writer and by the backfill itself, so it is
                // the unmigrated-row discriminator. file_id/symbol_id are legitimately empty
                // for many nodes and must not retrigger the backfill.
                "SELECT id, json FROM graph_nodes WHERE COALESCE(node_type, '') = ''",
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
                     evidence_available = ?4,
                     freshness = ?5
                 WHERE id = ?6",
                params![
                    format!("{:?}", node.node_type),
                    node.file_id.as_ref().map(|file_id| file_id.0.as_str()),
                    node.symbol_id
                        .as_ref()
                        .map(|symbol_id| symbol_id.0.as_str()),
                    false,
                    0,
                    id,
                ],
            )
            .map_err(storage_err)?;
        }
        tx.commit().map_err(storage_err)?;
    }

    Ok(())
}

/// Marker recording that the graph tables were reset because they held a pre-4.0 layout.
///
/// Set by [`reset_legacy_graph_storage`] and cleared by `replace_graph`, so a store whose
/// edges were discarded reports a rebuild instruction rather than answering relationship
/// questions from an empty table.
const GRAPH_REBUILD_REQUIRED_FLAG: &str = "graph_rebuild_required_v4";

/// Drop pre-4.0 `graph_edges` / `call_sites` so the schema batch can recreate them compact.
///
/// The two tables used to carry a `json` column holding a self-contained copy of every row;
/// 4.0 replaces it with typed columns and a string dictionary. The old rows cannot be read
/// by the new statements, so the shapes are mutually exclusive and the `json` column is an
/// exact discriminator.
///
/// The drops and the rebuild marker commit in **one** transaction. Writing the marker
/// afterwards would leave a window — an interrupt, an OOM kill, a failed write — in which the
/// edges are gone and nothing records it: the discriminating column would be gone too, so the
/// next open would create an empty compact table and answer relationship questions with a
/// confident zero, forever. `interrupted_reset` is the second layer, covering the same state
/// arrived at by any other route.
fn reset_legacy_graph_storage(conn: &mut Connection) -> Result<()> {
    let legacy =
        has_column(conn, "graph_edges", "json")? || has_column(conn, "call_sites", "json")?;
    // A store that has been initialized once always has both tables. If the index has content
    // but the edge table is missing, its edges were removed by something that did not record
    // the fact, and they must be rebuilt rather than reported as absent.
    let interrupted_reset = !legacy
        && table_exists(conn, "manifests")?
        && (!table_exists(conn, "graph_edges")? || !table_exists(conn, "call_sites")?);
    if !legacy && !interrupted_reset {
        return Ok(());
    }
    let tx = conn.transaction().map_err(storage_err)?;
    for stmt in [
        "DROP TABLE IF EXISTS graph_edges",
        "DROP TABLE IF EXISTS call_sites",
    ] {
        tx.execute(stmt, []).map_err(storage_err)?;
    }
    set_schema_meta_flag(&tx, GRAPH_REBUILD_REQUIRED_FLAG)?;
    tx.commit().map_err(storage_err)?;
    Ok(())
}

/// Whether `table` exists and has `column`. A missing table reports `false`.
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(storage_err)?;
    let mut rows = stmt.query([]).map_err(storage_err)?;
    while let Some(row) = rows.next().map_err(storage_err)? {
        let name: String = row.get(1).map_err(storage_err)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |_| Ok(()),
    )
    .optional()
    .map(|found| found.is_some())
    .map_err(storage_err)
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
        return Err(OkError::Storage(newer_sqlite_schema_message(version)));
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
    use super::{
        compact, decode_index_manifest, newer_index_message, schema_meta_flag,
        set_schema_meta_flag, SqliteStore, GRAPH_REBUILD_REQUIRED_FLAG,
        SQLITE_GRAPH_SCHEMA_VERSION, SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION,
    };
    use chrono::{TimeZone, Utc};
    use open_kioku_core::{
        AnalysisFact, ChurnEntityKind, CodeChunk, Confidence, EdgeId, Evidence, EvidenceId,
        EvidenceSourceType, File, FileId, FileRange, GitChangeKind, GitCochangeEdge, GitCommitId,
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
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};
    use std::time::Duration;

    fn make_store() -> SqliteStore {
        SqliteStore::open(":memory:").expect("in-memory store")
    }

    #[test]
    fn open_waits_for_a_concurrent_schema_writer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite");
        drop(SqliteStore::open(&path).unwrap());

        let blocker = Connection::open(&path).unwrap();
        blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();

        let open_path = path.clone();
        let opener = std::thread::spawn(move || SqliteStore::open(open_path));
        std::thread::sleep(Duration::from_millis(100));
        blocker.execute_batch("COMMIT").unwrap();

        opener
            .join()
            .expect("concurrent opener thread panicked")
            .expect("store should wait for the schema writer lock");
    }

    fn make_current_store() -> SqliteStore {
        let store = make_store();
        store
            .put_manifest(&make_manifest())
            .expect("current analysis-semantics manifest");
        store
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
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
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
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
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
            snapshot: None,
        }
    }

    /// The `json_extract` fast path must agree with decoding the whole manifest in every
    /// state, including the ones it cannot distinguish by construction. A fast path that
    /// returned `None` where the full decode returns `Some` would make a fully covered
    /// repository report unrecorded coverage, which context packs turn into a caveat and a
    /// cap - inventing missing evidence to save a decode.
    #[test]
    fn index_coverage_matches_the_full_manifest_decode() {
        let store = SqliteStore::open(":memory:").unwrap();
        let full_decode = |store: &SqliteStore| {
            MetadataStore::manifest(store)
                .unwrap()
                .and_then(|manifest| manifest.quality.coverage)
        };
        let raw_manifest = |store: &SqliteStore, json: &str| {
            let conn = store.connection.lock().unwrap();
            conn.execute(
                "INSERT INTO manifests(id, json) VALUES(1, ?1) ON CONFLICT(id) DO UPDATE SET json = excluded.json",
                rusqlite::params![json],
            )
            .unwrap();
        };

        // 1. No manifest row at all.
        assert_eq!(store.index_coverage().unwrap(), None);
        assert_eq!(store.index_coverage().unwrap(), full_decode(&store));

        // 2. A recorded record carrying a gap.
        let mut coverage = open_kioku_core::IndexCoverage::default();
        for index in 0..27 {
            coverage.record_discovered(&open_kioku_core::Language::Rust);
            if index < 2 {
                coverage.record_indexed(&open_kioku_core::Language::Rust, false);
            } else {
                coverage.record_skipped(
                    &open_kioku_core::Language::Rust,
                    open_kioku_core::SkipReason::Ignored,
                );
                coverage.record_policy_exclusion(
                    &open_kioku_core::Language::Rust,
                    open_kioku_core::SkipSource::GitIgnore,
                    Some("src"),
                );
            }
        }
        let mut manifest = make_manifest();
        manifest.quality.coverage = Some(coverage.clone());
        MetadataStore::put_manifest(&store, &manifest).unwrap();
        assert_eq!(store.index_coverage().unwrap(), Some(coverage));
        assert_eq!(store.index_coverage().unwrap(), full_decode(&store));
        assert_eq!(store.index_coverage().unwrap().unwrap().gaps().len(), 1);

        // 3. A recorded record with no gaps is `Some`, not `None`: "measured, nothing missing"
        // must never collapse into "unmeasured".
        let mut manifest = make_manifest();
        manifest.quality.coverage = Some(open_kioku_core::IndexCoverage::default());
        MetadataStore::put_manifest(&store, &manifest).unwrap();
        assert_eq!(
            store.index_coverage().unwrap(),
            Some(open_kioku_core::IndexCoverage::default())
        );
        assert_eq!(store.index_coverage().unwrap(), full_decode(&store));

        // 4. `quality` present, `coverage` key absent (how the field serializes when None).
        MetadataStore::put_manifest(&store, &make_manifest()).unwrap();
        assert_eq!(store.index_coverage().unwrap(), None);
        assert_eq!(store.index_coverage().unwrap(), full_decode(&store));

        // 5. `coverage: null` written literally: `json_extract` yields SQL NULL here exactly as
        // it does for an absent key, and the full decode yields `None` too.
        raw_manifest(
            &store,
            &serde_json::to_string(&serde_json::json!({
                "analysis_semantics": null,
                "repository": {"id": "repo", "name": "repo", "root": ".", "branch": null, "commit": null, "indexed_at": null},
                "file_count": 0, "symbol_count": 0, "chunk_count": 0,
                "indexed_at": "2026-01-01T00:00:00Z", "schema_version": 1,
                "index_mode": "full", "phase_reports": [],
                "quality": {"scip_enabled": false, "scip_mode": "off", "scip_indexes_imported": 0,
                            "scip_symbols": 0, "scip_occurrences": 0, "scip_exact_references": 0,
                            "test_count": 0, "import_count": 0, "coverage": null}
            }))
            .unwrap(),
        );
        assert_eq!(store.index_coverage().unwrap(), None);
        assert_eq!(store.index_coverage().unwrap(), full_decode(&store));

        // 6. A coverage record from an older schema, without the per-language source map.
        raw_manifest(
            &store,
            &serde_json::to_string(&serde_json::json!({
                "analysis_semantics": null,
                "repository": {"id": "repo", "name": "repo", "root": ".", "branch": null, "commit": null, "indexed_at": null},
                "file_count": 0, "symbol_count": 0, "chunk_count": 0,
                "indexed_at": "2026-01-01T00:00:00Z", "schema_version": 1,
                "index_mode": "full", "phase_reports": [],
                "quality": {"scip_enabled": false, "scip_mode": "off", "scip_indexes_imported": 0,
                            "scip_symbols": 0, "scip_occurrences": 0, "scip_exact_references": 0,
                            "test_count": 0, "import_count": 0,
                            "coverage": {"discovered": 10, "indexed": 2, "skipped": {"ignored": 8},
                                         "by_language": {"rust": {"discovered": 10, "indexed": 2,
                                                                  "skipped": {"ignored": 8}}}}}
            }))
            .unwrap(),
        );
        let legacy = store.index_coverage().unwrap().expect("legacy coverage");
        assert_eq!(legacy.discovered, 10);
        assert!(legacy.policy_excluded_by_language.is_empty());
        assert_eq!(store.index_coverage().unwrap(), full_decode(&store));
        assert!(legacy.gaps().is_empty());
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
        let future_version = SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION + 1;
        future
            .pragma_update(None, "user_version", future_version)
            .unwrap();
        future
            .execute_batch("CREATE TABLE future_history_marker (id INTEGER PRIMARY KEY);")
            .unwrap();
        drop(future);

        let error = match SqliteStore::open(&path) {
            Ok(_) => panic!("newer schema should be rejected"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(&format!(
            "newer than supported version {}",
            SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION
        )));

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
    fn history_score_reasons_stay_paired_with_their_components_after_sorting() {
        // Two similar changes and three co-change neighbours: sorted alone, "2 similar
        // historical change(s)" moves ahead of "3 persisted co-change neighbor(s)" while the
        // components keep production order, so a consumer pairing them by index swapped their
        // facts.
        let mut snapshot = history_snapshot();
        let mut third = snapshot.cochange_edges[1].clone();
        third.id = HistoryRecordId::new("cochange-bench");
        third.cochanged_path = "benches/library.rs".into();
        snapshot.cochange_edges.push(third);
        let store = make_store();
        store.put_history_snapshot(&snapshot).unwrap();

        let summary = store
            .history_score_components(
                &HistorySignalQuery {
                    path: "src/lib.rs".into(),
                    task: None,
                    symbols: Vec::new(),
                },
                8,
            )
            .unwrap();

        assert!(
            summary
                .reasons
                .iter()
                .any(|reason| reason.starts_with("similar change overlap: 2 similar")),
            "{summary:#?}"
        );
        assert!(
            summary
                .reasons
                .iter()
                .any(|reason| reason.starts_with("similar change overlap: 3 persisted")),
            "{summary:#?}"
        );
        assert_eq!(summary.reasons.len(), summary.components.len());
        let mut sorted = summary.reasons.clone();
        sorted.sort();
        assert_eq!(summary.reasons, sorted);
        for (reason, component) in summary.reasons.iter().zip(&summary.components) {
            let expected_prefix = if reason.starts_with("history churn") {
                "history-churn:"
            } else if reason.starts_with("ownership risk") {
                "history-author:"
            } else if reason.starts_with("reviewer affinity") {
                "history-reviewer:"
            } else if reason.contains("persisted co-change") {
                "history-cochange:"
            } else if reason.contains("similar historical change") {
                "history-similar:"
            } else {
                panic!("unexpected history reason `{reason}`");
            };
            assert!(
                component
                    .evidence_ids
                    .iter()
                    .all(|id| id.starts_with(expected_prefix)),
                "`{reason}` is paired with {:?}",
                component.evidence_ids
            );
        }
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
                scopes: &[],
                bindings: &[],
                call_sites: &[],
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
            scopes: &[],
            bindings: &[],
            call_sites: &[],
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
                    source_range: None,
                    is_definition: true,
                    confidence: Confidence::Exact,
                    provenance: EvidenceSourceType::StaticAnalysis,
                }],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
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
        // Anchored at nodes no file owns; its evidence range is what ties it to the file.
        let mut source_evidence = evidence();
        source_evidence.file_range = Some(FileRange {
            path: std::path::Path::new("src/main.rs").into(),
            line_range: Some(LineRange::single(3)),
        });
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
                scopes: &[],
                bindings: &[],
                call_sites: &[],
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

    /// Nodes no file owns (analysis targets) and an edge into one from an unchanged file, on
    /// top of the two-file index `partial_replace_updates_changed_files_and_cleans_deleted_graph_edges` builds.
    fn reconciliation_fixture() -> (SqliteStore, IndexManifest, File, File, Symbol, Symbol) {
        let store = make_store();
        let manifest = make_manifest();
        let file1 = make_file("f1", "src/main.rs");
        let file2 = make_file("f2", "src/lib.rs");
        let sym1 = make_symbol("s1", "main_fn", "f1");
        let sym2 = make_symbol("s2", "lib_fn", "f2");
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[file1.clone(), file2.clone()],
                symbols: &[sym1.clone(), sym2.clone()],
                chunks: &[],
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();
        (store, manifest, file1, file2, sym1, sym2)
    }

    fn symbol_node(symbol: &Symbol) -> GraphNode {
        GraphNode {
            id: NodeId::new(format!("symbol:{}", symbol.id.0)),
            node_type: GraphNodeType::Function,
            label: symbol.name.clone(),
            file_id: Some(symbol.file_id.clone()),
            symbol_id: Some(symbol.id.clone()),
            ..Default::default()
        }
    }

    fn analysis_node(label: &str) -> GraphNode {
        GraphNode {
            id: NodeId::new(format!("analysis:Function:{label}")),
            node_type: GraphNodeType::Function,
            label: label.into(),
            ..Default::default()
        }
    }

    fn calls(id: &str, from: &GraphNode, to: &GraphNode, path: &str, message: &str) -> GraphEdge {
        let mut evidence = evidence();
        evidence.file_range = Some(FileRange {
            path: std::path::Path::new(path).into(),
            line_range: Some(LineRange::single(1)),
        });
        evidence.message = message.into();
        GraphEdge {
            id: EdgeId::new(id),
            from: from.id.clone(),
            to: to.id.clone(),
            edge_type: GraphEdgeType::Calls,
            evidence,
            ..Default::default()
        }
    }

    fn graph_counts(store: &SqliteStore) -> (i64, i64, i64) {
        let conn = store.connection.lock().unwrap();
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).unwrap() };
        (
            count("SELECT COUNT(*) FROM graph_nodes"),
            count("SELECT COUNT(*) FROM graph_edges"),
            count("SELECT COUNT(*) FROM graph_strings"),
        )
    }

    /// #413: re-indexing `src/lib.rs` with `lib_fn` renamed must remove the call from the
    /// unchanged `src/main.rs` into the node that only ever existed because of `lib_fn`, add
    /// the call into the renamed symbol's node, and leave no dictionary entry behind. The
    /// result is compared with a clean `replace_graph` of the same graph, dictionary included.
    #[test]
    fn partial_replace_with_graph_matches_a_clean_rebuild_and_leaks_no_strings() {
        let (store, manifest, _file1, file2, sym1, sym2) = reconciliation_fixture();
        let node1 = symbol_node(&sym1);
        let node2 = symbol_node(&sym2);
        let target_old = analysis_node("module::lib_fn");
        let old_edges = vec![
            calls(
                "edge:main-calls-lib",
                &node1,
                &node2,
                "src/main.rs",
                "main_fn calls lib_fn",
            ),
            calls(
                "edge:main-calls-name",
                &node1,
                &target_old,
                "src/main.rs",
                "resolved lib_fn",
            ),
        ];
        store
            .replace_graph(
                &[node1.clone(), node2.clone(), target_old.clone()],
                &old_edges,
            )
            .unwrap();

        let mut renamed_file = file2.clone();
        renamed_file.content_hash = "renamed".into();
        let renamed = make_symbol("s2b", "lib_fn_new", "f2");
        let node2_new = symbol_node(&renamed);
        let target_new = analysis_node("module::lib_fn_new");
        let new_nodes = vec![node1.clone(), node2_new.clone(), target_new.clone()];
        let new_edges = vec![
            calls(
                "edge:main-calls-lib-new",
                &node1,
                &node2_new,
                "src/main.rs",
                "main_fn calls lib_fn_new",
            ),
            calls(
                "edge:main-calls-name-new",
                &node1,
                &target_new,
                "src/main.rs",
                "resolved lib_fn_new",
            ),
        ];
        let report = store
            .stage_files_index_with_graph(
                PartialIndexUpdate {
                    manifest: &manifest,
                    changed_files: std::slice::from_ref(&renamed_file),
                    deleted_file_ids: &[],
                    symbols: std::slice::from_ref(&renamed),
                    chunks: &[],
                    tests: &[],
                    imports: &[],
                    occurrences: &[],
                    analysis_facts: &[],
                    scopes: &[],
                    bindings: &[],
                    call_sites: &[],
                    graph_nodes: &[],
                    graph_edges: &[],
                },
                &new_nodes,
                &new_edges,
            )
            .unwrap();
        assert_eq!(report.edges_removed, 2, "{report:?}");
        assert_eq!(report.edges_added, 2, "{report:?}");
        assert_eq!(report.nodes_removed, 2, "{report:?}");
        assert_eq!(report.nodes_added, 2, "{report:?}");
        assert!(report.strings_removed > 0, "{report:?}");

        assert!(store.node_by_id(&node2.id.0).unwrap().is_none());
        assert!(store.node_by_id(&target_old.id.0).unwrap().is_none());
        let (_, edges) = store.neighbors(&node1.id.0, 10).unwrap();
        let mut targets = edges
            .iter()
            .map(|edge| edge.to.0.clone())
            .collect::<Vec<_>>();
        targets.sort();
        assert_eq!(
            targets,
            vec![target_new.id.0.clone(), node2_new.id.0.clone()]
        );
        let incremental = graph_counts(&store);

        let clean = make_store();
        clean.replace_graph(&new_nodes, &new_edges).unwrap();
        assert_eq!(incremental, graph_counts(&clean));
    }

    /// The previous manifest stays published through an incremental update; the caller
    /// publishes the new one after the search index is rebuilt.
    #[test]
    fn staged_partial_replace_keeps_the_previous_manifest() {
        let (store, manifest, _file1, file2, _sym1, sym2) = reconciliation_fixture();
        let mut next = manifest.clone();
        next.file_count = 99;
        let node2 = symbol_node(&sym2);
        store
            .stage_files_index_with_graph(
                PartialIndexUpdate {
                    manifest: &next,
                    changed_files: std::slice::from_ref(&file2),
                    deleted_file_ids: &[],
                    symbols: std::slice::from_ref(&sym2),
                    chunks: &[],
                    tests: &[],
                    imports: &[],
                    occurrences: &[],
                    analysis_facts: &[],
                    scopes: &[],
                    bindings: &[],
                    call_sites: &[],
                    graph_nodes: &[],
                    graph_edges: &[],
                },
                std::slice::from_ref(&node2),
                &[],
            )
            .unwrap();
        assert_eq!(
            store.manifest().unwrap().unwrap().file_count,
            manifest.file_count
        );
        store.put_manifest(&next).unwrap();
        assert_eq!(store.manifest().unwrap().unwrap().file_count, 99);
    }

    /// A full staged write unpublishes the index until `put_manifest`: the rows are there,
    /// the manifest is not.
    #[test]
    fn staged_full_replace_withholds_the_manifest_until_published() {
        let store = make_store();
        let manifest = make_manifest();
        store.put_manifest(&manifest).unwrap();
        let file = make_file("f1", "src/lib.rs");
        store
            .stage_index_with_documents(
                IndexData {
                    manifest: &manifest,
                    files: std::slice::from_ref(&file),
                    symbols: &[],
                    chunks: &[],
                    tests: &[],
                    imports: &[],
                    occurrences: &[],
                    analysis_facts: &[],
                    scopes: &[],
                    bindings: &[],
                    call_sites: &[],
                },
                &[],
            )
            .unwrap();
        assert!(store.manifest().unwrap().is_none());
        assert!(store
            .get_file_by_path(std::path::Path::new("src/lib.rs"))
            .unwrap()
            .is_some());
        store.put_manifest(&manifest).unwrap();
        assert!(store.manifest().unwrap().is_some());
    }

    #[test]
    fn manifest_from_a_newer_open_kioku_is_refused_with_the_upgrade_message() {
        let store = make_store();
        let mut json = serde_json::to_value(make_manifest()).unwrap();
        json["schema_version"] = json!(open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION + 1);
        // A field this version does not know, as a newer writer would add.
        json["from_the_future"] = json!(true);
        store
            .connection
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO manifests(id, json) VALUES(1, ?1)",
                params![json.to_string()],
            )
            .unwrap();
        let error = store.manifest().unwrap_err().to_string();
        assert!(error.contains("written by a newer Open Kioku"), "{error}");
        assert!(
            error.contains("upgrade Open Kioku or run `ok index`"),
            "{error}"
        );
        assert_eq!(
            error,
            format!(
                "index error: {}",
                newer_index_message(open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION + 1)
            )
        );

        // Older manifests still read, through serde defaults, as before.
        let mut older = serde_json::to_value(make_manifest()).unwrap();
        older["schema_version"] = json!(1);
        assert_eq!(
            decode_index_manifest(&older.to_string())
                .unwrap()
                .schema_version,
            1
        );
    }

    #[test]
    fn open_repo_index_reports_an_index_being_built_while_the_lock_is_held() {
        use open_kioku_storage::generations::{
            index_lock_path, index_write_in_progress, indexing_in_progress_message, IndexWriteLock,
        };
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        assert!(SqliteStore::open_repo_index(repo).unwrap().is_none());

        // Lock held, no database yet: being built, not unindexed.
        let lock = IndexWriteLock::acquire(repo, Duration::from_millis(10)).unwrap();
        assert_eq!(lock.path(), index_lock_path(repo));
        let error = SqliteStore::open_repo_index(repo)
            .err()
            .expect("an index being built is not served")
            .to_string();
        assert_eq!(
            error,
            format!("index error: {}", indexing_in_progress_message(repo))
        );
        // A second writer waits, then gives up with the lock's path.
        let error = IndexWriteLock::acquire(repo, Duration::from_millis(10))
            .expect_err("the lock is exclusive")
            .to_string();
        assert!(error.contains("locked by a running"), "{error}");

        // Lock held, rows staged, manifest withheld: still being built.
        let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
        let manifest = make_manifest();
        store
            .stage_index_with_documents(
                IndexData {
                    manifest: &manifest,
                    files: &[],
                    symbols: &[],
                    chunks: &[],
                    tests: &[],
                    imports: &[],
                    occurrences: &[],
                    analysis_facts: &[],
                    scopes: &[],
                    bindings: &[],
                    call_sites: &[],
                },
                &[],
            )
            .unwrap();
        assert!(SqliteStore::open_repo_index(repo).is_err());

        // Published: served, lock or no lock.
        store.put_manifest(&manifest).unwrap();
        assert!(store.has_manifest().unwrap());
        assert!(SqliteStore::open_repo_index(repo).unwrap().is_some());
        drop(lock);
        assert!(!index_write_in_progress(repo));
        assert!(SqliteStore::open_repo_index(repo).unwrap().is_some());

        // Withdrawn with no live writer, whatever file a dead one left: unindexed, not
        // being built.
        store
            .withdraw_manifest("an incremental update failed")
            .unwrap();
        assert!(!store.has_manifest().unwrap());
        std::fs::write(index_lock_path(repo), b"").unwrap();
        assert!(SqliteStore::open_repo_index(repo).unwrap().is_none());
    }

    /// Each refusal is classified from the index itself, and its error is the one
    /// `open_repo_index` returns for the same index.
    #[test]
    fn probe_classifies_every_refusal_state() {
        use open_kioku_storage::generations::{IndexRefusalState, IndexWriteLock};
        let state = |repo: &std::path::Path| {
            let refusal = SqliteStore::probe_repo_index(repo)
                .err()
                .expect("the index is refused");
            assert_eq!(
                refusal.error.to_string(),
                SqliteStore::open_repo_index(repo)
                    .err()
                    .expect("the same index is refused")
                    .to_string()
            );
            refusal.state
        };

        let temp = tempfile::tempdir().unwrap();
        let lock = IndexWriteLock::acquire(temp.path(), Duration::from_millis(10)).unwrap();
        assert_eq!(state(temp.path()), IndexRefusalState::IndexingInProgress);
        drop(lock);

        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".ok")).unwrap();
        std::fs::write(
            temp.path().join(".ok/index.sqlite"),
            b"this is not a sqlite database",
        )
        .unwrap();
        assert_eq!(state(temp.path()), IndexRefusalState::IndexUnavailable);

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(".ok/index.sqlite");
        SqliteStore::open(&db)
            .unwrap()
            .put_manifest(&make_manifest())
            .unwrap();
        let mut manifest = serde_json::to_value(make_manifest()).unwrap();
        manifest["schema_version"] = json!(open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION + 1);
        Connection::open(&db)
            .unwrap()
            .execute(
                "UPDATE manifests SET json = ?1 WHERE id = 1",
                params![manifest.to_string()],
            )
            .unwrap();
        assert_eq!(state(temp.path()), IndexRefusalState::IndexNewerThanBinary);

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(".ok/index.sqlite");
        SqliteStore::open(&db)
            .unwrap()
            .put_manifest(&make_manifest())
            .unwrap();
        Connection::open(&db)
            .unwrap()
            .pragma_update(
                None,
                "user_version",
                SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION + 1,
            )
            .unwrap();
        assert_eq!(state(temp.path()), IndexRefusalState::IndexNewerThanBinary);
    }

    /// An index that cannot be opened or read while a writer holds the lock is an index being
    /// built, not a broken one: telling the agent to rebuild would contradict `ok doctor`.
    #[test]
    fn a_live_writer_explains_an_index_that_cannot_be_read() {
        use open_kioku_storage::generations::{IndexRefusalState, IndexWriteLock};
        let state = |repo: &std::path::Path| {
            SqliteStore::probe_repo_index(repo)
                .err()
                .expect("the index is refused")
                .state
        };

        // A database that is not one, with no writer: unavailable.
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(".ok/index.sqlite");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"this is not a sqlite database").unwrap();
        assert_eq!(state(temp.path()), IndexRefusalState::IndexUnavailable);

        // The same database while `ok index` or `ok watch` holds the lock: being built.
        let lock = IndexWriteLock::acquire(temp.path(), Duration::from_millis(10)).unwrap();
        assert_eq!(state(temp.path()), IndexRefusalState::IndexingInProgress);
        drop(lock);

        // The snapshot-import window: the file is replaced under the probe while the importer
        // holds the lock.
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(".ok/index.sqlite");
        SqliteStore::open(&db)
            .unwrap()
            .put_manifest(&make_manifest())
            .unwrap();
        let lock = IndexWriteLock::acquire(temp.path(), Duration::from_millis(10)).unwrap();
        std::fs::write(&db, b"half an imported snapshot").unwrap();
        assert_eq!(state(temp.path()), IndexRefusalState::IndexingInProgress);
        drop(lock);
        assert_eq!(state(temp.path()), IndexRefusalState::IndexUnavailable);
    }

    /// Probing answers without writing: it adds nothing to an index that does not carry the
    /// withdrawal table, and it serves a database this process may not write to.
    #[test]
    fn probing_creates_no_schema_and_works_on_a_read_only_database() {
        fn withdrawal_tables(db: &std::path::Path) -> i64 {
            Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap()
                .query_row(
                    "SELECT count(*) FROM sqlite_master
                     WHERE type = 'table' AND name = 'manifest_withdrawals'",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        }

        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let db = repo.join(".ok/index.sqlite");
        let store = SqliteStore::open(&db).unwrap();
        store.put_manifest(&make_manifest()).unwrap();
        drop(store);
        // An index written before the withdrawal table existed.
        Connection::open(&db)
            .unwrap()
            .execute_batch("DROP TABLE manifest_withdrawals;")
            .unwrap();

        let probed = SqliteStore::probe_repo_index(repo)
            .expect("the index is served")
            .expect("the manifest is published");
        assert_eq!(probed.manifest_withdrawal().unwrap(), None);
        drop(probed);
        assert_eq!(
            withdrawal_tables(&db),
            0,
            "probing must not create the withdrawal table"
        );

        // A database this process may not write to. Rollback journalling first: a read-only
        // open of a WAL database whose `-shm` is gone cannot work at all, which is why the
        // probe keeps a read-write fallback for that case rather than refusing the index.
        Connection::open(&db)
            .unwrap()
            .query_row("PRAGMA journal_mode = DELETE", [], |_| Ok(()))
            .unwrap();
        let mut perms = std::fs::metadata(&db).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&db, perms).unwrap();

        let probed = SqliteStore::probe_repo_index(repo)
            .expect("a read-only index is served")
            .expect("the manifest is published");
        assert_eq!(probed.manifest_withdrawal().unwrap(), None);
        drop(probed);
        assert_eq!(withdrawal_tables(&db), 0);

        let mut perms = std::fs::metadata(&db).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&db, perms).unwrap();
    }

    /// Compacting an index written before secret-value redaction runs while a probe holds its
    /// read connection, leaves the index served, and withdraws nothing: a pre-redaction index
    /// is a published index, so its status carries no reason (#379).
    #[test]
    fn compacting_a_pre_redaction_index_keeps_it_served_under_a_probe() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
        store.put_manifest(&make_manifest()).unwrap();
        assert!(
            store
                .manifest()
                .unwrap()
                .unwrap()
                .predates_secret_redaction(),
            "the fixture manifest records no redaction count"
        );

        let probed = SqliteStore::probe_repo_index(repo)
            .expect("the index is served")
            .expect("the manifest is published");
        assert_eq!(probed.manifest_withdrawal().unwrap(), None);

        let wal = store.path().with_file_name(format!(
            "{}-wal",
            store.path().file_name().unwrap().to_string_lossy()
        ));
        store.vacuum().unwrap();

        // The point of the vacuum: a blocked checkpoint leaves the log in place and used to be
        // reported as success, so the bytes it holds must be gone, not merely claimed gone.
        let wal_bytes = std::fs::metadata(&wal).map(|meta| meta.len()).unwrap_or(0);
        assert_eq!(
            wal_bytes, 0,
            "the write-ahead log is truncated while the probe holds its read connection"
        );
        assert!(
            probed.manifest().unwrap().is_some(),
            "the probe still reads the index it opened before the vacuum"
        );
        assert_eq!(
            probed.manifest_withdrawal().unwrap(),
            None,
            "compacting withdraws nothing"
        );
        let status = SqliteStore::repo_not_indexed_status(repo).unwrap();
        assert_eq!(status.reason, None);
        assert!(SqliteStore::probe_repo_index(repo)
            .expect("the index is still served")
            .is_some());
    }

    #[test]
    fn a_withdrawal_reason_is_reported_until_a_manifest_is_published() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let never_indexed = SqliteStore::repo_not_indexed_status(repo).unwrap();
        assert_eq!(never_indexed.reason, None);
        assert!(
            serde_json::to_value(&never_indexed)
                .unwrap()
                .get("reason")
                .is_none(),
            "the field is absent, not null, when nothing was withdrawn"
        );
        assert!(
            !repo.join(".ok").exists(),
            "reading the status creates nothing"
        );

        let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
        store.put_manifest(&make_manifest()).unwrap();
        store.withdraw_manifest("the search stage failed").unwrap();
        assert!(SqliteStore::open_repo_index(repo).unwrap().is_none());
        let withdrawn = SqliteStore::repo_not_indexed_status(repo).unwrap();
        assert!(!withdrawn.indexed);
        assert_eq!(
            serde_json::to_value(&withdrawn).unwrap()["reason"],
            "the search stage failed"
        );
        assert_eq!(store.not_indexed_status(repo).unwrap(), withdrawn);

        store.put_manifest(&make_manifest()).unwrap();
        assert_eq!(store.manifest_withdrawal().unwrap(), None);

        // A binary that does not know the table publishes with its own statement: the trigger
        // ends the withdrawal, so unpublishing later does not bring the old reason back.
        store.withdraw_manifest("the search stage failed").unwrap();
        let manifest_json = serde_json::to_string(&make_manifest()).unwrap();
        let other_binary = Connection::open(store.path()).unwrap();
        other_binary
            .execute(
                "INSERT INTO manifests(id, json) VALUES(1, ?1)",
                params![manifest_json],
            )
            .unwrap();
        let rows: i64 = other_binary
            .query_row("SELECT count(*) FROM manifest_withdrawals", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 0, "publishing removed the withdrawal row");
        other_binary.execute("DELETE FROM manifests", []).unwrap();
        assert_eq!(
            SqliteStore::repo_not_indexed_status(repo).unwrap().reason,
            None
        );

        // A row present beside a published manifest, as in a database whose triggers are
        // missing, is never reported.
        other_binary
            .execute_batch(
                "DROP TRIGGER manifest_insert_ends_withdrawal;
                 DROP TRIGGER manifest_update_ends_withdrawal;",
            )
            .unwrap();
        other_binary
            .execute(
                "INSERT INTO manifests(id, json) VALUES(1, ?1)",
                params![manifest_json],
            )
            .unwrap();
        other_binary
            .execute(
                "INSERT INTO manifest_withdrawals(id, reason, withdrawn_at)
                 VALUES(1, 'a stale reason', '2026-01-01T00:00:00Z')",
                [],
            )
            .unwrap();
        assert_eq!(store.manifest_withdrawal().unwrap(), None);
        assert_eq!(store.not_indexed_status(repo).unwrap().reason, None);
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
                scopes: &[],
                bindings: &[],
                call_sites: &[],
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
                scopes: &[],
                bindings: &[],
                call_sites: &[],
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
        let implementation_fact = AnalysisFact {
            id: "implementation-1".into(),
            file_id: file.id.clone(),
            symbol_id: None,
            target: "billing::InvoicePublisher".into(),
            target_kind: GraphNodeType::Interface,
            edge_type: GraphEdgeType::Implements,
            range: Some(LineRange::single(24)),
            confidence: Confidence::High,
            source: "open-kioku-static".into(),
            source_type: EvidenceSourceType::StaticAnalysis,
            message: "static implementation evidence".into(),
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
                analysis_facts: &[
                    runtime_fact.clone(),
                    static_fact,
                    git_fact.clone(),
                    implementation_fact.clone(),
                ],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
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
        assert_eq!(all.len(), 4);
        let implementations = store
            .implementation_facts_for_target("InvoicePublisher", 10)
            .unwrap();
        assert_eq!(implementations.len(), 1);
        assert_eq!(implementations[0].id, implementation_fact.id);
        assert_eq!(implementations[0].target, implementation_fact.target);
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
                    scopes: &[],
                    bindings: &[],
                    call_sites: &[],
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
            scopes: &[],
            bindings: &[],
            call_sites: &[],
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
            scopes: &[],
            bindings: &[],
            call_sites: &[],
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

        let mut stale_manifest = manifest.clone();
        let mut semantics = stale_manifest.analysis_semantics.clone().unwrap();
        semantics.descriptor.relationship_resolver_version = "old-resolver".into();
        stale_manifest.analysis_semantics = Some(open_kioku_core::AnalysisSemanticsState::new(
            semantics.descriptor,
        ));
        store.put_manifest(&stale_manifest).unwrap();

        for error in [
            store.neighbors("file:src/lib.rs", 10).unwrap_err(),
            store
                .shortest_path("file:src/lib.rs", "symbol:s1", 4)
                .unwrap_err(),
            store
                .edges_by_type(GraphEdgeType::Defines, 10, 0)
                .unwrap_err(),
            store
                .graph_edges_between("file:src/lib.rs", "symbol:s1", 10)
                .unwrap_err(),
            store.imports().unwrap_err(),
            store
                .implementation_facts_for_target("worker", 10)
                .unwrap_err(),
            store
                .references_for_symbol(&SymbolId::new("s1"), 10)
                .unwrap_err(),
            store.occurrences_for_file(&FileId::new("f1")).unwrap_err(),
        ] {
            let message = error.to_string();
            assert!(message.contains("authoritative relationship evidence unavailable"));
            assert!(message.contains("RebuildRequired"));
        }

        assert!(store.node_by_id("file:src/lib.rs").unwrap().is_some());
        assert_eq!(store.graph_counts().unwrap().edges, 1);
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
            scopes: &[],
            bindings: &[],
            call_sites: &[],
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
            scopes: &[],
            bindings: &[],
            call_sites: &[],
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
    fn untyped_neighbor_reads_exclude_derived_siblings() {
        // `neighbors` backs `module_dependencies`, which a caller reads as imports and
        // dependents. A test paired with the module it is named after is not either.
        let store = make_store();
        let manifest = make_manifest();
        let files = vec![make_file("f1", "a.rs")];
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &files,
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
        let node = |path: &str| GraphNode {
            id: NodeId::new(format!("file:{path}")),
            node_type: GraphNodeType::File,
            label: path.into(),
            ..Default::default()
        };
        let edge = |id: &str, from: &str, to: &str, edge_type: GraphEdgeType| GraphEdge {
            id: EdgeId::new(id),
            from: NodeId::new(format!("file:{from}")),
            to: NodeId::new(format!("file:{to}")),
            edge_type,
            ..Default::default()
        };
        store
            .replace_graph(
                &[node("a.rs"), node("b.rs"), node("a_test.rs")],
                &[
                    edge("e-import", "a.rs", "b.rs", GraphEdgeType::Imports),
                    edge("e-derived", "a_test.rs", "a.rs", GraphEdgeType::DerivedFrom),
                ],
            )
            .unwrap();

        let (_, edges) = store.neighbors("file:a.rs", 50).unwrap();
        assert!(edges.iter().any(|e| e.id.0 == "e-import"));
        assert!(
            !edges.iter().any(|e| e.id.0 == "e-derived"),
            "derived siblings must not reach an untyped neighbor read: {edges:?}"
        );
        // Consumers that want them ask by type.
        let typed = store
            .edges_by_type_for_node(GraphEdgeType::DerivedFrom, "file:a.rs", false, 10, 0)
            .unwrap();
        assert_eq!(typed.len(), 1);
    }

    #[test]
    fn capped_reference_reads_keep_the_same_occurrences_whatever_the_write_order() {
        let store = make_store();
        let manifest = make_manifest();
        let files = vec![make_file("f1", "a.rs"), make_file("f2", "b.rs")];
        let occurrence = |file: &str, line: u32| SymbolOccurrence {
            symbol_id: SymbolId::new("s1"),
            file_id: FileId::new(file),
            range: Some(LineRange::single(line)),
            source_range: None,
            is_definition: false,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::StaticAnalysis,
        };
        // Six references across two files: `file_id` cannot order them on its own, and this read
        // is capped (exact_reference_impacts asks for 100 per symbol), so a non-total order let a
        // fresh index keep a different set of references for the same symbol.
        let forward = vec![
            occurrence("f1", 1),
            occurrence("f1", 2),
            occurrence("f1", 3),
            occurrence("f2", 1),
            occurrence("f2", 2),
            occurrence("f2", 3),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();
        let mut kept_per_write_order = Vec::new();
        for occurrences in [forward, reversed] {
            store
                .replace_index(IndexData {
                    manifest: &manifest,
                    files: &files,
                    symbols: &[],
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
            kept_per_write_order.push(
                store
                    .references_for_symbol(&SymbolId::new("s1"), 3)
                    .unwrap()
                    .into_iter()
                    .map(|occurrence| {
                        (
                            occurrence.file_id.0,
                            occurrence.range.map(|range| range.start),
                        )
                    })
                    .collect::<Vec<_>>(),
            );
        }
        assert_eq!(kept_per_write_order[0].len(), 3);
        assert_eq!(
            kept_per_write_order[0], kept_per_write_order[1],
            "a capped reference read must keep the same occurrences whatever order they were written in"
        );
    }

    #[test]
    fn neighbor_reads_keep_the_lowest_edge_ids_whatever_the_write_order() {
        let store = make_store();
        let manifest = make_manifest();
        let files = vec![make_file("f1", "a.rs")];
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &files,
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
        let node = |path: &str| GraphNode {
            id: NodeId::new(format!("file:{path}")),
            node_type: GraphNodeType::File,
            label: path.into(),
            ..Default::default()
        };
        let edge = |id: &str, to: &str| GraphEdge {
            id: EdgeId::new(id),
            from: NodeId::new("file:a.rs"),
            to: NodeId::new(format!("file:{to}")),
            edge_type: GraphEdgeType::Imports,
            ..Default::default()
        };
        let nodes = [node("a.rs"), node("b.rs"), node("c.rs"), node("d.rs")];
        let forward = [
            edge("e-1", "b.rs"),
            edge("e-2", "c.rs"),
            edge("e-3", "d.rs"),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();
        for edges in [reversed, forward] {
            store.replace_graph(&nodes, &edges).unwrap();
            let (_, kept) = store.neighbors("file:a.rs", 2).unwrap();
            let ids = kept
                .iter()
                .map(|edge| edge.id.0.as_str())
                .collect::<Vec<_>>();
            assert_eq!(ids, vec!["e-1", "e-2"]);
        }
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
            scopes: &[],
            bindings: &[],
            call_sites: &[],
        };
        store.replace_index(data).unwrap();
        store.replace_graph(&[], &[]).unwrap();

        let path = store.shortest_path("x", "y", 5).unwrap();
        assert!(path.is_empty());
    }

    #[test]
    fn legacy_graph_tables_are_discarded_and_report_a_rebuild_instead_of_empty_results() {
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

        // The legacy edge rows cannot be read by the compact statements, so they are dropped
        // rather than half-interpreted, and every relationship read says so.
        let edge_count: i64 = store
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM graph_edges", [], |row| row.get(0))
            .unwrap();
        assert_eq!(edge_count, 0);

        for error in [
            store
                .edges_by_type(GraphEdgeType::Defines, 10, 0)
                .unwrap_err()
                .to_string(),
            store
                .graph_edges_between("legacy_file", "legacy_symbol", 10)
                .unwrap_err()
                .to_string(),
        ] {
            assert!(
                error.contains("older index format") && error.contains("ok index"),
                "expected a rebuild instruction, got: {error}"
            );
        }

        // The surviving nodes are still readable and still true; the per-type edge
        // statistics are not, and say so rather than reporting a counted zero.
        assert_eq!(
            store
                .node_type_stats()
                .unwrap()
                .get("File")
                .map(|s| s.count),
            Some(1)
        );
        for error in [
            store.graph_schema_counts().unwrap_err().to_string(),
            store.edge_type_stats().unwrap_err().to_string(),
        ] {
            assert!(
                error.contains("older index format") && error.contains("ok index"),
                "expected a rebuild instruction, got: {error}"
            );
        }

        let node = GraphNode {
            id: NodeId::new("test_node"),
            node_type: GraphNodeType::File,
            label: "test".into(),
            ..Default::default()
        };
        store.replace_graph(&[node], &[]).unwrap();

        // A rebuild clears the marker, so relationship reads stop reporting it.
        assert!(!schema_meta_flag(
            &store.connection.lock().unwrap(),
            GRAPH_REBUILD_REQUIRED_FLAG
        )
        .unwrap());

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
                     'idx_graph_nodes_label',
                     'idx_graph_nodes_file',
                     'idx_graph_nodes_symbol',
                     'idx_graph_edges_type',
                     'idx_graph_edges_from_type',
                     'idx_graph_edges_to_type',
                     'idx_graph_edges_source_type',
                     'idx_graph_strings_vhash'
                   )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 9);
    }

    #[test]
    fn test_indexed_graph_anchor_lookups() {
        let store = make_store();
        let file = make_file("f1", "src/RouterRegistry.java");
        let symbol = make_symbol("s1", "RouterRegistry", "f1");
        let manifest = make_manifest();
        let files = vec![file];
        let symbols = vec![symbol];
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &files,
                symbols: &symbols,
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

        let file_node = GraphNode {
            id: NodeId::new("file:src/RouterRegistry.java"),
            node_type: GraphNodeType::File,
            label: "src/RouterRegistry.java".into(),
            file_id: Some(FileId::new("f1")),
            ..Default::default()
        };
        let symbol_node = GraphNode {
            id: NodeId::new("symbol:s1"),
            node_type: GraphNodeType::Function,
            label: "com.acme.web.RouterRegistry".into(),
            file_id: Some(FileId::new("f1")),
            symbol_id: Some(SymbolId::new("s1")),
            ..Default::default()
        };
        let edge = GraphEdge {
            id: EdgeId::new("e1"),
            from: file_node.id.clone(),
            to: symbol_node.id.clone(),
            edge_type: GraphEdgeType::Defines,
            evidence: evidence(),
            ..Default::default()
        };
        store
            .replace_graph(&[file_node, symbol_node], std::slice::from_ref(&edge))
            .unwrap();

        let nodes = store
            .nodes_by_label("RouterRegistry", Some(GraphNodeType::Function), 10, 0)
            .unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].id.0, "symbol:s1");

        let outgoing = store
            .edges_by_type_for_node(
                GraphEdgeType::Defines,
                "file:src/RouterRegistry.java",
                true,
                10,
                0,
            )
            .unwrap();
        let incoming = store
            .edges_by_type_for_node(GraphEdgeType::Defines, "symbol:s1", false, 10, 0)
            .unwrap();
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].id, edge.id);
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].id, edge.id);

        let batched = store
            .edges_by_type_for_nodes(
                GraphEdgeType::Defines,
                &["symbol:s1", "symbol:never-stored", "symbol:s1"],
                false,
            )
            .unwrap();
        assert_eq!(batched.len(), 1);
        assert_eq!(batched[0].id, edge.id);
        assert!(store
            .edges_by_type_for_nodes(GraphEdgeType::Calls, &["symbol:s1"], false)
            .unwrap()
            .is_empty());
        assert!(store
            .edges_by_type_for_nodes(GraphEdgeType::Defines, &[], false)
            .unwrap()
            .is_empty());
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
        let store = make_current_store();
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
        let store = make_current_store();
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

    /// Every field of an edge must survive the columnar layout: the compact tables replaced a
    /// self-describing JSON document, so a dropped field would now be silently lost rather
    /// than merely unread.
    #[test]
    fn compact_edge_rows_round_trip_every_field() {
        let store = make_store();
        store
            .replace_index(sample_index_data(&make_manifest()))
            .unwrap();

        let edge = GraphEdge {
            id: EdgeId::new("edge:1"),
            from: NodeId::new("file:src/lib.rs"),
            to: NodeId::new("symbol:s1"),
            edge_type: GraphEdgeType::DependsOn,
            evidence: Evidence {
                id: EvidenceId::new("ev-round-trip"),
                source: "open-kioku-import-resolver/manifest-package".into(),
                source_type: EvidenceSourceType::StaticAnalysis,
                file_range: Some(FileRange {
                    path: "src/lib.rs".into(),
                    line_range: Some(LineRange { start: 7, end: 11 }),
                }),
                symbol_id: Some(SymbolId::new("s1")),
                confidence: Confidence::Exact,
                message: "resolved `serde` as an external package dependency".into(),
                // Nanosecond precision on purpose: `Utc::now()` resolves to nanoseconds on
                // Linux and to microseconds on macOS, so a truncating storage format looks
                // correct on one host and rounds every evidence timestamp on the other.
                indexed_at: chrono::DateTime::parse_from_rfc3339("2026-09-07T03:41:26.594463123Z")
                    .unwrap()
                    .with_timezone(&Utc),
                confidence_score: Some(0.75),
                confidence_reason: Some("manifest match".into()),
                freshness: Some("fresh".into()),
            },
            properties: BTreeMap::from([
                ("target_kind".to_string(), serde_json::json!("Package")),
                ("call_sites".to_string(), serde_json::json!([{ "line": 7 }])),
            ]),
            schema_version: Some("graph-v1".into()),
            source_pass: Some("open-kioku-import-resolver/manifest-package".into()),
            index_mode: Some("full".into()),
            extractor_version: Some("1.2.3".into()),
            ambiguity: vec!["two candidates".into()],
            quality_notes: vec!["speculative".into()],
        };
        store
            .replace_graph(&[], std::slice::from_ref(&edge))
            .unwrap();

        let read = store
            .graph_edges_between("file:src/lib.rs", "symbol:s1", 10)
            .unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(
            serde_json::to_value(&read[0]).unwrap(),
            serde_json::to_value(&edge).unwrap()
        );
    }

    /// An edge with no evidence range, no properties and no optional tail must not acquire
    /// empty-but-present fields on the way through the columns.
    #[test]
    fn compact_edge_rows_round_trip_a_minimal_edge() {
        let store = make_store();
        store
            .replace_index(sample_index_data(&make_manifest()))
            .unwrap();
        let edge = GraphEdge {
            id: EdgeId::new("edge:minimal"),
            from: NodeId::new("a"),
            to: NodeId::new("b"),
            edge_type: GraphEdgeType::Calls,
            evidence: evidence(),
            ..Default::default()
        };
        store
            .replace_graph(&[], std::slice::from_ref(&edge))
            .unwrap();
        let read = store.graph_edges_between("a", "b", 10).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(
            serde_json::to_value(&read[0]).unwrap(),
            serde_json::to_value(&edge).unwrap()
        );
    }

    /// Call sites are write-only today, so the columns that replaced their JSON document are
    /// checked directly: nothing else would notice a field that stopped being persisted.
    #[test]
    fn compact_call_site_rows_round_trip_every_field() {
        let store = make_store();
        let manifest = make_manifest();
        let call_site = open_kioku_core::CallSite {
            id: open_kioku_core::CallSiteId::new("src/lib.rs:call:33:16:33:80:singleton"),
            file_id: FileId::new("f1"),
            scope_id: open_kioku_core::ScopeId::new("src/lib.rs:scope:32:3"),
            caller_symbol_id: Some(SymbolId::new("s1")),
            callee_name: "singleton".into(),
            receiver: Some("Collections".into()),
            receiver_kind: open_kioku_core::ReceiverKind::Type,
            range: open_kioku_core::SourceRange {
                start_line: 33,
                start_column: 16,
                end_line: 33,
                end_column: 80,
            },
        };
        let mut data = sample_index_data(&manifest);
        data.call_sites = std::slice::from_ref(&call_site);
        store.replace_index(data).unwrap();

        let conn = store.connection.lock().unwrap();
        let mut stmt = conn.prepare(compact::CALL_SITE_SELECT).unwrap();
        let mut rows = stmt.query([]).unwrap();
        let row = rows.next().unwrap().expect("one call site row");
        assert_eq!(
            serde_json::to_value(compact::call_site_from_row(row).unwrap()).unwrap(),
            serde_json::to_value(&call_site).unwrap()
        );
    }

    /// The dictionary is owned by the rows that reference it; a re-index must not leave
    /// entries behind that nothing points at.
    #[test]
    fn re_indexing_does_not_accumulate_dictionary_entries() {
        let store = make_store();
        store
            .replace_index(sample_index_data(&make_manifest()))
            .unwrap();
        let edge = GraphEdge {
            id: EdgeId::new("edge:1"),
            from: NodeId::new("a"),
            to: NodeId::new("b"),
            edge_type: GraphEdgeType::Calls,
            evidence: evidence(),
            ..Default::default()
        };
        store
            .replace_graph(&[], std::slice::from_ref(&edge))
            .unwrap();
        let first: i64 = store
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM graph_strings", [], |row| row.get(0))
            .unwrap();
        store
            .replace_graph(&[], std::slice::from_ref(&edge))
            .unwrap();
        let second: i64 = store
            .connection
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM graph_strings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(first, second);
    }

    /// An interrupted reset must not look like a repository with no relationships.
    ///
    /// The drops and the marker commit together, so this state is unreachable through the
    /// migration itself — but it is reachable by anything else that removes the tables, and
    /// the failure mode is the worst one this format has: successful, confident, zero-edge
    /// answers forever, because the `json` discriminator is gone too.
    #[test]
    fn graph_tables_missing_without_a_marker_report_a_rebuild_rather_than_zero_edges() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite");
        {
            let store = SqliteStore::open(&path).unwrap();
            store
                .replace_index(sample_index_data(&make_manifest()))
                .unwrap();
            let edge = GraphEdge {
                id: EdgeId::new("edge:1"),
                from: NodeId::new("a"),
                to: NodeId::new("b"),
                edge_type: GraphEdgeType::Calls,
                evidence: evidence(),
                ..Default::default()
            };
            store
                .replace_graph(&[], std::slice::from_ref(&edge))
                .unwrap();
            assert_eq!(store.graph_edges_between("a", "b", 10).unwrap().len(), 1);

            // Simulate a reset that lost its marker: tables gone, nothing recording it.
            let conn = store.connection.lock().unwrap();
            conn.execute("DROP TABLE graph_edges", []).unwrap();
            conn.execute("DROP TABLE call_sites", []).unwrap();
            conn.execute(
                "DELETE FROM schema_meta WHERE key = ?1",
                params![GRAPH_REBUILD_REQUIRED_FLAG],
            )
            .unwrap();
        }

        let reopened = SqliteStore::open(&path).unwrap();
        reopened.initialize().unwrap();
        let error = reopened
            .graph_edges_between("a", "b", 10)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("older index format") && error.contains("ok index"),
            "expected a rebuild instruction, got: {error}"
        );
        assert!(schema_meta_flag(
            &reopened.connection.lock().unwrap(),
            GRAPH_REBUILD_REQUIRED_FLAG
        )
        .unwrap());
    }

    /// The read-only readers the workspace linker uses carry the gate themselves, because
    /// that path never builds a store and so never runs `initialize`.
    #[test]
    fn read_only_edge_readers_refuse_an_index_awaiting_a_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite");
        {
            let store = SqliteStore::open(&path).unwrap();
            store
                .replace_index(sample_index_data(&make_manifest()))
                .unwrap();
            store.replace_graph(&[], &[]).unwrap();
            set_schema_meta_flag(
                &store.connection.lock().unwrap(),
                GRAPH_REBUILD_REQUIRED_FLAG,
            )
            .unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA query_only = ON;").unwrap();
        // Read-only: the marker check must not try to create `schema_meta`.
        assert!(crate::graph_rebuild_required(&conn).unwrap());
        for error in [
            crate::read_graph_edges_by_type(&conn, GraphEdgeType::ExposesEndpoint)
                .unwrap_err()
                .to_string(),
            crate::read_graph_edges_by_source_type(&conn, EvidenceSourceType::StaticAnalysis)
                .unwrap_err()
                .to_string(),
        ] {
            assert!(
                error.contains("older index format") && error.contains("ok index"),
                "expected a rebuild instruction, got: {error}"
            );
        }
    }

    /// A pre-4.0 index opened read-only never runs the reset, so it meets the old table shape
    /// directly. The failure must name the fix, not an internal column.
    #[test]
    fn read_only_edge_readers_translate_a_pre_4_layout_into_a_rebuild_instruction() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "CREATE TABLE graph_edges(id TEXT PRIMARY KEY, from_id TEXT, to_id TEXT, \
                 edge_type TEXT, source_type TEXT, json TEXT)",
                [],
            )
            .unwrap();
        }
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("PRAGMA query_only = ON;").unwrap();
        let error = crate::read_graph_edges_by_type(&conn, GraphEdgeType::ExposesEndpoint)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("older index format") && error.contains("ok index"),
            "expected a rebuild instruction, got: {error}"
        );
        assert!(
            !error.contains("from_sid"),
            "leaked an internal column: {error}"
        );
    }

    fn sample_index_data<'a>(manifest: &'a IndexManifest) -> IndexData<'a> {
        IndexData {
            manifest,
            files: &[],
            symbols: &[],
            chunks: &[],
            tests: &[],
            imports: &[],
            occurrences: &[],
            analysis_facts: &[],
            scopes: &[],
            bindings: &[],
            call_sites: &[],
        }
    }
}

#[cfg(test)]
mod document_corpus_tests {
    use super::*;
    use open_kioku_core::{DocumentSection, DocumentType, LineRange};
    use open_kioku_storage::MetadataStore;

    fn section(path: &str, hash: &str, content: &str) -> DocumentSection {
        DocumentSection {
            path: PathBuf::from(path),
            heading_path: vec!["Guide".into()],
            line_range: LineRange { start: 1, end: 3 },
            content_hash: hash.into(),
            content: content.into(),
            document_type: DocumentType::Markdown,
        }
    }

    #[test]
    fn partial_document_replacement_preserves_unaffected_paths() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(dir.path().join("index.sqlite")).unwrap();
        store
            .replace_document_corpus(&[
                section("docs/a.md", "a1", "old-a"),
                section("docs/b.md", "b1", "stable-b"),
            ])
            .unwrap();

        store
            .replace_document_sections_for_paths(
                &[PathBuf::from("docs/a.md")],
                &[
                    section("docs/a.md", "a2", "new-a"),
                    section("docs/b.md", "b2-ignored", "should-not-rewrite"),
                ],
            )
            .unwrap();

        let sections = store.document_sections().unwrap();
        assert_eq!(sections.len(), 2);
        assert!(sections.iter().any(|section| {
            section.path == Path::new("docs/a.md") && section.content == "new-a"
        }));
        assert!(sections.iter().any(|section| {
            section.path == Path::new("docs/b.md") && section.content == "stable-b"
        }));
    }
}
