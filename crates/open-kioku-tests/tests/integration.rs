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

/// Outside test-path files, only a function, method, or test symbol may be persisted as a
/// test target: a constant or struct beside an inline test module is not one.
fn assert_test_targets_are_callables(fixture: &str) {
    let conn = rusqlite::Connection::open(fixture_dir(fixture).join(".ok/index.sqlite")).unwrap();
    let mut statement = conn
        .prepare("SELECT t.json, s.json FROM tests t JOIN symbols s ON s.file_id = t.file_id")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap();
    for row in rows {
        let (target, symbol) = row.unwrap();
        let target: serde_json::Value = serde_json::from_str(&target).unwrap();
        let symbol: serde_json::Value = serde_json::from_str(&symbol).unwrap();
        if target["name"] != symbol["name"] || target["range"] != symbol["range"] {
            continue;
        }
        let kind = symbol["kind"].as_str().unwrap_or_default();
        assert!(
            ["function", "method", "test"]
                .iter()
                .any(|callable| kind.eq_ignore_ascii_case(callable)),
            "{fixture}: `{}` of kind {kind} was persisted as a test target",
            symbol["name"]
        );
    }
    // `rust-tests-fixture` holds a constant, a struct and a helper beside an inline test module
    // with a `#[test]` and an `#[rstest]` case stack. It is its own repository so the plan
    // snapshot over `rust-fixture` keeps one obvious file for its task.
    if fixture == "rust-tests-fixture" {
        let mut names_statement = conn.prepare("SELECT json FROM tests").unwrap();
        let names = names_statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|row| {
                serde_json::from_str::<serde_json::Value>(&row.unwrap()).unwrap()["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect::<std::collections::BTreeSet<_>>();
        for expected in ["clamp_keeps_small_counts", "clamp_bounds_each_case"] {
            assert!(
                names.contains(expected),
                "{expected} missing from {names:?}"
            );
        }
        for unexpected in ["CACHE_LIMIT", "CacheEntry", "clamp_hits", "tests"] {
            assert!(!names.contains(unexpected), "{unexpected} in {names:?}");
        }
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
    assert_test_targets_are_callables(fixture);

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
fn test_rust_tests_fixture_lifecycle() {
    run_lifecycle_test("rust-tests-fixture", "clamp_hits", "src/lib.rs");
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
        std::fs::write(&snapshot_file, formatted).unwrap();
    }

    std::fs::remove_dir_all(&temp).unwrap();
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Recursively normalize nondeterministic values in a `plan_change` response so
/// the golden snapshot captures schema/shape rather than machine-specific values:
/// - timestamp fields become "<TIMESTAMP>"
/// - internal numeric ids assigned in index-insertion order become "<ID>"
/// - floating-point scores/weights become 0.0 (rankings for the tiny fixture are
///   deterministic, but float scoring internals are not part of the contract)
/// - absolute repo paths become "<REPO>"
fn normalize_plan_response(value: &mut serde_json::Value, repo_paths: &[String]) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, entry) in map.iter_mut() {
                match key.as_str() {
                    "indexed_at" | "occurred_at" | "generated_at" | "created_at" | "updated_at" => {
                        *entry = serde_json::Value::String("<TIMESTAMP>".into());
                    }
                    "file_id" | "symbol_id" => {
                        *entry = serde_json::Value::String("<ID>".into());
                    }
                    _ => normalize_plan_response(entry, repo_paths),
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_plan_response(item, repo_paths);
            }
        }
        serde_json::Value::String(text) => {
            for repo_path in repo_paths {
                if text.contains(repo_path.as_str()) {
                    *text = text.replace(repo_path.as_str(), "<REPO>");
                }
            }
        }
        serde_json::Value::Number(number) if number.is_f64() => {
            *value = serde_json::Value::from(0.0);
        }
        _ => {}
    }
}

#[test]
fn test_mcp_plan_change_snapshot() {
    let temp = std::env::temp_dir().join(format!("kioku-test-plan-{}", uuid::Uuid::new_v4()));
    copy_dir_recursive(&fixture_dir("rust-fixture"), &temp);

    // Init & Index a deterministic fixture repo.
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

    // MCP tools/call plan_change with a fixed task against the fixture.
    let mcp_req = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"plan_change","arguments":{"task":"add a subtract function next to add","limit":5,"format":"json"}}}"#;
    let mut cmd = Command::cargo_bin("ok").unwrap();
    let assert = cmd
        .current_dir(&temp)
        .args(["mcp", "serve", "--repo", "."])
        .write_stdin(mcp_req)
        .assert()
        .success()
        .stdout(predicate::str::contains("structuredContent"));

    let output = assert.get_output();
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let json_lines: Vec<&str> = stdout_str.lines().filter(|l| l.starts_with("{")).collect();
    let last_json = json_lines.last().expect("should output JSON");
    let parsed: serde_json::Value = serde_json::from_str(last_json).unwrap();

    let mut result = parsed
        .get("result")
        .cloned()
        .expect("plan_change should return a JSON-RPC result");

    // Every evidence record id is unique. Each evidence line of a primary result resolves to
    // exactly one record under its derived id; any other ref the result cites resolves to at
    // most one record, and never to a retrieval record of another file.
    let plan = &result["structuredContent"];
    let records = plan["evidence"]
        .as_array()
        .expect("plan evidence is an array");
    let record_ids = records
        .iter()
        .map(|record| record["id"].as_str().expect("evidence id").to_string())
        .collect::<Vec<_>>();
    let unique_ids = record_ids.iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique_ids.len(), record_ids.len(), "{record_ids:?}");
    for context in plan["primary_context"].as_array().unwrap() {
        let path = context["path"].as_str().unwrap();
        let range = match (
            context["line_range"]["start"].as_u64(),
            context["line_range"]["end"].as_u64(),
        ) {
            (Some(start), Some(end)) => format!("{start}-{end}"),
            _ => "unknown".to_string(),
        };
        for index in 0..context["evidence"].as_array().unwrap().len() {
            let derived = format!("search:{path}:{range}:{index}");
            assert_eq!(
                record_ids.iter().filter(|id| **id == derived).count(),
                1,
                "{derived} should resolve to one record"
            );
        }
        // Refs pair with lines: the i-th ref names the fact the i-th line states, and a
        // retrieval ref resolves to the record carrying that very line.
        let lines = context["evidence"].as_array().unwrap();
        let refs = context["evidence_refs"].as_array().unwrap();
        assert_eq!(refs.len(), lines.len(), "{path}: {refs:?} vs {lines:?}");
        let unique_refs = refs
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique_refs.len(), refs.len(), "{path}: {refs:?}");
        for (line, evidence_ref) in lines.iter().zip(refs) {
            let evidence_ref = evidence_ref.as_str().unwrap();
            if !evidence_ref.starts_with("search:") {
                continue;
            }
            let record = records
                .iter()
                .find(|record| record["id"] == evidence_ref)
                .unwrap_or_else(|| panic!("{evidence_ref} resolves to no record"));
            assert_eq!(
                &record["message"], line,
                "{evidence_ref} names another line"
            );
        }
        for evidence_ref in context["evidence_refs"].as_array().unwrap() {
            let evidence_ref = evidence_ref.as_str().unwrap();
            let matching = records
                .iter()
                .filter(|record| record["id"] == evidence_ref)
                .collect::<Vec<_>>();
            assert!(matching.len() <= 1, "{evidence_ref} names several records");
            if let (Some(record), true) = (matching.first(), evidence_ref.starts_with("search:")) {
                assert_eq!(
                    record["file_range"]["path"].as_str(),
                    Some(path),
                    "{evidence_ref} resolved to another file's record"
                );
            }
        }
    }

    // The text content block duplicates structuredContent as pretty-printed JSON;
    // keep only the envelope shape for it.
    let content = result
        .get_mut("content")
        .and_then(serde_json::Value::as_array_mut)
        .expect("result should include a content array");
    assert_eq!(content.len(), 1, "expected a single text content block");
    assert_eq!(content[0]["type"], "text");
    content[0]["text"] = serde_json::Value::String("<STRUCTURED_CONTENT_AS_TEXT>".into());

    // Normalize nondeterministic values. Cover both the tempdir path and its
    // canonicalized form (macOS /var vs /private/var).
    let mut repo_paths = vec![temp.to_string_lossy().into_owned()];
    if let Ok(canonical) = temp.canonicalize() {
        let canonical = canonical.to_string_lossy().into_owned();
        if !repo_paths.contains(&canonical) {
            repo_paths.push(canonical);
        }
    }
    normalize_plan_response(&mut result, &repo_paths);

    let formatted = serde_json::to_string_pretty(&result).unwrap();
    let snapshot_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("snapshots");
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    let snapshot_file = snapshot_dir.join("plan_change.json");

    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(&snapshot_file, formatted).unwrap();
    } else if snapshot_file.exists() {
        let expected = std::fs::read_to_string(&snapshot_file).unwrap();
        assert_eq!(
            expected.trim(),
            formatted.trim(),
            "plan_change.json snapshot mismatch"
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
        .stdout(predicate::str::contains("## Query Syntax"))
        .stdout(predicate::str::contains(
            r"A query is MATCH \<path\> \[WHERE \<filter\>",
        ))
        .stdout(predicate::str::contains("## Query Examples"))
        .stdout(predicate::str::contains("## Unsupported Query Forms"))
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
