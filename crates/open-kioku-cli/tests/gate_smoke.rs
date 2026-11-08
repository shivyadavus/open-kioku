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

fn edit_input(repo: &Path, tool: &str, relative: &str) -> String {
    serde_json::json!({
        "session_id": "test",
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
    assert_eq!(applied["added"], 5);
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
    assert!(again["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("FAIL"));

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
    let prompt: serde_json::Value =
        serde_json::from_str(&hook(&repo, "prompt", r#"{"prompt":"hi"}"#)).unwrap();
    assert!(prompt["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("ok gate start"));
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
        .replacen("mode = \"warn\"", "mode = \"enforce\"", 1)
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
    let (code, out, _) = hook_status(&repo, "prompt", "{not json");
    assert_eq!(code, Some(0));
    assert!(out.contains("additionalContext"));

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
    std::fs::remove_file(repo.join(".ok/gate/session.json")).unwrap();

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
