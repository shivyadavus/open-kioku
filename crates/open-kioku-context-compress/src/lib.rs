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
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
            handles.push(self.store_original("test", &test.name, None, &original)?);
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
        let stored: StoredContext = serde_json::from_str(&raw)?;
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
        self.store_original(kind, &title, file_range, &result.snippet)
    }

    fn store_original(
        &self,
        kind: &str,
        title: &str,
        file_range: Option<FileRange>,
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

    fn initialize(&self) -> Result<()> {
        let conn = self
            .connection
            .lock()
            .map_err(|_| OkError::Storage("context sqlite mutex poisoned".into()))?;
        conn.execute_batch(
            "
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
        let pack = ContextPack {
            task: "token".into(),
            intent: "code_change".into(),
            primary_files: vec![SearchResult {
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
            }],
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
        };

        let compressed = store.compress_pack(&pack).unwrap();
        let retrieved = store.retrieve(&compressed.handles[0].id).unwrap().unwrap();

        assert!(compressed.compression_ratio < 1.0);
        assert!(retrieved.original.contains("issue_token"));
    }
}
