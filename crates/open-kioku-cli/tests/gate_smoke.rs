//! `ok gate` end to end on the sample repository: install the hooks, declare a task, drive
//! the edit hooks the way Claude Code does (JSON on stdin), stop, and finish into a dossier.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn ok() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ok"));
    command.env("RUST_BACKTRACE", "0");
    command
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

fn run_failure(mut command: Command) -> (String, String) {
    let output = command.output().expect("command should run");
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    (
        String::from_utf8(output.stdout).expect("stdout should be utf-8"),
        String::from_utf8(output.stderr).expect("stderr should be utf-8"),
    )
}

fn hook(repo: &Path, event: &str, stdin: &str) -> String {
    let mut child = ok()
        .arg("--repo")
        .arg(repo)
        .args(["gate", "hook", event])
        .env_remove("CLAUDE_PROJECT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hook should spawn");
    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(stdin.as_bytes())
        .expect("stdin should accept input");
    let output = child.wait_with_output().expect("hook should finish");
    assert!(
        output.status.success(),
        "hook failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("stdout should be utf-8")
}

/// An edit tool call from conversation `A`, the one the prompts of these tests come from.
fn edit_input(repo: &Path, tool: &str, relative: &str) -> String {
    edit_input_as(repo, tool, relative, "A")
}

fn edit_input_as(repo: &Path, tool: &str, relative: &str, conversation: &str) -> String {
    serde_json::json!({
        "session_id": conversation,
        "cwd": repo,
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": {"file_path": repo.join(relative), "content": "x"}
    })
    .to_string()
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.email=gate@test", "-c", "user.name=gate"])
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("git should run");
    assert!(status.success(), "git {args:?} failed");
}

fn demo_repo() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path().join("demo");
    run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&repo);
        command
    });
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    (temp, repo)
}

#[test]
fn gate_install_is_idempotent_and_keeps_other_settings() {
    let (_temp, repo) = demo_repo();
    let settings = repo.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{"permissions": {"allow": ["Bash(cargo test)"]}}"#,
    )
    .unwrap();
    let preview = run({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "install", "claude"]);
        c
    });
    assert!(
        preview.contains("nothing written without --apply"),
        "{preview}"
    );
    assert!(
        std::fs::read_to_string(&settings)
            .unwrap()
            .contains("Bash(cargo test)"),
        "preview must not write"
    );
    let applied: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "install", "claude", "--apply"]);
        c
    }))
    .unwrap();
    assert_eq!(applied["added"], 6);
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(written["permissions"]["allow"][0], "Bash(cargo test)");
    assert_eq!(
        written["hooks"]["PreToolUse"][0]["matcher"],
        "Edit|Write|MultiEdit|NotebookEdit|Bash"
    );
    assert_eq!(
        written["hooks"]["Stop"][0]["hooks"][0]["command"],
        "ok gate hook stop"
    );
    let again: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "install", "claude", "--apply"]);
        c
    }))
    .unwrap();
    assert_eq!(again["added"], 0);
}

#[test]
fn gate_enforces_the_boundary_through_the_hooks_and_writes_a_dossier() {
    let (_temp, repo) = demo_repo();
    // A file dirty before the session is not the session's change.
    std::fs::write(repo.join("NOTES.md"), "scratch\n").unwrap();

    let started: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json").arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "enforce",
        ]);
        c
    }))
    .unwrap();
    assert_eq!(started["mode"], "enforce");
    let allowed = started["allowed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(allowed.contains(&"src/auth.rs".to_string()), "{allowed:?}");
    assert!(repo.join(".ok/gate/session.json").exists());
    assert!(repo.join(".ok/gate/plan.json").exists());

    // Starting over an active session is refused without --force.
    let (_, err) = run_failure({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "start", "something else"]);
        c
    });
    assert!(err.contains("already active"), "{err}");

    // Hooks: a non-edit tool is ignored; an allowed edit is silent; an outside edit is denied
    // with the remedy; a secret-like path is denied as forbidden.
    let bash = hook(
        &repo,
        "pre-tool",
        &serde_json::json!({"cwd": repo, "tool_name": "Bash", "tool_input": {"command": "ls"}})
            .to_string(),
    );
    assert!(bash.trim().is_empty(), "{bash}");
    let inside = hook(&repo, "pre-tool", &edit_input(&repo, "Edit", "src/auth.rs"));
    assert!(inside.trim().is_empty(), "{inside}");
    // The edit is recorded once it has happened, by the post-tool hook.
    let after = hook(
        &repo,
        "post-tool",
        &edit_input(&repo, "Edit", "src/auth.rs"),
    );
    assert!(after.trim().is_empty(), "{after}");
    let outside: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "pre-tool",
        &edit_input(&repo, "Write", "src/retry.rs"),
    ))
    .unwrap();
    assert_eq!(outside["hookSpecificOutput"]["permissionDecision"], "deny");
    let reason = outside["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.contains("ok gate expand src/retry.rs"), "{reason}");
    let secret: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "pre-tool",
        &edit_input(&repo, "Write", ".env"),
    ))
    .unwrap();
    assert_eq!(secret["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(secret["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .contains("forbidden"));

    // Widening: refused for a forbidden path, recorded with its reason otherwise.
    let (_, err) = run_failure({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "expand", ".env", "--reason", "need it"]);
        c
    });
    assert!(err.contains("forbidden"), "{err}");
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "expand",
            "src/retry.rs",
            "--reason",
            "the retry helper is a new module",
        ]);
        c
    });
    let check = run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "check", "src/retry.rs"]);
        c
    });
    let check: serde_json::Value = serde_json::from_str(&check).unwrap();
    assert_eq!(check["class"], "expanded");
    assert_eq!(check["action"], "allow");

    // The real change: inside the boundary, in the widened path, and one file created
    // without any hook (an agent that bypassed the harness).
    let mut auth = std::fs::read_to_string(repo.join("src/auth.rs")).unwrap();
    auth.push_str("\n// retry\n");
    std::fs::write(repo.join("src/auth.rs"), auth).unwrap();
    std::fs::write(repo.join("src/retry.rs"), "pub fn retry() {}\n").unwrap();
    std::fs::write(repo.join("BYPASS.md"), "x\n").unwrap();

    // Stop: the bypass fails verification, so the agent is held.
    let stop: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "stop",
        &serde_json::json!({"cwd": repo, "hook_event_name": "Stop", "stop_hook_active": false})
            .to_string(),
    ))
    .unwrap();
    assert_eq!(stop["decision"], "block", "{stop}");
    let reason = stop["reason"].as_str().unwrap();
    assert!(reason.contains("BYPASS.md"), "{reason}");
    assert!(
        !reason.contains("NOTES.md"),
        "pre-existing file must not be judged: {reason}"
    );

    // A stop hook that is already continuing never blocks again.
    let again: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "stop",
        &serde_json::json!({"cwd": repo, "hook_event_name": "Stop", "stop_hook_active": true})
            .to_string(),
    ))
    .unwrap();
    assert!(again.get("decision").is_none(), "{again}");
    assert!(
        again["systemMessage"].as_str().unwrap().contains("FAIL"),
        "a stop that does not block informs the user, never the model's turn: {again}"
    );

    // Remove the bypass; finish passes (with warnings) and ends the session.
    std::fs::remove_file(repo.join("BYPASS.md")).unwrap();
    let finished: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "finish"]);
        c
    }))
    .unwrap();
    let dossier = &finished["dossier"];
    assert_eq!(dossier["schema_version"], 1);
    assert_ne!(dossier["verdict"], "fail", "{dossier}");
    assert_eq!(dossier["trail"]["edits_denied"], 2);
    assert_eq!(dossier["trail"]["edits_allowed"], 1);
    assert_eq!(dossier["boundary"]["expansions"][0]["path"], "src/retry.rs");
    let changed = dossier["change"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        changed,
        vec!["src/auth.rs".to_string(), "src/retry.rs".to_string()]
    );
    assert_eq!(dossier["change"]["preexisting_excluded"][0], "NOTES.md");
    assert!(dossier["verification"]["boundary_violations"]
        .as_array()
        .unwrap()
        .is_empty());
    let markdown_path = PathBuf::from(finished["dossier_markdown"].as_str().unwrap());
    let markdown = std::fs::read_to_string(&markdown_path).unwrap();
    assert!(markdown.starts_with("## "), "{markdown}");
    assert!(markdown.contains("the retry helper is a new module"));
    assert!(
        !markdown.contains("pub fn retry"),
        "no source text in a dossier"
    );
    assert!(
        !repo.join(".ok/gate/session.json").exists(),
        "finish ends the session"
    );

    // With no session the hooks are silent, except the prompt reminder.
    let quiet = hook(
        &repo,
        "pre-tool",
        &edit_input(&repo, "Write", "src/retry.rs"),
    );
    assert!(quiet.trim().is_empty(), "{quiet}");
    // Auto mode with nothing to plan from says nothing at all.
    let prompt = hook(&repo, "prompt", r#"{"prompt":"hi"}"#);
    assert!(prompt.trim().is_empty(), "{prompt}");
}

#[test]
fn gate_warn_mode_allows_outside_edits_and_the_verifier_catches_them() {
    let (_temp, repo) = demo_repo();
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "warn",
        ]);
        c
    });
    let pre = hook(
        &repo,
        "pre-tool",
        &edit_input(&repo, "Write", "src/retry.rs"),
    );
    assert!(pre.trim().is_empty(), "warn mode allows: {pre}");
    let post: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "post-tool",
        &edit_input(&repo, "Write", "src/retry.rs"),
    ))
    .unwrap();
    assert!(post["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("outside the declared boundary"));
    std::fs::write(repo.join("src/retry.rs"), "pub fn retry() {}\n").unwrap();
    let (stdout, stderr) = run_failure({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "finish", "--format", "text"]);
        c
    });
    assert!(stdout.contains("Change Dossier FAIL"), "{stdout}");
    assert!(stderr.contains("FAIL"), "{stderr}");
    let status: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "status"]);
        c
    }))
    .unwrap();
    assert_eq!(status["active"], false);
}

#[test]
fn gate_require_plan_denies_edits_before_a_session_exists() {
    let (_temp, repo) = demo_repo();
    // `ok demo` writes the default ok.toml, which already carries a `[gate]` section.
    let config = repo.join("ok.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let (head, gate) = text.split_once("[gate]\n").expect("a [gate] section");
    let gate = gate
        .replacen("mode = \"warn\"", "mode = \"Enforce\"", 1)
        .replacen("require_plan = false", "require_plan = true", 1);
    assert!(gate.contains("require_plan = true"), "{gate}");
    std::fs::write(&config, format!("{head}[gate]\n{gate}")).unwrap();
    let denied: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "pre-tool",
        &edit_input(&repo, "Edit", "src/auth.rs"),
    ))
    .unwrap();
    assert_eq!(denied["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(denied["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .contains("ok gate start"));
    let started: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json").arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
        ]);
        c
    }))
    .unwrap();
    assert_eq!(
        started["mode"], "enforce",
        "the configured default mode applies"
    );
    let allowed = hook(&repo, "pre-tool", &edit_input(&repo, "Edit", "src/auth.rs"));
    assert!(allowed.trim().is_empty(), "{allowed}");
}

#[test]
fn gate_judges_commits_made_during_the_session_and_paired_renames() {
    let (_temp, repo) = demo_repo();
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "warn",
        ]);
        c
    });
    // An out-of-boundary file written with no hook, then committed before stopping: the
    // session's base revision, not HEAD, is what the change is read against.
    std::fs::write(repo.join("BYPASS.md"), "x\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "mid-session commit"]);
    let stop: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "stop",
        &serde_json::json!({"cwd": repo, "hook_event_name": "Stop", "stop_hook_active": false})
            .to_string(),
    ))
    .unwrap();
    assert_eq!(stop["decision"], "block", "{stop}");
    assert!(
        stop["reason"].as_str().unwrap().contains("BYPASS.md"),
        "{stop}"
    );

    // A rename keeps both paths in the judged change.
    git(&repo, &["mv", "src/auth.rs", "src/authn.rs"]);
    let finished: serde_json::Value = serde_json::from_str(&{
        let output = ok()
            .arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "finish"])
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap()
    })
    .unwrap();
    let changed = finished["dossier"]["change"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(changed.contains(&"src/auth.rs".to_string()), "{changed:?}");
    assert!(changed.contains(&"src/authn.rs".to_string()), "{changed:?}");
    assert!(changed.contains(&"BYPASS.md".to_string()), "{changed:?}");
    let source = finished["dossier"]["change"]["source"].as_str().unwrap();
    assert!(
        source.contains("1 commit(s) made during the session"),
        "{source}"
    );
}

#[test]
fn gate_check_and_expand_accept_absolute_paths_from_the_repository_root() {
    let (_temp, repo) = demo_repo();
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "enforce",
        ]);
        c
    });
    // No --repo: the default `.` is the current directory, and the path is absolute.
    let check: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.current_dir(&repo)
            .arg("--json")
            .args(["gate", "check"])
            .arg(repo.join("src/auth.rs"));
        c
    }))
    .unwrap();
    assert_eq!(check["path"], "src/auth.rs");
    assert_eq!(check["action"], "allow");
    run({
        let mut c = ok();
        c.current_dir(&repo)
            .args(["gate", "expand"])
            .arg(repo.join("src/retry.rs"))
            .args(["--reason", "new module"]);
        c
    });
    let status: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.current_dir(&repo).arg("--json").args(["gate", "status"]);
        c
    }))
    .unwrap();
    assert_eq!(
        status["session"]["boundary"]["expansions"][0]["path"],
        "src/retry.rs"
    );
}

#[test]
fn gate_force_start_keeps_the_old_session_when_planning_fails() {
    let (_temp, repo) = demo_repo();
    run({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "start", "add retry handling to token refresh"]);
        c
    });
    // Break planning: the index directory is gone.
    let ok_dir = repo.join(".ok");
    let saved = _temp.path().join("ok-saved");
    std::fs::rename(&ok_dir, &saved).unwrap();
    std::fs::create_dir_all(&ok_dir).unwrap();
    std::fs::rename(saved.join("gate"), ok_dir.join("gate")).unwrap();
    let (_, err) = run_failure({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "start", "another task", "--force"]);
        c
    });
    assert!(!err.contains("already active"), "{err}");
    let status: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "status"]);
        c
    }))
    .unwrap();
    assert_eq!(
        status["active"], true,
        "the old session survives a failed replacement"
    );
    assert_eq!(
        status["session"]["task"],
        "add retry handling to token refresh"
    );
}

#[test]
fn gate_shell_commands_are_judged_by_named_targets_and_by_what_they_changed() {
    let (_temp, repo) = demo_repo();
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "enforce",
        ]);
        c
    });
    let bash = |command: &str, id: &str| {
        serde_json::json!({
            "session_id": "test", "cwd": repo, "tool_name": "Bash", "tool_use_id": id,
            "tool_input": {"command": command}
        })
        .to_string()
    };
    // A redirection into an outside file is denied before it runs; one into /tmp is not the
    // gate's concern; one into an allowed file passes.
    let denied: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "pre-tool",
        &bash("echo x > src/retry.rs", "t1"),
    ))
    .unwrap();
    assert_eq!(
        denied["hookSpecificOutput"]["permissionDecision"], "deny",
        "{denied}"
    );
    assert!(denied["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .contains("Edit or Write tool"));
    let scratch = hook(
        &repo,
        "pre-tool",
        &bash("cargo test > /tmp/gate-test.log", "t2"),
    );
    assert!(scratch.trim().is_empty(), "{scratch}");
    let inside = hook(&repo, "pre-tool", &bash("echo '// x' >> src/auth.rs", "t3"));
    assert!(inside.trim().is_empty(), "{inside}");

    // A command the hook cannot read (a heredoc) writes an outside file: the pre-tool hook
    // snapshots, the post-tool hook sees the new file and tells the agent.
    let pre = hook(
        &repo,
        "pre-tool",
        &bash(
            "python3 - <<'EOF'\nopen('src/retry.rs','w').write('x')\nEOF",
            "t4",
        ),
    );
    assert!(pre.trim().is_empty(), "{pre}");
    std::fs::write(repo.join("src/retry.rs"), "pub fn retry() {}\n").unwrap();
    let post: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "post-tool",
        &bash(
            "python3 - <<'EOF'\nopen('src/retry.rs','w').write('x')\nEOF",
            "t4",
        ),
    ))
    .unwrap();
    let text = post["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("src/retry.rs") && text.contains("outside"),
        "{text}"
    );
    // The trail recorded the shell write like any edit.
    let status: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "status"]);
        c
    }))
    .unwrap();
    assert_eq!(status["edits_denied"], 1, "{status}");
    assert_eq!(
        status["edits_warned"], 1,
        "a shell write that already landed is recorded as warned, not denied: {status}"
    );
}

fn hook_status(repo: &Path, event: &str, stdin: &str) -> (Option<i32>, String, String) {
    let mut child = ok()
        .arg("--repo")
        .arg(repo)
        .args(["gate", "hook", event])
        .env_remove("CLAUDE_PROJECT_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hook should spawn");
    child
        .stdin
        .take()
        .expect("stdin should be piped")
        .write_all(stdin.as_bytes())
        .expect("stdin should accept input");
    let output = child.wait_with_output().expect("hook should finish");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn gate_edit_hook_fails_closed_and_require_plan_covers_shell_writes() {
    let (_temp, repo) = demo_repo();
    // Malformed input: the edit hook refuses (exit 2) rather than letting the edit through;
    // an informing hook stays quiet.
    let (code, _, err) = hook_status(&repo, "pre-tool", "{not json");
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("refused"), "{err}");
    let (code, _, _) = hook_status(&repo, "prompt", "{not json");
    assert_eq!(code, Some(0), "an informing hook never fails on bad input");

    // A torn session file: refused, with the way out named.
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "enforce",
        ]);
        c
    });
    std::fs::write(
        repo.join(".ok/gate/session.json"),
        "{\"schema_version\": 1, \"id\": ",
    )
    .unwrap();
    let (code, _, err) = hook_status(&repo, "pre-tool", &edit_input(&repo, "Edit", "src/auth.rs"));
    assert_eq!(code, Some(2), "{err}");
    assert!(err.contains("ok gate abandon"), "{err}");
    let cleared = run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args(["gate", "abandon"]);
        c
    });
    assert!(cleared.contains("removed"), "{cleared}");
    let quiet = hook(&repo, "pre-tool", &edit_input(&repo, "Edit", "src/auth.rs"));
    assert!(
        quiet.trim().is_empty(),
        "after abandon the hook is quiet again: {quiet}"
    );

    // require_plan with no session: an Edit and a shell write into the repository are both
    // denied; a shell write to /tmp is not.
    let config = repo.join("ok.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let (head, gate) = text.split_once("[gate]\n").expect("a [gate] section");
    let gate = gate.replacen("require_plan = false", "require_plan = true", 1);
    std::fs::write(&config, format!("{head}[gate]\n{gate}")).unwrap();
    let bash = |command: &str| {
        serde_json::json!({"cwd": repo, "tool_name": "Bash", "tool_use_id": "t", "tool_input": {"command": command}})
            .to_string()
    };
    let denied: serde_json::Value =
        serde_json::from_str(&hook(&repo, "pre-tool", &bash("echo x > src/auth.rs"))).unwrap();
    assert_eq!(
        denied["hookSpecificOutput"]["permissionDecision"], "deny",
        "{denied}"
    );
    let scratch = hook(&repo, "pre-tool", &bash("echo x > /tmp/gate-scratch.log"));
    assert!(scratch.trim().is_empty(), "{scratch}");
    let listing = hook(&repo, "pre-tool", &bash("ls -la"));
    assert!(listing.trim().is_empty(), "{listing}");
}

#[test]
fn gate_stop_hook_reuses_the_last_verification_until_something_changes() {
    let (_temp, repo) = demo_repo();
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "warn",
        ]);
        c
    });
    std::fs::write(repo.join("BYPASS.md"), "x\n").unwrap();
    let stop = |active: bool| {
        serde_json::json!({"cwd": repo, "hook_event_name": "Stop", "stop_hook_active": active})
            .to_string()
    };
    let first: serde_json::Value =
        serde_json::from_str(&hook(&repo, "stop", &stop(false))).unwrap();
    assert_eq!(first["decision"], "block");
    let events_after_first = std::fs::read_to_string(repo.join(".ok/gate/events.jsonl")).unwrap();
    assert_eq!(events_after_first.matches("verify_run").count(), 1);
    // Nothing changed: the second stop answers from the first verification, no new VerifyRun.
    let second: serde_json::Value =
        serde_json::from_str(&hook(&repo, "stop", &stop(false))).unwrap();
    assert_eq!(second["decision"], "block");
    assert!(
        second["reason"].as_str().unwrap().contains("BYPASS.md"),
        "the cached verdict keeps its violations: {second}"
    );
    let events_after_second = std::fs::read_to_string(repo.join(".ok/gate/events.jsonl")).unwrap();
    assert_eq!(
        events_after_second.matches("verify_run").count(),
        1,
        "{events_after_second}"
    );
    // The bypass is removed: the tree changed, so verification runs again and passes.
    std::fs::remove_file(repo.join("BYPASS.md")).unwrap();
    let third: serde_json::Value =
        serde_json::from_str(&hook(&repo, "stop", &stop(false))).unwrap();
    assert!(third.get("decision").is_none(), "{third}");
    assert!(
        third["systemMessage"]
            .as_str()
            .unwrap()
            .contains("Dossier:"),
        "{third}"
    );
    let events_after_third = std::fs::read_to_string(repo.join(".ok/gate/events.jsonl")).unwrap();
    assert_eq!(events_after_third.matches("verify_run").count(), 2);
}

#[test]
fn gate_works_in_a_repository_with_no_commits_yet() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    run({
        let mut c = ok();
        c.arg("demo").arg("--path").arg(&repo);
        c
    });
    git(&repo, &["init", "-q"]);
    let started: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json").arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "warn",
        ]);
        c
    }))
    .unwrap();
    assert_eq!(
        started["base_rev"], "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        "the empty tree is the base when there is no HEAD"
    );
    // Every file is untracked and pre-existing; a commit made during the session and a new
    // file are the change.
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "first"]);
    std::fs::write(repo.join("BYPASS.md"), "x\n").unwrap();
    let output = ok()
        .arg("--json")
        .arg("--repo")
        .arg(&repo)
        .args(["gate", "finish"])
        .output()
        .unwrap();
    let finished: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    let dossier = &finished["dossier"];
    assert_eq!(
        dossier["repo"]["base_rev"],
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    );
    let changed = dossier["change"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(changed.contains(&"BYPASS.md".to_string()), "{changed:?}");
    assert!(
        !dossier["caveats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().contains("not a git repository")),
        "{dossier}"
    );
}

#[test]
fn gate_shell_writes_to_already_dirty_files_are_seen_and_warn_mode_names_outside_targets() {
    let (_temp, repo) = demo_repo();
    // Dirty before the session and outside the boundary: a shell write to it during the
    // session must still count as the session's change.
    std::fs::write(repo.join("src/other.rs"), "pub fn other() {}\n").unwrap();
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "warn",
        ]);
        c
    });
    let bash = |command: &str, id: &str| {
        serde_json::json!({"cwd": repo, "tool_name": "Bash", "tool_use_id": id, "tool_input": {"command": command}})
            .to_string()
    };
    // A named outside target in warn mode is allowed, and the agent is told after the fact.
    let pre = hook(&repo, "pre-tool", &bash("echo x > src/retry.rs", "n1"));
    assert!(pre.trim().is_empty(), "{pre}");
    std::fs::write(repo.join("src/retry.rs"), "x\n").unwrap();
    let post: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "post-tool",
        &bash("echo x > src/retry.rs", "n1"),
    ))
    .unwrap();
    let text = post["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("src/retry.rs") && text.contains("ok gate expand"),
        "{text}"
    );
    // An unnamed write to the already-dirty file: its stamp changed, so it is recorded.
    let heredoc = "python3 - <<'EOF'\nopen('src/other.rs','a').write('x')\nEOF";
    let pre = hook(&repo, "pre-tool", &bash(heredoc, "n2"));
    assert!(pre.trim().is_empty(), "{pre}");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(repo.join("src/other.rs"), "pub fn other() {}\n// touched\n").unwrap();
    let post: serde_json::Value =
        serde_json::from_str(&hook(&repo, "post-tool", &bash(heredoc, "n2"))).unwrap();
    assert!(post["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("src/other.rs"));
    let output = ok()
        .arg("--json")
        .arg("--repo")
        .arg(&repo)
        .args(["gate", "finish"])
        .output()
        .unwrap();
    let finished: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    let changed = finished["dossier"]["change"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(changed.contains(&"src/other.rs".to_string()), "{changed:?}");
    // The field is omitted from JSON when empty.
    assert!(finished["dossier"]["change"]["preexisting_excluded"]
        .as_array()
        .is_none_or(|a| a.is_empty()));
}

#[test]
fn gate_force_start_keeps_the_replaced_sessions_edits_as_the_change() {
    let (_temp, repo) = demo_repo();
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "warn",
        ]);
        c
    });
    hook(
        &repo,
        "pre-tool",
        &edit_input(&repo, "Write", "src/retry.rs"),
    );
    std::fs::write(repo.join("src/retry.rs"), "pub fn retry() {}\n").unwrap();
    hook(
        &repo,
        "post-tool",
        &edit_input(&repo, "Write", "src/retry.rs"),
    );
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "refined task",
            "--mode",
            "warn",
            "--force",
        ]);
        c
    });
    assert!(repo.join(".ok/gate").read_dir().unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("events-")));
    let (stdout, _) = run_failure({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "finish"]);
        c
    });
    let finished: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let changed = finished["dossier"]["change"]["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(changed, vec!["src/retry.rs".to_string()], "{finished}");
    assert_eq!(finished["dossier"]["verdict"], "fail");
}

#[test]
fn gate_runs_outside_git_from_the_hooks_trail_and_finds_the_session_from_a_subdirectory() {
    // Not a git repository: the change is what the hooks reported.
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("demo");
    run({
        let mut c = ok();
        c.arg("demo").arg("--path").arg(&repo);
        c
    });
    let started: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json").arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "warn",
        ]);
        c
    }))
    .unwrap();
    assert!(started["base_rev"].is_null(), "{started}");
    hook(&repo, "pre-tool", &edit_input(&repo, "Edit", "src/auth.rs"));
    let mut auth = std::fs::read_to_string(repo.join("src/auth.rs")).unwrap();
    auth.push_str("// x\n");
    std::fs::write(repo.join("src/auth.rs"), auth).unwrap();
    hook(
        &repo,
        "post-tool",
        &edit_input(&repo, "Edit", "src/auth.rs"),
    );
    // An edit the hook allowed but that never happened is not in the trail.
    hook(&repo, "pre-tool", &edit_input(&repo, "Edit", "src/lib.rs"));
    let finished: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "finish"]);
        c
    }))
    .unwrap();
    let dossier = &finished["dossier"];
    assert_eq!(
        dossier["change"]["changed_files"],
        serde_json::json!(["src/auth.rs"])
    );
    assert_eq!(dossier["trail"]["edits_allowed"], 1, "{dossier}");
    assert!(dossier["caveats"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c.as_str().unwrap().contains("not a git repository")));

    // In a git repository, `ok gate expand` from a subdirectory finds the session and resolves
    // the path against the caller's directory, as the hook's denial message assumes.
    let (_temp, repo) = demo_repo();
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "enforce",
        ]);
        c
    });
    run({
        let mut c = ok();
        c.current_dir(repo.join("src")).args([
            "gate",
            "expand",
            "src/retry.rs",
            "--reason",
            "new module",
        ]);
        c
    });
    let status: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.current_dir(repo.join("src"))
            .arg("--json")
            .args(["gate", "status"]);
        c
    }))
    .unwrap();
    assert_eq!(status["active"], true);
    assert_eq!(
        status["session"]["boundary"]["expansions"][0]["path"],
        "src/retry.rs"
    );
}

#[test]
fn gate_reads_a_relative_path_from_the_current_directory_only_when_it_exists_there() {
    let (_temp, repo) = demo_repo();
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "enforce",
        ]);
        c
    });
    std::fs::write(repo.join("src/local.rs"), "").unwrap();
    run({
        let mut c = ok();
        c.current_dir(repo.join("src")).args([
            "gate",
            "expand",
            "local.rs",
            "--reason",
            "sibling module",
        ]);
        c
    });
    let status: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "status"]);
        c
    }))
    .unwrap();
    assert_eq!(
        status["session"]["boundary"]["expansions"][0]["path"],
        "src/local.rs"
    );
}

fn wait_until(what: &str, timeout: std::time::Duration, mut check: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn gate_auto_mode_plans_from_the_prompt_and_lasts_for_the_conversation() {
    let (_temp, repo) = demo_repo();
    let prompt = |text: &str, conversation: &str| {
        hook(
            &repo,
            "prompt",
            &serde_json::json!({"prompt": text, "session_id": conversation, "cwd": repo})
                .to_string(),
        )
    };
    let planner_done = || {
        wait_until(
            "the planner to finish",
            std::time::Duration::from_secs(60),
            || !repo.join(".ok/gate/starting.json").exists(),
        )
    };
    let status = || -> serde_json::Value {
        serde_json::from_str(&run({
            let mut c = ok();
            c.arg("--json")
                .arg("--repo")
                .arg(&repo)
                .args(["gate", "status"]);
            c
        }))
        .unwrap()
    };
    let stop = |conversation: &str| {
        hook(
            &repo,
            "stop",
            &serde_json::json!({"cwd": repo, "session_id": conversation, "stop_hook_active": false})
                .to_string(),
        )
    };

    // A slash command is not a task: nothing is said, nothing starts.
    assert!(prompt("/help", "A").trim().is_empty());
    assert!(!repo.join(".ok/gate/starting.json").exists());

    // A task starts a session in the background. A prompt that begins with dashes is a task
    // like any other: it never reaches a command line.
    let started: serde_json::Value =
        serde_json::from_str(&prompt("--update the auth flow test for issue_token", "A")).unwrap();
    assert!(started["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("planning an edit boundary"));
    planner_done();
    let first = status();
    assert_eq!(first["session"]["auto"], true, "{first}");
    assert_eq!(
        first["session"]["advisory"], true,
        "warn mode: nobody declared this"
    );
    assert_eq!(first["session"]["harness_session"], "A");
    assert_eq!(
        first["session"]["task"],
        "--update the auth flow test for issue_token"
    );
    let session_id = first["session"]["id"].as_str().unwrap().to_string();
    assert!(first["session"]["boundary"]["expansions"]
        .as_array()
        .is_none_or(|a| a.is_empty()));

    // A follow-up prompt of the same conversation widens the boundary; it does not replace
    // the session.
    let follow: serde_json::Value =
        serde_json::from_str(&prompt("change issue_token to take a ttl", "A")).unwrap();
    assert!(follow["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("being added to the session's boundary"));
    planner_done();
    let widened = status();
    assert_eq!(widened["session"]["id"], session_id.as_str());
    let expansion = &widened["session"]["boundary"]["expansions"][0];
    assert_eq!(expansion["path"], "README.md", "{widened}");
    assert!(expansion["reason"]
        .as_str()
        .unwrap()
        .contains("follow-up prompt: \"change issue_token to take a ttl\""));

    // An edit inside the boundary and one outside it. The session is advisory, so the stop
    // reports the failing verdict and holds nothing; the session stays for the conversation.
    hook(&repo, "pre-tool", &edit_input(&repo, "Edit", "src/auth.rs"));
    let mut auth = std::fs::read_to_string(repo.join("src/auth.rs")).unwrap();
    auth.push_str("// ttl\n");
    std::fs::write(repo.join("src/auth.rs"), auth).unwrap();
    hook(
        &repo,
        "post-tool",
        &edit_input(&repo, "Edit", "src/auth.rs"),
    );
    let outside = hook(
        &repo,
        "pre-tool",
        &edit_input(&repo, "Write", "docs/ttl.md"),
    );
    assert!(outside.trim().is_empty(), "advisory: allowed: {outside}");
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::write(repo.join("docs/ttl.md"), "ttl\n").unwrap();
    let told: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "post-tool",
        &edit_input(&repo, "Write", "docs/ttl.md"),
    ))
    .unwrap();
    assert!(told["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("advisory"));
    let stopped: serde_json::Value = serde_json::from_str(&stop("A")).unwrap();
    assert!(
        stopped.get("decision").is_none(),
        "advisory never holds: {stopped}"
    );
    let said = stopped["systemMessage"].as_str().unwrap();
    assert!(
        said.contains("WARN") && said.contains("1 outside the planned boundary (advisory)"),
        "{said}"
    );
    assert_eq!(status()["session"]["id"], session_id.as_str());

    // Another conversation's first prompt closes this session, dossier kept, and starts its own.
    prompt("rename login in src/auth.rs", "B");
    wait_until(
        "the replacement session",
        std::time::Duration::from_secs(60),
        || {
            !repo.join(".ok/gate/starting.json").exists()
                && status()["session"]["harness_session"] == "B"
        },
    );
    let dossier = std::fs::read_to_string(
        repo.join(".ok/gate/dossiers")
            .join(format!("{session_id}.md")),
    )
    .unwrap();
    assert!(dossier.contains("advisory session"), "{dossier}");
    assert!(dossier.contains("docs/ttl.md"));
    // The stop had verified exactly this state: closing the session did not verify it again.
    let trail = std::fs::read_to_string(
        repo.join(".ok/gate")
            .join(format!("events-{session_id}.jsonl")),
    )
    .unwrap();
    assert_eq!(trail.matches("\"verify_run\"").count(), 1, "{trail}");
    // What the first conversation left uncommitted predates the second.
    let second = status();
    let preexisting = second["session"]["preexisting_changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(
        preexisting.contains(&"docs/ttl.md".to_string()),
        "{preexisting:?}"
    );

    // Nothing edited: the stop says nothing and writes nothing; the end of the conversation
    // ends the session.
    assert!(stop("B").trim().is_empty());
    let dossiers_before = std::fs::read_dir(repo.join(".ok/gate/dossiers"))
        .unwrap()
        .count();
    hook(
        &repo,
        "session-end",
        &serde_json::json!({"cwd": repo, "session_id": "B"}).to_string(),
    );
    assert_eq!(status()["active"], false);
    assert_eq!(
        std::fs::read_dir(repo.join(".ok/gate/dossiers"))
            .unwrap()
            .count(),
        dossiers_before,
        "a session that changed nothing leaves no dossier"
    );
}

#[test]
fn gate_edit_hook_waits_for_a_session_being_planned() {
    let (_temp, repo) = demo_repo();
    let config = repo.join("ok.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let (head, gate) = text.split_once("[gate]\n").expect("a [gate] section");
    let gate = gate.replacen("mode = \"warn\"", "mode = \"enforce\"", 1);
    std::fs::write(&config, format!("{head}[gate]\n{gate}")).unwrap();
    hook(
        &repo,
        "prompt",
        r#"{"prompt":"update the auth flow test for issue_token"}"#,
    );
    // Immediately: no session yet, a fresh marker. The edit hook waits for the session and
    // then judges the edit against it.
    let outside: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "pre-tool",
        &edit_input(&repo, "Write", "src/retry.rs"),
    ))
    .unwrap();
    assert_eq!(
        outside["hookSpecificOutput"]["permissionDecision"], "deny",
        "{outside}"
    );
    assert!(repo.join(".ok/gate/session.json").exists());
    // The planner is done before the repository is deleted under it.
    wait_until(
        "the planner to finish",
        std::time::Duration::from_secs(60),
        || !repo.join(".ok/gate/starting.json").exists(),
    );
}

#[test]
fn gate_session_start_indexes_a_repository_that_has_no_index() {
    let (_temp, repo) = demo_repo();
    // A repository that neither tracks nor ignores `.ok/`, with the marker of an indexer that
    // was killed: the marker is not believed, and indexing starts.
    git(&repo, &["rm", "-r", "-q", "--cached", ".ok"]);
    git(&repo, &["commit", "-qm", "untrack the index"]);
    std::fs::remove_dir_all(repo.join(".ok")).unwrap();
    std::fs::create_dir_all(repo.join(".ok/gate")).unwrap();
    std::fs::write(
        repo.join(".ok/gate/indexing.json"),
        serde_json::json!({"at": utc_now(), "nonce": "i1", "pid": dead_pid()}).to_string(),
    )
    .unwrap();
    let start: serde_json::Value =
        serde_json::from_str(&hook(&repo, "session-start", r#"{"source":"startup"}"#)).unwrap();
    assert!(start["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("running in the background"));
    wait_until("the index", std::time::Duration::from_secs(90), || {
        ok().arg("--repo")
            .arg(&repo)
            .args(["status", "--exit-code"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    });
    // The next prompt finds the index and plans.
    let prompt: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "prompt",
        r#"{"prompt":"add retry handling to token refresh in the auth module"}"#,
    ))
    .unwrap();
    assert!(prompt["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("planning an edit boundary"));
    assert!(!repo.join(".ok/gate/indexing.json").exists());
    wait_until(
        "the planner to finish",
        std::time::Duration::from_secs(60),
        || !repo.join(".ok/gate/starting.json").exists(),
    );
    // What the hooks created unasked (the index, the prompt, the plan) is not left for
    // `git add -A` to pick up.
    let porcelain = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .output()
        .unwrap();
    let porcelain = String::from_utf8_lossy(&porcelain.stdout).to_string();
    assert!(!porcelain.contains(".ok"), "{porcelain}");
}

#[test]
fn gate_does_not_index_unasked_a_repository_larger_than_the_limit_tracked_or_not() {
    let (_temp, repo) = demo_repo();
    std::fs::remove_dir_all(repo.join(".ok")).unwrap();
    // One more file than the limit, none of them tracked: the indexer would walk them all.
    std::fs::create_dir_all(repo.join("bulk")).unwrap();
    for i in 0..20_001 {
        std::fs::write(repo.join(format!("bulk/f{i}.txt")), "").unwrap();
    }
    let start: serde_json::Value =
        serde_json::from_str(&hook(&repo, "session-start", r#"{"source":"startup"}"#)).unwrap();
    assert!(
        start["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains("more than the hooks index unasked"),
        "{start}"
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!repo.join(".ok/gate/indexing.json").exists());
    assert!(!repo.join(".ok/index.sqlite").exists());
}

#[test]
fn gate_hooks_judge_a_conversation_only_against_its_own_session() {
    let (_temp, repo) = demo_repo();
    set_gate_mode(&repo, "enforce");
    let as_conversation = |event: &str, conversation: &str| {
        hook(
            &repo,
            event,
            &serde_json::json!({"cwd": repo, "session_id": conversation, "stop_hook_active": false})
                .to_string(),
        )
    };
    hook(
        &repo,
        "prompt",
        &serde_json::json!({"prompt": "rename login in src/auth.rs", "session_id": "A", "cwd": repo})
            .to_string(),
    );
    planner_done(&repo);
    assert_eq!(gate_status(&repo)["session"]["harness_session"], "A");

    // Conversation B sent no task. A's session is not B's: B's edit is not judged against
    // A's task, and B's stop is not held for it.
    let edit = hook(
        &repo,
        "pre-tool",
        &edit_input_as(&repo, "Write", "docs/b.md", "B"),
    );
    assert!(edit.trim().is_empty(), "{edit}");
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::write(repo.join("docs/b.md"), "b\n").unwrap();
    let after = hook(
        &repo,
        "post-tool",
        &edit_input_as(&repo, "Write", "docs/b.md", "B"),
    );
    assert!(after.trim().is_empty(), "{after}");
    let stop = as_conversation("stop", "B");
    assert!(stop.trim().is_empty(), "{stop}");
    std::fs::remove_file(repo.join("docs/b.md")).unwrap();

    // A is still held to its own boundary.
    let denied: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "pre-tool",
        &edit_input(&repo, "Write", "docs/a.md"),
    ))
    .unwrap();
    assert_eq!(
        denied["hookSpecificOutput"]["permissionDecision"], "deny",
        "{denied}"
    );

    // B's stop and B's tool calls leave a plan being built for A alone, and do not wait
    // for it.
    write_marker_json(
        &repo,
        serde_json::json!({
            "at": utc_now(), "detail": "also update the auth flow test", "nonce": "nA",
            "harness_session": "A", "pid": std::process::id(),
        }),
    );
    let started = std::time::Instant::now();
    assert!(as_conversation("stop", "B").trim().is_empty());
    hook(
        &repo,
        "pre-tool",
        &edit_input_as(&repo, "Write", "docs/b.md", "B"),
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(8));
    assert!(repo.join(".ok/gate/starting.json").exists());
    // The end of B's conversation ends neither A's plan nor A's session.
    as_conversation("session-end", "B");
    assert!(repo.join(".ok/gate/starting.json").exists());
    assert_eq!(gate_status(&repo)["session"]["harness_session"], "A");
    std::fs::remove_file(repo.join(".ok/gate/starting.json")).unwrap();
}

#[test]
fn gate_starts_no_session_from_a_prompt_its_plan_has_low_confidence_in() {
    let (_temp, repo) = demo_repo();
    set_gate_mode(&repo, "enforce");
    let prompt = |text: &str| {
        hook(
            &repo,
            "prompt",
            &serde_json::json!({"prompt": text, "session_id": "A", "cwd": repo}).to_string(),
        )
    };
    // A greeting plans with Low confidence. It is not a task, and nothing is held to it.
    prompt("hello, what does this project do?");
    planner_done(&repo);
    assert_eq!(gate_status(&repo)["active"], false);
    let log = std::fs::read_to_string(repo.join(".ok/gate/start.log")).unwrap();
    assert!(log.contains("Low confidence"), "{log}");
    let edit = hook(&repo, "pre-tool", &edit_input(&repo, "Edit", "src/auth.rs"));
    assert!(edit.trim().is_empty(), "{edit}");

    // The task that follows is planned on its own and becomes the session's task.
    prompt("rename login in src/auth.rs");
    planner_done(&repo);
    let status = gate_status(&repo);
    assert_eq!(status["session"]["task"], "rename login in src/auth.rs");
}

#[test]
fn gate_stop_waits_for_the_plan_so_the_turns_edits_are_judged() {
    let (_temp, repo) = demo_repo();
    // The prompt arrived (its marker holds the baseline), the turn created a file, and the
    // turn ends while the plan is still being built.
    write_marker_json(
        &repo,
        serde_json::json!({
            "at": utc_now(), "detail": "rename login in src/auth.rs", "nonce": "n1",
            "harness_session": "A", "base_rev": head(&repo), "pid": std::process::id(),
        }),
    );
    std::fs::write(repo.join("BYPASS.md"), "x\n").unwrap();
    let stop = std::thread::spawn({
        let repo = repo.clone();
        move || {
            hook(
                &repo,
                "stop",
                &serde_json::json!({"cwd": repo, "session_id": "A", "stop_hook_active": false})
                    .to_string(),
            )
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(400));
    run({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "start", "--auto-nonce", "n1"]);
        c
    });
    let stopped: serde_json::Value = serde_json::from_str(&stop.join().unwrap()).unwrap();
    let message = stopped["systemMessage"].as_str().unwrap();
    assert!(
        message.contains("WARN") && message.contains("1 outside the planned boundary (advisory)"),
        "{message}"
    );
    assert!(gate_status(&repo)["session"]["preexisting_changes"].is_null());
}

#[test]
fn gate_newer_prompt_supersedes_a_plan_still_being_built() {
    let (_temp, repo) = demo_repo();
    let base = head(&repo);
    // A plan is being built for the conversation's first prompt; a file was created since.
    write_marker_json(
        &repo,
        serde_json::json!({
            "at": utc_now(), "detail": "update the auth flow test for issue_token",
            "nonce": "n1", "harness_session": "A", "base_rev": base,
            "pid": std::process::id(),
        }),
    );
    std::fs::write(repo.join("BYPASS.md"), "x\n").unwrap();
    let answer: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "prompt",
        &serde_json::json!({"prompt": "rename login in src/auth.rs", "session_id": "A", "cwd": repo})
            .to_string(),
    ))
    .unwrap();
    assert!(answer["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("planning an edit boundary"));
    planner_done(&repo);
    // The earlier planner finds its marker replaced and backs out.
    let earlier = run({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "start", "--auto-nonce", "n1"]);
        c
    });
    assert!(earlier.contains("nothing started"), "{earlier}");
    // Both prompts are the task, and the baseline is the first prompt's: the file created in
    // between is the session's change.
    let status = gate_status(&repo);
    assert_eq!(
        status["session"]["task"],
        "update the auth flow test for issue_token rename login in src/auth.rs"
    );
    assert!(
        status["session"]["preexisting_changes"].is_null(),
        "{status}"
    );
}

#[test]
fn gate_binding_follows_the_configuration_as_it_is_now() {
    let (_temp, repo) = demo_repo();
    hook(
        &repo,
        "prompt",
        &serde_json::json!({"prompt": "rename login in src/auth.rs", "session_id": "A", "cwd": repo})
            .to_string(),
    );
    planner_done(&repo);
    assert_eq!(gate_status(&repo)["session"]["advisory"], true);
    let outside = || {
        hook(
            &repo,
            "pre-tool",
            &edit_input(&repo, "Write", "docs/ttl.md"),
        )
    };
    assert!(outside().trim().is_empty(), "advisory: nothing is denied");
    // `enforce` set while the conversation is under way binds from the next tool call.
    set_gate_mode(&repo, "enforce");
    let denied: serde_json::Value = serde_json::from_str(&outside()).unwrap();
    assert_eq!(
        denied["hookSpecificOutput"]["permissionDecision"], "deny",
        "{denied}"
    );
    let status = gate_status(&repo);
    assert_eq!(status["session"]["mode"], "enforce");
    assert!(status["session"].get("advisory").is_none(), "{status}");
    // `observe` is no more binding than `warn`: the session is advisory again, and a file
    // outside its boundary is reported, not failed.
    set_gate_mode(&repo, "observe");
    assert!(outside().trim().is_empty());
    let status = gate_status(&repo);
    assert_eq!(status["session"]["mode"], "observe");
    assert_eq!(status["session"]["advisory"], true, "{status}");
}

#[test]
fn gate_ci_judges_a_finished_change_from_the_task_text_alone() {
    let (_temp, repo) = demo_repo();
    let mut auth = std::fs::read_to_string(repo.join("src/auth.rs")).unwrap();
    auth.push_str("// retry\n");
    std::fs::write(repo.join("src/auth.rs"), auth).unwrap();
    std::fs::write(repo.join("BYPASS.md"), "x\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "agent change"]);
    let ci = |extra: &[&str]| {
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args([
                "gate",
                "ci",
                "--base",
                "HEAD~1",
                "--task",
                "add retry handling to token refresh",
            ])
            .args(extra);
        c
    };
    // By default the boundary is advisory: it was planned from the task's words and nobody
    // declared it, so a file outside it is reported and the run does not fail.
    let advisory: serde_json::Value = serde_json::from_str(&run(ci(&[]))).unwrap();
    let advisory = &advisory["dossier"];
    assert_eq!(advisory["verdict"], "warn", "{advisory}");
    assert_eq!(
        advisory["change"]["outside_boundary"],
        serde_json::json!(["BYPASS.md"])
    );
    assert!(advisory["verification"]["boundary_violations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(advisory["caveats"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c.as_str().unwrap().starts_with("advisory session")));

    // `--mode enforce` makes the same boundary binding.
    let dossier_path = _temp.path().join("dossier.md");
    let (stdout, stderr) = run_failure(ci(&[
        "--mode",
        "enforce",
        "--write",
        dossier_path.to_str().unwrap(),
    ]));
    assert!(stderr.contains("FAIL"), "{stderr}");
    let output: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let dossier = &output["dossier"];
    assert_eq!(dossier["verdict"], "fail");
    assert!(dossier["change"].get("outside_boundary").is_none());
    let violations = dossier["verification"]["boundary_violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(violations, vec!["BYPASS.md".to_string()]);
    assert_eq!(dossier["trail"]["events_total"], 0, "no hooks ran");
    let written = std::fs::read_to_string(_temp.path().join("dossier.md")).unwrap();
    assert!(written.starts_with("## "), "{written}");
    assert!(
        !repo.join(".ok/gate/session.json").exists(),
        "ci leaves no session"
    );
    // Removing the bypass passes, enforced or not.
    git(&repo, &["rm", "-q", "BYPASS.md"]);
    git(&repo, &["commit", "-qm", "drop bypass"]);
    let after = |extra: &[&str]| {
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args([
                "gate",
                "ci",
                "--base",
                "HEAD~2",
                "--task",
                "add retry handling to token refresh",
                "--format",
                "text",
            ])
            .args(extra);
        c
    };
    run(after(&["--mode", "enforce"]));
    run(after(&[]));

    // A forbidden path fails without `enforce`: that rule is policy, not a plan.
    std::fs::write(repo.join(".env"), "TOKEN=x\n").unwrap();
    git(&repo, &["add", "-f", ".env"]);
    git(&repo, &["commit", "-qm", "a secret file"]);
    let (stdout, _) = run_failure({
        let mut c = ok();
        c.arg("--json").arg("--repo").arg(&repo).args([
            "gate",
            "ci",
            "--base",
            "HEAD~1",
            "--task",
            "add retry handling to token refresh",
        ]);
        c
    });
    let output: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(output["dossier"]["verdict"], "fail");
    let kinds = output["dossier"]["verification"]["boundary_violations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec!["forbidden_boundary".to_string()], "{output}");
}

#[test]
fn gate_denies_a_forbidden_path_with_no_session_and_with_no_index() {
    let (_temp, repo) = demo_repo();
    assert_eq!(gate_status(&repo)["active"], false);
    let pre = |input: String| hook(&repo, "pre-tool", &input);
    let shell = |command: &str| {
        serde_json::json!({
            "session_id": "A", "cwd": repo, "tool_name": "Bash", "tool_use_id": "t1",
            "tool_input": {"command": command}
        })
        .to_string()
    };
    let denied = |output: String, path: &str| {
        let value: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            value["hookSpecificOutput"]["permissionDecision"], "deny",
            "{value}"
        );
        let reason = value["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap();
        assert!(reason.contains(path), "{reason}");
        assert!(
            reason.contains("with or without a gate session"),
            "{reason}"
        );
    };
    // No session: a prompt the planner had Low confidence in, a slash command, `auto = false`
    // or a planner that is not done all leave the hooks here. The policy still holds.
    denied(pre(edit_input(&repo, "Edit", ".env")), ".env");
    denied(
        pre(edit_input(&repo, "Write", "vendor/tinylib/util.py")),
        "vendor/tinylib/util.py",
    );
    denied(
        pre(edit_input(&repo, "Write", "web/node_modules/x/index.js")),
        "web/node_modules/x/index.js",
    );
    denied(pre(shell("echo TOKEN=x > .env")), ".env");
    denied(
        pre(shell("sed -i s/a/b/ certs/server.pem")),
        "certs/server.pem",
    );
    // Everything else passes, as before: there is no boundary to hold it to.
    assert!(pre(edit_input(&repo, "Edit", "src/auth.rs"))
        .trim()
        .is_empty());
    assert!(pre(shell("ls -la && cat .env")).trim().is_empty());

    // The same with no index at all: the policy reads nothing but the path.
    std::fs::remove_dir_all(repo.join(".ok")).unwrap();
    denied(pre(edit_input(&repo, "Edit", ".env")), ".env");
    assert!(pre(edit_input(&repo, "Edit", "src/auth.rs"))
        .trim()
        .is_empty());

    // `observe` denies nothing, with or without a session.
    set_gate_mode(&repo, "observe");
    assert!(pre(edit_input(&repo, "Edit", ".env")).trim().is_empty());
}

#[test]
fn gate_ci_takes_its_mode_from_the_base_revision_not_from_the_change_it_judges() {
    let (_temp, repo) = demo_repo();
    git(&repo, &["branch", "-M", "main"]);
    // The repository declares the boundary binding, on its main branch.
    set_gate_mode(&repo, "enforce");
    git(&repo, &["commit", "-qam", "bind the boundary"]);
    // A branch adds a file outside any plan for its task, and edits ok.toml to make its own
    // judgement advisory.
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    std::fs::create_dir_all(repo.join("src/billing")).unwrap();
    std::fs::write(repo.join("src/billing/unrelated.rs"), "pub fn x() {}\n").unwrap();
    set_gate_mode(&repo, "warn");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "feature"]);
    let ci = |extra: &[&str]| {
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "ci", "--base", "main"])
            .args(extra);
        c
    };
    // The base revision's mode applies: the outside file fails, and the dossier says which
    // setting it went by.
    let (stdout, _) = run_failure(ci(&["--task", "change token expiration"]));
    let output: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let dossier = &output["dossier"];
    assert_eq!(dossier["verdict"], "fail", "{dossier}");
    assert_eq!(dossier["mode"], "enforce");
    assert!(dossier.get("advisory").is_none());
    assert!(
        dossier["caveats"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().contains("the base revision's applies")),
        "{dossier}"
    );
    // The caller of the check, not the change, may say otherwise.
    let advisory: serde_json::Value = serde_json::from_str(&run(ci(&[
        "--task",
        "change token expiration",
        "--mode",
        "warn",
    ])))
    .unwrap();
    assert_eq!(advisory["dossier"]["verdict"], "warn");
    assert_eq!(advisory["dossier"]["advisory"], true);

    // A task that begins with a dash is a task, in either spelling of the option, and reaches
    // the Markdown as one escaped line.
    for spelling in [
        vec!["--task", "--fix <b>token</b> expiration", "--mode", "warn"],
        vec!["--task=- [x] fix token expiration", "--mode", "warn"],
    ] {
        let dashed: serde_json::Value = serde_json::from_str(&run(ci(&spelling))).unwrap();
        assert!(dashed["dossier"]["task"]
            .as_str()
            .unwrap()
            .ends_with("expiration"));
    }
    let markdown = run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "ci",
            "--base",
            "main",
            "--mode",
            "warn",
            "--task",
            "--fix <b>token</b> expiration\n\n## not a heading",
        ]);
        c
    });
    assert!(
        markdown
            .contains("**Task:** --fix &lt;b&gt;token&lt;/b&gt; expiration \\#\\# not a heading"),
        "{markdown}"
    );
    assert!(
        markdown.contains("outside the planned boundary, which is advisory here"),
        "{markdown}"
    );
    assert!(!markdown.contains("every changed file is inside the declared boundary"));
}

#[test]
fn gate_sibling_caution_admits_a_neighbour_with_a_warning_when_enabled() {
    let (_temp, repo) = demo_repo();
    let config = repo.join("ok.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let (head, gate) = text.split_once("[gate]\n").expect("a [gate] section");
    let gate = gate
        .replacen("mode = \"warn\"", "mode = \"enforce\"", 1)
        .replacen("sibling_caution = false", "sibling_caution = true", 1);
    assert!(gate.contains("sibling_caution = true"), "{gate}");
    std::fs::write(&config, format!("{head}[gate]\n{gate}")).unwrap();
    run({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "start", "add retry handling to token refresh"]);
        c
    });
    let check: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "check", "src/retry.rs"]);
        c
    }))
    .unwrap();
    assert_eq!(check["class"], "sibling");
    assert_eq!(check["action"], "warn");
    let (_, err) = run_failure({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "check", "docs/new.md"]);
        c
    });
    assert!(err.contains("would be denied"), "{err}");
    // A top-level name is not a sibling of the allowed top-level files.
    let (_, err) = run_failure({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "check", "LICENSE"]);
        c
    });
    assert!(err.contains("would be denied"), "{err}");
    // Verification says what the hook said: the sibling is a warning, not a violation.
    hook(
        &repo,
        "pre-tool",
        &edit_input(&repo, "Write", "src/retry.rs"),
    );
    std::fs::write(repo.join("src/retry.rs"), "pub fn retry() {}\n").unwrap();
    hook(
        &repo,
        "post-tool",
        &edit_input(&repo, "Write", "src/retry.rs"),
    );
    let finished: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "finish"]);
        c
    }))
    .unwrap();
    assert_eq!(finished["dossier"]["verdict"], "warn", "{finished}");
    assert!(finished["dossier"]["verification"]["boundary_violations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(finished["dossier"]["trail"]["edits_caution"], 1);
}

fn write_marker(repo: &Path, nonce: &str, task: &str) {
    write_marker_json(
        repo,
        serde_json::json!({"at": utc_now(), "detail": task, "nonce": nonce}),
    );
}

fn write_marker_json(repo: &Path, marker: serde_json::Value) {
    std::fs::create_dir_all(repo.join(".ok/gate")).unwrap();
    std::fs::write(repo.join(".ok/gate/starting.json"), marker.to_string()).unwrap();
}

/// The id of a process that has exited and been reaped.
fn dead_pid() -> u32 {
    let mut child = Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .spawn()
        .expect("git should run");
    let pid = child.id();
    child.wait().expect("git should exit");
    pid
}

fn head(repo: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git should run");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn gate_status(repo: &Path) -> serde_json::Value {
    serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(repo)
            .args(["gate", "status"]);
        c
    }))
    .unwrap()
}

fn set_gate_mode(repo: &Path, mode: &str) {
    let config = repo.join("ok.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let (head, gate) = text.split_once("[gate]\n").expect("a [gate] section");
    let gate = gate
        .replacen("mode = \"warn\"", &format!("mode = \"{mode}\""), 1)
        .replacen("mode = \"enforce\"", &format!("mode = \"{mode}\""), 1);
    std::fs::write(&config, format!("{head}[gate]\n{gate}")).unwrap();
}

fn planner_done(repo: &Path) {
    wait_until(
        "the planner to finish",
        std::time::Duration::from_secs(60),
        || !repo.join(".ok/gate/starting.json").exists(),
    );
}

/// The current time as RFC 3339, from `date`, so the test needs no time crate.
fn utc_now() -> String {
    let output = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .expect("date should run");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn gate_planner_backs_out_when_its_prompt_is_gone_or_a_manual_session_took_over() {
    let (_temp, repo) = demo_repo();
    let auto_start = |nonce: &str| {
        run({
            let mut c = ok();
            c.arg("--repo")
                .arg(&repo)
                .args(["gate", "start", "--auto-nonce", nonce]);
            c
        })
    };
    // A planner whose process is gone is not waited for: the stop returns at once, the
    // marker is cleared, and a planner that somehow ran later would find its prompt gone.
    write_marker_json(
        &repo,
        serde_json::json!({
            "at": utc_now(), "detail": "rename login in src/auth.rs", "nonce": "n1",
            "pid": dead_pid(),
        }),
    );
    let started = std::time::Instant::now();
    let stop = hook(
        &repo,
        "stop",
        &serde_json::json!({"cwd": repo, "stop_hook_active": false}).to_string(),
    );
    assert!(stop.trim().is_empty(), "{stop}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(!repo.join(".ok/gate/starting.json").exists());
    let out = auto_start("n1");
    assert!(out.contains("nothing started"), "{out}");
    assert!(!repo.join(".ok/gate/session.json").exists());

    // The end of the conversation cancels a plan that outlasts the wait.
    write_marker_json(
        &repo,
        serde_json::json!({
            "at": utc_now(), "detail": "rename login in src/auth.rs", "nonce": "n1b",
            "harness_session": "A", "pid": std::process::id(),
        }),
    );
    hook(
        &repo,
        "session-end",
        &serde_json::json!({"cwd": repo, "session_id": "A"}).to_string(),
    );
    assert!(!repo.join(".ok/gate/starting.json").exists());
    assert!(auto_start("n1b").contains("nothing started"));

    // A session started by hand takes the marker, and a planner that was already running
    // for a prompt finds the manual session and leaves it alone.
    write_marker(&repo, "n2", "rename login in src/auth.rs");
    run({
        let mut c = ok();
        c.arg("--repo").arg(&repo).args([
            "gate",
            "start",
            "add retry handling to token refresh",
            "--mode",
            "enforce",
        ]);
        c
    });
    assert!(
        !repo.join(".ok/gate/starting.json").exists(),
        "a manual start takes the marker"
    );
    assert!(auto_start("n2").contains("nothing started"));
    write_marker(&repo, "n2", "rename login in src/auth.rs");
    let out = auto_start("n2");
    assert!(out.contains("a session started by hand is active"), "{out}");
    let status: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "status"]);
        c
    }))
    .unwrap();
    assert_eq!(
        status["session"]["task"],
        "add retry handling to token refresh"
    );
    assert_eq!(status["session"]["mode"], "enforce");
    assert!(status["session"].get("auto").is_none(), "{status}");

    // A marker dated in the future, or one whose planner died, does not hold tool calls.
    std::fs::write(
        repo.join(".ok/gate/starting.json"),
        r#"{"at":"2099-01-01T00:00:00Z","detail":"x","nonce":"n3"}"#,
    )
    .unwrap();
    let started = std::time::Instant::now();
    let inside = hook(&repo, "pre-tool", &edit_input(&repo, "Edit", "src/auth.rs"));
    assert!(inside.trim().is_empty(), "{inside}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(!repo.join(".ok/gate/starting.json").exists());
}

#[test]
fn gate_judges_an_edit_made_while_the_plan_was_still_being_built() {
    let (_temp, repo) = demo_repo();
    // The baseline is taken when the prompt arrives. A file created before the planner has
    // written the session is therefore the session's change, not something that predates it.
    hook(
        &repo,
        "prompt",
        &serde_json::json!({"prompt": "rename login in src/auth.rs", "session_id": "A", "cwd": repo})
            .to_string(),
    );
    std::fs::write(repo.join("BYPASS.md"), "x\n").unwrap();
    wait_until(
        "the planner to finish",
        std::time::Duration::from_secs(60),
        || !repo.join(".ok/gate/starting.json").exists(),
    );
    let status: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "status"]);
        c
    }))
    .unwrap();
    assert!(
        status["session"]["preexisting_changes"].is_null(),
        "nothing predates the prompt: {status}"
    );
    let stopped: serde_json::Value = serde_json::from_str(&hook(
        &repo,
        "stop",
        &serde_json::json!({"cwd": repo, "session_id": "A", "stop_hook_active": false}).to_string(),
    ))
    .unwrap();
    let message = stopped["systemMessage"].as_str().unwrap();
    assert!(
        message.contains("WARN") && message.contains("1 outside the planned boundary (advisory)"),
        "{message}"
    );
}

#[test]
fn gate_enforce_in_the_configuration_makes_a_prompt_started_session_binding() {
    let (_temp, repo) = demo_repo();
    let config = repo.join("ok.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    let (head, gate) = text.split_once("[gate]\n").expect("a [gate] section");
    let gate = gate.replacen("mode = \"warn\"", "mode = \"enforce\"", 1);
    std::fs::write(&config, format!("{head}[gate]\n{gate}")).unwrap();
    hook(
        &repo,
        "prompt",
        &serde_json::json!({"prompt": "rename login in src/auth.rs", "session_id": "A", "cwd": repo})
            .to_string(),
    );
    wait_until(
        "the planner to finish",
        std::time::Duration::from_secs(60),
        || !repo.join(".ok/gate/starting.json").exists(),
    );
    let stop = |active: bool| -> serde_json::Value {
        serde_json::from_str(&hook(
            &repo,
            "stop",
            &serde_json::json!({"cwd": repo, "session_id": "A", "stop_hook_active": active})
                .to_string(),
        ))
        .unwrap()
    };
    std::fs::write(repo.join("BYPASS.md"), "x\n").unwrap();
    assert_eq!(stop(false)["decision"], "block");
    // Continuing after the hold does not end the session: the failing change is still its
    // change, and the next prompt of the conversation belongs to it.
    assert!(stop(true)["systemMessage"]
        .as_str()
        .unwrap()
        .contains("FAIL"));
    let status: serde_json::Value = serde_json::from_str(&run({
        let mut c = ok();
        c.arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args(["gate", "status"]);
        c
    }))
    .unwrap();
    assert_eq!(status["active"], true);
    assert!(status["session"].get("advisory").is_none(), "{status}");
    // A manual start replaces the auto session without --force.
    run({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args(["gate", "start", "add retry handling to token refresh"]);
        c
    });
}

#[test]
fn gate_does_nothing_automatic_outside_a_git_repository_or_with_a_broken_configuration() {
    // A plain directory: not indexed, not a git working tree. The hooks stay out of it.
    let temp = tempfile::tempdir().unwrap();
    let plain = temp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("notes.txt"), "x\n").unwrap();
    let out = hook(&plain, "session-start", r#"{"source":"startup"}"#);
    assert!(out.trim().is_empty(), "{out}");
    let out = hook(
        &plain,
        "prompt",
        r#"{"prompt":"add retry handling to token refresh"}"#,
    );
    assert!(out.trim().is_empty(), "{out}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !plain.join(".ok").exists(),
        "nothing was indexed or written"
    );

    // An index does not change that: an indexed directory outside git, and an indexed
    // subdirectory of a git working tree, get no prompt-started session.
    let nogit = temp.path().join("nogit");
    run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&nogit);
        command
    });
    let parent = temp.path().join("parent");
    std::fs::create_dir_all(&parent).unwrap();
    git(&parent, &["init", "-q"]);
    let sub = parent.join("sub");
    run({
        let mut command = ok();
        command.arg("demo").arg("--path").arg(&sub);
        command
    });
    for place in [&nogit, &sub] {
        let out = hook(
            place,
            "prompt",
            r#"{"prompt":"rename login in src/auth.rs"}"#,
        );
        assert!(out.trim().is_empty(), "{out}");
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(!place.join(".ok/gate/starting.json").exists());
        assert!(!place.join(".ok/gate/session.json").exists());
    }

    // A configuration that cannot be read turns automatic mode off, and says so.
    let (_temp, repo) = demo_repo();
    std::fs::remove_dir_all(repo.join(".ok")).unwrap();
    std::fs::write(repo.join("ok.toml"), "[gate\nauto = false\n").unwrap();
    let out: serde_json::Value =
        serde_json::from_str(&hook(&repo, "session-start", r#"{"source":"startup"}"#)).unwrap();
    assert!(out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("ok.toml cannot be read"));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!repo.join(".ok/gate/indexing.json").exists());
    assert!(!repo.join(".ok/index.sqlite").exists());
}

#[test]
fn gate_ci_reads_the_change_from_the_merge_base_and_ignores_its_own_dossier_file() {
    let (_temp, repo) = demo_repo();
    git(&repo, &["branch", "-M", "main"]);
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    let mut auth = std::fs::read_to_string(repo.join("src/auth.rs")).unwrap();
    auth.push_str("// retry\n");
    std::fs::write(repo.join("src/auth.rs"), auth).unwrap();
    git(&repo, &["commit", "-qam", "feature change"]);
    // main moves on with files the feature branch never touched.
    git(&repo, &["checkout", "-q", "main"]);
    std::fs::write(repo.join("NOTES.md"), "notes\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "main moves on"]);
    git(&repo, &["checkout", "-q", "feature"]);
    let ci = || -> serde_json::Value {
        let output = ok()
            .arg("--json")
            .arg("--repo")
            .arg(&repo)
            .args([
                "gate",
                "ci",
                "--base",
                "main",
                "--task",
                "add retry handling to token refresh",
                "--write",
            ])
            .arg(repo.join("dossier.md"))
            .output()
            .unwrap();
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap()
    };
    let first = ci();
    assert_eq!(
        first["dossier"]["change"]["changed_files"],
        serde_json::json!(["src/auth.rs"]),
        "only what the branch added since it left main: {first}"
    );
    assert_ne!(first["dossier"]["verdict"], "fail");
    // The dossier the first run wrote into the repository is not the second run's change.
    let second = ci();
    assert_eq!(
        second["dossier"]["change"]["changed_files"],
        serde_json::json!(["src/auth.rs"])
    );
    assert_eq!(
        second["dossier"]["change"]["preexisting_excluded"],
        serde_json::json!(["dossier.md"])
    );

    // Naming a file the branch itself added as the dossier path does not take it out of
    // what is judged, and does not overwrite it: the run is refused.
    std::fs::create_dir_all(repo.join("docs")).unwrap();
    std::fs::write(repo.join("docs/OUTSIDE.md"), "outside\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "an out-of-boundary file"]);
    let (_, stderr) = run_failure({
        let mut c = ok();
        c.arg("--repo")
            .arg(&repo)
            .args([
                "gate",
                "ci",
                "--base",
                "main",
                "--task",
                "add retry handling to token refresh",
                "--write",
            ])
            .arg(repo.join("docs/OUTSIDE.md"));
        c
    });
    assert!(
        stderr.contains("names a file this repository tracks"),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("docs/OUTSIDE.md")).unwrap(),
        "outside\n"
    );
}
