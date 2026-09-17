use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use open_kioku_config::OkConfig;
use open_kioku_errors::{OkError, Result};
use open_kioku_graph::InMemoryGraph;
use open_kioku_ingest::Indexer;
use open_kioku_search_tantivy::{default_index_dir, rebuild_disk_index};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_storage::generations::IndexWriteLock;
use open_kioku_storage::{
    analysis_semantics_compatibility, changed_document_paths, classify_file_changes,
    partial_index_supported, GraphStore, HistoryStore, IndexChangeKind, IndexData, MetadataStore,
    PartialIndexUpdate,
};
use open_kioku_storage_sqlite::SqliteStore;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchIndexStatus {
    pub files: usize,
    pub symbols: usize,
    pub chunks: usize,
    pub elapsed: Duration,
    pub partial: bool,
    pub changed_files: usize,
    pub deleted_files: usize,
}

pub fn watch_repo(root: impl AsRef<Path>) -> Result<()> {
    watch_repo_with_debounce(root, DEBOUNCE)
}

pub fn watch_repo_with_debounce(root: impl AsRef<Path>, debounce: Duration) -> Result<()> {
    let root = root.as_ref().canonicalize()?;
    let initial = reindex_repo(&root)?;
    eprintln!(
        "watching {} (indexed {} files, {} symbols, {} chunks in {:.2?})",
        root.display(),
        initial.files,
        initial.symbols,
        initial.chunks,
        initial.elapsed
    );

    let (tx, rx) = mpsc::channel();
    let mut watcher = RecommendedWatcher::new(
        move |event| {
            let _ = tx.send(event);
        },
        Config::default(),
    )
    .map_err(watch_err)?;
    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(watch_err)?;

    let mut pending_paths = BTreeSet::<PathBuf>::new();
    loop {
        match rx.recv_timeout(debounce) {
            Ok(Ok(event)) => {
                if is_relevant_event(&root, &event) {
                    pending_paths.extend(
                        event
                            .paths
                            .iter()
                            .filter(|path| is_relevant_path(&root, path))
                            .cloned(),
                    );
                }
            }
            Ok(Err(err)) => return Err(watch_err(err)),
            Err(mpsc::RecvTimeoutError::Timeout) if !pending_paths.is_empty() => {
                let changed_paths = std::mem::take(&mut pending_paths);
                match reindex_repo_after_changes(&root, changed_paths.iter().map(PathBuf::as_path))
                {
                    Ok(status) => eprintln!(
                        "{}indexed {} files, {} symbols, {} chunks in {:.2?}",
                        if status.partial { "partially re" } else { "re" },
                        status.files,
                        status.symbols,
                        status.chunks,
                        status.elapsed
                    ),
                    Err(err) => eprintln!("watch reindex failed: {err}"),
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(OkError::Index("watch channel disconnected".into()));
            }
        }
    }
}

pub fn reindex_repo(root: impl AsRef<Path>) -> Result<WatchIndexStatus> {
    reindex_repo_full(root)
}

pub fn reindex_repo_after_changes<'a>(
    root: impl AsRef<Path>,
    changed_paths: impl IntoIterator<Item = &'a Path>,
) -> Result<WatchIndexStatus> {
    let root = root.as_ref();
    let started = Instant::now();
    let _lock = IndexWriteLock::acquire(root, IndexWriteLock::DEFAULT_WAIT)?;
    let config = OkConfig::load_from_repo(root)?;
    let (mut snapshot, history) = Indexer::default().index_repo_with_history(root, &config)?;
    let store = SqliteStore::open(
        open_kioku_storage::generations::resolve_index_location(root).sqlite_path(),
    )?;
    let previous_manifest = store.manifest()?;
    let previous_files = store.list_files(usize::MAX, 0)?;
    let previous_documents = store.document_sections()?;
    if previous_manifest.is_some() {
        let compatibility =
            analysis_semantics_compatibility(previous_manifest.as_ref(), &snapshot.manifest);
        if !compatibility.status.allows_partial_index_update() {
            // Watch refuses rather than rebuilding on its own: an incompatible index keeps its
            // manifest until a full `ok index` replaces it, so the next step names this root.
            return Err(OkError::Index(format!(
                "analysis semantics {}: {}; stored={}, current={}; run `ok index {}` to rebuild the index with current analysis semantics",
                format!("{:?}", compatibility.status).to_ascii_lowercase(),
                compatibility.reasons.join("; "),
                compatibility
                    .stored_fingerprint
                    .as_deref()
                    .unwrap_or("missing"),
                compatibility.current_fingerprint,
                root.display()
            )));
        }
    }
    let changed_paths = changed_paths
        .into_iter()
        .filter_map(|path| path.strip_prefix(root).ok().or(Some(path)))
        .map(Path::to_path_buf)
        .collect::<BTreeSet<_>>();
    // Published with the manifest below: a pre-redaction index is rebuilt in full (a partial
    // update is refused over one), and the work stays recorded until the clearing succeeds.
    snapshot.manifest.quality.pending_pre_redaction_compaction = previous_manifest
        .as_ref()
        .is_some_and(|previous| previous.needs_pre_redaction_compaction());
    let can_partial = config.index.incremental
        && !changed_paths.is_empty()
        && partial_index_supported(previous_manifest.as_ref(), &snapshot.manifest);

    let mut partial = false;
    // Set once the partial update has committed: from then on the previous manifest
    // describes rows and a graph this run has already replaced.
    let mut staged_partial = false;
    let mut changed_file_count = 0;
    let mut deleted_file_count = 0;
    if can_partial {
        let changes = classify_file_changes(
            previous_manifest.as_ref(),
            &snapshot.manifest,
            &previous_files,
            &snapshot.files,
        );
        let changed_ids = changes
            .iter()
            .filter(|change| {
                matches!(
                    change.kind,
                    IndexChangeKind::Added | IndexChangeKind::Modified | IndexChangeKind::Renamed
                )
            })
            .filter_map(|change| change.file_id.clone())
            .collect::<BTreeSet<_>>();
        let deleted_ids = changes
            .iter()
            .filter(|change| change.kind == IndexChangeKind::Deleted)
            .filter_map(|change| change.file_id.clone())
            .collect::<Vec<_>>();
        changed_file_count = changed_ids.len();
        deleted_file_count = deleted_ids.len();
        if !changed_ids.is_empty() || !deleted_ids.is_empty() {
            let affected_symbols = snapshot
                .symbols
                .iter()
                .filter(|symbol| changed_ids.contains(&symbol.file_id))
                .cloned()
                .collect::<Vec<_>>();
            let affected_symbol_ids = affected_symbols
                .iter()
                .map(|symbol| symbol.id.clone())
                .collect::<BTreeSet<_>>();
            let changed_files = snapshot
                .files
                .iter()
                .filter(|file| changed_ids.contains(&file.id))
                .cloned()
                .collect::<Vec<_>>();
            let changed_chunks = snapshot
                .chunks
                .iter()
                .filter(|chunk| changed_ids.contains(&chunk.file_id))
                .cloned()
                .collect::<Vec<_>>();
            let changed_tests = snapshot
                .tests
                .iter()
                .filter(|test| changed_ids.contains(&test.file_id))
                .cloned()
                .collect::<Vec<_>>();
            let changed_imports = snapshot
                .imports
                .iter()
                .filter(|import| changed_ids.contains(&import.file_id))
                .cloned()
                .collect::<Vec<_>>();
            let changed_occurrences = snapshot
                .occurrences
                .iter()
                .filter(|occurrence| {
                    changed_ids.contains(&occurrence.file_id)
                        || affected_symbol_ids.contains(&occurrence.symbol_id)
                })
                .cloned()
                .collect::<Vec<_>>();
            let changed_facts = snapshot
                .analysis_facts
                .iter()
                .filter(|fact| changed_ids.contains(&fact.file_id))
                .cloned()
                .collect::<Vec<_>>();
            // The whole graph of the new snapshot, not the changed files' share of it: the
            // store replaces the changed files' edges and reconciles the rest by identity, so
            // an edge from an unchanged file to a symbol this change renamed goes too.
            let graph = graph_from_snapshot(&snapshot);
            let mut nodes = graph.nodes.into_values().collect::<Vec<_>>();
            nodes.sort_unstable_by(|left, right| left.id.0.cmp(&right.id.0));
            match store.stage_files_index_with_graph(
                PartialIndexUpdate {
                    manifest: &snapshot.manifest,
                    changed_files: &changed_files,
                    deleted_file_ids: &deleted_ids,
                    symbols: &affected_symbols,
                    chunks: &changed_chunks,
                    tests: &changed_tests,
                    imports: &changed_imports,
                    occurrences: &changed_occurrences,
                    analysis_facts: &changed_facts,
                    graph_nodes: &[],
                    graph_edges: &[],
                    scopes: &[],
                    bindings: &[],
                    call_sites: &[],
                },
                &nodes,
                &graph.edges,
            ) {
                Ok(_) => {
                    partial = true;
                    staged_partial = true;
                }
                Err(err) => {
                    eprintln!("watch partial update failed, rebuilding the index: {err}");
                    persist_full_snapshot(&store, &snapshot)?;
                }
            }
        } else {
            partial = true;
        }
    } else {
        persist_full_snapshot(&store, &snapshot)?;
    }
    let finish = || -> Result<()> {
        if partial {
            let changed_documents =
                changed_document_paths(&previous_documents, &snapshot.document_sections)
                    .into_iter()
                    .collect::<Vec<_>>();
            if !changed_documents.is_empty() {
                store.replace_document_sections_for_paths(
                    &changed_documents,
                    &snapshot.document_sections,
                )?;
            }
        }
        store.put_history_snapshot(&history)?;

        if !partial {
            let graph = graph_from_snapshot(&snapshot);
            let mut nodes = graph.nodes.values().cloned().collect::<Vec<_>>();
            nodes.sort_unstable_by(|left, right| left.id.0.cmp(&right.id.0));
            store.replace_graph(&nodes, &graph.edges)?;
        }
        if !partial || changed_file_count > 0 || deleted_file_count > 0 {
            // Rebuilt in place: the directory is removed first, so a failure here leaves no
            // search index at all.
            rebuild_disk_index(
                default_index_dir(root),
                &snapshot.chunks,
                &snapshot.files,
                &snapshot.symbols,
            )?;
        }
        // Published last: every component the manifest describes is in place by now.
        store.put_manifest(&snapshot.manifest)
    };
    if let Err(err) = finish() {
        // A full rebuild staged its rows with the manifest removed, so its failure already
        // reads as unindexed. A committed partial update did not: the previous manifest would
        // keep describing rows and a graph this run replaced, over a search index it may have
        // removed. Withdraw it, so reads report the repository unindexed until a run completes;
        // the next event finds no manifest and rebuilds in full.
        if !staged_partial {
            return Err(err);
        }
        let reason = format!(
            "an incremental update replaced the changed files' rows and then failed ({err}); the \
             previous manifest was withdrawn so it is not served over those rows. The rows are \
             kept; the next change re-indexes in full, or run `ok index {}` now",
            root.display()
        );
        return Err(match store.withdraw_manifest(&reason) {
            Ok(()) => OkError::Index(format!(
                "{err}; the index manifest was withdrawn, so reads report the repository as \
                 unindexed; the next change re-indexes in full, or run `ok index {}` now",
                root.display()
            )),
            Err(withdraw_err) => OkError::Index(format!(
                "{err}; withdrawing the previous index manifest also failed ({withdraw_err}), so \
                 it still describes the index before this run; run `ok index`"
            )),
        });
    }
    // A partial update is refused over an index written before secret-value redaction, so it
    // was replaced in full above; its unredacted rows linger in free pages until compacted.
    if !partial
        && previous_manifest
            .as_ref()
            .is_some_and(|previous| previous.needs_pre_redaction_compaction())
    {
        match compact_pre_redaction_bytes(root, &store) {
            Ok(()) => {
                snapshot.manifest.quality.pending_pre_redaction_compaction = false;
                store.put_manifest(&snapshot.manifest)?;
            }
            Err(err) => eprintln!(
                "watch: clearing bytes stored before secret-value redaction failed ({err}); the manifest records the work as outstanding and the next index run retries it"
            ),
        }
    }
    maintain_semantic_index(root, &store, &config);

    Ok(WatchIndexStatus {
        files: snapshot.manifest.file_count,
        symbols: snapshot.manifest.symbol_count,
        chunks: snapshot.manifest.chunk_count,
        elapsed: started.elapsed(),
        partial,
        changed_files: changed_file_count,
        deleted_files: deleted_file_count,
    })
}

/// The semantic vector store and the database's free pages and write-ahead log, which an index
/// written before secret-value redaction filled with values the index no longer holds.
fn compact_pre_redaction_bytes(root: &Path, store: &SqliteStore) -> Result<()> {
    open_kioku_semantic::discard_vector_store(root)?;
    store.vacuum()
}

fn reindex_repo_full(root: impl AsRef<Path>) -> Result<WatchIndexStatus> {
    let root = root.as_ref();
    let started = Instant::now();
    let _lock = IndexWriteLock::acquire(root, IndexWriteLock::DEFAULT_WAIT)?;
    let config = OkConfig::load_from_repo(root)?;
    let (mut snapshot, history) = Indexer::default().index_repo_with_history(root, &config)?;
    let store = SqliteStore::open(
        open_kioku_storage::generations::resolve_index_location(root).sqlite_path(),
    )?;
    let compact_after_publish = store
        .manifest()
        .ok()
        .flatten()
        .is_some_and(|previous| previous.needs_pre_redaction_compaction());
    snapshot.manifest.quality.pending_pre_redaction_compaction = compact_after_publish;
    persist_full_snapshot(&store, &snapshot)?;
    store.put_history_snapshot(&history)?;
    let graph = graph_from_snapshot(&snapshot);
    let mut nodes = graph.nodes.values().cloned().collect::<Vec<_>>();
    nodes.sort_unstable_by(|left, right| left.id.0.cmp(&right.id.0));
    store.replace_graph(&nodes, &graph.edges)?;
    rebuild_disk_index(
        default_index_dir(root),
        &snapshot.chunks,
        &snapshot.files,
        &snapshot.symbols,
    )?;
    // Published last: every component the manifest describes is in place by now.
    store.put_manifest(&snapshot.manifest)?;
    // Bytes written before secret-value redaction linger until cleared. The manifest is
    // published either way and carries the work as outstanding, so a reader holding the
    // database delays this to the next run rather than failing one.
    if compact_after_publish {
        match compact_pre_redaction_bytes(root, &store) {
            Ok(()) => {
                snapshot.manifest.quality.pending_pre_redaction_compaction = false;
                store.put_manifest(&snapshot.manifest)?;
            }
            Err(err) => eprintln!(
                "watch: clearing bytes stored before secret-value redaction failed ({err}); the manifest records the work as outstanding and the next index run retries it"
            ),
        }
    }
    maintain_semantic_index(root, &store, &config);

    Ok(WatchIndexStatus {
        files: snapshot.manifest.file_count,
        symbols: snapshot.manifest.symbol_count,
        chunks: snapshot.manifest.chunk_count,
        elapsed: started.elapsed(),
        partial: false,
        changed_files: snapshot.manifest.file_count,
        deleted_files: 0,
    })
}

fn maintain_semantic_index(root: &Path, store: &SqliteStore, config: &OkConfig) {
    if !config.semantic.enabled {
        return;
    }
    let manager = SemanticIndexManager::new(root, store, &config.semantic);
    match manager.index() {
        Ok(report) => eprintln!(
            "watch semantic refresh: state={}, backend={}, ann={}, vectors={}, embedded={}, reused={}, removed={}",
            report.status.state,
            report.status.backend,
            report.status.ann_active,
            report.status.vector_count,
            report.embedded_count,
            report.reused_embeddings,
            report.removed_count
        ),
        Err(err) => {
            let status = manager.status();
            eprintln!(
                "watch semantic maintenance failed: {err} (state={}, ready={}, vectors={})",
                status.state, status.ready, status.vector_count
            );
        }
    }
}

/// Rows and documents without the manifest; the caller publishes it once the graph and the
/// search index are written.
fn persist_full_snapshot(
    store: &SqliteStore,
    snapshot: &open_kioku_ingest::IndexSnapshot,
) -> Result<()> {
    store.stage_index_with_documents(
        IndexData {
            manifest: &snapshot.manifest,
            files: &snapshot.files,
            symbols: &snapshot.symbols,
            chunks: &snapshot.chunks,
            tests: &snapshot.tests,
            imports: &snapshot.imports,
            occurrences: &snapshot.occurrences,
            analysis_facts: &snapshot.analysis_facts,
            scopes: &[],
            bindings: &[],
            call_sites: &[],
        },
        &snapshot.document_sections,
    )
}

fn graph_from_snapshot(snapshot: &open_kioku_ingest::IndexSnapshot) -> InMemoryGraph {
    InMemoryGraph::from_index_with_resolved_relationships(
        &snapshot.files,
        &snapshot.symbols,
        &snapshot.chunks,
        &snapshot.occurrences,
        &snapshot.imports,
        &snapshot.analysis_facts,
        &snapshot.resolved_relationships,
    )
}

fn is_relevant_event(root: &Path, event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) && event.paths.iter().any(|path| is_relevant_path(root, path))
}

fn is_relevant_path(root: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    !has_component(rel, ".git")
        && !has_component(rel, ".ok")
        && !has_component(rel, "target")
        && !has_component(rel, "node_modules")
        && !has_component(rel, ".venv")
}

fn has_component(path: &Path, name: &str) -> bool {
    path.components().any(|component| match component {
        Component::Normal(value) => value == name,
        _ => false,
    })
}

fn watch_err(err: notify::Error) -> OkError {
    OkError::Index(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    #[test]
    fn filters_internal_index_paths() {
        let root = Path::new("/repo");
        assert!(!is_relevant_path(root, Path::new("/repo/.ok/index.sqlite")));
        assert!(!is_relevant_path(root, Path::new("/repo/.git/index")));
        assert!(!is_relevant_path(root, Path::new("/repo/target/debug/app")));
        assert!(is_relevant_path(root, Path::new("/repo/src/lib.rs")));
    }

    #[test]
    fn incremental_reindex_refuses_incompatible_semantics_and_preserves_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/lib.rs"), "pub fn stable() {}\n").unwrap();
        OkConfig::write_default(repo.join("ok.toml")).unwrap();
        git(repo, &["init", "--quiet"]);
        git(repo, &["config", "user.email", "watch@example.com"]);
        git(repo, &["config", "user.name", "Watch Test"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "initial source"]);

        reindex_repo(repo).unwrap();
        let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
        let mut legacy = store.manifest().unwrap().unwrap();
        let mut state = legacy.analysis_semantics.clone().unwrap();
        state.descriptor.relationship_resolver_version = "old-resolver".into();
        legacy.analysis_semantics = Some(open_kioku_core::AnalysisSemanticsState::new(
            state.descriptor,
        ));
        let legacy_fingerprint = legacy
            .analysis_semantics
            .as_ref()
            .unwrap()
            .fingerprint
            .clone();
        store.put_manifest(&legacy).unwrap();

        fs::write(repo.join("src/lib.rs"), "pub fn stable() { let _ = 1; }\n").unwrap();
        let err = reindex_repo_after_changes(
            repo,
            [repo.join("src/lib.rs")].iter().map(PathBuf::as_path),
        )
        .unwrap_err();
        assert!(err.to_string().contains("analysis semantics"));
        assert!(
            err.to_string()
                .contains(&format!("run `ok index {}`", repo.display())),
            "{err}"
        );

        let persisted = store.manifest().unwrap().unwrap();
        assert_eq!(
            persisted.analysis_semantics.as_ref().unwrap().fingerprint,
            legacy_fingerprint
        );
    }

    #[test]
    fn reindex_repo_writes_sqlite_and_search_indexes() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn issue_token() -> &'static str { \"token\" }\n",
        )
        .unwrap();
        OkConfig::write_default(repo.join("ok.toml")).unwrap();
        git(repo, &["init", "--quiet"]);
        git(repo, &["config", "user.email", "watch@example.com"]);
        git(repo, &["config", "user.name", "Watch Test"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "initial source"]);

        let status = reindex_repo(repo).unwrap();

        assert!(status.files >= 1);
        assert!(status.symbols >= 1);
        assert!(repo.join(".ok/index.sqlite").exists());
        assert!(repo.join(".ok/search/tantivy").exists());

        let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
        let chunks = store.all_chunks().unwrap();
        assert!(chunks
            .iter()
            .any(|chunk| chunk.text.contains("issue_token")));
        assert_eq!(store.recent_commits(10).unwrap().len(), 1);
        let history = store.history_for_file(Path::new("src/lib.rs"), 10).unwrap();
        assert_eq!(history.file_touches.len(), 1);

        let search = open_kioku_search_tantivy::TantivySearchIndex::open_or_create(
            repo.join(".ok/search/tantivy"),
        )
        .unwrap();
        let results = open_kioku_storage::SearchIndex::search(&search, "issue_token", 5).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn reindex_repo_after_changes_uses_partial_update_when_incremental_enabled() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn issue_token() -> &'static str { \"token\" }\n",
        )
        .unwrap();
        fs::write(repo.join("src/other.rs"), "pub fn other_token() {}\n").unwrap();
        OkConfig::write_default(repo.join("ok.toml")).unwrap();
        git(repo, &["init", "--quiet"]);
        git(repo, &["config", "user.email", "watch@example.com"]);
        git(repo, &["config", "user.name", "Watch Test"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "initial source"]);

        let initial = reindex_repo(repo).unwrap();
        assert!(!initial.partial);

        fs::write(
            repo.join("src/lib.rs"),
            "pub fn issue_token() -> &'static str { \"updated\" }\n",
        )
        .unwrap();
        let changed_path = repo.join("src/lib.rs");
        let status = reindex_repo_after_changes(repo, [changed_path.as_path()]).unwrap();

        assert!(status.partial);
        assert_eq!(status.changed_files, 1);
        assert_eq!(status.deleted_files, 0);
        let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
        assert!(store
            .all_chunks()
            .unwrap()
            .iter()
            .any(|chunk| chunk.text.contains("updated")));
        assert!(store
            .all_chunks()
            .unwrap()
            .iter()
            .any(|chunk| chunk.text.contains("other_token")));
    }

    #[test]
    fn incremental_relationship_graph_matches_clean_final_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn target() {}\npub fn caller() { target(); }\n",
        )
        .unwrap();
        fs::write(repo.join("src/other.rs"), "pub fn unrelated() {}\n").unwrap();
        OkConfig::write_default(repo.join("ok.toml")).unwrap();
        git(repo, &["init", "--quiet"]);
        git(repo, &["config", "user.email", "watch@example.com"]);
        git(repo, &["config", "user.name", "Watch Test"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "initial source"]);

        reindex_repo(repo).unwrap();
        fs::write(
            repo.join("src/other.rs"),
            "pub fn unrelated() { let _stable = 1; }\n",
        )
        .unwrap();
        let changed = repo.join("src/other.rs");
        let status = reindex_repo_after_changes(repo, [changed.as_path()]).unwrap();
        assert!(status.partial);

        let semantic_projection = |mut edges: Vec<open_kioku_core::GraphEdge>| {
            edges.sort_by(|left, right| {
                left.from
                    .0
                    .cmp(&right.from.0)
                    .then_with(|| left.to.0.cmp(&right.to.0))
                    .then_with(|| {
                        format!("{:?}", left.edge_type).cmp(&format!("{:?}", right.edge_type))
                    })
            });
            edges
                .into_iter()
                .map(|edge| {
                    (
                        edge.from.0,
                        edge.to.0,
                        format!("{:?}", edge.edge_type),
                        edge.evidence.source,
                        format!("{:?}", edge.evidence.source_type),
                        format!("{:?}", edge.evidence.file_range),
                        edge.evidence.symbol_id.map(|id| id.0),
                        format!("{:?}", edge.evidence.confidence),
                        edge.evidence.message,
                        edge.properties,
                        edge.schema_version,
                        edge.source_pass,
                    )
                })
                .collect::<Vec<_>>()
        };

        let incremental_store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
        let incremental = incremental_store
            .edges_by_type(open_kioku_core::GraphEdgeType::Calls, usize::MAX, 0)
            .unwrap();
        assert!(!incremental.is_empty(), "fixture should emit a CALLS edge");
        let incremental_projection = semantic_projection(incremental);

        fs::remove_dir_all(repo.join(".ok")).unwrap();
        reindex_repo(repo).unwrap();
        let clean_store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
        let clean = clean_store
            .edges_by_type(open_kioku_core::GraphEdgeType::Calls, usize::MAX, 0)
            .unwrap();
        let clean_projection = semantic_projection(clean);

        assert_eq!(
            incremental_projection, clean_projection,
            "incremental and clean CALLS truth/proof projection diverged"
        );
    }

    /// #413: a partial re-index must replace exactly the changed file's edges. Old callers of
    /// a renamed symbol used to stay in the graph because the per-file delete keyed on the
    /// producing pass name instead of the file.
    #[test]
    fn incremental_reindex_drops_edges_the_changed_file_no_longer_supports() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn target() {}\npub fn caller() { target(); }\n",
        )
        .unwrap();
        // Never modified: its call into `lib.rs` is the edge from an unchanged file that the
        // per-file delete cannot see and reconciliation has to keep, drop, or move.
        fs::write(
            repo.join("src/other.rs"),
            "pub fn unrelated() { target(); }\n",
        )
        .unwrap();
        OkConfig::write_default(repo.join("ok.toml")).unwrap();
        git(repo, &["init", "--quiet"]);
        git(repo, &["config", "user.email", "watch@example.com"]);
        git(repo, &["config", "user.name", "Watch Test"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "initial source"]);
        reindex_repo(repo).unwrap();

        let db = repo.join(".ok/index.sqlite");
        let store = SqliteStore::open(&db).unwrap();
        let calls_into = |store: &SqliteStore, name: &str| -> Vec<String> {
            let symbols = store.symbols_named(name, 10).unwrap();
            let Some(symbol) = symbols.iter().find(|symbol| symbol.name == name) else {
                return Vec::new();
            };
            let node = format!("symbol:{}", symbol.id.0);
            let (nodes, edges) = store.neighbors(&node, 100).unwrap();
            edges
                .iter()
                .filter(|edge| {
                    edge.edge_type == open_kioku_core::GraphEdgeType::Calls && edge.to.0 == node
                })
                .map(|edge| {
                    nodes
                        .iter()
                        .find(|candidate| candidate.id == edge.from)
                        .map(|candidate| candidate.label.clone())
                        .unwrap_or_else(|| edge.from.0.clone())
                })
                .collect()
        };
        let unchanged_file_calls = |store: &SqliteStore| call_targets_from(store, "unrelated");
        assert!(
            calls_into(&store, "target").contains(&"src::lib::caller".to_string()),
            "{:?}",
            calls_into(&store, "target")
        );
        assert!(
            unchanged_file_calls(&store).contains("src::lib::target"),
            "fixture should emit a CALLS edge from src/other.rs into target: {:?}",
            unchanged_file_calls(&store)
        );
        let (edges_before, _) = graph_table_counts(&store);

        fs::write(
            repo.join("src/lib.rs"),
            "pub fn renamed_target() {}\npub fn caller() { renamed_target(); }\n",
        )
        .unwrap();
        let status = reindex_repo_after_changes(repo, [repo.join("src/lib.rs").as_path()]).unwrap();
        assert!(status.partial, "{status:?}");

        assert!(
            calls_into(&store, "target").is_empty(),
            "the renamed symbol must not keep its old callers"
        );
        assert!(
            calls_into(&store, "renamed_target").contains(&"src::lib::caller".to_string()),
            "{:?}",
            calls_into(&store, "renamed_target")
        );
        // The unchanged file still says `target()`. No symbol has that name any more, so its
        // edge into the old symbol must be gone. The symbol registry's fuzzy fallback may match
        // the call to `renamed_target`; that is a heuristic, so the edge is not required here,
        // and if it exists it must carry the fallback's low confidence and source rather than
        // read as a resolved call.
        let after_rename = call_edges_from(&store, "unrelated");
        assert!(
            after_rename
                .iter()
                .all(|(label, _)| label != "src::lib::target"),
            "src/other.rs kept its edge into the renamed symbol: {after_rename:?}"
        );
        for (_, edge) in after_rename
            .iter()
            .filter(|(label, _)| label == "src::lib::renamed_target")
        {
            assert_eq!(
                edge.evidence.confidence,
                open_kioku_core::Confidence::Low,
                "{edge:?}"
            );
            assert_eq!(
                edge.evidence.source.as_str(),
                "open-kioku-symbol-registry/fuzzy-fallback",
                "{edge:?}"
            );
        }
        let stale_edges = store
            .edges_by_type(open_kioku_core::GraphEdgeType::Calls, usize::MAX, 0)
            .unwrap()
            .into_iter()
            .filter(|edge| store.node_by_id(&edge.to.0).unwrap().is_none())
            .count();
        assert_eq!(
            stale_edges, 0,
            "no CALLS edge may point at a node that no longer exists"
        );
        drop(store);
        assert_incremental_graph_matches_a_clean_rebuild(repo, "after the rename");
        let store = SqliteStore::open(&db).unwrap();

        // Repeated incremental runs over a tree that ends up unchanged must not grow the
        // edge table or the string dictionary, and must keep the unchanged file's edge. The
        // first cycle (which also reverts the rename) may add a few dictionary entries: edges
        // re-derived from unchanged files legitimately carry the run that re-derived them.
        let touch_and_revert = || {
            fs::write(
                repo.join("src/lib.rs"),
                "pub fn target() {}\npub fn caller() { target(); }\n// touched\n",
            )
            .unwrap();
            let status =
                reindex_repo_after_changes(repo, [repo.join("src/lib.rs").as_path()]).unwrap();
            assert!(status.partial, "{status:?}");
            fs::write(
                repo.join("src/lib.rs"),
                "pub fn target() {}\npub fn caller() { target(); }\n",
            )
            .unwrap();
            let status =
                reindex_repo_after_changes(repo, [repo.join("src/lib.rs").as_path()]).unwrap();
            assert!(status.partial, "{status:?}");
        };
        touch_and_revert();
        assert!(
            unchanged_file_calls(&store).contains("src::lib::target"),
            "{:?}",
            unchanged_file_calls(&store)
        );
        let (edges_after_first, strings_after_first) = graph_table_counts(&store);
        assert_eq!(
            edges_after_first, edges_before,
            "edge rows grew across incremental runs"
        );
        for cycle in 0..3 {
            touch_and_revert();
            let calls = unchanged_file_calls(&store);
            assert!(
                calls.contains("src::lib::target") && !calls.contains("src::lib::renamed_target"),
                "cycle {cycle}: src/other.rs's edge into target did not survive: {calls:?}"
            );
        }
        let (edges_after, strings_after) = graph_table_counts(&store);
        assert_eq!(
            edges_after, edges_before,
            "edge rows grew across incremental runs"
        );
        assert_eq!(
            strings_after, strings_after_first,
            "graph_strings grew across incremental runs"
        );
        assert!(calls_into(&store, "target").contains(&"src::lib::caller".to_string()));

        drop(store);
        assert_incremental_graph_matches_a_clean_rebuild(repo, "after the touch-and-revert cycles");
    }

    /// The persisted CALLS edges from `name`'s symbol, each with the label of the node it
    /// reaches. A resolved symbol node and the symbol registry's analysis node both carry the
    /// callee's qualified name as their label, so either form of the edge is found.
    fn call_edges_from(
        store: &SqliteStore,
        name: &str,
    ) -> Vec<(String, open_kioku_core::GraphEdge)> {
        let Some(symbol) = store
            .symbols_named(name, 10)
            .unwrap()
            .into_iter()
            .find(|symbol| symbol.name == name)
        else {
            return Vec::new();
        };
        let node = format!("symbol:{}", symbol.id.0);
        let (nodes, edges) = store.neighbors(&node, 100).unwrap();
        edges
            .into_iter()
            .filter(|edge| {
                edge.edge_type == open_kioku_core::GraphEdgeType::Calls && edge.from.0 == node
            })
            .filter_map(|edge| {
                let label = nodes
                    .iter()
                    .find(|candidate| candidate.id == edge.to)?
                    .label
                    .clone();
                Some((label, edge))
            })
            .collect()
    }

    /// The labels [`call_edges_from`] reaches.
    fn call_targets_from(store: &SqliteStore, name: &str) -> BTreeSet<String> {
        call_edges_from(store, name)
            .into_iter()
            .map(|(label, _)| label)
            .collect()
    }

    /// Node and edge ids as stored, in id order.
    fn stored_graph_ids(db: &Path) -> (Vec<String>, Vec<String>) {
        let conn =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .unwrap();
        let ids = |sql: &str| {
            conn.prepare(sql)
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        (
            ids("SELECT id FROM graph_nodes ORDER BY id"),
            ids("SELECT id FROM graph_edges ORDER BY id"),
        )
    }

    /// What the incremental path holds is what a clean rebuild of the same tree holds, node
    /// for node and edge for edge. The clean rebuild replaces the index; close every store on
    /// it first.
    fn assert_incremental_graph_matches_a_clean_rebuild(repo: &Path, stage: &str) {
        let db = repo.join(".ok/index.sqlite");
        let incremental = stored_graph_ids(&db);
        fs::remove_dir_all(repo.join(".ok")).unwrap();
        reindex_repo(repo).unwrap();
        assert_eq!(
            incremental,
            stored_graph_ids(&db),
            "incremental graph diverged from a clean rebuild {stage}"
        );
    }

    /// A search-index failure after a partial update has committed must not leave the
    /// previous manifest published over this run's rows with no search index.
    #[test]
    fn incremental_search_index_failure_withdraws_the_manifest() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(repo.join("src/lib.rs"), "pub fn target() {}\n").unwrap();
        fs::write(repo.join("src/other.rs"), "pub fn unrelated() {}\n").unwrap();
        OkConfig::write_default(repo.join("ok.toml")).unwrap();
        git(repo, &["init", "--quiet"]);
        git(repo, &["config", "user.email", "watch@example.com"]);
        git(repo, &["config", "user.name", "Watch Test"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "initial source"]);
        reindex_repo(repo).unwrap();
        assert!(SqliteStore::open_repo_index(repo).unwrap().is_some());

        // A regular file where the search index directory goes fails the in-place rebuild,
        // which runs after the partial update commits.
        let search_dir = repo.join(".ok/search/tantivy");
        fs::remove_dir_all(&search_dir).unwrap();
        fs::write(&search_dir, b"not a directory").unwrap();
        fs::write(repo.join("src/lib.rs"), "pub fn target() { let _ = 1; }\n").unwrap();
        let changed = repo.join("src/lib.rs");
        let error = reindex_repo_after_changes(repo, [changed.as_path()])
            .expect_err("the search stage fails")
            .to_string();
        assert!(error.contains("manifest was withdrawn"), "{error}");
        assert!(
            error.ends_with(&format!(
                "the next change re-indexes in full, or run `ok index {}` now",
                repo.display()
            )),
            "the error must end with the next step: {error}"
        );
        assert!(
            SqliteStore::open_repo_index(repo).unwrap().is_none(),
            "the repository reads as unindexed, not as the previous index"
        );
        // `repo_status` and `ok --json status` say why, instead of describing an index nobody
        // built; the reason ends with the same next step as the error.
        let withdrawn = SqliteStore::repo_not_indexed_status(repo).unwrap();
        assert!(!withdrawn.indexed);
        let reason = withdrawn.reason.expect("the withdrawal records its reason");
        assert!(reason.contains("incremental update"), "{reason}");
        assert!(
            reason.ends_with(&format!("or run `ok index {}` now", repo.display())),
            "{reason}"
        );

        // Repaired, the next event finds no manifest and rebuilds in full.
        fs::remove_file(&search_dir).unwrap();
        let status = reindex_repo_after_changes(repo, [changed.as_path()]).unwrap();
        assert!(!status.partial, "{status:?}");
        assert!(SqliteStore::open_repo_index(repo).unwrap().is_some());
        assert_eq!(
            SqliteStore::repo_not_indexed_status(repo).unwrap().reason,
            None,
            "a published manifest ends the withdrawal"
        );
        assert!(open_kioku_search_tantivy::TantivySearchIndex::exists(
            &search_dir
        ));
    }

    fn graph_table_counts(store: &SqliteStore) -> (i64, i64) {
        let conn = rusqlite::Connection::open_with_flags(
            store.path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let edges = conn
            .query_row("SELECT COUNT(*) FROM graph_edges", [], |row| row.get(0))
            .unwrap();
        let strings = conn
            .query_row("SELECT COUNT(*) FROM graph_strings", [], |row| row.get(0))
            .unwrap();
        (edges, strings)
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn enable_ann_semantics(repo: &Path) {
        let config_path = repo.join("ok.toml");
        let text = fs::read_to_string(&config_path).unwrap();
        let start = text
            .find("[semantic]")
            .expect("default ok.toml has [semantic]");
        let section_end = text[start + 1..]
            .find("\n[")
            .map(|offset| start + 1 + offset + 1)
            .unwrap_or(text.len());
        let replacement = concat!(
            "[semantic]\n",
            "enabled = true\n",
            "backend = \"usearch-hnsw-f32\"\n",
            "provider = \"local\"\n",
            "model = \"local-hash\"\n",
            "dimensions = 64\n",
            "distance = \"cosine\"\n",
            "batch_size = 16\n",
            "ann_min_rows = 1\n",
            "index_symbols = true\n",
            "index_chunks = true\n",
            "index_docs = false\n",
            "index_memory = false\n",
            "external_provider_allowed = false\n\n",
        );
        let mut updated = String::with_capacity(text.len() + replacement.len());
        updated.push_str(&text[..start]);
        updated.push_str(replacement);
        updated.push_str(&text[section_end..]);
        fs::write(&config_path, updated).unwrap();
    }

    /// CC5.3: the watch maintenance path must never leak stale semantic vectors. Content
    /// deleted or renamed on disk cannot keep surfacing from the persistent ANN generation
    /// after the watch-triggered refresh.
    #[test]
    fn watch_semantic_refresh_never_leaks_deleted_or_renamed_vectors() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("src/alpha.rs"),
            "pub fn alpha_secret_token() -> &'static str { \"alpha\" }\n",
        )
        .unwrap();
        fs::write(
            repo.join("src/beta.rs"),
            "pub fn beta_helper_routine() -> &'static str { \"beta\" }\n",
        )
        .unwrap();
        OkConfig::write_default(repo.join("ok.toml")).unwrap();
        enable_ann_semantics(repo);
        git(repo, &["init", "--quiet"]);
        git(repo, &["config", "user.email", "watch@example.com"]);
        git(repo, &["config", "user.name", "Watch Test"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
        git(repo, &["add", "."]);
        git(repo, &["commit", "--quiet", "-m", "initial source"]);

        reindex_repo(repo).unwrap();

        let config = OkConfig::load_from_repo(repo).unwrap();
        {
            let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
            let manager = SemanticIndexManager::new(repo, &store, &config.semantic);
            let status = manager.status();
            assert!(status.ready, "semantic index should be ready: {status:?}");
            assert!(status.ann_active, "ANN backend should be active");
            let hits = manager.search("alpha_secret_token", 5).unwrap();
            assert!(
                hits.iter().any(|hit| hit.path == Path::new("src/alpha.rs")),
                "expected the live file to be retrievable before the mutation"
            );
        }

        // Delete one file and rename the other, then refresh through the watch path.
        fs::remove_file(repo.join("src/alpha.rs")).unwrap();
        fs::rename(repo.join("src/beta.rs"), repo.join("src/gamma.rs")).unwrap();
        let alpha = repo.join("src/alpha.rs");
        let beta = repo.join("src/beta.rs");
        let gamma = repo.join("src/gamma.rs");
        reindex_repo_after_changes(repo, [alpha.as_path(), beta.as_path(), gamma.as_path()])
            .unwrap();

        let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
        let manager = SemanticIndexManager::new(repo, &store, &config.semantic);
        let status = manager.status();
        assert!(
            status.ready && status.ann_active,
            "watch refresh must republish a ready ANN generation: {status:?}"
        );
        assert!(!status.rebuild_required, "{:?}", status.rebuild_reasons);

        let deleted_hits = manager.search("alpha_secret_token", 10).unwrap();
        assert!(
            deleted_hits
                .iter()
                .all(|hit| hit.path != Path::new("src/alpha.rs")),
            "deleted content must not resurface from a stale vector: {deleted_hits:?}"
        );

        let renamed_hits = manager.search("beta_helper_routine", 10).unwrap();
        assert!(
            renamed_hits
                .iter()
                .any(|hit| hit.path == Path::new("src/gamma.rs")),
            "renamed content should be retrievable at its new path: {renamed_hits:?}"
        );
        assert!(
            renamed_hits
                .iter()
                .all(|hit| hit.path != Path::new("src/beta.rs")),
            "renamed content must not keep a stale identity at the old path: {renamed_hits:?}"
        );
    }
}
