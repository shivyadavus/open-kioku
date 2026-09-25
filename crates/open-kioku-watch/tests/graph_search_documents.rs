//! A watch rebuild writes the same search index `ok index` does: graph-node documents
//! alongside the code chunks, so graph search answers the same before and after a change.

use open_kioku_config::OkConfig;
use open_kioku_search_tantivy::TantivySearchIndex;
use open_kioku_storage_sqlite::SqliteStore;
use open_kioku_watch::{reindex_repo, reindex_repo_after_changes};
use std::fs;
use std::path::Path;
use std::process::Command;

fn initialize_repo(repo: &Path) {
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn ledger_target() {}\npub fn ledger_caller() { ledger_target(); }\n",
    )
    .unwrap();
    fs::write(repo.join("src/other.rs"), "pub fn quiet_neighbour() {}\n").unwrap();
    OkConfig::write_default(repo.join("ok.toml")).unwrap();
    git(repo, &["init", "--quiet"]);
    git(repo, &["config", "user.email", "watch@example.com"]);
    git(repo, &["config", "user.name", "Watch Graph Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "--quiet", "-m", "initial source"]);
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// What graph search returns for `query`, as (path, symbol name, snippet); only graph-node
/// documents answer it.
fn graph_hits(repo: &Path, query: &str) -> Vec<(String, Option<String>, String)> {
    let index = TantivySearchIndex::open_or_create(repo.join(".ok/search/tantivy")).unwrap();
    index
        .search_graph(query, 20)
        .unwrap()
        .into_iter()
        .map(|result| {
            (
                result.path.to_string_lossy().replace('\\', "/"),
                result.symbol.map(|symbol| symbol.name),
                result.snippet,
            )
        })
        .collect()
}

/// Graph search finds the node of symbol `name`, defined in `path`.
fn assert_graph_search_finds(repo: &Path, name: &str, path: &str, stage: &str) {
    let hits = graph_hits(repo, name);
    assert!(
        hits.iter()
            .any(|(hit_path, symbol, _)| hit_path == path && symbol.as_deref() == Some(name)),
        "{stage}: graph search for `{name}` should return its node in {path}, got {hits:?}"
    );
}

#[test]
fn watch_rebuilds_keep_graph_node_documents_in_the_search_index() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    initialize_repo(repo);

    let initial = reindex_repo(repo).unwrap();
    assert!(!initial.partial);
    assert_graph_search_finds(repo, "ledger_target", "src/lib.rs", "initial watch index");
    assert_graph_search_finds(
        repo,
        "quiet_neighbour",
        "src/other.rs",
        "initial watch index",
    );

    // An incremental update: only src/lib.rs changed, and its symbol was renamed.
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn ledger_renamed() {}\npub fn ledger_caller() { ledger_renamed(); }\n",
    )
    .unwrap();
    let changed = repo.join("src/lib.rs");
    let status = reindex_repo_after_changes(repo, [changed.as_path()]).unwrap();
    assert!(status.partial, "expected an incremental update: {status:?}");
    assert_eq!(status.changed_files, 1);
    // The changed file's new node, and an unchanged file's node the update did not touch.
    assert_graph_search_finds(repo, "ledger_renamed", "src/lib.rs", "after partial update");
    assert_graph_search_finds(
        repo,
        "quiet_neighbour",
        "src/other.rs",
        "after partial update",
    );
    // The renamed symbol's node is gone from the store, so its document must be too; its
    // name's parts still match other nodes, which is why this reads the snippets.
    let stale = graph_hits(repo, "ledger_target")
        .into_iter()
        .filter(|(_, symbol, snippet)| {
            symbol.as_deref() == Some("ledger_target") || snippet.contains("ledger_target")
        })
        .collect::<Vec<_>>();
    assert!(
        stale.is_empty(),
        "the renamed symbol's old node must not survive in graph search: {stale:?}"
    );

    // A watch event with no usable path set rebuilds in full.
    fs::write(repo.join("src/other.rs"), "pub fn quiet_neighbour() { }\n").unwrap();
    let full = reindex_repo_after_changes(repo, std::iter::empty()).unwrap();
    assert!(!full.partial);
    assert_graph_search_finds(repo, "ledger_renamed", "src/lib.rs", "after full rebuild");
    assert_graph_search_finds(
        repo,
        "quiet_neighbour",
        "src/other.rs",
        "after full rebuild",
    );
}

#[test]
fn watch_rebuilds_in_full_while_the_graph_awaits_a_rebuild() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    initialize_repo(repo);
    reindex_repo(repo).unwrap();

    // The mark an open leaves when it discarded a graph's edges. Only a full graph
    // replacement clears it, so an incremental update would leave graph reads refusing.
    let db = repo.join(".ok/index.sqlite");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO schema_meta(key, value) VALUES('graph_rebuild_required_v4', '1')",
        [],
    )
    .unwrap();
    drop(conn);
    assert!(SqliteStore::open(&db)
        .unwrap()
        .graph_rebuild_required()
        .unwrap());

    fs::write(
        repo.join("src/lib.rs"),
        "pub fn ledger_target() { let _ = 1; }\npub fn ledger_caller() { ledger_target(); }\n",
    )
    .unwrap();
    let changed = repo.join("src/lib.rs");
    let status = reindex_repo_after_changes(repo, [changed.as_path()]).unwrap();

    assert!(
        !status.partial,
        "a graph awaiting a rebuild must be rebuilt in full, not reconciled: {status:?}"
    );
    assert!(!SqliteStore::open(&db)
        .unwrap()
        .graph_rebuild_required()
        .unwrap());
    assert_graph_search_finds(repo, "ledger_target", "src/lib.rs", "after forced full");
    assert_graph_search_finds(repo, "quiet_neighbour", "src/other.rs", "after forced full");
}
