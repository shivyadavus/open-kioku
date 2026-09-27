//! Stores derived from the index that quote repository text: the semantic vector store and the
//! context handle store. An index run that removes a path (the file was deleted, or the policy
//! now excludes or denies it) removes that path's text from both in the same run, so neither
//! keeps serving it after the index stopped holding it (#564). `ok index` and both `ok watch`
//! writers go through here, after their manifest is published and under the index write lock.

use open_kioku_context_compress::ContextHandleStore;
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
                 vector store; it stays stale until `ok semantic index` rebuilds it",
                vectors.removed_targets, vectors.removed_paths
            ));
        }
        if vectors.discarded_generations > 0 {
            parts.push(format!(
                "discarded {} unreadable semantic generation(s)",
                vectors.discarded_generations
            ));
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
