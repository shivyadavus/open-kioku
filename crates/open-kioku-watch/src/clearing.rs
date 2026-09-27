//! Clearing deleted rows from the index files after an index run (#553).
//!
//! The store deletes with `secure_delete = FAST`, which zeroes a row's bytes in the page it
//! leaves and in a page it reuses, but not in the pages it frees. A run that removed a path
//! the policy now excludes therefore compacts the database with `VACUUM`, and every run ends
//! with a truncating checkpoint so the write-ahead log keeps no page an earlier transaction
//! wrote. `ok index` and both `ok watch` writers go through here, so they clear the same
//! things under the same conditions.

use open_kioku_config::OkConfig;
use open_kioku_core::IndexManifest;
use open_kioku_errors::{OkError, Result};
use open_kioku_ingest::path_policy::{IndexPathPolicy, SecurityPathPolicy};
use open_kioku_storage::MetadataStore;
use open_kioku_storage_sqlite::SqliteStore;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The paths a store named before a run replaced its rows, and whether it had a published
/// manifest then.
pub struct PathsBefore {
    indexed: BTreeSet<PathBuf>,
    history: BTreeSet<PathBuf>,
    published: bool,
}

impl PathsBefore {
    pub fn read(store: &SqliteStore) -> Result<Self> {
        let (indexed, history) = store.repository_paths()?;
        // A manifest this version cannot read (a newer one) says nothing about what the rows
        // held, so it counts as unpublished and the run compacts.
        let published = matches!(store.manifest(), Ok(Some(_)));
        Ok(Self {
            indexed,
            history,
            published,
        })
    }
}

/// Whether a writer attempts the whole clearing or only what its own run owes: `ok watch`
/// handles each file event with `Incremental`, so a compaction that keeps failing is retried
/// by the next `ok index` or watcher start rather than once per event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearingScope {
    Full,
    Incremental,
}

/// Whether this run owes a compaction, read after its rows are written and before its manifest
/// is published: it removed a path the policy excludes (indexed content by every rule
/// discovery applies, Git history by the security rules), the database was written by an
/// earlier Open Kioku that did not compact after such a removal, the previous run's clearing
/// did not finish, or the previous run was interrupted before it published (its removals are
/// unknown). A policy that cannot be evaluated counts as owing it.
pub fn compaction_owed(
    root: &Path,
    config: &OkConfig,
    store: &SqliteStore,
    before: &PathsBefore,
    previous: Option<&IndexManifest>,
    scope: ClearingScope,
) -> Result<bool> {
    if scope == ClearingScope::Full
        && (!store.excluded_content_cleared()?
            || previous.is_some_and(|manifest| manifest.quality.pending_deleted_content_clearing))
    {
        return Ok(true);
    }
    if !before.published && !before.indexed.is_empty() {
        return Ok(true);
    }
    let (indexed, history) = store.repository_paths()?;
    let dropped_indexed = before
        .indexed
        .difference(&indexed)
        .cloned()
        .collect::<Vec<_>>();
    let security = match SecurityPathPolicy::new(config) {
        Ok(policy) => policy,
        Err(_) => return Ok(true),
    };
    if before
        .history
        .difference(&history)
        .any(|path| security.exclusion(path).is_some())
    {
        return Ok(true);
    }
    if dropped_indexed.is_empty() {
        return Ok(false);
    }
    let policy = match IndexPathPolicy::for_paths(root, config, &dropped_indexed) {
        Ok(policy) => policy,
        Err(_) => return Ok(true),
    };
    Ok(dropped_indexed
        .iter()
        .any(|path| policy.exclusion(path).is_some()))
}

/// Compact when `compact`, then truncate the write-ahead log. Runs after the manifest is
/// published: the index is correct either way, so a failure is returned for the caller to
/// record in the manifest, not to fail the run.
pub fn clear_deleted_content(store: &SqliteStore, compact: bool) -> Result<()> {
    if compact {
        store.compact()?;
    }
    store.truncate_wal()
}

/// Publish the outcome of [`clear_deleted_content`]: `manifest` was published with
/// `pending_deleted_content_clearing` set to what the run owed, and is written again only
/// when the outcome changes it. `resolved` is whether a success settles every earlier
/// pending clearing too (a compaction, or a `Full` run's checkpoint); an incremental run
/// that only truncated the log leaves an earlier pending compaction reported.
pub fn record_clearing(
    store: &SqliteStore,
    manifest: &mut IndexManifest,
    outcome: &Result<()>,
    resolved: bool,
) -> Result<()> {
    let pending = match outcome {
        Ok(()) => manifest.quality.pending_deleted_content_clearing && !resolved,
        Err(_) => true,
    };
    if pending != manifest.quality.pending_deleted_content_clearing {
        manifest.quality.pending_deleted_content_clearing = pending;
        store.put_manifest(manifest)?;
    }
    Ok(())
}

/// The sentence a writer prints when clearing failed.
pub fn clearing_failure_message(err: &OkError) -> String {
    format!(
        "clearing deleted rows from the index files failed ({err}); they may remain readable \
         in the database or its write-ahead log until a later `ok index` succeeds, and \
         `ok doctor` reports it meanwhile"
    )
}
