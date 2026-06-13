use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use open_kioku_config::OkConfig;
use open_kioku_errors::{OkError, Result};
use open_kioku_graph::InMemoryGraph;
use open_kioku_ingest::Indexer;
use open_kioku_search_tantivy::{default_index_dir, rebuild_disk_index};
use open_kioku_storage::{GraphStore, HistoryStore, IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::{Component, Path};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchIndexStatus {
    pub files: usize,
    pub symbols: usize,
    pub chunks: usize,
    pub elapsed: Duration,
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

    let mut pending = false;
    loop {
        match rx.recv_timeout(debounce) {
            Ok(Ok(event)) => {
                if is_relevant_event(&root, &event) {
                    pending = true;
                }
            }
            Ok(Err(err)) => return Err(watch_err(err)),
            Err(mpsc::RecvTimeoutError::Timeout) if pending => {
                pending = false;
                match reindex_repo(&root) {
                    Ok(status) => eprintln!(
                        "reindexed {} files, {} symbols, {} chunks in {:.2?}",
                        status.files, status.symbols, status.chunks, status.elapsed
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
    let root = root.as_ref();
    let started = Instant::now();
    let config = OkConfig::load_from_repo(root)?;
    let (snapshot, history) = Indexer::default().index_repo_with_history(root, &config)?;
    let store = SqliteStore::open(root.join(".ok/index.sqlite"))?;
    store.replace_index(IndexData {
        manifest: &snapshot.manifest,
        files: &snapshot.files,
        symbols: &snapshot.symbols,
        chunks: &snapshot.chunks,
        tests: &snapshot.tests,
        imports: &snapshot.imports,
        occurrences: &snapshot.occurrences,
        analysis_facts: &snapshot.analysis_facts,
    })?;
    store.put_history_snapshot(&history)?;

    let graph = InMemoryGraph::from_index_with_analysis(
        &snapshot.files,
        &snapshot.symbols,
        &snapshot.chunks,
        &snapshot.occurrences,
        &snapshot.imports,
        &snapshot.analysis_facts,
    );
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
    })
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
