use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use open_kioku_config::OkConfig;
use open_kioku_errors::{OkError, Result};
use open_kioku_graph::InMemoryGraph;
use open_kioku_ingest::Indexer;
use open_kioku_search_tantivy::{default_index_dir, rebuild_disk_index};
use open_kioku_storage::{
    classify_file_changes, partial_index_supported, GraphStore, HistoryStore, IndexChangeKind,
    IndexData, MetadataStore, PartialIndexUpdate,
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
    let config = OkConfig::load_from_repo(root)?;
    let (snapshot, history) = Indexer::default().index_repo_with_history(root, &config)?;
    let store = SqliteStore::open(root.join(".ok/index.sqlite"))?;
    let previous_manifest = store.manifest()?;
    let previous_files = store.list_files(usize::MAX, 0)?;
    let changed_paths = changed_paths
        .into_iter()
        .filter_map(|path| path.strip_prefix(root).ok().or(Some(path)))
        .map(Path::to_path_buf)
        .collect::<BTreeSet<_>>();
    let can_partial = config.index.incremental
        && !changed_paths.is_empty()
        && partial_index_supported(previous_manifest.as_ref(), &snapshot.manifest);

    let mut partial = false;
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
            let graph = graph_from_snapshot(&snapshot);
            let affected_nodes = graph
                .nodes
                .values()
                .filter(|node| {
                    node.file_id
                        .as_ref()
                        .is_some_and(|file_id| changed_ids.contains(file_id))
                        || node
                            .symbol_id
                            .as_ref()
                            .is_some_and(|symbol_id| affected_symbol_ids.contains(symbol_id))
                })
                .cloned()
                .collect::<Vec<_>>();
            let affected_node_ids = affected_nodes
                .iter()
                .map(|node| node.id.clone())
                .collect::<BTreeSet<_>>();
            let affected_edges = graph
                .edges
                .iter()
                .filter(|edge| {
                    affected_node_ids.contains(&edge.from) || affected_node_ids.contains(&edge.to)
                })
                .cloned()
                .collect::<Vec<_>>();
            match store.replace_files_index(PartialIndexUpdate {
                manifest: &snapshot.manifest,
                changed_files: &changed_files,
                deleted_file_ids: &deleted_ids,
                symbols: &affected_symbols,
                chunks: &changed_chunks,
                tests: &changed_tests,
                imports: &changed_imports,
                occurrences: &changed_occurrences,
                analysis_facts: &changed_facts,
                graph_nodes: &affected_nodes,
                graph_edges: &affected_edges,
            }) {
                Ok(()) => partial = true,
                Err(_) => persist_full_snapshot(&store, &snapshot)?,
            }
        } else {
            partial = true;
            store.put_manifest(&snapshot.manifest)?;
        }
    } else {
        persist_full_snapshot(&store, &snapshot)?;
    }
    store.put_history_snapshot(&history)?;

    if !partial {
        let graph = graph_from_snapshot(&snapshot);
        store.replace_graph(
            &graph.nodes.values().cloned().collect::<Vec<_>>(),
            &graph.edges,
        )?;
    }
    rebuild_disk_index(
        default_index_dir(root),
        &snapshot.chunks,
        &snapshot.files,
        &snapshot.symbols,
    )?;

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

fn reindex_repo_full(root: impl AsRef<Path>) -> Result<WatchIndexStatus> {
    let root = root.as_ref();
    let started = Instant::now();
    let config = OkConfig::load_from_repo(root)?;
    let (snapshot, history) = Indexer::default().index_repo_with_history(root, &config)?;
    let store = SqliteStore::open(root.join(".ok/index.sqlite"))?;
    persist_full_snapshot(&store, &snapshot)?;
    store.put_history_snapshot(&history)?;
    let graph = graph_from_snapshot(&snapshot);
    store.replace_graph(
        &graph.nodes.values().cloned().collect::<Vec<_>>(),
        &graph.edges,
    )?;
    rebuild_disk_index(
        default_index_dir(root),
        &snapshot.chunks,
        &snapshot.files,
        &snapshot.symbols,
    )?;

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

fn persist_full_snapshot(
    store: &SqliteStore,
    snapshot: &open_kioku_ingest::IndexSnapshot,
) -> Result<()> {
    store.replace_index(IndexData {
        manifest: &snapshot.manifest,
        files: &snapshot.files,
        symbols: &snapshot.symbols,
        chunks: &snapshot.chunks,
        tests: &snapshot.tests,
        imports: &snapshot.imports,
        occurrences: &snapshot.occurrences,
        analysis_facts: &snapshot.analysis_facts,
    })
}

fn graph_from_snapshot(snapshot: &open_kioku_ingest::IndexSnapshot) -> InMemoryGraph {
    InMemoryGraph::from_index_with_analysis(
        &snapshot.files,
        &snapshot.symbols,
        &snapshot.chunks,
        &snapshot.occurrences,
        &snapshot.imports,
        &snapshot.analysis_facts,
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

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}
