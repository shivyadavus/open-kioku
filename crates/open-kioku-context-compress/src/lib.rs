//! Reversible compression of context packs into compact, retrievable handles.
//!
//! A [`ContextPack`] produced by the planner can be far too large to hand to a
//! model directly: it carries full file snippets for every primary and
//! supporting file plus the validation plan. This crate shrinks such a pack
//! into a [`CompressedContextPack`] whose entries are short one-line
//! [`ContextHandle`] summaries, while the full original text of every entry is
//! persisted locally in a SQLite database (by default `.ok/context.sqlite`
//! under the repository root). Each handle carries a stable content-derived id
//! (`ctx:<hash>`), so a caller can later exchange the id for the untruncated
//! original via [`ContextHandleStore::retrieve`] — compression is lossless as
//! long as the local store is available.
//!
//! Token figures on handles and packs are cheap heuristics (word count vs.
//! `len / 4`), intended only for reporting the estimated compression ratio,
//! not for exact budget accounting.
//!
//! Typical flow:
//!
//! 1. Open a store with [`ContextHandleStore::open_repo`] (or
//!    [`ContextHandleStore::open`] for an explicit database path).
//! 2. Compress a pack with [`ContextHandleStore::compress_pack`].
//! 3. When the full text of an entry is needed again, call
//!    [`ContextHandleStore::retrieve`] with the handle id.

#![warn(missing_docs)]

use chrono::Utc;
use open_kioku_core::{
    CompressedContextPack, Confidence, ContextHandle, ContextHandleId, ContextPack, Evidence,
    EvidenceId, EvidenceSourceType, FileRange, LineRange, SearchResult,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_memory::extract_entities;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// SQLite-backed store that maps context handle ids to their original text.
///
/// The store owns a single SQLite connection guarded by a [`Mutex`], so one
/// instance can be shared across threads. Writes use `INSERT OR REPLACE`
/// keyed on the content-derived handle id, which makes
/// [`compress_pack`](Self::compress_pack) idempotent: compressing the same
/// pack twice produces the same handles and does not duplicate rows.
pub struct ContextHandleStore {
    connection: Mutex<Connection>,
}

/// A handle paired with the full original text it was compressed from.
///
/// Returned by [`ContextHandleStore::retrieve`] when a handle id is found in
/// the store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievedContext {
    /// The compact handle as it appeared in the compressed pack, including
    /// its summary, entities, and token estimates.
    pub handle: ContextHandle,
    /// The untruncated original text (e.g. a file snippet or a validation
    /// test description) that the handle stands in for.
    pub original: String,
    /// When this entry was written to the store (UTC).
    pub created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredContext {
    handle: ContextHandle,
    original: String,
    created_at: chrono::DateTime<Utc>,
    /// The file the original was read from. A handle carries a file range only when its
    /// result had a line range, so pruning a removed path needs the path recorded here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
}

impl ContextHandleStore {
    /// Opens (or creates) a handle store backed by the SQLite file at `path`.
    ///
    /// Missing parent directories are created, and the `context_handles`
    /// schema is initialized if it does not exist yet, so this is safe to
    /// call on a fresh repository.
    ///
    /// # Errors
    ///
    /// Returns [`OkError::Storage`] if the parent directory cannot be
    /// created, the database cannot be opened, or schema initialization
    /// fails.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| OkError::Storage(format!("create context dir: {err}")))?;
        }
        let connection = Connection::open(path).map_err(storage_err)?;
        // A `retrieve_context` read or a `compress_pack` write overlapping an index run's
        // pruning waits for it rather than failing it; the index store waits as long.
        connection.busy_timeout(BUSY_TIMEOUT).map_err(storage_err)?;
        let store = Self {
            connection: Mutex::new(connection),
        };
        store.initialize()?;
        Ok(store)
    }

    /// Opens the store at the repository's default location,
    /// `<repo>/.ok/context.sqlite` (see [`default_context_path`]).
    ///
    /// # Errors
    ///
    /// Same as [`open`](Self::open).
    pub fn open_repo(repo: impl AsRef<Path>) -> Result<Self> {
        Self::open(default_context_path(repo))
    }

    /// The repository's handle store if a compressed pack has ever been written, `None`
    /// otherwise. Reads go through here so that looking up a handle never creates
    /// `.ok/context.sqlite`; `open_repo` is for the writer.
    pub fn open_repo_existing(repo: impl AsRef<Path>) -> Result<Option<Self>> {
        let path = default_context_path(repo);
        if !path.is_file() {
            return Ok(None);
        }
        Self::open(path).map(Some)
    }

    /// Compresses a context pack into handles, persisting each original.
    ///
    /// Every primary file (kind `"primary"`), supporting file (kind
    /// `"impact"`), and validation-plan test (kind `"test"`) becomes one
    /// [`ContextHandle`] whose one-line summary replaces the original text in
    /// the returned pack. Handles are sorted by id and deduplicated, so
    /// identical snippets collapse to a single entry. The returned pack's
    /// token estimates and `compression_ratio` are heuristic word-count
    /// figures (a ratio of `1.0` is reported when the pack is empty), and a
    /// single heuristic [`Evidence`] entry records that compression took
    /// place.
    ///
    /// Originals are written with `INSERT OR REPLACE` under content-derived
    /// ids, so calling this repeatedly with the same pack is idempotent.
    ///
    /// # Errors
    ///
    /// Returns [`OkError::Storage`] if the SQLite write fails or the
    /// connection mutex is poisoned, or a serialization error if an entry
    /// cannot be encoded as JSON.
    pub fn compress_pack(&self, pack: &ContextPack) -> Result<CompressedContextPack> {
        let mut handles = Vec::new();
        for result in &pack.primary_files {
            handles.push(self.store_search_result("primary", result)?);
        }
        for result in &pack.supporting_files {
            handles.push(self.store_search_result("impact", result)?);
        }
        for test in &pack.validation_plan.tests {
            let original = format!(
                "{}\ncommand: {}\nreason: {}",
                test.name,
                test.command.as_deref().unwrap_or("manual validation"),
                test.reason
            );
            handles.push(self.store_original("test", &test.name, None, None, &original)?);
        }

        handles.sort_by(|a, b| a.id.cmp(&b.id));
        handles.dedup_by(|a, b| a.id == b.id);

        let original_tokens = handles
            .iter()
            .map(|handle| handle.original_tokens_estimate)
            .sum::<usize>();
        let compressed_tokens = handles
            .iter()
            .map(|handle| handle.compressed_tokens_estimate)
            .sum::<usize>();
        let compression_ratio = if original_tokens == 0 {
            1.0
        } else {
            compressed_tokens as f32 / original_tokens as f32
        };
        let summary = format!(
            "{} handle(s), estimated {} -> {} tokens. Retrieve originals with `retrieve_context`.",
            handles.len(),
            original_tokens,
            compressed_tokens
        );
        Ok(CompressedContextPack {
            task: pack.task.clone(),
            summary,
            handles,
            original_tokens_estimate: original_tokens,
            compressed_tokens_estimate: compressed_tokens,
            compression_ratio,
            evidence: vec![Evidence {
                id: EvidenceId::new(format!("context-compress:{}", stable_hash(&pack.task, 12))),
                source: "open-kioku-context-compress".into(),
                source_type: EvidenceSourceType::Heuristic,
                file_range: None,
                symbol_id: None,
                confidence: Confidence::Medium,
                message: "context pack compressed into reversible local handles".into(),
                indexed_at: Utc::now(),
                ..Default::default()
            }],
        })
    }

    /// Looks up the original text stored for a handle id.
    ///
    /// Returns `Ok(None)` if the id is unknown to this store — for example
    /// when the handle came from a different repository's database or the
    /// store file was deleted.
    ///
    /// # Errors
    ///
    /// Returns [`OkError::Storage`] if the SQLite query fails or the
    /// connection mutex is poisoned, or a deserialization error if the
    /// stored JSON row cannot be decoded.
    pub fn retrieve(&self, handle: &ContextHandleId) -> Result<Option<RetrievedContext>> {
        Ok(self.load(handle)?.map(|stored| RetrievedContext {
            handle: stored.handle,
            original: stored.original,
            created_at: stored.created_at,
        }))
    }

    fn load(&self, handle: &ContextHandleId) -> Result<Option<StoredContext>> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("context sqlite mutex poisoned".into()))?;
        let raw = conn
            .query_row(
                "SELECT json FROM context_handles WHERE id = ?1",
                params![&handle.0],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(storage_err)?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(&raw)?))
    }

    /// [`Self::retrieve`] for a reader serving the text: a handle whose original was read from
    /// a file `indexed` says the published index no longer holds (deleted, or excluded by the
    /// policy since the handle was stored) is refused with an error that carries neither its
    /// text nor its path. `ok index` deletes such handles after it publishes, but a run killed
    /// or blocked between the two would otherwise leave them served (#585). A row with no
    /// recorded file cannot be checked and is refused the same way, as the prune deletes it;
    /// test handles name a test rather than a file and are served.
    ///
    /// # Errors
    ///
    /// As [`Self::retrieve`], plus [`OkError::Index`] for a refused handle and any error
    /// `indexed` returns.
    pub fn retrieve_indexed(
        &self,
        handle: &ContextHandleId,
        indexed: impl Fn(&Path) -> Result<bool>,
    ) -> Result<Option<RetrievedContext>> {
        let Some(stored) = self.load(handle)? else {
            return Ok(None);
        };
        let source = handle_source(
            &stored.handle.kind,
            stored.path.clone(),
            stored.handle.file_range.as_ref(),
        );
        let held = match source {
            HandleSource::File(path) => indexed(&path)?,
            HandleSource::NoFile => true,
            HandleSource::Unknown => false,
        };
        if !held {
            return Err(OkError::Index(format!(
                "context handle `{}` quotes a file the index no longer holds (deleted, or \
                 excluded by the index policy since the handle was stored), so its text is \
                 withheld; run `ok index` to delete such handles and build a new compressed pack",
                handle.0
            )));
        }
        Ok(Some(RetrievedContext {
            handle: stored.handle,
            original: stored.original,
            created_at: stored.created_at,
        }))
    }

    fn store_search_result(&self, kind: &str, result: &SearchResult) -> Result<ContextHandle> {
        let title = format!(
            "{}{}",
            result.path.display(),
            line_suffix(&result.line_range)
        );
        let file_range = result.line_range.clone().map(|line_range| FileRange {
            path: result.path.clone().into(),
            line_range: Some(line_range),
        });
        self.store_original(
            kind,
            &title,
            file_range,
            Some(result.path.clone()),
            &result.snippet,
        )
    }

    fn store_original(
        &self,
        kind: &str,
        title: &str,
        file_range: Option<FileRange>,
        path: Option<PathBuf>,
        original: &str,
    ) -> Result<ContextHandle> {
        let summary = summarize(kind, title, original);
        let compressed_tokens_estimate = compressed_token_estimate(&summary);
        let handle = ContextHandle {
            id: ContextHandleId::new(format!(
                "ctx:{}",
                stable_hash(&format!("{kind}:{title}:{original}"), 16)
            )),
            kind: kind.into(),
            summary,
            file_range,
            entities: extract_entities(&format!("{title} {original}")),
            original_tokens_estimate: estimate_tokens(original),
            compressed_tokens_estimate,
        };
        let stored = StoredContext {
            handle: handle.clone(),
            original: original.into(),
            created_at: Utc::now(),
            path,
        };
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("context sqlite mutex poisoned".into()))?;
        conn.execute(
            "INSERT OR REPLACE INTO context_handles(id, kind, created_at, json) VALUES(?1, ?2, ?3, ?4)",
            params![
                &handle.id.0,
                &handle.kind,
                stored.created_at.to_rfc3339(),
                serde_json::to_string(&stored)?
            ],
        )
        .map_err(storage_err)?;
        Ok(handle)
    }

    /// Deletes every stored original read from a file outside `indexed_paths`, the paths the
    /// published index holds, and returns how many it deleted. A handle whose file was deleted
    /// or is now excluded by the index policy would otherwise keep serving that file's text
    /// through `retrieve_context` (#564). Test handles name a test rather than a file and are
    /// kept; a row written before paths were recorded, with no file range to read one from,
    /// cannot be checked and is deleted.
    ///
    /// Deleted rows are zeroed (`secure_delete`) and, when any were deleted, the database is
    /// compacted, so no file under `.ok` keeps their text.
    ///
    /// # Errors
    ///
    /// Returns [`OkError::Storage`] if reading, deleting or compacting fails.
    pub fn prune_removed_paths(&self, indexed_paths: &HashSet<PathBuf>) -> Result<usize> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("context sqlite mutex poisoned".into()))?;
        let mut statement = conn
            .prepare("SELECT id, kind, json FROM context_handles")
            .map_err(storage_err)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(storage_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage_err)?;
        drop(statement);
        let stale = rows
            .into_iter()
            .filter(|(_, kind, json)| !handle_path_still_indexed(kind, json, indexed_paths))
            .map(|(id, _, _)| id)
            .collect::<Vec<_>>();
        if stale.is_empty() {
            return Ok(0);
        }
        conn.execute_batch("BEGIN IMMEDIATE").map_err(storage_err)?;
        let deleted = (|| {
            let mut statement = conn
                .prepare("DELETE FROM context_handles WHERE id = ?1")
                .map_err(storage_err)?;
            for id in &stale {
                statement.execute(params![id]).map_err(storage_err)?;
            }
            Ok::<_, OkError>(())
        })();
        match deleted {
            Ok(()) => conn.execute_batch("COMMIT").map_err(storage_err)?,
            Err(err) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(err);
            }
        }
        conn.execute_batch("VACUUM").map_err(storage_err)?;
        Ok(stale.len())
    }

    fn initialize(&self) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("context sqlite mutex poisoned".into()))?;
        conn.execute_batch(
            "
            PRAGMA secure_delete = ON;
            CREATE TABLE IF NOT EXISTS context_handles (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                created_at TEXT NOT NULL,
                json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_context_kind ON context_handles(kind);
            CREATE INDEX IF NOT EXISTS idx_context_created_at ON context_handles(created_at);
            ",
        )
        .map_err(storage_err)?;
        Ok(())
    }
}

/// Returns the default context store path for a repository:
/// `<repo>/.ok/context.sqlite`.
///
/// This is where [`ContextHandleStore::open_repo`] keeps compressed context
/// originals; the path is purely computed and is not created or checked for
/// existence here.
pub fn default_context_path(repo: impl AsRef<Path>) -> PathBuf {
    repo.as_ref().join(".ok/context.sqlite")
}

/// Builds the one-line handle summary: the kind, a compacted title, and the
/// first eight words of the first non-empty line of the original.
fn summarize(kind: &str, title: &str, original: &str) -> String {
    let signal = original
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    let title = compact_title(title);
    if signal.is_empty() {
        format!("{kind} {title}")
    } else {
        format!("{kind} {title}: {signal}")
    }
}

/// Heuristic token count: the larger of the whitespace word count and
/// `len / 4` bytes, so dense code without spaces is not undercounted.
fn estimate_tokens(value: &str) -> usize {
    value.split_whitespace().count().max(value.len() / 4)
}

fn compressed_token_estimate(summary: &str) -> usize {
    summary.split_whitespace().count().saturating_add(3).max(4)
}

/// Shortens `path/to/file.rs:10-20` titles to `file.rs:10-20`; titles without
/// a trailing line range are returned unchanged.
fn compact_title(title: &str) -> String {
    let Some((path, range)) = title.rsplit_once(':') else {
        return title.into();
    };
    let file = path.rsplit('/').next().unwrap_or(path);
    if range.contains('-') && range.chars().all(|ch| ch.is_ascii_digit() || ch == '-') {
        format!("{file}:{range}")
    } else {
        title.into()
    }
}

fn line_suffix(range: &Option<LineRange>) -> String {
    range
        .as_ref()
        .map(|range| format!(":{}-{}", range.start, range.end))
        .unwrap_or_default()
}

/// Returns the first `len` hex nibbles of the SHA-256 digest of `value`,
/// used to derive stable, content-addressed handle and evidence ids.
fn stable_hash(value: &str, len: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    digest
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .take(len)
        .map(|nibble| char::from_digit(nibble as u32, 16).unwrap_or('0'))
        .collect()
}

/// Where a stored original was read from.
enum HandleSource {
    File(PathBuf),
    /// A test handle, which names a test rather than a file.
    NoFile,
    /// A row written before paths were recorded, with no file range to read one from.
    Unknown,
}

fn handle_source(
    kind: &str,
    path: Option<PathBuf>,
    file_range: Option<&FileRange>,
) -> HandleSource {
    match path.or_else(|| file_range.map(|range| range.path.to_path_buf())) {
        Some(path) => HandleSource::File(path),
        None if kind == "test" => HandleSource::NoFile,
        None => HandleSource::Unknown,
    }
}

/// Whether a stored row still describes a file the index holds; see
/// [`ContextHandleStore::prune_removed_paths`].
fn handle_path_still_indexed(kind: &str, json: &str, indexed_paths: &HashSet<PathBuf>) -> bool {
    let Ok(stored) = serde_json::from_str::<StoredContext>(json) else {
        return false;
    };
    match handle_source(kind, stored.path, stored.handle.file_range.as_ref()) {
        HandleSource::File(path) => indexed_paths.contains(&path),
        HandleSource::NoFile => true,
        HandleSource::Unknown => false,
    }
}

fn storage_err(err: rusqlite::Error) -> OkError {
    OkError::Storage(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{ChangeBoundary, RiskReport, ScoreComponent, ValidationPlan};

    #[test]
    fn compresses_and_retrieves_context_handles() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContextHandleStore::open_repo(dir.path()).unwrap();
        let pack = pack(vec![SearchResult {
            path: "src/auth.rs".into(),
            line_range: Some(LineRange { start: 1, end: 18 }),
            snippet: r#"pub fn issue_token(user: &User, grants: &[Grant]) -> Result<String> {
    let subject = user.subject().ok_or(AuthError::MissingSubject)?;
    let audience = grants
        .iter()
        .filter(|grant| grant.is_active())
        .map(|grant| grant.audience())
        .collect::<Vec<_>>();
    let claims = TokenClaims {
        subject: subject.to_owned(),
        audience,
        issued_at: clock::now(),
        expires_at: clock::now() + TOKEN_TTL,
    };
    signer::sign_claims(&claims).map_err(AuthError::from)
}"#
            .into(),
            symbol: None,
            score: 1.0,
            match_reason: "test".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 1.0,
            score_breakdown: vec![ScoreComponent::single(
                "test_score",
                1.0,
                Vec::new(),
                "test fixture",
            )],
            exact_reference_provenance: None,
        }]);

        let compressed = store.compress_pack(&pack).unwrap();
        let retrieved = store.retrieve(&compressed.handles[0].id).unwrap().unwrap();

        assert!(compressed.compression_ratio < 1.0);
        assert!(retrieved.original.contains("issue_token"));
    }

    fn pack(primary_files: Vec<SearchResult>) -> ContextPack {
        ContextPack {
            task: "token".into(),
            intent: "code_change".into(),
            primary_files,
            primary_symbols: Vec::new(),
            supporting_files: Vec::new(),
            dependency_edges: Vec::new(),
            runtime_signals: Vec::new(),
            test_candidates: Vec::new(),
            risk_report: RiskReport {
                level: "low".into(),
                score: 0.1,
                reasons: Vec::new(),
            },
            recommended_change_boundary: ChangeBoundary {
                allowed_files: Vec::new(),
                caution_files: Vec::new(),
                forbidden_files: Vec::new(),
                evidence_refs: Vec::new(),
                ..Default::default()
            },
            validation_plan: ValidationPlan {
                commands: Vec::new(),
                tests: Vec::new(),
                requires_approval: false,
                evidence: Vec::new(),
            },
            evidence: Vec::new(),
            negative_evidence: Vec::new(),
            architecture_policy: None,
            confidence_summary: "test".into(),
            confidence_breakdown: open_kioku_core::ConfidenceBreakdown::default(),
            retrieval_diagnostics: Default::default(),
        }
    }

    fn result(path: &str, line_range: Option<LineRange>, snippet: &str) -> SearchResult {
        SearchResult {
            path: path.into(),
            line_range,
            snippet: snippet.into(),
            symbol: None,
            score: 1.0,
            match_reason: "test".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 1.0,
            score_breakdown: Vec::new(),
            exact_reference_provenance: None,
        }
    }

    /// Every stored original read from a path the index no longer holds is deleted, with or
    /// without a line range, and its text is gone from the database file (#564).
    #[test]
    fn prune_removed_paths_deletes_originals_of_paths_the_index_no_longer_holds() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContextHandleStore::open_repo(dir.path()).unwrap();
        let compressed = store
            .compress_pack(&pack(vec![
                result(
                    "src/invoice.rs",
                    Some(LineRange { start: 1, end: 3 }),
                    "pub fn render_invoice_total() {}",
                ),
                result(
                    "src/payroll.rs",
                    Some(LineRange { start: 1, end: 3 }),
                    "pub fn zebra_payroll_marker() {}",
                ),
                result("config/payroll.toml", None, "okapi_rate_marker = 3"),
            ]))
            .unwrap();
        let handle = |needle: &str| {
            compressed
                .handles
                .iter()
                .find(|handle| handle.summary.contains(needle))
                .unwrap()
                .id
                .clone()
        };
        let (kept, removed, removed_without_range) = (
            handle("render_invoice_total"),
            handle("zebra_payroll_marker"),
            handle("okapi_rate_marker"),
        );
        let indexed = [PathBuf::from("src/invoice.rs")].into_iter().collect();

        assert_eq!(store.prune_removed_paths(&indexed).unwrap(), 2);

        assert!(store.retrieve(&kept).unwrap().is_some());
        assert!(store.retrieve(&removed).unwrap().is_none());
        assert!(store.retrieve(&removed_without_range).unwrap().is_none());
        assert_eq!(store.prune_removed_paths(&indexed).unwrap(), 0);
        drop(store);
        for entry in std::fs::read_dir(dir.path().join(".ok")).unwrap() {
            let path = entry.unwrap().path();
            let bytes = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
            for needle in ["zebra_payroll_marker", "okapi_rate_marker", "payroll"] {
                assert!(
                    !bytes.contains(needle),
                    "{} holds `{needle}`",
                    path.display()
                );
            }
        }
    }

    /// A reader serving handles refuses one quoting a file the index no longer holds, with or
    /// without a line range, even before a prune has deleted it, and says nothing of its text
    /// or path; a handle whose file is held is served as before (#585).
    #[test]
    fn retrieve_indexed_refuses_a_handle_whose_file_the_index_no_longer_holds() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContextHandleStore::open_repo(dir.path()).unwrap();
        let compressed = store
            .compress_pack(&pack(vec![
                result(
                    "src/invoice.rs",
                    Some(LineRange { start: 1, end: 3 }),
                    "pub fn render_invoice_total() {}",
                ),
                result(
                    "src/payroll.rs",
                    Some(LineRange { start: 1, end: 3 }),
                    "pub fn zebra_payroll_marker() {}",
                ),
                result("config/payroll.toml", None, "okapi_rate_marker = 3"),
            ]))
            .unwrap();
        let handle = |needle: &str| {
            compressed
                .handles
                .iter()
                .find(|handle| handle.summary.contains(needle))
                .unwrap()
                .id
                .clone()
        };
        let indexed = |path: &Path| Ok(path == Path::new("src/invoice.rs"));

        let kept = store
            .retrieve_indexed(&handle("render_invoice_total"), indexed)
            .unwrap()
            .unwrap();
        assert!(kept.original.contains("render_invoice_total"));
        for removed in ["zebra_payroll_marker", "okapi_rate_marker"] {
            let err = store
                .retrieve_indexed(&handle(removed), indexed)
                .unwrap_err()
                .to_string();
            assert!(err.contains("no longer holds"), "{err}");
            assert!(!err.contains(removed) && !err.contains("payroll"), "{err}");
            // The row is still stored: only a prune deletes it.
            assert!(store.retrieve(&handle(removed)).unwrap().is_some());
        }
        assert!(store
            .retrieve_indexed(&ContextHandleId::new("ctx:unknown"), indexed)
            .unwrap()
            .is_none());
    }

    /// A row written before paths were recorded is judged by its file range, and one with no
    /// file range cannot be checked, so it goes; test handles name no file and stay.
    #[test]
    fn prune_removed_paths_judges_rows_without_a_recorded_path() {
        let indexed = [PathBuf::from("src/invoice.rs")].into_iter().collect();
        let legacy = |kind: &str, file_range: Option<&str>| {
            serde_json::json!({
                "handle": {
                    "id": "ctx:legacy",
                    "kind": kind,
                    "summary": "legacy",
                    "file_range": file_range.map(|path| serde_json::json!({
                        "path": path,
                        "line_range": {"start": 1, "end": 2}
                    })),
                    "entities": [],
                    "original_tokens_estimate": 1,
                    "compressed_tokens_estimate": 1
                },
                "original": "text",
                "created_at": Utc::now()
            })
            .to_string()
        };

        assert!(handle_path_still_indexed(
            "primary",
            &legacy("primary", Some("src/invoice.rs")),
            &indexed
        ));
        assert!(!handle_path_still_indexed(
            "primary",
            &legacy("primary", Some("src/payroll.rs")),
            &indexed
        ));
        assert!(!handle_path_still_indexed(
            "primary",
            &legacy("primary", None),
            &indexed
        ));
        assert!(handle_path_still_indexed(
            "test",
            &legacy("test", None),
            &indexed
        ));
        assert!(!handle_path_still_indexed("primary", "not json", &indexed));
    }

    /// A reader holding the database when an index run prunes is waited out instead of
    /// failing the prune with "database is locked". Held past rusqlite's default 5 s busy
    /// timeout, which is what the store's own timeout replaces.
    #[test]
    fn prune_removed_paths_waits_for_a_reader_holding_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let store = ContextHandleStore::open_repo(dir.path()).unwrap();
        store
            .compress_pack(&pack(vec![result(
                "src/payroll.rs",
                Some(LineRange { start: 1, end: 3 }),
                "pub fn zebra_payroll_marker() {}",
            )]))
            .unwrap();
        let reader = Connection::open(default_context_path(dir.path())).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let rows: i64 = reader
            .query_row("SELECT COUNT(*) FROM context_handles", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        let released = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(6));
            reader.execute_batch("COMMIT").unwrap();
        });

        assert_eq!(store.prune_removed_paths(&HashSet::new()).unwrap(), 1);
        released.join().unwrap();
    }
}
