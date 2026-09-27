//! Clearing deleted rows from the index files after an index run (#553).
//!
//! The store deletes with `secure_delete = FAST`, which zeroes a row's bytes in the page it
//! leaves and in a page it reuses, but not in the pages it frees. A run that removed a path
//! the policy now excludes, or removed a path while the security rules began excluding a new
//! one (content moved out of the index, #567), therefore compacts the database with `VACUUM`,
//! and every run ends
//! with a truncating checkpoint so the write-ahead log keeps no page an earlier transaction
//! wrote. `ok index` and both `ok watch` writers go through here, so they clear the same
//! things under the same conditions.

use open_kioku_config::OkConfig;
use open_kioku_core::{IndexManifest, SkipReason, SkipSource, SkippedPath};
use open_kioku_errors::{OkError, Result};
use open_kioku_ingest::path_policy::{IndexPathPolicy, SecurityPathPolicy};
use open_kioku_storage::MetadataStore;
use open_kioku_storage_sqlite::SqliteStore;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The paths a store named before a run replaced its rows, the paths its published manifest
/// recorded the security rules skipping, and whether it had a published manifest then.
pub struct PathsBefore {
    indexed: BTreeSet<PathBuf>,
    history: BTreeSet<PathBuf>,
    security_skips: SecuritySkips,
    published: bool,
}

impl PathsBefore {
    pub fn read(store: &SqliteStore) -> Result<Self> {
        let (indexed, history) = store.repository_paths()?;
        // A manifest this version cannot read (a newer one) says nothing about what the rows
        // held, so it counts as unpublished and the run compacts.
        let (published, security_skips) = match store.manifest() {
            Ok(Some(manifest)) => (true, security_skips(&manifest.quality.skipped_paths)),
            _ => (false, SecuritySkips::new()),
        };
        Ok(Self {
            indexed,
            history,
            security_skips,
            published,
        })
    }
}

/// How many times each entry the security rules produced appears among a run's skipped paths,
/// keyed as the manifest records it: a secret-like path withheld by redaction is recorded as
/// `[redacted]`, so those are counted together. Held in memory only; nothing here is written.
type SecuritySkips = BTreeMap<(SkipReason, PathBuf), usize>;

fn security_skips(skipped: &[SkippedPath]) -> SecuritySkips {
    let mut counts = SecuritySkips::new();
    for skip in skipped
        .iter()
        .filter(|skip| skip.source == SkipSource::SecurityPolicy)
    {
        *counts.entry((skip.reason, skip.path.clone())).or_default() += 1;
    }
    counts
}

/// Whether `after` holds a security-rule skip `before` did not, counting withheld paths: one
/// more `[redacted]` entry is a secret-like file that was not there before. Withheld paths
/// are indistinguishable, so a run in which one secret-like file disappears as another
/// appears gains none.
fn gained_security_skip(before: &SecuritySkips, after: &SecuritySkips) -> bool {
    after
        .iter()
        .any(|(key, count)| before.get(key).copied().unwrap_or(0) < *count)
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
/// discovery applies, Git history by the security rules), it removed an indexed path while
/// the security rules skip a path they did not skip before (`skipped_now`, this run's
/// discovery; content moved or renamed into a denied or secret-like path leaves from a path
/// the policy still admits, #567), the database was written by an earlier Open Kioku that did
/// not compact after such a removal, the previous run's clearing did not finish, or the
/// previous run was interrupted before it published (its removals are unknown). A policy that
/// cannot be evaluated counts as owing it.
pub fn compaction_owed(
    root: &Path,
    config: &OkConfig,
    store: &SqliteStore,
    before: &PathsBefore,
    skipped_now: &[SkippedPath],
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
    // Where removed content went is not asked of the files: a skipped secret-like file is
    // never read, not even to compare its hash. A removal in the same run as a new skip by the
    // security rules is taken as a move into it. Git-ignored, hidden and `[index] exclude`d
    // skips are not counted: build output and editor files add them on most runs, and a
    // compaction rewrites the whole database.
    if gained_security_skip(&before.security_skips, &security_skips(skipped_now)) {
        return Ok(true);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn skip(path: &str, reason: SkipReason, source: SkipSource) -> SkippedPath {
        SkippedPath {
            path: PathBuf::from(path),
            reason,
            safe_to_show: path != "[redacted]",
            source,
        }
    }

    fn gained(before: &[SkippedPath], after: &[SkippedPath]) -> bool {
        gained_security_skip(&security_skips(before), &security_skips(after))
    }

    #[test]
    fn a_new_denied_or_withheld_path_is_a_gain_and_other_skips_are_not() {
        let env = skip(
            "[redacted]",
            SkipReason::SecretPolicy,
            SkipSource::SecurityPolicy,
        );
        let denied = skip(
            "private/keys.rs",
            SkipReason::Denied,
            SkipSource::SecurityPolicy,
        );
        let ignored = skip("out/app.log", SkipReason::Ignored, SkipSource::GitIgnore);
        let hidden = skip(".cache/x.rs", SkipReason::Hidden, SkipSource::HiddenPolicy);

        assert!(gained(&[], std::slice::from_ref(&denied)));
        assert!(!gained(
            std::slice::from_ref(&denied),
            std::slice::from_ref(&denied)
        ));
        // Withheld paths all read `[redacted]`: one more of them is a new secret-like file.
        assert!(gained(
            std::slice::from_ref(&env),
            &[env.clone(), env.clone()]
        ));
        assert!(!gained(
            &[env.clone(), env.clone()],
            std::slice::from_ref(&env)
        ));
        // Build output and editor files come and go on most runs.
        assert!(!gained(&[], &[ignored, hidden]));
    }
}
