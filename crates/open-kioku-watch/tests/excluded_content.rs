//! A path denied while `ok watch` runs leaves none of its content in the index files once the
//! watcher has re-indexed (#553), as with `ok index`.

use open_kioku_config::OkConfig;
use open_kioku_watch::{reindex_repo, reindex_repo_after_changes};
use std::fs;
use std::path::Path;
use std::process::Command;

const VAULT_ONLY_NAMES: [&str; 3] = ["rotate_sealed_material", "seal_inner", "sealing_probe_"];

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn initialize_repo(repo: &Path) {
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("internal/vault")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn ledger_target() {}\n").unwrap();
    let probes = (0..200)
        .map(|n| format!("    sealing_probe_{n:03}();\n"))
        .collect::<String>();
    fs::write(
        repo.join("internal/vault/keys.rs"),
        format!(
            "pub fn rotate_sealed_material() -> u32 {{\n{probes}    seal_inner()\n}}\n\n\
             fn seal_inner() -> u32 {{\n    7\n}}\n"
        ),
    )
    .unwrap();
    OkConfig::write_default(repo.join("ok.toml")).unwrap();
    git(repo, &["init", "--quiet"]);
    git(repo, &["config", "user.email", "watch@example.com"]);
    git(repo, &["config", "user.name", "Watch Exclusion Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "--quiet", "-m", "initial source"]);
}

/// Every byte of the active index database and its sidecars.
fn index_bytes(repo: &Path) -> Vec<u8> {
    let db = open_kioku_storage::generations::resolve_index_location(repo).sqlite_path();
    let mut bytes = Vec::new();
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut path = db.clone().into_os_string();
        path.push(suffix);
        if let Ok(read) = fs::read(&path) {
            bytes.extend(read);
        }
    }
    bytes
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn watch_reindex_after_a_path_is_denied_leaves_none_of_its_content_on_disk() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    initialize_repo(repo);
    reindex_repo(repo).unwrap();
    let before = index_bytes(repo);
    for needle in VAULT_ONLY_NAMES {
        assert!(holds(&before, needle), "the first index lacks `{needle}`");
    }

    let mut config = OkConfig::load_from_repo(repo).unwrap();
    config.paths.deny.push("internal/vault/**".into());
    fs::write(
        repo.join("ok.toml"),
        toml::to_string_pretty(&config).unwrap(),
    )
    .unwrap();
    let changed = repo.join("ok.toml");
    reindex_repo_after_changes(repo, [changed.as_path()]).unwrap();

    let bytes = index_bytes(repo);
    for needle in VAULT_ONLY_NAMES {
        assert!(!holds(&bytes, needle), "the watched index holds `{needle}`");
    }
}
