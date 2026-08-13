#[derive(Debug, Clone, Serialize)]
struct AgentSetupReport {
    client: String,
    repo: PathBuf,
    mode: String,
    applied: bool,
    ready: bool,
    index_path: PathBuf,
    config_path: PathBuf,
    skill_path: PathBuf,
    backup_path: Option<PathBuf>,
    checks: Vec<AgentSetupCheck>,
    next_step: String,
}

#[derive(Debug, Clone, Serialize)]
struct AgentSetupCheck {
    name: String,
    status: String,
    detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AgentSetupState {
    version: u32,
    client: String,
    repo: PathBuf,
    config_path: PathBuf,
    skill_path: PathBuf,
    expected_server: serde_json::Value,
}

const ONBOARDING_STATE_VERSION: u32 = 1;
const MANAGED_SKILL_MARKER: &str = "<!-- Open Kioku managed onboarding file -->";
const MCP_HEALTHCHECK_TIMEOUT: Duration = Duration::from_secs(5);

fn setup_agent(args: SetupAgentArgs, cli_json: bool, global_repo: &Path) -> anyhow::Result<()> {
    let repo = resolve_repo(global_repo, args.repo);
    let client = supported_onboarding_client(args.client)?;
    if args.apply {
        let report = apply_agent_setup(client, &repo)?;
        print_agent_setup_report(&report, cli_json)?;
        return Ok(());
    }
    if args.check {
        let report = check_agent_setup(client, &repo)?;
        let ready = report.ready;
        print_agent_setup_report(&report, cli_json)?;
        if !ready {
            anyhow::bail!(
                "Open Kioku is not ready for {}; run `ok setup agent {} --repo {} --apply`",
                client.as_str(),
                client.as_str(),
                repo.display()
            );
        }
        return Ok(());
    }
    if args.uninstall {
        let report = uninstall_agent_setup(client, &repo)?;
        print_agent_setup_report(&report, cli_json)?;
        return Ok(());
    }
    let report = dry_run_agent_setup(client, &repo)?;
    print_agent_setup_report(&report, cli_json)
}

fn supported_onboarding_client(client: McpClient) -> anyhow::Result<McpClient> {
    match client {
        McpClient::Claude | McpClient::Cursor => Ok(client),
        other => anyhow::bail!(
            "`ok setup agent {}` is not available yet; use `ok mcp install {}` for a manual, read-only configuration snippet",
            other.as_str(),
            other.as_str()
        ),
    }
}

fn dry_run_agent_setup(client: McpClient, repo: &Path) -> anyhow::Result<AgentSetupReport> {
    let layout = agent_setup_layout(client, repo)?;
    Ok(AgentSetupReport {
        client: client.as_str().into(),
        repo: repo.to_path_buf(),
        mode: "dry_run".into(),
        applied: false,
        ready: false,
        index_path: repo.join(".ok/index.sqlite"),
        config_path: layout.config_path,
        skill_path: layout.skill_path,
        backup_path: layout.backup_path,
        checks: vec![
            agent_setup_check(
                "writes",
                "planned",
                "no files were changed; rerun with --apply to index and configure this repository",
            ),
            agent_setup_check(
                "mcp_mode",
                "planned",
                "the installed server is local stdio, read-only, and network-denied by default",
            ),
        ],
        next_step: format!(
            "Review the repository-scoped targets above, then run `ok setup agent {} --repo {} --apply`.",
            client.as_str(),
            repo.display()
        ),
    })
}

fn apply_agent_setup(client: McpClient, repo: &Path) -> anyhow::Result<AgentSetupReport> {
    ensure_safe_repo_root(repo)?;
    let layout = agent_setup_layout(client, repo)?;
    let expected_server = expected_mcp_server(repo);
    let state_path = onboarding_state_path(repo, client);

    // Index before touching agent configuration. A failed index must never leave
    // the client pointing at a repository that cannot serve MCP requests.
    if !repo.join("ok.toml").exists() {
        OkConfig::write_default(repo.join("ok.toml"))?;
    }
    let snapshot = index_repo(repo)?;

    let (config_changed, backup_path) = merge_managed_mcp_server(
        &layout.config_path,
        &expected_server,
        layout.backup_path.as_deref(),
    )?;
    let skill_changed = match write_managed_skill(&layout.skill_path, client) {
        Ok(changed) => changed,
        Err(error) => {
            if config_changed {
                let _ = remove_managed_mcp_server(&layout.config_path, &expected_server);
            }
            return Err(error);
        }
    };

    let state = AgentSetupState {
        version: ONBOARDING_STATE_VERSION,
        client: client.as_str().into(),
        repo: repo.to_path_buf(),
        config_path: layout.config_path.clone(),
        skill_path: layout.skill_path.clone(),
        expected_server,
    };
    atomic_write_onboarding_json(&state_path, &state)?;

    let reachable = mcp_server_reachable(repo)?;
    let mut checks = vec![
        agent_setup_check(
            "config",
            if config_changed { "applied" } else { "unchanged" },
            format!("managed `open-kioku` entry at {}", layout.config_path.display()),
        ),
        agent_setup_check(
            "skill",
            if skill_changed { "applied" } else { "unchanged" },
            format!("managed guidance at {}", layout.skill_path.display()),
        ),
        agent_setup_check(
            "index",
            "passed",
            format!(
                "indexed {} files and {} symbols",
                snapshot.manifest.file_count, snapshot.manifest.symbol_count
            ),
        ),
    ];
    checks.push(agent_setup_check(
        "mcp_stdio",
        if reachable { "passed" } else { "failed" },
        if reachable {
            "the local server answered an MCP initialize request"
        } else {
            "the local server did not answer an MCP initialize request"
        },
    ));
    if !reachable {
        anyhow::bail!(
            "Open Kioku configured files but the local MCP server did not respond; inspect {} and rerun `ok setup agent {} --repo {} --check`",
            layout.config_path.display(),
            client.as_str(),
            repo.display()
        );
    }
    Ok(AgentSetupReport {
        client: client.as_str().into(),
        repo: repo.to_path_buf(),
        mode: "apply".into(),
        applied: true,
        ready: true,
        index_path: repo.join(".ok/index.sqlite"),
        config_path: layout.config_path,
        skill_path: layout.skill_path,
        backup_path,
        checks,
        next_step: format!(
            "Open this repository in {} and ask for a pre-edit plan. To verify later, run `ok setup agent {} --repo {} --check`.",
            client_display_name(client),
            client.as_str(),
            repo.display()
        ),
    })
}

fn check_agent_setup(client: McpClient, repo: &Path) -> anyhow::Result<AgentSetupReport> {
    let layout = agent_setup_layout(client, repo)?;
    let expected_server = expected_mcp_server(repo);
    let config_ready = managed_mcp_server_matches(&layout.config_path, &expected_server)?;
    let skill_ready = managed_skill_matches(&layout.skill_path)?;
    let index_ready = repo.join(".ok/index.sqlite").is_file();
    let mcp_ready = if index_ready { mcp_server_reachable(repo)? } else { false };
    let ready = config_ready && skill_ready && index_ready && mcp_ready;
    Ok(AgentSetupReport {
        client: client.as_str().into(),
        repo: repo.to_path_buf(),
        mode: "check".into(),
        applied: false,
        ready,
        index_path: repo.join(".ok/index.sqlite"),
        config_path: layout.config_path,
        skill_path: layout.skill_path,
        backup_path: layout.backup_path,
        checks: vec![
            agent_setup_check("config", check_status(config_ready), "managed MCP entry"),
            agent_setup_check("skill", check_status(skill_ready), "managed pre-edit guidance"),
            agent_setup_check("index", check_status(index_ready), "local SQLite index"),
            agent_setup_check("mcp_stdio", check_status(mcp_ready), "MCP initialize response"),
        ],
        next_step: if ready {
            "Open Kioku is ready for this repository.".into()
        } else {
            format!(
                "Run `ok setup agent {} --repo {} --apply` to repair the missing setup.",
                client.as_str(),
                repo.display()
            )
        },
    })
}

fn uninstall_agent_setup(client: McpClient, repo: &Path) -> anyhow::Result<AgentSetupReport> {
    let layout = agent_setup_layout(client, repo)?;
    let state_path = onboarding_state_path(repo, client);
    let state = read_onboarding_state(&state_path)?;
    let mut checks = Vec::new();
    if let Some(state) = state {
        if state.client != client.as_str() || state.repo != repo {
            anyhow::bail!("onboarding state does not match this repository and client");
        }
        let removed_server = remove_managed_mcp_server(&state.config_path, &state.expected_server)?;
        checks.push(agent_setup_check(
            "config",
            if removed_server { "removed" } else { "unchanged" },
            "removed only the matching Open Kioku MCP entry",
        ));
        let removed_skill = remove_managed_skill(&state.skill_path)?;
        checks.push(agent_setup_check(
            "skill",
            if removed_skill { "removed" } else { "unchanged" },
            "removed only the Open Kioku-managed guidance file",
        ));
        fs::remove_file(&state_path)?;
    } else {
        checks.push(agent_setup_check(
            "state",
            "unchanged",
            "no Open Kioku onboarding state was found; no configuration was removed",
        ));
    }
    Ok(AgentSetupReport {
        client: client.as_str().into(),
        repo: repo.to_path_buf(),
        mode: "uninstall".into(),
        applied: false,
        ready: false,
        index_path: repo.join(".ok/index.sqlite"),
        config_path: layout.config_path,
        skill_path: layout.skill_path,
        backup_path: layout.backup_path,
        checks,
        next_step: "The local .ok index was preserved. Remove it manually only if you no longer need Open Kioku's local data.".into(),
    })
}

#[derive(Debug, Clone)]
struct AgentSetupLayout {
    config_path: PathBuf,
    skill_path: PathBuf,
    backup_path: Option<PathBuf>,
}

fn agent_setup_layout(client: McpClient, repo: &Path) -> anyhow::Result<AgentSetupLayout> {
    let backup_root = repo.join(".ok/onboarding-backups");
    match client {
        McpClient::Claude => Ok(AgentSetupLayout {
            config_path: repo.join(".mcp.json"),
            skill_path: repo.join(".claude/skills/open-kioku/SKILL.md"),
            backup_path: Some(backup_root.join("claude-mcp.json")),
        }),
        McpClient::Cursor => Ok(AgentSetupLayout {
            config_path: repo.join(".cursor/mcp.json"),
            skill_path: repo.join(".cursor/rules/open-kioku-preflight.mdc"),
            backup_path: Some(backup_root.join("cursor-mcp.json")),
        }),
        other => anyhow::bail!("unsupported onboarding client {}", other.as_str()),
    }
}

fn client_display_name(client: McpClient) -> &'static str {
    match client {
        McpClient::Claude => "Claude Code",
        McpClient::Cursor => "Cursor",
        _ => client.as_str(),
    }
}

fn expected_mcp_server(repo: &Path) -> serde_json::Value {
    serde_json::json!({
        "command": "ok",
        "args": ["mcp", "serve", "--repo", repo.display().to_string(), "--read-only"]
    })
}

fn onboarding_state_path(repo: &Path, client: McpClient) -> PathBuf {
    repo.join(".ok/onboarding")
        .join(format!("{}.json", client.as_str()))
}

fn ensure_safe_repo_root(repo: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(repo)
        .with_context(|| format!("repository does not exist: {}", repo.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!(
            "repository root must be an existing non-symlink directory: {}",
            repo.display()
        );
    }
    Ok(())
}

fn merge_managed_mcp_server(
    config_path: &Path,
    expected_server: &serde_json::Value,
    backup_path: Option<&Path>,
) -> anyhow::Result<(bool, Option<PathBuf>)> {
    ensure_safe_target(config_path)?;
    let existed = config_path.exists();
    let mut config = if existed {
        serde_json::from_slice::<serde_json::Value>(&fs::read(config_path)?)
            .with_context(|| format!("invalid MCP JSON at {}", config_path.display()))?
    } else {
        serde_json::json!({})
    };
    let root = config
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("MCP config must be a JSON object: {}", config_path.display()))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("mcpServers must be a JSON object: {}", config_path.display()))?;
    if let Some(existing) = servers.get("open-kioku") {
        if existing == expected_server {
            return Ok((false, None));
        }
        anyhow::bail!(
            "{} already contains an `open-kioku` MCP entry that Open Kioku did not create; preserve it and configure manually",
            config_path.display()
        );
    }
    let backup = if existed {
        let backup_path = backup_path.ok_or_else(|| anyhow::anyhow!("missing backup path"))?;
        if !backup_path.exists() {
            atomic_write_bytes(backup_path, &fs::read(config_path)?)?;
        }
        Some(backup_path.to_path_buf())
    } else {
        None
    };
    servers.insert("open-kioku".into(), expected_server.clone());
    atomic_write_onboarding_json(config_path, &config)?;
    Ok((true, backup))
}

fn managed_mcp_server_matches(config_path: &Path, expected_server: &serde_json::Value) -> anyhow::Result<bool> {
    if !config_path.exists() {
        return Ok(false);
    }
    ensure_safe_target(config_path)?;
    let config: serde_json::Value = serde_json::from_slice(&fs::read(config_path)?)
        .with_context(|| format!("invalid MCP JSON at {}", config_path.display()))?;
    Ok(config
        .get("mcpServers")
        .and_then(|servers| servers.get("open-kioku"))
        == Some(expected_server))
}

fn remove_managed_mcp_server(
    config_path: &Path,
    expected_server: &serde_json::Value,
) -> anyhow::Result<bool> {
    if !config_path.exists() {
        return Ok(false);
    }
    ensure_safe_target(config_path)?;
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(config_path)?)
        .with_context(|| format!("invalid MCP JSON at {}", config_path.display()))?;
    let Some(servers) = config.get_mut("mcpServers").and_then(serde_json::Value::as_object_mut) else {
        return Ok(false);
    };
    if servers.get("open-kioku") != Some(expected_server) {
        return Ok(false);
    }
    servers.remove("open-kioku");
    atomic_write_onboarding_json(config_path, &config)?;
    Ok(true)
}

fn write_managed_skill(path: &Path, client: McpClient) -> anyhow::Result<bool> {
    ensure_safe_target(path)?;
    let desired = managed_skill_contents(client);
    if path.exists() && fs::read_to_string(path)? == desired {
        return Ok(false);
    }
    if path.exists() {
        let existing = fs::read_to_string(path)?;
        if !existing.contains(MANAGED_SKILL_MARKER) {
            anyhow::bail!(
                "{} already exists and is not Open Kioku-managed; preserve it and configure manually",
                path.display()
            );
        }
    }
    atomic_write_bytes(path, desired.as_bytes())?;
    Ok(true)
}

fn managed_skill_matches(path: &Path) -> anyhow::Result<bool> {
    Ok(path.exists() && fs::read_to_string(path)?.contains(MANAGED_SKILL_MARKER))
}

fn remove_managed_skill(path: &Path) -> anyhow::Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    ensure_safe_target(path)?;
    if !fs::read_to_string(path)?.contains(MANAGED_SKILL_MARKER) {
        return Ok(false);
    }
    fs::remove_file(path)?;
    Ok(true)
}

fn managed_skill_contents(client: McpClient) -> String {
    match client {
        McpClient::Claude => format!(
            "{MANAGED_SKILL_MARKER}\n\
# Open Kioku pre-edit workflow\n\n\
Use Open Kioku when navigating unfamiliar code, investigating a bug, planning a\n\
multi-file change, or changing a public API. Its local index is evidence, not\n\
a replacement for reading the relevant source.\n\n\
## Routine\n\n\
1. **Explore** with `search_code` or `get_definition` before claiming what\n\
   exists.\n\
2. **Preflight** with `preflight_change` before a multi-file edit, rename,\n\
   deletion, or public interface change. Read its caveats before editing.\n\
3. **Edit** only within the returned scope unless new evidence justifies an\n\
   expansion.\n\
4. **Verify** the changed files and selected tests before finishing. For\n\
   boundary verification, save a detailed `plan_change` result and pass it to\n\
   `verify_change`.\n\n\
Report caveats from Open Kioku exactly. Do not describe a guided MCP workflow as\n\
enforced behavior.\n"
        ),
        McpClient::Cursor => format!(
            "---\n\
description: Use local Open Kioku evidence before risky multi-file edits.\n\
alwaysApply: true\n\
---\n\n\
{MANAGED_SKILL_MARKER}\n\n\
# Open Kioku pre-edit workflow\n\n\
Follow this routine: **Explore -> Preflight -> Edit -> Verify**. For unfamiliar\n\
code, investigate with `search_code` or `get_definition` before making claims.\n\
Before a multi-file edit, rename, deletion, or public API change, run\n\
`preflight_change` and read its caveats. Keep edits within its returned scope\n\
unless new evidence supports expansion. Before finishing, run the selected tests\n\
and verify the changed files. When boundary verification is needed, save a\n\
detailed `plan_change` result and pass it to `verify_change`.\n\n\
Treat Open Kioku caveats as uncertainty. This rule guides tool selection; it\n\
does not enforce a tool call.\n"
        ),
        _ => unreachable!("unsupported onboarding clients are rejected first"),
    }
}

fn read_onboarding_state(path: &Path) -> anyhow::Result<Option<AgentSetupState>> {
    if !path.exists() {
        return Ok(None);
    }
    ensure_safe_target(path)?;
    let state = serde_json::from_slice(&fs::read(path)?)
        .with_context(|| format!("invalid onboarding state at {}", path.display()))?;
    Ok(Some(state))
}

fn ensure_safe_target(path: &Path) -> anyhow::Result<()> {
    if path.exists() && fs::symlink_metadata(path)?.file_type().is_symlink() {
        anyhow::bail!("refusing to modify symlinked onboarding target: {}", path.display());
    }
    if let Some(parent) = path.parent() {
        if parent.exists() && fs::symlink_metadata(parent)?.file_type().is_symlink() {
            anyhow::bail!("refusing to modify target below symlinked directory: {}", parent.display());
        }
    }
    Ok(())
}

fn atomic_write_onboarding_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    atomic_write_bytes(path, &serde_json::to_vec_pretty(value)?)
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    ensure_safe_target(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("onboarding target has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    ensure_safe_target(path)?;
    let tmp = parent.join(format!(
        ".{}.open-kioku-{}.tmp",
        path.file_name().and_then(|name| name.to_str()).unwrap_or("config"),
        std::process::id()
    ));
    if tmp.exists() {
        fs::remove_file(&tmp)?;
    }
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn mcp_server_reachable(repo: &Path) -> anyhow::Result<bool> {
    let executable = std::env::current_exe().context("could not locate the running ok executable")?;
    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "open-kioku-onboarding",
        "method": "initialize",
        "params": {"protocolVersion": "2024-11-05"}
    });
    let mut child = ProcessCommand::new(executable)
        .arg("--repo")
        .arg(repo)
        .arg("mcp")
        .arg("serve")
        .arg("--read-only")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("could not start local Open Kioku MCP server")?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("could not open local MCP stdin"))?;
    stdin.write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())?;
    drop(stdin);
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("could not open local MCP stdout"))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= MCP_HEALTHCHECK_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(10));
    };
    if !status.success() {
        return Ok(false);
    }
    let mut response = String::new();
    stdout.read_to_string(&mut response)?;
    Ok(response.lines().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|value| value.get("result").cloned())
            .and_then(|result| result.get("serverInfo").cloned())
            .and_then(|info| info.get("name").and_then(serde_json::Value::as_str).map(str::to_owned))
            .as_deref()
            == Some("open-kioku")
    }))
}

fn agent_setup_check(name: impl Into<String>, status: impl Into<String>, detail: impl Into<String>) -> AgentSetupCheck {
    AgentSetupCheck {
        name: name.into(),
        status: status.into(),
        detail: detail.into(),
    }
}

fn check_status(ready: bool) -> &'static str {
    if ready { "passed" } else { "missing" }
}

fn print_agent_setup_report(report: &AgentSetupReport, cli_json: bool) -> anyhow::Result<()> {
    if cli_json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("Open Kioku {} setup ({})", report.client, report.mode);
    println!("Repository: {}", report.repo.display());
    println!("MCP config: {}", report.config_path.display());
    println!("Guidance: {}", report.skill_path.display());
    for check in &report.checks {
        println!("- [{}] {}: {}", check.status, check.name, check.detail);
    }
    println!("\n{}", report.next_step);
    Ok(())
}

#[cfg(test)]
mod onboarding_tests {
    use super::*;

    #[test]
    fn cursor_layout_is_repository_scoped() {
        let repo = Path::new("/tmp/repository");
        let layout = agent_setup_layout(McpClient::Cursor, repo).unwrap();
        assert_eq!(layout.config_path, repo.join(".cursor/mcp.json"));
        assert_eq!(layout.skill_path, repo.join(".cursor/rules/open-kioku-preflight.mdc"));
    }

    #[test]
    fn installed_guidance_uses_the_canonical_advisory_routine() {
        for client in [McpClient::Claude, McpClient::Cursor] {
            let guidance = managed_skill_contents(client);
            assert!(guidance.contains("Explore"));
            assert!(guidance.contains("Preflight"));
            assert!(guidance.contains("Edit"));
            assert!(guidance.contains("Verify"));
            assert!(guidance.contains("preflight_change"));
            assert!(guidance.contains("plan_change"));
            assert!(guidance.contains("verify_change"));
            assert!(guidance.contains("enforced behavior") || guidance.contains("does not enforce"));
        }
    }

    #[test]
    fn merge_preserves_unrelated_servers_and_refuses_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(".cursor/mcp.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"other"}}}"#,
        )
        .unwrap();
        let expected = expected_mcp_server(temp.path());
        let backup = temp.path().join("backup.json");
        assert!(merge_managed_mcp_server(&path, &expected, Some(&backup)).unwrap().0);
        let value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["mcpServers"]["other"]["command"], "other");
        assert_eq!(value["mcpServers"]["open-kioku"], expected);
        assert!(!merge_managed_mcp_server(&path, &expected, Some(&backup)).unwrap().0);
        let conflicting = serde_json::json!({"command": "not-ok"});
        assert!(merge_managed_mcp_server(&path, &conflicting, Some(&backup))
            .unwrap_err()
            .to_string()
            .contains("did not create"));
    }

    #[test]
    fn uninstall_removes_only_matching_server_and_managed_skill() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let layout = agent_setup_layout(McpClient::Claude, repo).unwrap();
        let expected = expected_mcp_server(repo);
        merge_managed_mcp_server(&layout.config_path, &expected, layout.backup_path.as_deref())
            .unwrap();
        write_managed_skill(&layout.skill_path, McpClient::Claude).unwrap();
        assert!(remove_managed_mcp_server(&layout.config_path, &expected).unwrap());
        assert!(remove_managed_skill(&layout.skill_path).unwrap());
        let config: serde_json::Value = serde_json::from_slice(&fs::read(&layout.config_path).unwrap()).unwrap();
        assert!(config["mcpServers"].get("open-kioku").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn onboarding_refuses_symlinked_config_targets() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = temp.path().join("outside.json");
        fs::write(&outside, "{}").unwrap();
        let config = temp.path().join(".mcp.json");
        symlink(&outside, &config).unwrap();
        let error = merge_managed_mcp_server(
            &config,
            &expected_mcp_server(temp.path()),
            Some(&temp.path().join("backup.json")),
        )
        .unwrap_err();
        assert!(error.to_string().contains("symlinked"));
    }
}
