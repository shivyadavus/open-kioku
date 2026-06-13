use std::fs;
use std::io::Write;
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
    git(&["config", "user.email", "test@example.com"]);
    git(&["config", "user.name", "Test User"]);
    git(&["config", "commit.gpgsign", "false"]);

    fs::create_dir_all(repo.join("src")).unwrap();
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

    let index_output = run({
        let mut command = ok();
        command.arg("index").arg(repo);
        command
    });
    assert!(index_output.contains("Indexed"));

    let store_path = repo.join(".ok/index.sqlite");
    let store = open_kioku_storage_sqlite::SqliteStore::open(store_path).unwrap();
    use open_kioku_storage::HistoryStore;
    let commits = store.recent_commits(10).unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].summary, "second commit");
    assert_eq!(commits[1].summary, "first commit");

    let summary = store
        .history_for_file(std::path::Path::new("src/a.rs"), 10)
        .unwrap();
    assert_eq!(summary.recent_commits.len(), 1);
    assert_eq!(summary.recent_commits[0].summary, "first commit");
}
