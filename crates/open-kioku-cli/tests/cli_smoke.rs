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
    assert!(plan.contains("## Primary Context"));
    assert!(plan.contains("## Agent Tool Calls"));
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
