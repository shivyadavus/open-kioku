use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

fn ok() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ok"))
}

fn run(mut command: Command) -> String {
    let output = command.output().expect("command should run");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout should be utf-8")
}

fn run_ok_with_stderr(mut command: Command) -> (String, String) {
    let output = command.output().expect("command should run");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("stdout should be utf-8"),
        String::from_utf8(output.stderr).expect("stderr should be utf-8"),
    )
}

fn run_failure(mut command: Command) -> (String, String) {
    let output = command.output().expect("command should run");
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8(output.stdout).expect("stdout should be utf-8"),
        String::from_utf8(output.stderr).expect("stderr should be utf-8"),
    )
}

fn run_with_stdin(mut command: Command, stdin: &str) -> String {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("command should spawn");
    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(stdin.as_bytes())
        .expect("stdin should write");
    let output = child.wait_with_output().expect("command should finish");
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout should be utf-8")
}

#[test]
fn history_bench_covers_public_api_families() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cases = repo.join("benchmarks/history-cases.json");
    let output = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("history")
            .arg("bench")
            .arg("--cases-file")
            .arg(cases);
        command
    });
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["reviewer_accuracy"], 1.0);
    assert_eq!(report["similar_recall_at_5"], 1.0);
    assert_eq!(report["family_counts"]["similar"], 2);
    assert_eq!(report["family_counts"]["ownership"], 1);
    assert_eq!(report["family_counts"]["reviewers"], 1);
    assert_eq!(report["family_counts"]["churn"], 3);
    assert_eq!(report["family_counts"]["provenance"], 1);
    assert!(report["failures"].as_array().unwrap().is_empty());
}

#[test]
fn architecture_policy_validate_and_print_are_index_independent() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    let policy_dir = repo.join(".open-kioku");
    fs::create_dir_all(&policy_dir).unwrap();
    fs::write(
        policy_dir.join("architecture.toml"),
        include_str!("../../../examples/architecture-policy.toml"),
    )
    .unwrap();

    let validation = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("policy")
            .arg("validate");
        command
    });
    let validation: serde_json::Value = serde_json::from_str(&validation).unwrap();
    assert_eq!(validation["valid"], true);
    assert_eq!(validation["configured"], true);
    assert_eq!(validation["source"], "canonical");
    assert_eq!(validation["policy"]["version"], "v1");

    let validation_markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("policy")
            .arg("validate")
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(validation_markdown.contains("# Architecture Policy Validation"));
    assert!(validation_markdown.contains("- Layers:"));

    let printed = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("policy")
            .arg("print");
        command
    });
    assert!(printed.contains("# source: canonical"));
    assert!(printed.contains("version = \"v1\""));
    assert!(printed.contains("api-must-not-depend-on-storage"));

    let explicit = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("policy")
            .arg("validate")
            .arg("--path")
            .arg(".open-kioku/architecture.toml");
        command
    });
    let explicit: serde_json::Value = serde_json::from_str(&explicit).unwrap();
    assert_eq!(explicit["source"], "explicit");

    let no_policy = run({
        let empty = tempfile::tempdir().unwrap();
        let mut command = ok();
        command
            .arg("--repo")
            .arg(empty.path())
            .arg("architecture")
            .arg("policy")
            .arg("validate");
        command
    });
    assert!(no_policy.contains("Heuristic architecture detection remains active"));

    fs::write(
        policy_dir.join("architecture.toml"),
        include_str!("../../../examples/architecture-policy.toml")
            .replace("severity = \"error\"", "severity = \"urgent\""),
    )
    .unwrap();
    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("policy")
            .arg("validate");
        command
    });
    assert!(stderr.contains("architecture.toml"));
    assert!(stderr.contains("unknown variant `urgent`"));
}

#[test]
fn architecture_policy_check_and_explain_public_api_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join(".open-kioku")).unwrap();
    fs::create_dir_all(repo.join("src/api/internal")).unwrap();
    fs::create_dir_all(repo.join("src/domain")).unwrap();
    fs::write(
        repo.join(".open-kioku/architecture.toml"),
        r#"version = "v1"

[[layers]]
id = "api"
paths = ["src/api/**"]

[[layers]]
id = "domain"
paths = ["src/domain/**"]

[[public_api_rules]]
id = "api-public-boundary"
component = "api"
public_globs = ["src/api/mod.rs"]
internal_globs = ["src/api/internal/**"]
severity = "error"
reason = "domain code must use the api facade"
"#,
    )
    .unwrap();
    fs::write(repo.join("src/lib.rs"), "pub mod api;\npub mod domain;\n").unwrap();
    fs::write(repo.join("src/api/mod.rs"), "pub mod internal;\n").unwrap();
    fs::write(
        repo.join("src/api/internal/mod.rs"),
        "pub struct Session;\n",
    )
    .unwrap();
    fs::write(
        repo.join("src/domain/mod.rs"),
        "use crate::api::internal;\npub fn leak() -> internal::Session { internal::Session }\n",
    )
    .unwrap();

    let _ = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    let check = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("policy")
            .arg("check");
        command
    });
    let check: serde_json::Value = serde_json::from_str(&check).unwrap();
    assert_eq!(check["public_api_violation_count"], 1);
    assert_eq!(check["violations"][0]["rule_id"], "api-public-boundary");

    let plan = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("plan")
            .arg("domain");
        command
    });
    let plan: serde_json::Value = serde_json::from_str(&plan).unwrap();
    assert_eq!(plan["architecture_policy"]["configured"], true);
    assert_eq!(plan["impact"]["architecture_policy"]["configured"], true);
    assert_eq!(
        plan["architecture_policy"]["violations"][0]["rule_id"],
        "api-public-boundary"
    );

    let impact = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("impact")
            .arg("--file")
            .arg("src/domain/mod.rs");
        command
    });
    let impact: serde_json::Value = serde_json::from_str(&impact).unwrap();
    assert_eq!(impact["architecture_policy"]["configured"], true);
    assert_eq!(
        impact["architecture_policy"]["violations"][0]["rule_id"],
        "api-public-boundary"
    );

    let check_markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("policy")
            .arg("check")
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(check_markdown.contains("# Architecture Policy Check"));
    assert!(check_markdown.contains("api-public-boundary"));

    let explain = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("policy")
            .arg("explain")
            .arg("--file")
            .arg("src/api/internal/mod.rs");
        command
    });
    let explain: serde_json::Value = serde_json::from_str(&explain).unwrap();
    assert_eq!(explain["configured"], true);
    assert_eq!(explain["components"][0]["component_id"], "api");
    assert_eq!(explain["violations"][0]["rule_id"], "api-public-boundary");

    let repo_explain = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("architecture")
            .arg("policy")
            .arg("explain");
        command
    });
    let repo_explain: serde_json::Value = serde_json::from_str(&repo_explain).unwrap();
    assert_eq!(repo_explain["query_kind"], "repo");
    assert_eq!(
        repo_explain["violations"][0]["rule_id"],
        "api-public-boundary"
    );

    let explain_markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("architecture")
            .arg("policy")
            .arg("explain")
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(explain_markdown.contains("# Architecture Policy Explanation"));
    assert!(explain_markdown.contains("api-public-boundary"));

    let mcp_validate = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"architecture_policy_validate","arguments":{}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp_validate.trim()).unwrap();
    assert_eq!(response["result"]["structuredContent"]["configured"], true);
    assert_eq!(
        response["result"]["structuredContent"]["source"],
        "canonical"
    );

    let mcp_explain = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"architecture_policy_explain","arguments":{"scope":"repo"}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp_explain.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["query_kind"],
        "repo"
    );
    assert_eq!(
        response["result"]["structuredContent"]["violations"][0]["rule_id"],
        "api-public-boundary"
    );

    let mcp_plan = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"plan_change","arguments":{"task":"domain","limit":5}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp_plan.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["architecture_policy"]["configured"],
        true
    );

    let mcp_impact = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"impact_analysis","arguments":{"path":"src/domain/mod.rs"}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp_impact.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["architecture_policy"]["configured"],
        true
    );
}

#[test]
fn verify_enforces_configured_architecture_policy_without_dependency_flag() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join(".open-kioku")).unwrap();
    fs::create_dir_all(repo.join("src/domain")).unwrap();
    fs::create_dir_all(repo.join("src/api")).unwrap();
    fs::write(
        repo.join(".open-kioku/architecture.toml"),
        r#"version = "v1"

[[layers]]
id = "domain"
paths = ["src/domain/**"]

[[layers]]
id = "api"
paths = ["src/api/**"]

[[dependency_rules]]
id = "domain-must-not-import-api"
from = "domain"
to = "api"
action = "forbid"
severity = "error"
reason = "domain cannot import api"
"#,
    )
    .unwrap();
    fs::write(repo.join("src/lib.rs"), "pub mod api;\npub mod domain;\n").unwrap();
    fs::write(repo.join("src/api/mod.rs"), "pub mod secret;\n").unwrap();
    fs::write(repo.join("src/api/secret.rs"), "pub fn secret() {}\n").unwrap();
    fs::write(repo.join("src/domain/mod.rs"), "pub mod order;\n").unwrap();
    fs::write(
        repo.join("src/domain/order.rs"),
        "pub fn order() -> u32 { 1 }\n",
    )
    .unwrap();

    let _ = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });

    let plan_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("plan")
            .arg("order")
            .arg("--limit")
            .arg("5");
        command
    });
    let plan_path = repo.join("plan.json");
    fs::write(&plan_path, &plan_json).unwrap();

    fs::write(
        repo.join("src/domain/order.rs"),
        "use crate::api::secret;\npub fn order() -> u32 { secret(); 1 }\n",
    )
    .unwrap();

    let (stdout, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/domain/order.rs");
        command
    });
    assert!(stdout.contains("\"verdict\": \"fail\""));
    assert!(stdout.contains("domain-must-not-import-api"));
    assert!(stdout.contains("dependency_deltas"));
    assert!(stderr.contains("change verification failed"));

    let plan_value: serde_json::Value = serde_json::from_str(&plan_json).unwrap();
    let mcp_verify_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": "verify_change",
            "arguments": {
                "plan": plan_value,
                "changed_files": ["src/domain/order.rs"]
            }
        }
    })
    .to_string();
    let mcp_verify = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_verify_req + "\n"),
    );
    let response: serde_json::Value = serde_json::from_str(mcp_verify.trim()).unwrap();
    assert_eq!(response["result"]["structuredContent"]["verdict"], "fail");
    assert!(mcp_verify.contains("domain-must-not-import-api"));
}

#[test]
fn architecture_policy_bench_scores_checked_in_corpus() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = root.join("benchmarks/architecture-policy-fixture");
    let cases = root.join("benchmarks/architecture-policy-cases.json");
    let _ = fs::remove_dir_all(fixture.join(".ok"));

    let report = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("architecture")
            .arg("bench")
            .arg(&fixture)
            .arg("--cases-file")
            .arg(&cases)
            .arg("--min-precision")
            .arg("0.95")
            .arg("--min-recall")
            .arg("0.90");
        command
    });
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["case_count"], 8);
    assert_eq!(report["summary"]["precision"], 1.0);
    assert_eq!(report["summary"]["recall"], 1.0);
    assert!(report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|case| case["id"] == "dependency-forbidden-call" && case["passed"] == true));
    assert!(report["rule_families"]
        .as_array()
        .unwrap()
        .iter()
        .any(|family| family["rule_family"] == "public_api_rule" && family["recall"] == 1.0));
    assert!(report["rule_families"]
        .as_array()
        .unwrap()
        .iter()
        .any(|family| family["rule_family"] == "internal_only_rule" && family["recall"] == 1.0));

    let _ = fs::remove_dir_all(fixture.join(".ok"));
}

#[test]
fn contract_bench_scores_checked_in_corpus() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture = root.join("benchmarks/contract-fixture");
    let cases = root.join("benchmarks/contract-cases.json");

    let report = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("contract-bench")
            .arg(&fixture)
            .arg("--cases-file")
            .arg(&cases)
            .arg("--min-cases")
            .arg("7")
            .arg("--min-verdict-accuracy")
            .arg("0.95")
            .arg("--min-verification-precision")
            .arg("0.95")
            .arg("--min-boundary-precision")
            .arg("0.97")
            .arg("--min-boundary-recall")
            .arg("0.90")
            .arg("--min-toon-reduction")
            .arg("0.35");
        command
    });
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["case_count"], 7);
    assert_eq!(report["summary"]["verdict_accuracy"], 1.0);
    assert_eq!(report["summary"]["verification_precision"], 1.0);
    assert!(report["summary"]["min_toon_reduction"].as_f64().unwrap() >= 0.35);
    assert!(report["failures"].as_array().unwrap().is_empty());
    assert!(report["rule_families"]
        .as_array()
        .unwrap()
        .iter()
        .any(|family| family["rule_family"] == "api_surface_delta"
            && family["verdict_accuracy"] == 1.0));
    assert!(report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .any(|case| case["id"] == "contract-dependency-delta" && case["passed"] == true));
}

#[test]
fn init_index_search_and_doctor_work_together() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub struct Worker;\nimpl Worker { pub fn run(&self) {} }\n",
    )
    .unwrap();

    let init = run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    assert!(init.contains("Open Kioku is ready"));
    assert!(repo.join("ok.toml").exists());

    let index = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    assert!(index.contains("Indexed"));

    let status = run({
        let mut command = ok();
        command.arg("--json").arg("status").arg(repo);
        command
    });
    assert!(status.contains("\"file_count\""));

    let search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("search")
            .arg("Worker");
        command
    });
    assert!(search.contains("src/lib.rs"));

    let doctor = run({
        let mut command = ok();
        command.arg("doctor").arg(repo);
        command
    });
    assert!(doctor.contains("Open Kioku doctor"));
    assert!(doctor.contains("[ok]   repo"));
    assert!(doctor.contains("[ok]   index"));

    let status_markdown_path = repo.join("ok-status.md");
    let status_markdown = run({
        let mut command = ok();
        command
            .arg("status")
            .arg(repo)
            .arg("--markdown")
            .arg("--write")
            .arg(&status_markdown_path);
        command
    });
    assert!(status_markdown.contains("Wrote Open Kioku status snapshot"));
    let written_status = fs::read_to_string(&status_markdown_path).unwrap();
    assert!(written_status.contains("# Open Kioku Status"));
    assert!(written_status.contains("## Readiness Checks"));

    let setup_audit = run({
        let mut command = ok();
        command.arg("setup").arg("audit").arg(repo);
        command
    });
    assert!(setup_audit.contains("Open Kioku setup audit"));
    assert!(setup_audit.contains("MCP clients"));
    assert!(setup_audit.contains("Quality signals"));
    assert!(setup_audit.contains("Advanced providers (optional)"));
    assert!(setup_audit.contains("ok mcp install codex"));

    let setup_markdown = run({
        let mut command = ok();
        command
            .arg("setup")
            .arg("audit")
            .arg(repo)
            .arg("--markdown");
        command
    });
    assert!(setup_markdown.contains("# Open Kioku Setup Audit"));
    assert!(setup_markdown.contains("## MCP Client Matrix"));
    assert!(setup_markdown.contains("## Quality Signals"));
    assert!(setup_markdown.contains("## Advanced Providers"));
    assert!(!setup_markdown.contains("codeql CLI/database not detected"));
    assert!(!setup_markdown.contains("0 BSP descriptor"));
}

fn snapshot_fixture_repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub struct Worker;\nimpl Worker { pub fn run(&self) {} }\n",
    )
    .unwrap();
    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    temp
}

#[test]
fn snapshot_export_import_round_trip_rebuilds_search_and_bootstraps_index() {
    let temp = snapshot_fixture_repo();
    let repo = temp.path();
    let artifact_path = repo.join(".ok/artifacts/index.snapshot.zst");
    let metadata_path = repo.join(".ok/artifacts/index.snapshot.json");
    let gitattributes_path = repo.join(".ok/artifacts/.gitattributes");
    let search_meta = repo.join(".ok/search/tantivy/meta.json");

    let exported = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("snapshot")
            .arg("export")
            .arg("--quality")
            .arg("best");
        command
    });
    let exported: serde_json::Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(exported["ok"], true);
    assert_eq!(exported["quality"], "best");
    assert_eq!(exported["metadata"]["artifact_kind"], "index-snapshot");
    assert_eq!(exported["metadata"]["schema_version"], "1.0.0");
    assert_eq!(exported["metadata"]["compression_level"], 9);
    assert!(exported["metadata"]["file_count"].as_u64().unwrap() >= 1);
    assert!(exported["metadata"]["chunk_count"].as_u64().unwrap() >= 1);
    assert!(artifact_path.exists());
    assert!(metadata_path.exists());
    assert!(gitattributes_path.exists());
    assert!(fs::read_to_string(&gitattributes_path)
        .unwrap()
        .contains("*.snapshot.zst binary -merge"));

    fs::write(repo.join(".ok/memory.sqlite"), b"private memory").unwrap();
    fs::write(repo.join(".ok/context.sqlite"), b"private context").unwrap();
    fs::remove_file(repo.join(".ok/index.sqlite")).unwrap();
    fs::remove_dir_all(repo.join(".ok/search")).unwrap();
    fs::remove_file(repo.join(".ok/memory.sqlite")).unwrap();
    fs::remove_file(repo.join(".ok/context.sqlite")).unwrap();
    fs::write(repo.join(".ok/index.sqlite-wal"), b"stale wal").unwrap();
    fs::write(repo.join(".ok/index.sqlite-shm"), b"stale shm").unwrap();

    let imported = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("snapshot")
            .arg("import");
        command
    });
    let imported: serde_json::Value = serde_json::from_str(&imported).unwrap();
    assert_eq!(imported["ok"], true);
    assert_eq!(imported["imported"], true);
    assert_eq!(imported["rebuilt_search"], true);
    assert!(repo.join(".ok/index.sqlite").exists());
    assert!(search_meta.exists());
    assert!(!repo.join(".ok/index.sqlite-wal").exists());
    assert!(!repo.join(".ok/index.sqlite-shm").exists());
    assert!(!repo.join(".ok/memory.sqlite").exists());
    assert!(!repo.join(".ok/context.sqlite").exists());

    let search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("search")
            .arg("Worker");
        command
    });
    assert!(search.contains("src/lib.rs"));

    let graph_search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("search")
            .arg("Worker")
            .arg("--kind")
            .arg("graph");
        command
    });
    assert!(graph_search.contains("Worker"));

    let doctor = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("snapshot")
            .arg("doctor");
        command
    });
    let doctor: serde_json::Value = serde_json::from_str(&doctor).unwrap();
    assert_eq!(doctor["ok"], true);

    fs::remove_file(repo.join(".ok/index.sqlite")).unwrap();
    fs::remove_dir_all(repo.join(".ok/search")).unwrap();
    let bootstrapped = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("index")
            .arg("--from-snapshot")
            .arg("auto");
        command
    });
    let bootstrapped: serde_json::Value = serde_json::from_str(&bootstrapped).unwrap();
    assert_eq!(bootstrapped["imported"], true);
    assert!(repo.join(".ok/index.sqlite").exists());
    assert!(search_meta.exists());
}

#[test]
fn snapshot_import_rejects_invalid_artifacts_without_replacing_existing_index() {
    let temp = snapshot_fixture_repo();
    let repo = temp.path();
    let artifact_path = repo.join(".ok/artifacts/index.snapshot.zst");
    let metadata_path = repo.join(".ok/artifacts/index.snapshot.json");

    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("snapshot")
            .arg("export")
            .arg("--quality")
            .arg("fast");
        command
    });
    let original_index = fs::read(repo.join(".ok/index.sqlite")).unwrap();
    let original_metadata = fs::read_to_string(&metadata_path).unwrap();
    let original_artifact = fs::read(&artifact_path).unwrap();

    fs::remove_file(&metadata_path).unwrap();
    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("snapshot")
            .arg("import");
        command
    });
    assert!(stderr.contains("snapshot metadata is missing"));
    assert_eq!(
        fs::read(repo.join(".ok/index.sqlite")).unwrap(),
        original_index
    );
    fs::write(&metadata_path, &original_metadata).unwrap();

    fs::write(&artifact_path, b"not a zstd snapshot").unwrap();
    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("snapshot")
            .arg("import");
        command
    });
    assert!(stderr.contains("snapshot compressed size mismatch"));
    assert_eq!(
        fs::read(repo.join(".ok/index.sqlite")).unwrap(),
        original_index
    );
    fs::write(&artifact_path, &original_artifact).unwrap();

    let mut metadata: serde_json::Value = serde_json::from_str(&original_metadata).unwrap();
    metadata["schema_version"] = serde_json::Value::String("9.9.9".into());
    fs::write(
        &metadata_path,
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("snapshot")
            .arg("import");
        command
    });
    assert!(stderr.contains("unsupported snapshot schema version"));
    assert_eq!(
        fs::read(repo.join(".ok/index.sqlite")).unwrap(),
        original_index
    );

    let mut metadata: serde_json::Value = serde_json::from_str(&original_metadata).unwrap();
    metadata["open_kioku_version"] = serde_json::Value::String("0.0.0".into());
    fs::write(
        &metadata_path,
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .unwrap();
    let imported = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("snapshot")
            .arg("import");
        command
    });
    let imported: serde_json::Value = serde_json::from_str(&imported).unwrap();
    assert_eq!(imported["ok"], true);
    assert!(imported["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning
            .as_str()
            .unwrap()
            .contains("snapshot was exported by Open Kioku 0.0.0")));
}

#[test]
fn impact_and_plan_accept_since_changed_ranges() {
    fn git(repo: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn token() -> &'static str {\n    \"old\"\n}\n",
    )
    .unwrap();
    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });
    git(repo, &["init", "--quiet"]);
    git(repo, &["config", "user.email", "cli@example.com"]);
    git(repo, &["config", "user.name", "CLI Test"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    git(repo, &["add", "."]);
    git(repo, &["commit", "--quiet", "-m", "initial"]);
    run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn token() -> &'static str {\n    \"new\"\n}\n",
    )
    .unwrap();

    let impact = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("impact")
            .arg("--since")
            .arg("HEAD");
        command
    });
    let impact: serde_json::Value = serde_json::from_str(&impact).unwrap();
    assert_eq!(impact["since"], "HEAD");
    assert_eq!(impact["changed_files"][0]["new_path"], "src/lib.rs");
    assert_eq!(
        impact["changed_files"][0]["hunks"][0]["new_range"]["start"],
        2
    );

    let plan = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("plan")
            .arg("update token")
            .arg("--since")
            .arg("HEAD")
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(plan.contains("git diff HEAD --unified=0"));
    assert!(plan.contains("src/lib.rs"));
}

#[test]
fn index_mode_is_reported_by_index_and_status_json() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("docs")).unwrap();
    fs::write(repo.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
    fs::write(repo.join("docs/guide.rs"), "pub fn docs_only() {}\n").unwrap();

    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });

    let indexed = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("index")
            .arg(repo)
            .arg("--mode")
            .arg("fast");
        command
    });
    let indexed: serde_json::Value = serde_json::from_str(&indexed).unwrap();
    assert_eq!(indexed["index_mode"], "fast");
    assert!(indexed["phase_reports"].as_array().unwrap().len() >= 2);
    assert_eq!(indexed["quality"]["skip_counts"]["fast_mode"], 1);
    assert!(indexed["quality"]["skipped_paths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path["reason"] == "fast_mode" && path["source"] == "fast_mode"));
    assert!(indexed["quality"]["quality_notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|note| note.as_str().unwrap_or_default().contains("fast mode")));

    let status = run({
        let mut command = ok();
        command.arg("--json").arg("status").arg(repo);
        command
    });
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["index_mode"], "fast");
    assert_eq!(status["quality"]["skip_counts"]["fast_mode"], 1);
    assert!(status["quality"]["skipped_paths"]
        .as_array()
        .unwrap()
        .iter()
        .any(|path| path["reason"] == "fast_mode" && path["source"] == "fast_mode"));

    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("index")
            .arg(repo)
            .arg("--mode")
            .arg("unsupported");
        command
    });
    assert!(stderr.contains("unsupported index mode"));
}

#[test]
fn cross_project_workspace_links_existing_project_indexes() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("fleet");
    let service_a = temp.path().join("service-a");
    let service_b = temp.path().join("service-b");
    let service_c = temp.path().join("service-c");
    let service_missing = temp.path().join("service-missing");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&service_a).unwrap();
    fs::create_dir_all(&service_b).unwrap();
    fs::create_dir_all(&service_c).unwrap();
    fs::create_dir_all(&service_missing).unwrap();

    fs::write(
        service_a.join("index.ts"),
        r#"
export function register(router: { get: Function }, consumer: { subscribe: Function }) {
  router.get("/v1/orders", () => "ok");
  consumer.subscribe("orders.created", () => {});
}
"#,
    )
    .unwrap();
    fs::write(
        service_b.join("index.ts"),
        r#"
export async function callOrders() {
  return fetch("https://service-a.local/v1/orders");
}

export function publishOrder(producer: { send: Function }) {
  producer.send({ topic: "orders.created" });
}
"#,
    )
    .unwrap();
    fs::write(
        service_c.join("index.ts"),
        r#"
export function register(router: { get: Function }) {
  router.get("/v1/orders", () => "alternate");
}
"#,
    )
    .unwrap();

    for repo in [&service_a, &service_b, &service_c] {
        run({
            let mut command = ok();
            command.arg("init").arg(repo);
            command
        });
        run({
            let mut command = ok();
            command.arg("index").arg(repo);
            command
        });
    }

    let service_a_index = service_a.join(".ok/index.sqlite");
    let service_a_modified = fs::metadata(&service_a_index).unwrap().modified().unwrap();

    fs::write(
        workspace.join("ok-workspace.toml"),
        format!(
            r#"[workspace]
projects = [
  {{ name = "service-a", repo = "{}" }},
  {{ name = "service-b", repo = "{}" }},
]
"#,
            service_a.display(),
            service_b.display()
        ),
    )
    .unwrap();

    let linked = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("index")
            .arg("--mode")
            .arg("cross-project")
            .arg("--workspace")
            .arg(&workspace);
        command
    });
    let linked: serde_json::Value = serde_json::from_str(&linked).unwrap();
    assert_eq!(linked["project_count"], 2);
    assert_eq!(linked["link_count"], 2);
    assert!(linked["graph_path"]
        .as_str()
        .unwrap()
        .ends_with("workspace.sqlite"));
    assert!(workspace.join(".ok/workspace.sqlite").exists());
    assert_eq!(
        fs::metadata(&service_a_index).unwrap().modified().unwrap(),
        service_a_modified,
        "cross-project indexing must not mutate project indexes"
    );
    assert!(linked["links"].as_array().unwrap().iter().any(|link| {
        link["source_project"] == "service-b"
            && link["target_project"] == "service-a"
            && link["target"] == "/v1/orders"
            && link["edge_type"] == "CALLS_ENDPOINT"
    }));
    assert!(linked["links"].as_array().unwrap().iter().any(|link| {
        link["source_project"] == "service-b"
            && link["target_project"] == "service-a"
            && link["target"] == "orders.created"
            && link["edge_type"] == "PUBLISHES_EVENT"
    }));

    let fleet = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("architecture")
            .arg("fleet")
            .arg("--workspace")
            .arg(&workspace);
        command
    });
    let fleet: serde_json::Value = serde_json::from_str(&fleet).unwrap();
    assert_eq!(fleet["project_count"], 2);
    assert_eq!(fleet["link_count"], 2);

    fs::write(
        service_a.join("alternate.ts"),
        r#"
export function registerAlternate(router: { get: Function }) {
  router.get("/v1/orders", () => "alternate");
}
"#,
    )
    .unwrap();
    run({
        let mut command = ok();
        command.arg("index").arg(&service_a);
        command
    });
    fs::write(
        workspace.join("ok-workspace.toml"),
        format!(
            r#"[workspace]
projects = [
  {{ name = "service-a", repo = "{}" }},
  {{ name = "service-b", repo = "{}" }},
  {{ name = "service-c", repo = "{}" }},
]
"#,
            service_a.display(),
            service_b.display(),
            service_c.display()
        ),
    )
    .unwrap();
    let ambiguous = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("index")
            .arg("--mode")
            .arg("cross-project")
            .arg("--workspace")
            .arg(&workspace);
        command
    });
    let ambiguous: serde_json::Value = serde_json::from_str(&ambiguous).unwrap();
    let ambiguous_http_links = ambiguous["links"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|link| {
            link["source_project"] == "service-b"
                && link["target_project"] == "service-a"
                && link["target"] == "/v1/orders"
        })
        .collect::<Vec<_>>();
    assert_eq!(ambiguous_http_links.len(), 2);
    assert!(ambiguous_http_links.iter().all(|link| {
        link["confidence"] == "medium"
            && link["ambiguity"].as_array().unwrap().iter().any(|note| {
                note.as_str()
                    .unwrap_or_default()
                    .contains("candidate cross-project targets")
            })
    }));

    fs::write(
        workspace.join("ok-workspace.toml"),
        format!(
            r#"[workspace]
projects = [
  {{ name = "service-a", repo = "{}" }},
]
"#,
            service_a.display()
        ),
    )
    .unwrap();
    let relinked = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("index")
            .arg("--mode")
            .arg("cross-project")
            .arg("--workspace")
            .arg(&workspace);
        command
    });
    let relinked: serde_json::Value = serde_json::from_str(&relinked).unwrap();
    assert_eq!(
        relinked["link_count"], 0,
        "stale workspace edges are removed"
    );

    fs::write(
        workspace.join("ok-workspace.toml"),
        format!(
            r#"[workspace]
projects = [
  {{ name = "service-missing", repo = "{}" }},
]
"#,
            service_missing.display()
        ),
    )
    .unwrap();
    let (_, stderr) = run_failure({
        let mut command = ok();
        command
            .arg("index")
            .arg("--mode")
            .arg("cross-project")
            .arg("--workspace")
            .arg(&workspace);
        command
    });
    assert!(stderr.contains("missing project index"));
}

#[test]
fn mcp_install_prints_client_config() {
    let temp = tempfile::tempdir().unwrap();
    let output = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("claude")
            .arg("--repo")
            .arg(temp.path());
        command
    });

    assert!(output.contains("mcpServers"));
    assert!(output.contains("\"command\": \"ok\""));
    assert!(output.contains("--read-only"));

    let codex = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("codex")
            .arg("--repo")
            .arg(temp.path());
        command
    });
    assert!(codex.contains("[mcp_servers.open-kioku]"));
    assert!(codex.contains("command = \"ok\""));

    let opencode = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("opencode")
            .arg("--repo")
            .arg(temp.path());
        command
    });
    assert!(opencode.contains("\"mcp\""));
    assert!(opencode.contains("\"type\": \"local\""));

    let zed = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("zed")
            .arg("--repo")
            .arg(temp.path());
        command
    });
    assert!(zed.contains("context_servers"));
    assert!(zed.contains("open-kioku"));

    let windsurf = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("windsurf")
            .arg("--repo")
            .arg(temp.path());
        command
    });
    assert!(windsurf.contains("mcpServers"));
    assert!(windsurf.contains("open-kioku"));

    let trae = run({
        let mut command = ok();
        command
            .arg("mcp")
            .arg("install")
            .arg("trae")
            .arg("--repo")
            .arg(temp.path());
        command
    });
    assert!(trae.contains("mcpServers"));
    assert!(trae.contains("open-kioku"));
}

#[test]
fn demo_creates_indexed_sample_repo() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    let output = run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });

    assert!(output.contains("Open Kioku is ready"));
    assert!(repo.join("ok.toml").exists());
    assert!(repo.join(".ok/index.sqlite").exists());

    let search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("search")
            .arg("issue_token");
        command
    });
    assert!(search.contains("src/auth.rs"));

    let graph_search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("search")
            .arg("--kind")
            .arg("graph")
            .arg("issue token")
            .arg("--limit")
            .arg("5");
        command
    });
    assert!(graph_search.contains("graph node identifier match"));
    assert!(graph_search.contains("graph_node_identifier"));
    assert!(graph_search.contains("issue_token"));

    let explained_search = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("search")
            .arg("issue_token")
            .arg("--explain-ranking");
        command
    });
    assert!(explained_search.contains("ranking:"));
    assert!(explained_search.contains("text_relevance"));

    let semantic_status = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("semantic")
            .arg("status");
        command
    });
    assert!(semantic_status.contains("\"state\": \"disabled\""));

    let semantic_index = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("semantic")
            .arg("index");
        command
    });
    assert!(semantic_index.contains("\"state\": \"ready\""));
    assert!(semantic_index.contains("\"vector_count\""));
    assert!(repo.join(".ok/vectors/current/manifest.json").exists());

    let semantic_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("search")
            .arg("--semantic")
            .arg("session token")
            .arg("--limit")
            .arg("5");
        command
    });
    assert!(semantic_json.contains("src/auth.rs"));
    assert!(semantic_json.contains("semantic_similarity"));

    let hybrid = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("search")
            .arg("--hybrid")
            .arg("--explain-ranking")
            .arg("session token");
        command
    });
    assert!(hybrid.contains("semantic_similarity"));

    let mcp_semantic_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 24,
        "method": "tools/call",
        "params": {
            "name": "hybrid_search",
            "arguments": {"query": "session token", "limit": 5}
        }
    })
    .to_string();
    let mcp_semantic = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_semantic_req + "\n"),
    );
    assert!(mcp_semantic.contains("semantic_status"));
    assert!(mcp_semantic.contains("semantic_similarity"));

    let plan = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("plan")
            .arg("token")
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(plan.contains("# Plan: token"));
    assert!(plan.contains("## Confidence"));
    assert!(plan.contains("## Negative Evidence"));
    assert!(plan.contains("## Evidence Provenance"));
    assert!(plan.contains("exact_references"));
    assert!(plan.contains("evidence:"));
    assert!(plan.contains("## Primary Context"));
    assert!(plan.contains("## Agent Tool Calls"));

    let plan_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("plan")
            .arg("token");
        command
    });
    assert!(plan_json.contains("\"confidence_breakdown\""));
    assert!(plan_json.contains("\"overall_score\""));
    assert!(plan_json.contains("\"components\""));
    assert!(plan_json.contains("\"caveats\""));
    assert!(plan_json.contains("\"negative_evidence\""));
    assert!(plan_json.contains("\"evidence_by_section\""));
    assert!(plan_json.contains("\"evidence_refs\""));
    assert!(plan_json.contains("\"allowed_rules\""));
    assert!(plan_json.contains("\"caution_rules\""));
    assert!(plan_json.contains("\"forbidden_rules\""));
    assert!(plan_json.contains("\"expansion_requirements\""));

    let plan_path = repo.join("plan.json");
    fs::write(&plan_path, &plan_json).unwrap();
    let verify_allowed = run({
        let mut command = ok();
        command
            .arg("verify-boundary")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/auth.rs");
        command
    });
    assert!(verify_allowed.contains("Boundary verification passed"));

    let (_boundary_stdout, boundary_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("verify-boundary")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/out_of_scope.rs");
        command
    });
    assert!(boundary_stderr.contains("out of saved plan boundary"));
    assert!(boundary_stderr.contains("boundary expansion requires explicit evidence"));

    let verify_expansion = run({
        let mut command = ok();
        command
            .arg("verify-boundary")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/out_of_scope.rs")
            .arg("--evidence-ref")
            .arg("search:src/out_of_scope.rs:1-2:0");
        command
    });
    assert!(verify_expansion.contains("Boundary verification passed"));

    let (_forbidden_stdout, forbidden_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("verify-boundary")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("vendor/generated.rs")
            .arg("--evidence-ref")
            .arg("manual:vendor");
        command
    });
    assert!(forbidden_stderr.contains("forbidden boundary edit"));

    let verify_diff_path = repo.join("auth.diff");
    fs::write(
        &verify_diff_path,
        "diff --git a/src/auth.rs b/src/auth.rs\n--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -3,0 +4 @@\n+// verifier smoke\n",
    )
    .unwrap();
    let verify_pass = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--diff")
            .arg(&verify_diff_path);
        command
    });
    assert!(verify_pass.contains("\"verdict\": \"pass\""));
    assert!(verify_pass.contains("\"changed_symbols\""));

    let verify_warn = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/out_of_scope.rs")
            .arg("--evidence-ref")
            .arg("search:src/out_of_scope.rs:1-2:0");
        command
    });
    assert!(verify_warn.contains("\"verdict\": \"warn\""));
    assert!(verify_warn.contains("boundary_expansion"));
    assert!(verify_warn.contains("\"traceability\""));

    let (verify_strict_stdout, verify_strict_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/out_of_scope.rs")
            .arg("--evidence-ref")
            .arg("tampered:evidence")
            .arg("--traceability-strict");
        command
    });
    assert!(verify_strict_stdout.contains("\"verdict\": \"fail\""));
    assert!(verify_strict_stdout.contains("unknown_evidence_ref"));
    assert!(verify_strict_stderr.contains("change verification failed"));

    let (verify_fail_stdout, verify_fail_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("verify")
            .arg("--plan")
            .arg(&plan_path)
            .arg("--changed")
            .arg("src/out_of_scope.rs");
        command
    });
    assert!(verify_fail_stdout.contains("\"verdict\": \"fail\""));
    assert!(verify_fail_stdout.contains("out_of_boundary"));
    assert!(verify_fail_stderr.contains("change verification failed"));

    let plan_value: serde_json::Value = serde_json::from_str(&plan_json).unwrap();
    let mcp_verify_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "tools/call",
        "params": {
            "name": "verify_change",
            "arguments": {
                "plan": plan_value,
                "changed_files": ["src/auth.rs"]
            }
        }
    })
    .to_string();
    let mcp_verify = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_verify_req + "\n"),
    );
    assert!(mcp_verify.contains("structuredContent"));
    assert!(mcp_verify.contains("\"verdict\""));
    assert!(mcp_verify.contains("changed_symbols"));

    let (_warn_stdout, warn_stderr) = run_ok_with_stderr({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("plan")
            .arg("token")
            .arg("--verify-evidence")
            .arg("warn");
        command
    });
    assert!(warn_stderr.contains("negative evidence"));

    let (_fail_stdout, fail_stderr) = run_failure({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("plan")
            .arg("token")
            .arg("--verify-evidence")
            .arg("fail");
        command
    });
    assert!(fail_stderr.contains("plan evidence verification failed"));

    let eval = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("eval")
            .arg(&repo)
            .arg("--case")
            .arg("issue_token=src/auth.rs")
            .arg("--limit")
            .arg("5")
            .arg("--no-index");
        command
    });
    assert!(eval.contains("\"baseline\""));
    assert!(eval.contains("\"fusion\""));
    assert!(eval.contains("\"semantic\""));
    assert!(eval.contains("\"ablations\""));
    assert!(eval.contains("\"signal\": \"text_relevance\""));
    assert!(eval.contains("\"signal\": \"semantic_similarity\""));
    assert!(eval.contains("\"top_search_signals\""));

    let workflow_cases = repo.join("workflow-cases.json");
    fs::write(
        &workflow_cases,
        r#"[
          {
            "id": "auth-token",
            "task": "issue_token",
            "expected_primary_context": ["src/auth.rs"],
            "expected_boundary": ["src/auth.rs"],
            "changed_files": ["src/auth.rs"],
            "expected_verdict": "pass",
            "expected_confidence": true
          }
        ]"#,
    )
    .unwrap();
    let workflow_bench = run({
        let mut command = ok();
        command
            .arg("--json")
            .arg("workflow-bench")
            .arg(&repo)
            .arg("--cases-file")
            .arg(&workflow_cases)
            .arg("--limit")
            .arg("5")
            .arg("--min-cases")
            .arg("1")
            .arg("--no-index");
        command
    });
    assert!(workflow_bench.contains("\"workflow\""));
    assert!(workflow_bench.contains("\"deltas\""));
    assert!(workflow_bench.contains("\"context_recall_at_k\""));
    assert!(workflow_bench.contains("\"verification_verdict_accuracy\""));
}

#[test]
fn contract_cli_and_mcp_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    let _ = run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });

    let create_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("contract")
            .arg("create")
            .arg("token")
            .arg("--limit")
            .arg("5");
        command
    });
    let create: serde_json::Value = serde_json::from_str(&create_json).unwrap();
    let contract_id = create["contract_id"].as_str().unwrap().to_string();
    assert_eq!(create["stored"], true);
    assert!(repo
        .join(".ok/contracts")
        .join(format!("{contract_id}.json"))
        .exists());

    let show_markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("contract")
            .arg("show")
            .arg(&contract_id)
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(show_markdown.contains("# Change Contract"));

    let export_toon = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("contract")
            .arg("export")
            .arg(&contract_id)
            .arg("--format")
            .arg("toon");
        command
    });
    assert!(export_toon.contains("type: change_contract"));

    let explain_markdown = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("contract")
            .arg("explain")
            .arg("--id")
            .arg(&contract_id)
            .arg("--format")
            .arg("markdown");
        command
    });
    assert!(explain_markdown.contains("# Contract Explanation"));

    let verify_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("contract")
            .arg("verify")
            .arg("--id")
            .arg(&contract_id)
            .arg("--changed")
            .arg("src/auth.rs");
        command
    });
    let verification: serde_json::Value = serde_json::from_str(&verify_json).unwrap();
    assert_eq!(verification["contract_id"], contract_id);
    assert!(verification["decision"].as_str().is_some());
    assert!(repo
        .join(".ok/contracts")
        .join(format!("{contract_id}.verify.jsonl"))
        .exists());

    let inline_contract = serde_json::to_string(&create["contract"]).unwrap();
    let inline_verify_json = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("contract")
            .arg("verify")
            .arg("--contract-json")
            .arg(&inline_contract)
            .arg("--changed")
            .arg("src/auth.rs");
        command
    });
    let inline_verification: serde_json::Value = serde_json::from_str(&inline_verify_json).unwrap();
    assert_eq!(inline_verification["contract_id"], contract_id);

    let mcp_create = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"create_change_contract","arguments":{"task":"token","limit":5}}}"#,
    );
    let mcp_create: serde_json::Value = serde_json::from_str(mcp_create.trim()).unwrap();
    let mcp_contract_id = mcp_create["result"]["structuredContent"]["contract_id"]
        .as_str()
        .unwrap()
        .to_string();

    let mcp_get_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "get_change_contract",
            "arguments": {
                "contract_id": mcp_contract_id,
                "format": "markdown"
            }
        }
    })
    .to_string();
    let mcp_get = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_get_req + "\n"),
    );
    let mcp_get: serde_json::Value = serde_json::from_str(mcp_get.trim()).unwrap();
    assert!(mcp_get["result"]["structuredContent"]
        .as_str()
        .unwrap()
        .contains("# Change Contract"));

    let mcp_verify_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "verify_change_contract",
            "arguments": {
                "contract_id": mcp_contract_id,
                "changed_files": ["src/auth.rs"]
            }
        }
    })
    .to_string();
    let mcp_verify = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_verify_req + "\n"),
    );
    let mcp_verify: serde_json::Value = serde_json::from_str(mcp_verify.trim()).unwrap();
    let mcp_report = mcp_verify["result"]["structuredContent"].clone();
    assert!(mcp_report["decision"].as_str().is_some());

    let mcp_explain_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "explain_verification",
            "arguments": {
                "verification": mcp_report,
                "format": "markdown"
            }
        }
    })
    .to_string();
    let mcp_explain = run_with_stdin(
        {
            let mut command = ok();
            command.arg("--repo").arg(&repo).arg("mcp").arg("serve");
            command
        },
        &(mcp_explain_req + "\n"),
    );
    let mcp_explain: serde_json::Value = serde_json::from_str(mcp_explain.trim()).unwrap();
    assert!(mcp_explain["result"]["structuredContent"]
        .as_str()
        .unwrap()
        .contains("# Verification Explanation"));
}

#[test]
fn memory_and_compressed_context_are_available() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });

    let remembered = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("memory")
            .arg("remember")
            .arg("RATE-7031 maps issue_token to tests/auth_flow.rs")
            .arg("--source")
            .arg("cli-smoke")
            .arg("--confidence")
            .arg("high");
        command
    });
    assert!(remembered.contains("RATE-7031"));

    let memory = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("memory")
            .arg("search")
            .arg("RATE-7031 issue_token");
        command
    });
    assert!(memory.contains("entity link"));

    let compressed = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("context")
            .arg("token")
            .arg("--compressed");
        command
    });
    assert!(compressed.contains("\"handles\""));
    assert!(compressed.contains("ctx:"));

    let compressed_toon = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("context")
            .arg("token")
            .arg("--compressed")
            .arg("--format")
            .arg("toon");
        command
    });
    assert!(compressed_toon.contains("type: compressed_context_pack"));
    assert!(compressed_toon.contains("handles["));
    assert!(compressed_toon.contains("ctx:"));

    let parsed: serde_json::Value = serde_json::from_str(&compressed).unwrap();
    let handle = parsed["handles"][0]["id"].as_str().unwrap();
    let retrieved = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("retrieve-context")
            .arg(handle);
        command
    });
    assert!(retrieved.contains("token"));

    let plan_toon = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("plan")
            .arg("token")
            .arg("--format")
            .arg("toon");
        command
    });
    assert!(plan_toon.contains("type: plan_report"));
    assert!(plan_toon.contains("primary_context["));
}

#[test]
fn prove_generates_shareable_report_without_source_snippets() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });

    let markdown = run({
        let mut command = ok();
        command
            .arg("prove")
            .arg(&repo)
            .arg("--task")
            .arg("token")
            .arg("--limit")
            .arg("8");
        command
    });
    assert!(markdown.contains("# Open Kioku Proof"));
    assert!(markdown.contains("Average proof score"));
    assert!(markdown.contains("Source snippets included: `false`"));
    assert!(!markdown.contains("pub fn issue_token"));

    let json = run({
        let mut command = ok();
        command
            .arg("prove")
            .arg(&repo)
            .arg("--task")
            .arg("token")
            .arg("--format")
            .arg("json");
        command
    });
    assert!(json.contains("\"generated_by\": \"ok prove\""));
    assert!(json.contains("\"source_snippets_included\": false"));
    assert!(json.contains("\"tasks_scored\": 1"));
}

#[test]
fn index_captures_git_history() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();

    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git command failed: {:?}", args);
    };

    git(&["init", "--quiet"]);
    git(&["config", "user.email", "dev@example.com"]);
    git(&["config", "user.name", "Test User"]);
    git(&["config", "commit.gpgsign", "false"]);

    fs::create_dir_all(repo.join(".github")).unwrap();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join(".github/CODEOWNERS"), "src/** dev@example.com\n").unwrap();
    fs::write(repo.join("src/a.rs"), "pub fn a() {}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "first commit"]);

    std::thread::sleep(std::time::Duration::from_millis(1100));

    fs::write(repo.join("src/b.rs"), "pub fn b() {}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "second commit"]);

    run({
        let mut command = ok();
        command.arg("init").arg(repo);
        command
    });

    run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("memory")
            .arg("remember")
            .arg("src/a.rs maintainer dev@example.com")
            .arg("--source")
            .arg("cli-smoke")
            .arg("--confidence")
            .arg("high");
        command
    });

    let index_output = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    assert!(index_output.contains("Indexed"));

    let store_path = repo.join(".ok/index.sqlite");
    let store = open_kioku_storage_sqlite::SqliteStore::open(store_path).unwrap();
    use open_kioku_storage::{HistoryStore, MetadataStore};
    let commits = store.recent_commits(10).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].summary, "second commit");
    assert_eq!(commits[1].summary, "first commit");

    let summary = store
        .history_for_file(std::path::Path::new("src/a.rs"), 10)
        .unwrap();
    assert_eq!(summary.recent_commits.len(), 1);
    assert_eq!(summary.recent_commits[0].summary, "first commit");

    let file_provenance = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("provenance")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let file_provenance: serde_json::Value = serde_json::from_str(&file_provenance).unwrap();
    assert_eq!(
        file_provenance["first_seen"]["commit"]["summary"],
        "first commit"
    );
    assert_eq!(
        file_provenance["last_touched"]["commit"]["summary"],
        "first commit"
    );

    let file_churn = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("churn")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let file_churn: serde_json::Value = serde_json::from_str(&file_churn).unwrap();
    assert_eq!(file_churn["stats"]["all_time"], 1);
    assert_eq!(file_churn["stats"]["last_90d"], 1);
    assert_eq!(file_churn["confidence"], "exact");

    let similar = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("similar")
            .arg("--task")
            .arg("first commit")
            .arg("--path")
            .arg("src/a.rs")
            .arg("--limit")
            .arg("5");
        command
    });
    let similar: serde_json::Value = serde_json::from_str(&similar).unwrap();
    assert_eq!(
        similar["hits"][0]["change"]["commit"]["summary"],
        "first commit"
    );
    let similar_sources = similar["hits"][0]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|evidence| evidence["source_type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(similar_sources.contains(&"task_text"));
    assert!(similar_sources.contains(&"path"));

    let ownership = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("ownership")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let ownership: serde_json::Value = serde_json::from_str(&ownership).unwrap();
    assert_eq!(ownership["owners"][0]["owner"]["email"], "dev@example.com");
    let owner_sources = ownership["owners"][0]["source_types"].as_array().unwrap();
    assert!(owner_sources.iter().any(|source| source == "codeowners"));
    assert!(owner_sources.iter().any(|source| source == "git_history"));
    assert!(owner_sources.iter().any(|source| source == "repo_memory"));
    assert_eq!(ownership["owners"][0]["confidence"], "exact");

    let reviewers = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("reviewers")
            .arg("--path")
            .arg("src/a.rs");
        command
    });
    let reviewers: serde_json::Value = serde_json::from_str(&reviewers).unwrap();
    assert_eq!(
        reviewers["availability"],
        "inferred_from_ownership_and_authors"
    );
    assert_eq!(
        reviewers["suggestions"][0]["reviewer"]["email"],
        "dev@example.com"
    );
    assert_eq!(reviewers["suggestions"][0]["actual_review_evidence"], false);
    assert_eq!(reviewers["suggestions"][0]["inferred_from_authors"], true);
    let reviewer_sources = reviewers["suggestions"][0]["source_types"]
        .as_array()
        .unwrap();
    assert!(reviewer_sources.iter().any(|source| source == "ownership"));
    assert!(reviewer_sources.iter().any(|source| source == "git_author"));
    assert!(reviewers["uncertainty"]
        .as_array()
        .unwrap()
        .iter()
        .any(|note| note
            .as_str()
            .unwrap()
            .contains("actual PR-review evidence is unavailable")));

    let symbol_provenance = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("provenance")
            .arg("--symbol")
            .arg("a");
        command
    });
    let symbol_provenance: serde_json::Value = serde_json::from_str(&symbol_provenance).unwrap();
    assert_eq!(
        symbol_provenance["recent_touches"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        symbol_provenance["recent_touches"][0]["commit"]["author"]["name"],
        "Test User"
    );
    assert!(!symbol_provenance["uncertainty"]
        .as_array()
        .unwrap()
        .is_empty());

    let symbol_id = store
        .list_symbols(Some("a"), 10, 0)
        .unwrap()
        .into_iter()
        .find(|symbol| symbol.name == "a")
        .unwrap()
        .id;
    let symbol_by_id = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(repo)
            .arg("--json")
            .arg("history")
            .arg("provenance")
            .arg("--symbol")
            .arg(&symbol_id.0);
        command
    });
    let symbol_by_id: serde_json::Value = serde_json::from_str(&symbol_by_id).unwrap();
    assert_eq!(symbol_by_id["symbol_id"], symbol_id.0);

    let mcp = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"history_provenance_lookup","arguments":{"path":"src/a.rs","limit":5}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["first_seen"]["commit"]["summary"],
        "first commit"
    );

    let mcp_churn = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"churn_analysis","arguments":{"path":"src/a.rs"}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp_churn.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["stats"]["all_time"],
        1
    );
    assert_eq!(
        response["result"]["structuredContent"]["confidence"],
        "exact"
    );

    let mcp_similar = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"history_similar_changes","arguments":{"task":"first commit","path":"src/a.rs","limit":5}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp_similar.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["hits"][0]["change"]["commit"]["summary"],
        "first commit"
    );

    let mcp_ownership = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ownership_lookup","arguments":{"path":"src/a.rs"}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp_ownership.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["owners"][0]["owner"]["email"],
        "dev@example.com"
    );
    assert!(
        response["result"]["structuredContent"]["owners"][0]["source_types"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source == "repo_memory")
    );

    let mcp_reviewers = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"reviewer_suggestions","arguments":{"path":"src/a.rs"}}}"#,
    );
    let response: serde_json::Value = serde_json::from_str(mcp_reviewers.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["availability"],
        "inferred_from_ownership_and_authors"
    );
    assert_eq!(
        response["result"]["structuredContent"]["suggestions"][0]["reviewer"]["email"],
        "dev@example.com"
    );
    assert_eq!(
        response["result"]["structuredContent"]["suggestions"][0]["actual_review_evidence"],
        false
    );
    assert_eq!(
        response["result"]["structuredContent"]["suggestions"][0]["inferred_from_authors"],
        true
    );

    let mcp_symbol = run_with_stdin(
        {
            let mut command = ok();
            command.arg("mcp").arg("serve").arg("--repo").arg(repo);
            command
        },
        &format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":"history_provenance_lookup","arguments":{{"symbol":"{}","limit":5}}}}}}"#,
            symbol_id.0
        ),
    );
    let response: serde_json::Value = serde_json::from_str(mcp_symbol.trim()).unwrap();
    assert_eq!(
        response["result"]["structuredContent"]["symbol_id"],
        symbol_id.0
    );
}

#[test]
fn reviewer_benchmark_corpus_passes() {
    let repo = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let output = run({
        let mut command = ok();
        command
            .arg("--repo")
            .arg(&repo)
            .arg("--json")
            .arg("history")
            .arg("reviewers-bench")
            .arg("--min-accuracy")
            .arg("0.80");
        command
    });
    let report: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(report["case_count"], 5);
    assert!(
        report["accuracy"].as_f64().unwrap() >= 0.80,
        "reviewer benchmark report: {report}"
    );
    assert!(report["failures"].as_array().unwrap().is_empty());
}
