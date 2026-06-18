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

fn run_lifecycle_test(fixture: &str, search_term: &str, expected_path: &str) {
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
        .arg(search_term)
        .assert()
        .success()
        .stdout(predicate::str::contains(search_term));

    // 5. Quality benchmark
    let quality_case = format!("{search_term}={expected_path}");
    let mut cmd = Command::cargo_bin("ok").unwrap();
    cmd.current_dir(fixture_dir(fixture))
        .args([
            "bench",
            ".",
            "--quality-case",
            &quality_case,
            "--quality-min-precision-at-1",
            "1.0",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Quality: precision@1 1.000"));

    cleanup_ok_dir(fixture);
}

#[test]
fn test_rust_fixture_lifecycle() {
    run_lifecycle_test("rust-fixture", "add", "src/main.rs");
}

#[test]
fn test_typescript_fixture_lifecycle() {
    run_lifecycle_test("typescript-fixture", "greet", "index.ts");
}

#[test]
fn test_python_fixture_lifecycle() {
    run_lifecycle_test("python-fixture", "multiply", "app.py");
}

#[test]
fn test_go_fixture_lifecycle() {
    run_lifecycle_test("go-fixture", "main", "main.go");
}

#[test]
fn test_java_fixture_lifecycle() {
    run_lifecycle_test("java-fixture", "hello", "App.java");
}

#[test]
fn test_mcp_tools_list_snapshot() {
    let temp = std::env::temp_dir().join(format!("kioku-test-mcp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).unwrap();

    // Init & Index
    Command::cargo_bin("ok")
        .unwrap()
        .current_dir(&temp)
        .args(["init", "."])
        .assert()
        .success();
    Command::cargo_bin("ok")
        .unwrap()
        .current_dir(&temp)
        .args(["index", "."])
        .assert()
        .success();

    // MCP tools/list
    let mcp_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let mut cmd = Command::cargo_bin("ok").unwrap();
    let assert = cmd
        .current_dir(&temp)
        .args(["mcp", "serve", "--repo", "."])
        .write_stdin(mcp_req)
        .assert()
        .success()
        .stdout(predicate::str::contains("search_code"));

    let output = assert.get_output();
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let json_lines: Vec<&str> = stdout_str.lines().filter(|l| l.starts_with("{")).collect();
    let last_json = json_lines.last().expect("should output JSON");

    // Validate snapshot
    let snapshot_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    let snapshot_file = snapshot_dir.join("tools_list.json");

    // Ensure the output parses as JSON
    let parsed: serde_json::Value = serde_json::from_str(last_json).unwrap();
    let formatted = serde_json::to_string_pretty(&parsed).unwrap();

    if snapshot_file.exists() {
        let expected = std::fs::read_to_string(&snapshot_file).unwrap();
        assert_eq!(
            expected.trim(),
            formatted.trim(),
            "tools_list.json snapshot mismatch"
        );
    } else {
        std::fs::write(&snapshot_file, formatted).unwrap();
    }

    std::fs::remove_dir_all(&temp).unwrap();
}

#[test]
fn test_cli_graph_schema_markdown() {
    let mut cmd = Command::cargo_bin("ok").unwrap();
    cmd.args(["graph", "schema", "--format", "markdown"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "# Open Kioku Evidence Graph Schema v1.0.0",
        ))
        .stdout(predicate::str::contains("## Query Features"))
        .stdout(predicate::str::contains("## Evidence Source Types"))
        .stdout(predicate::str::contains(
            "## Optional Evidence Availability",
        ))
        .stdout(predicate::str::contains("## Node Types"))
        .stdout(predicate::str::contains("### File (Stable)"));
}

#[test]
fn test_cli_graph_schema_json() {
    let mut cmd = Command::cargo_bin("ok").unwrap();
    cmd.args(["graph", "schema", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"version\": \"1.0.0\""))
        .stdout(predicate::str::contains("\"evidence_source_types\": ["))
        .stdout(predicate::str::contains("\"query_features\": ["))
        .stdout(predicate::str::contains("\"optional_evidence\": ["))
        .stdout(predicate::str::contains("\"node_types\": ["))
        .stdout(predicate::str::contains("\"name\": \"File\""));
}
