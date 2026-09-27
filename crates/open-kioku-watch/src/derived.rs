//! Stores derived from the index that quote repository text: the semantic vector store and the
//! context handle store. An index run that removes a path (the file was deleted, or the policy
//! now excludes or denies it) removes that path's text from both in the same run, so neither
//! keeps serving it after the index stopped holding it (#564). `ok index` and both `ok watch`
//! writers go through here, after their manifest is published and under the index write lock.

use open_kioku_context_compress::ContextHandleStore;
use open_kioku_core::IndexManifest;
use open_kioku_errors::Result;
use open_kioku_semantic::VectorStorePrune;
use open_kioku_storage::MetadataStore;
use open_kioku_storage_sqlite::SqliteStore;
use std::collections::HashSet;
use std::path::Path;

/// What [`prune_removed_paths`] removed.
#[derive(Debug, Clone, Default)]
pub struct DerivedStoresPruned {
    pub vectors: VectorStorePrune,
    pub context_handles: usize,
}

impl DerivedStoresPruned {
    /// One line naming what was removed, or `None` when nothing was.
    pub fn summary(&self) -> Option<String> {
        let mut parts = Vec::new();
        let vectors = &self.vectors;
        if vectors.removed_targets > 0 {
            parts.push(format!(
                "removed {} semantic target(s) for {} path(s) the index no longer embeds from the \
                 vector store, with its vector index; it stays stale until `ok semantic index` \
                 rebuilds it from the kept embeddings",
                vectors.removed_targets, vectors.removed_paths
            ));
        }
        for generation in &vectors.discarded_generations {
            parts.push(format!("discarded semantic generation {generation}"));
        }
        if vectors.discarded_builds > 0 {
            parts.push(format!(
                "discarded {} interrupted semantic build(s)",
                vectors.discarded_builds
            ));
        }
        if self.context_handles > 0 {
            parts.push(format!(
                "deleted {} stored context handle(s) quoting paths the index no longer holds",
                self.context_handles
            ));
        }
        (!parts.is_empty()).then(|| parts.join("; "))
    }
}

/// Removes, from the vector store and the context handle store, the text of every path the
/// published index in `store` no longer holds. Neither store is created when absent.
pub fn prune_removed_paths(root: &Path, store: &SqliteStore) -> Result<DerivedStoresPruned> {
    let files = store.list_files(usize::MAX, 0)?;
    let vectors = open_kioku_semantic::prune_vector_store(root, &files)?;
    let context_handles = match ContextHandleStore::open_repo_existing(root)? {
        Some(handles) => {
            let indexed = files
                .into_iter()
                .map(|file| file.path)
                .collect::<HashSet<_>>();
            handles.prune_removed_paths(&indexed)?
        }
        None => 0,
    };
    Ok(DerivedStoresPruned {
        vectors,
        context_handles,
    })
}

/// [`prune_removed_paths`] for a writer that has published `manifest`: the line to report, if
/// any. A failed prune does not fail the run, whose index is correct either way; it is recorded
/// as `pending_derived_store_pruning`, so `ok status`, `repo_status` and `ok doctor` report it
/// and the next `ok index` or watcher start retries it (a store held by a long reader is the
/// usual cause). A prune that succeeds clears it, having checked every path. Neither outcome
/// touches `pending_deleted_content_clearing`, which is the database's own compaction (#585).
/// The manifest is written only when the outcome changes it. Errs only when that write fails.
pub fn prune_and_record(
    root: &Path,
    store: &SqliteStore,
    manifest: &mut IndexManifest,
) -> Result<Option<String>> {
    let outcome = prune_removed_paths(root, store);
    let pending = outcome.is_err();
    if manifest.quality.pending_derived_store_pruning != pending {
        manifest.quality.pending_derived_store_pruning = pending;
        store.put_manifest(manifest)?;
    }
    Ok(match outcome {
        Ok(pruned) => pruned.summary(),
        Err(err) => Some(prune_failure_message(&err)),
    })
}

/// The sentence a writer prints when pruning failed.
pub fn prune_failure_message(err: &open_kioku_errors::OkError) -> String {
    format!(
        "removing the text of paths the index no longer holds from the semantic vector store \
         and context handle store failed ({err}); it may remain there until a later `ok index` \
         or watcher start prunes them, and `ok doctor` reports it meanwhile"
    )
}
