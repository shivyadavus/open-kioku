#!/usr/bin/env python3
from pathlib import Path

path = Path('crates/open-kioku-watch/src/lib.rs')
text = path.read_text()
anchor = '    fn git(root: &Path, args: &[&str]) {'
if anchor not in text:
    raise SystemExit('watch test insertion anchor missing')
if 'incremental_relationship_graph_matches_clean_final_rebuild' in text:
    raise SystemExit(0)
lines = [
'    #[test]',
'    fn incremental_relationship_graph_matches_clean_final_rebuild() {',
'        let temp = tempfile::tempdir().unwrap();',
'        let repo = temp.path();',
'        fs::create_dir_all(repo.join("src")).unwrap();',
'        fs::write(',
'            repo.join("src/lib.rs"),',
'            "pub fn target() {}\\npub fn caller() { target(); }\\n",',
'        )',
'        .unwrap();',
'        fs::write(repo.join("src/other.rs"), "pub fn unrelated() {}\\n").unwrap();',
'        OkConfig::write_default(repo.join("ok.toml")).unwrap();',
'        git(repo, &["init", "--quiet"]);',
'        git(repo, &["config", "user.email", "watch@example.com"]);',
'        git(repo, &["config", "user.name", "Watch Test"]);',
'        git(repo, &["config", "commit.gpgsign", "false"]);',
'        git(repo, &["add", "."]);',
'        git(repo, &["commit", "--quiet", "-m", "initial source"]);',
'',
'        reindex_repo(repo).unwrap();',
'        fs::write(',
'            repo.join("src/other.rs"),',
'            "pub fn unrelated() { let _stable = 1; }\\n",',
'        )',
'        .unwrap();',
'        let changed = repo.join("src/other.rs");',
'        let status = reindex_repo_after_changes(repo, [changed.as_path()]).unwrap();',
'        assert!(status.partial);',
'',
'        let incremental_store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();',
'        let mut incremental = incremental_store',
'            .edges_by_type(open_kioku_core::GraphEdgeType::Calls, usize::MAX, 0)',
'            .unwrap();',
'        incremental.sort_by(|left, right| left.id.0.cmp(&right.id.0));',
'        assert!(!incremental.is_empty(), "fixture should emit a CALLS edge");',
'',
'        fs::remove_dir_all(repo.join(".ok")).unwrap();',
'        reindex_repo(repo).unwrap();',
'        let clean_store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();',
'        let mut clean = clean_store',
'            .edges_by_type(open_kioku_core::GraphEdgeType::Calls, usize::MAX, 0)',
'            .unwrap();',
'        clean.sort_by(|left, right| left.id.0.cmp(&right.id.0));',
'',
'        assert_eq!(incremental, clean, "incremental and clean CALLS truth diverged");',
'    }',
'',
]
test = '\n'.join(lines) + '\n'
path.write_text(text.replace(anchor, test + anchor, 1))
