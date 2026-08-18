use open_kioku_config::OkConfig;
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_storage::MetadataStore;
use open_kioku_storage_sqlite::SqliteStore;
use open_kioku_watch::{reindex_repo, reindex_repo_after_changes};
use std::fs;
use std::path::Path;
use std::process::Command;

fn semantic_config(repo: &Path) -> OkConfig {
    let mut config = OkConfig::default();
    config.repo.root = repo.to_path_buf();
    config.semantic.enabled = true;
    config.semantic.backend = "auto".into();
    config.semantic.provider = "local".into();
    config.semantic.model = "local-hash".into();
    config.semantic.dimensions = 64;
    config.semantic.ann_min_rows = 1;
    config.semantic.index_symbols = false;
    config.semantic.index_chunks = true;
    config.semantic.index_docs = false;
    config.semantic.index_memory = false;
    config
}

fn write_config(repo: &Path, config: &OkConfig) {
    fs::write(
        repo.join("ok.toml"),
        toml::to_string_pretty(config).unwrap(),
    )
    .unwrap();
}

fn initialize_repo(repo: &Path) {
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/auth.rs"),
        "pub fn alpha_watch_token() { alpha_watch_token(); }\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/billing.rs"),
        "pub fn billing_watch_token() { billing_watch_token(); }\n",
    )
    .unwrap();
    write_config(repo, &semantic_config(repo));
    git(repo, &["init", "--quiet"]);
    git(repo, &["config", "user.email", "watch@example.com"]);
    git(repo, &["config", "user.name", "Watch Semantic Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "--quiet", "-m", "initial source"]);
}

fn assert_semantic_path(repo: &Path, query: &str, expected: &str) {
    let config = OkConfig::load_from_repo(repo).unwrap();
    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    let manager = SemanticIndexManager::new(repo, &store, &config.semantic);
    let status = manager.status();
    assert!(status.ready, "semantic status was {status:?}");
    assert!(status.ann_active, "semantic status was {status:?}");
    let results = manager.search(query, 20).unwrap();
    assert!(
        results
            .iter()
            .any(|result| result.path == Path::new(expected)),
        "expected {expected:?} for {query:?}, got {results:?}"
    );
}

#[test]
fn watch_refreshes_semantic_generation_after_update_rename_and_delete() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    initialize_repo(repo);

    reindex_repo(repo).unwrap();
    assert_semantic_path(repo, "alpha watch token", "src/auth.rs");

    fs::write(
        repo.join("src/auth.rs"),
        "pub fn beta_watch_token() { beta_watch_token(); }\n",
    )
    .unwrap();
    let auth = repo.join("src/auth.rs");
    let updated = reindex_repo_after_changes(repo, [auth.as_path()]).unwrap();
    assert!(updated.partial);
    assert_eq!(updated.changed_files, 1);
    assert_semantic_path(repo, "beta watch token", "src/auth.rs");

    fs::create_dir_all(repo.join("src/security")).unwrap();
    let renamed = repo.join("src/security/auth.rs");
    fs::rename(&auth, &renamed).unwrap();
    reindex_repo_after_changes(repo, [auth.as_path(), renamed.as_path()]).unwrap();
    assert_semantic_path(repo, "beta watch token", "src/security/auth.rs");
    {
        let config = OkConfig::load_from_repo(repo).unwrap();
        let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
        let manager = SemanticIndexManager::new(repo, &store, &config.semantic);
        let results = manager.search("beta watch token", 20).unwrap();
        assert!(!results
            .iter()
            .any(|result| result.path == Path::new("src/auth.rs")));
    }

    fs::remove_file(&renamed).unwrap();
    let deleted = reindex_repo_after_changes(repo, [renamed.as_path()]).unwrap();
    assert_eq!(deleted.deleted_files, 1);
    let config = OkConfig::load_from_repo(repo).unwrap();
    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    let manager = SemanticIndexManager::new(repo, &store, &config.semantic);
    let status = manager.status();
    assert!(status.ready, "semantic status was {status:?}");
    assert!(status.ann_active, "semantic status was {status:?}");
    let results = manager.search("beta watch token", 20).unwrap();
    assert!(!results
        .iter()
        .any(|result| result.path == Path::new("src/security/auth.rs")));
}

#[test]
fn semantic_refresh_failure_does_not_rollback_authoritative_watch_index() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    initialize_repo(repo);
    reindex_repo(repo).unwrap();

    let mut config = semantic_config(repo);
    config.semantic.backend = "unsupported-backend".into();
    write_config(repo, &config);
    fs::write(
        repo.join("src/auth.rs"),
        "pub fn authoritative_survives_semantic_failure() {}\n",
    )
    .unwrap();
    let auth = repo.join("src/auth.rs");

    let status = reindex_repo_after_changes(repo, [auth.as_path()]).unwrap();
    assert!(status.partial);
    // Updating the fixture's semantic config is itself a repository change, so the watch status
    // may report more than the one source file passed to this call. The contract under test is
    // that the authoritative source update is published even when optional semantic refresh fails.
    assert!(status.changed_files >= 1);

    let store = SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap();
    assert!(store.all_chunks().unwrap().iter().any(|chunk| chunk
        .text
        .contains("authoritative_survives_semantic_failure")));
    let manager = SemanticIndexManager::new(repo, &store, &config.semantic);
    let semantic_status = manager.status();
    assert!(
        !semantic_status.ready,
        "semantic status was {semantic_status:?}"
    );
    assert!(
        semantic_status.stale,
        "semantic status was {semantic_status:?}"
    );
    assert_eq!(semantic_status.state, "stale");
    assert!(semantic_status.notes.iter().any(|note| note
        .contains("semantic index is stale for the current authoritative index generation")));
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
