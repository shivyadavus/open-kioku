//! After commits and an incremental watch update, the stored graph equals the one a clean
//! index of the same tree writes, node for node and edge for edge (#591, #581).

use open_kioku_config::OkConfig;
use open_kioku_watch::{reindex_repo, reindex_repo_after_changes};
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn commit(root: &Path, message: &str) {
    git(root, &["add", "."]);
    git(root, &["commit", "--quiet", "-m", message]);
}

fn append(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    let mut content = fs::read_to_string(&path).unwrap();
    content.push_str(text);
    fs::write(path, content).unwrap();
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == ".ok" {
            continue;
        }
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

type Row = Vec<Option<String>>;

/// Every stored node and edge by id, with its content hash and every stored field except the
/// edge's `indexed_at`, which is stamped per run.
fn stored_graph(repo: &Path) -> (BTreeMap<String, Row>, BTreeMap<String, Row>) {
    let path = open_kioku_storage::generations::resolve_index_location(repo).sqlite_path();
    let conn = Connection::open(path).unwrap();
    let read = |sql: &str| {
        let mut stmt = conn.prepare(sql).unwrap();
        let columns = stmt.column_count();
        stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let fields = (1..columns)
                .map(|index| {
                    row.get::<_, rusqlite::types::Value>(index)
                        .map(|value| match value {
                            rusqlite::types::Value::Null => None,
                            rusqlite::types::Value::Integer(value) => Some(value.to_string()),
                            rusqlite::types::Value::Real(value) => Some(value.to_string()),
                            rusqlite::types::Value::Text(value) => Some(value),
                            rusqlite::types::Value::Blob(value) => Some(format!("{value:?}")),
                        })
                })
                .collect::<rusqlite::Result<Row>>()?;
            Ok((id, fields))
        })
        .unwrap()
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()
        .unwrap()
    };
    let nodes = read(
        "SELECT id, content_hash, label, node_type, file_id, symbol_id, json FROM graph_nodes",
    );
    let string = |column: &str| format!("(SELECT value FROM graph_strings WHERE sid = e.{column})");
    let edges = read(&format!(
        "SELECT e.id, e.content_hash, {}, {}, e.edge_type, e.confidence, e.source_type, {}, \
         e.ev_id, {}, e.ev_line_start, e.ev_line_end, {}, {}, {}, e.window_rank \
         FROM graph_edges e",
        string("from_sid"),
        string("to_sid"),
        string("source_sid"),
        string("ev_path_sid"),
        string("ev_symbol_sid"),
        string("ev_message_sid"),
        string("extra_sid"),
    ));
    (nodes, edges)
}

fn assert_same_rows(
    kind: &str,
    incremental: &BTreeMap<String, Row>,
    clean: &BTreeMap<String, Row>,
) {
    let differing = incremental
        .iter()
        .filter(|(id, row)| clean.get(*id) != Some(*row))
        .map(|(id, row)| (id, row, clean.get(id)))
        .collect::<Vec<_>>();
    let missing = clean
        .keys()
        .filter(|id| !incremental.contains_key(*id))
        .collect::<Vec<_>>();
    assert!(
        differing.is_empty() && missing.is_empty(),
        "incremental {kind} differ from a clean index: \
         {} differ or are extra (incremental, clean): {differing:#?}; missing: {missing:?}",
        differing.len()
    );
}

/// A file node's `source_pass` names the commits of a co-change fact that points at it, so a
/// new commit changes the stored node of a file this update does not edit. The incremental
/// update must rewrite it, not keep it by id.
#[test]
fn incremental_update_after_commits_equals_a_clean_index_node_for_node() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "mod alpha;\nmod beta;\nmod gamma;\npub fn root() {}\n",
    )
    .unwrap();
    OkConfig::write_default(repo.join("ok.toml")).unwrap();
    git(&repo, &["init", "--quiet"]);
    git(&repo, &["config", "user.email", "watch@example.com"]);
    git(&repo, &["config", "user.name", "Watch Graph Test"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    commit(&repo, "root");
    // alpha and beta change together; gamma on its own.
    fs::write(
        repo.join("src/alpha.rs"),
        "pub fn alpha() { crate::root(); }\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/beta.rs"),
        "pub fn beta() { crate::alpha::alpha(); }\n",
    )
    .unwrap();
    commit(&repo, "alpha and beta");
    fs::write(
        repo.join("src/gamma.rs"),
        "pub fn gamma() { crate::beta::beta(); }\n",
    )
    .unwrap();
    commit(&repo, "gamma");
    // Edited before the first index and committed after it: their content is what the index
    // holds, so the update below does not count them as changed.
    append(&repo, "src/alpha.rs", "pub fn alpha_again() {}\n");
    append(&repo, "src/beta.rs", "pub fn beta_again() {}\n");

    let initial = reindex_repo(&repo).unwrap();
    assert!(!initial.partial);
    let (initial_nodes, _) = stored_graph(&repo);

    // A second commit of alpha and beta changes the co-change facts that point at each, and
    // so their file nodes, while their content stays what the index holds.
    commit(&repo, "alpha and beta again");
    // A content edit to an unrelated file makes the update partial.
    append(&repo, "src/gamma.rs", "// edited\n");
    let status = reindex_repo_after_changes(&repo, [repo.join("src/gamma.rs").as_path()]).unwrap();
    assert!(status.partial, "expected an incremental update: {status:?}");
    assert_eq!(status.changed_files, 1, "only gamma changed: {status:?}");

    let clean = temp.path().join("clean");
    copy_tree(&repo, &clean);
    let full = reindex_repo(&clean).unwrap();
    assert!(!full.partial);

    let (incremental_nodes, incremental_edges) = stored_graph(&repo);
    let (clean_nodes, clean_edges) = stored_graph(&clean);
    let node_json = |nodes: &BTreeMap<String, Row>, id: &str| {
        nodes
            .get(id)
            .and_then(|row| row.last().cloned().flatten())
            .unwrap_or_default()
    };
    assert_ne!(
        node_json(&initial_nodes, "file:src/beta.rs"),
        node_json(&clean_nodes, "file:src/beta.rs"),
        "the fixture must change a node no edited file owns"
    );
    assert_same_rows("nodes", &incremental_nodes, &clean_nodes);
    assert_same_rows("edges", &incremental_edges, &clean_edges);
}
