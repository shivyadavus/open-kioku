use assert_cmd::Command;
use predicates::prelude::*;
use std::path::PathBuf;

fn fixture_dir(name: &str) -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("fixtures");
    path.push(name);
    path
}

fn cleanup_ok_dir(fixture: &str) {
    let dir = fixture_dir(fixture).join(".ok");
    if dir.exists() {
        std::fs::remove_dir_all(dir).unwrap();
    }
    let config = fixture_dir(fixture).join("ok.toml");
    if config.exists() {
        std::fs::remove_file(config).unwrap();
    }
}

#[test]
fn test_rust_fixture_lifecycle() {
    let fixture = "rust-fixture";
    cleanup_ok_dir(fixture);

    // 1. Init
    let mut cmd = Command::cargo_bin("ok").unwrap();
    cmd.current_dir(fixture_dir(fixture))
        .arg("init")
        .arg(".")
        .assert()
        .success();

    assert!(fixture_dir(fixture).join(".ok").exists());
    assert!(fixture_dir(fixture).join("ok.toml").exists());

    // 2. Index
    let mut cmd = Command::cargo_bin("ok").unwrap();
    cmd.current_dir(fixture_dir(fixture))
        .arg("index")
        .arg(".")
        .assert()
        .success();

    assert!(fixture_dir(fixture).join(".ok/index.sqlite").exists());

    // 3. Status
    let mut cmd = Command::cargo_bin("ok").unwrap();
    cmd.current_dir(fixture_dir(fixture))
        .arg("status")
        .arg(".")
        .assert()
        .success()
        .stdout(predicate::str::contains("Healthy index"));

    // 4. Search
    let mut cmd = Command::cargo_bin("ok").unwrap();
    cmd.current_dir(fixture_dir(fixture))
        .arg("search")
        .arg("add")
        .assert()
        .success()
        .stdout(predicate::str::contains("add("));

    // 5. MCP tools/list
    let mcp_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let mut cmd = Command::cargo_bin("ok").unwrap();
    cmd.current_dir(fixture_dir(fixture))
        .args(&["mcp", "serve", "--repo", "."])
        .write_stdin(mcp_req)
        .assert()
        .success()
        .stdout(predicate::str::contains("search_code"));

    cleanup_ok_dir(fixture);
}
