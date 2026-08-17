use std::path::PathBuf;
use std::process::Command;

#[test]
fn indexes_and_searches_fixture_repo() {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::write(
        fixture.path().join("main.rs"),
        "fn important_symbol() -> usize { 42 }\n",
    )
    .unwrap();

    let ok = assert_cmd::cargo::cargo_bin!("ok");
    let index = Command::new(ok)
        .arg("--repo")
        .arg(fixture.path())
        .arg("index")
        .output()
        .unwrap();
    assert!(index.status.success(), "index stderr: {}", String::from_utf8_lossy(&index.stderr));

    let search = Command::new(ok)
        .arg("--repo")
        .arg(fixture.path())
        .arg("search")
        .arg("important_symbol")
        .output()
        .unwrap();
    assert!(search.status.success(), "search stderr: {}", String::from_utf8_lossy(&search.stderr));
    assert!(String::from_utf8_lossy(&search.stdout).contains("main.rs"));
}

#[test]
fn graph_query_smoke() {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::write(
        fixture.path().join("lib.rs"),
        "pub fn graph_target() {}\n",
    )
    .unwrap();

    let ok = assert_cmd::cargo::cargo_bin!("ok");
    let index = Command::new(ok)
        .arg("--repo")
        .arg(fixture.path())
        .arg("index")
        .output()
        .unwrap();
    assert!(index.status.success());

    let query = Command::new(ok)
        .arg("--repo")
        .arg(fixture.path())
        .arg("graph")
        .arg("query")
        .arg("--dsl")
        .arg("MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s LIMIT 2")
        .output()
        .unwrap();
    assert!(query.status.success(), "query stderr: {}", String::from_utf8_lossy(&query.stderr));
}

#[test]
fn mcp_tools_list_matches_golden_snapshot() {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::write(fixture.path().join("lib.rs"), "pub fn demo() {}\n").unwrap();

    let ok = assert_cmd::cargo::cargo_bin!("ok");
    let index = Command::new(ok)
        .arg("--repo")
        .arg(fixture.path())
        .arg("index")
        .output()
        .unwrap();
    assert!(index.status.success());

    let input = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{},\"clientInfo\":{\"name\":\"test\",\"version\":\"1\"}}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\",\"params\":{}}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\",\"params\":{}}\n"
    );

    let mut child = Command::new(ok)
        .arg("--repo")
        .arg(fixture.path())
        .arg("mcp")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child.stdin.as_mut().unwrap().write_all(input.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let json_lines: Vec<&str> = stdout_str.lines().filter(|l| l.starts_with("{")).collect();
    let last_json = json_lines.last().expect("should output JSON");

    // Validate the complete public tool-list contract, including descriptions as well as schemas.
    // Intentional MCP description changes therefore require an explicit golden snapshot update.
    let snapshot_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    let snapshot_file = snapshot_dir.join("tools_list.json");

    // Ensure the output parses as JSON
    let parsed: serde_json::Value = serde_json::from_str(last_json).unwrap();
    let formatted = serde_json::to_string_pretty(&parsed).unwrap();

    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&snapshot_file, formatted).unwrap();
    } else if snapshot_file.exists() {
        let expected = std::fs::read_to_string(&snapshot_file).unwrap();
        assert_eq!(
            expected.trim(),
            formatted.trim(),
            "tools_list.json snapshot mismatch"
        );
    } else {
        panic!("missing tools_list.json snapshot; set UPDATE_SNAPSHOTS=1 to create it");
    }
}
