use std::fs;
use std::process::Command;

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
    assert!(init.contains("initialized Open Kioku repository"));
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

    assert!(output.contains("Demo repo ready"));
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
    assert!(eval.contains("\"ablations\""));
    assert!(eval.contains("\"signal\": \"text_relevance\""));
    assert!(eval.contains("\"top_search_signals\""));
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
