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
        let next_step = report.next_step.clone();
        print_agent_setup_report(&report, cli_json)?;
        if !ready {
            // The report already worked out whether `--apply` would succeed on this state
            // or what has to change first; repeat that rather than a fixed command.
            anyhow::bail!(
                "Open Kioku is not ready for {}: {next_step}",
                client.as_str()
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
        index_path: open_kioku_storage::generations::resolve_index_location(repo).sqlite_path(),
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

    // Decide what will happen to the client configuration and the guidance file before
    // indexing, so a refusal costs nothing: indexing is the expensive step, and no index
    // repairs a conflicting entry.
    let entry = inspect_mcp_server_entry(&layout.config_path, repo, &expected_server)?;
    if let McpServerEntry::Incompatible(conflict) = &entry {
        anyhow::bail!("{}", conflict.apply_message(client, repo));
    }
    if let SkillFile::Foreign = inspect_skill_file(&layout.skill_path)? {
        anyhow::bail!(
            "{} already exists and is not Open Kioku-managed; preserve it and configure manually",
            layout.skill_path.display()
        );
    }

    // Index before writing agent configuration. A failed index must never leave
    // the client pointing at a repository that cannot serve MCP requests.
    if !repo.join("ok.toml").exists() {
        OkConfig::write_default(repo.join("ok.toml"))?;
    }
    let snapshot = index_repo(repo)?;

    let (config_status, config_detail, config_changed, backup_path) = match &entry {
        McpServerEntry::Absent => {
            let (changed, backup) = merge_managed_mcp_server(
                &layout.config_path,
                &expected_server,
                layout.backup_path.as_deref(),
            )?;
            (
                if changed { "applied" } else { "unchanged" },
                format!("managed `open-kioku` entry at {}", layout.config_path.display()),
                changed,
                backup,
            )
        }
        McpServerEntry::Managed => (
            "unchanged",
            format!("managed `open-kioku` entry at {}", layout.config_path.display()),
            false,
            None,
        ),
        // A hand-written entry that already launches this repository's read-only server is
        // kept byte for byte: rewriting a tracked `.mcp.json` to say the same thing would
        // only dirty the working tree.
        McpServerEntry::Adopted => (
            "kept",
            format!(
                "existing `open-kioku` entry at {} launches `ok mcp serve` for this repository read-only; existing entry preserved",
                layout.config_path.display()
            ),
            false,
            None,
        ),
        McpServerEntry::Incompatible(conflict) => {
            anyhow::bail!("{}", conflict.apply_message(client, repo))
        }
    };
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
        agent_setup_check("config", config_status, config_detail),
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
        index_path: open_kioku_storage::generations::resolve_index_location(repo).sqlite_path(),
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
    let entry = inspect_mcp_server_entry(&layout.config_path, repo, &expected_server)?;
    let (config_status, config_detail, config_ready) = match &entry {
        McpServerEntry::Absent => ("missing", "no `open-kioku` MCP entry".to_string(), false),
        McpServerEntry::Managed => ("passed", "managed MCP entry".to_string(), true),
        McpServerEntry::Adopted => (
            "passed",
            "existing `open-kioku` entry launches `ok mcp serve` for this repository read-only; existing entry preserved".to_string(),
            true,
        ),
        McpServerEntry::Incompatible(conflict) => {
            ("mismatch", conflict.check_detail(&layout.config_path), false)
        }
    };
    let skill = inspect_skill_file(&layout.skill_path)?;
    let (skill_status, skill_detail, skill_ready) = match skill {
        SkillFile::Missing => ("missing", "managed pre-edit guidance".to_string(), false),
        SkillFile::Managed => ("passed", "managed pre-edit guidance".to_string(), true),
        SkillFile::Foreign => (
            "mismatch",
            format!(
                "{} exists and is not Open Kioku-managed; move it aside or merge the guidance by hand",
                layout.skill_path.display()
            ),
            false,
        ),
    };
    let index_ready = SqliteStore::open_repo_index(repo)?.is_some();
    let mcp_ready = if index_ready { mcp_server_reachable(repo)? } else { false };
    let ready = config_ready && skill_ready && index_ready && mcp_ready;
    // `--apply` refuses the two mismatch states before it does anything, so recommending it
    // there would send the user in a loop; the detail lines above name the actual edit.
    let blocked_by_conflict = matches!(entry, McpServerEntry::Incompatible(_))
        || matches!(skill, SkillFile::Foreign);
    Ok(AgentSetupReport {
        client: client.as_str().into(),
        repo: repo.to_path_buf(),
        mode: "check".into(),
        applied: false,
        ready,
        index_path: open_kioku_storage::generations::resolve_index_location(repo).sqlite_path(),
        config_path: layout.config_path,
        skill_path: layout.skill_path,
        backup_path: layout.backup_path,
        checks: vec![
            agent_setup_check("config", config_status, config_detail),
            agent_setup_check("skill", skill_status, skill_detail),
            agent_setup_check("index", check_status(index_ready), "local SQLite index"),
            agent_setup_check("mcp_stdio", check_status(mcp_ready), "MCP initialize response"),
        ],
        next_step: if ready {
            "Open Kioku is ready for this repository.".into()
        } else if blocked_by_conflict {
            format!(
                "Resolve the [mismatch] entries above by hand, then run `ok setup agent {} --repo {} --check` again.",
                client.as_str(),
                repo.display()
            )
        } else {
            format!(
                "Run `ok setup agent {} --repo {} --apply` to repair the missing setup.",
                client.as_str(),
                repo.display()
            )
        },
    })
}

/// What `apply` and `check` find at the client's `open-kioku` entry, decided before anything
/// is indexed or written.
#[derive(Debug, Clone, PartialEq)]
enum McpServerEntry {
    Absent,
    /// Byte for byte the entry Open Kioku writes.
    Managed,
    /// Written by someone else, but it launches `ok mcp serve` for this repository, which
    /// is read-only by construction; it is kept as it is.
    Adopted,
    Incompatible(McpEntryConflict),
}

#[derive(Debug, Clone, PartialEq)]
struct McpEntryConflict {
    /// What the existing entry does instead of serving this repository.
    problem: String,
    /// The value `mcpServers.open-kioku` would need, pretty-printed.
    edit: String,
}

impl McpEntryConflict {
    fn apply_message(&self, client: McpClient, repo: &Path) -> String {
        format!(
            "the `mcpServers.open-kioku` entry {problem}; Open Kioku did not write it and will not overwrite it. \
             To make it compatible, set `mcpServers.open-kioku` to:\n{edit}\n\
             Nothing was indexed or written. Once the entry launches `ok mcp serve --repo {repo}` it is kept as-is, \
             and rerunning `ok setup agent {client} --repo {repo} --apply` writes the guidance file and the index.",
            problem = self.problem,
            edit = self.edit,
            repo = repo.display(),
            client = client.as_str(),
        )
    }

    fn check_detail(&self, config_path: &Path) -> String {
        format!(
            "`mcpServers.open-kioku` in {} {}; set it to {}",
            config_path.display(),
            self.problem,
            self.edit.split_whitespace().collect::<Vec<_>>().join(" ")
        )
    }
}

fn inspect_mcp_server_entry(
    config_path: &Path,
    repo: &Path,
    expected_server: &serde_json::Value,
) -> anyhow::Result<McpServerEntry> {
    if !config_path.exists() {
        return Ok(McpServerEntry::Absent);
    }
    ensure_safe_target(config_path)?;
    let config: serde_json::Value = serde_json::from_slice(&fs::read(config_path)?)
        .with_context(|| format!("invalid MCP JSON at {}", config_path.display()))?;
    let Some(existing) = config
        .get("mcpServers")
        .and_then(|servers| servers.get("open-kioku"))
    else {
        return Ok(McpServerEntry::Absent);
    };
    if existing == expected_server {
        return Ok(McpServerEntry::Managed);
    }
    Ok(match describe_incompatible_server(existing, repo) {
        None => McpServerEntry::Adopted,
        Some(problem) => McpServerEntry::Incompatible(McpEntryConflict {
            problem,
            edit: serde_json::to_string_pretty(expected_server)?,
        }),
    })
}

/// Why `existing` cannot stand in for the managed entry, or `None` when it launches
/// `ok mcp serve` for `repo`. `ok mcp serve` forces read-only mode whatever flags it is
/// given, so any invocation of it is a read-only server; what has to match is the binary,
/// the subcommand, and the repository.
fn describe_incompatible_server(existing: &serde_json::Value, repo: &Path) -> Option<String> {
    let Some(command) = existing.get("command").and_then(serde_json::Value::as_str) else {
        return Some("has no string `command`".into());
    };
    let launches_ok = command == "ok"
        || Path::new(command)
            .file_name()
            .is_some_and(|name| name == "ok" || name == "ok.exe");
    if !launches_ok {
        return Some(format!("launches `{command}` instead of `ok`"));
    }
    let Some(args) = existing.get("args").and_then(serde_json::Value::as_array) else {
        return Some("has no `args` array".into());
    };
    let Some(args) = args
        .iter()
        .map(serde_json::Value::as_str)
        .collect::<Option<Vec<_>>>()
    else {
        return Some("has a non-string entry in `args`".into());
    };
    // `--repo` is accepted before or after `mcp serve`; `--allow-command` is the only other
    // flag that takes a value. What is left must be exactly the subcommand.
    let mut positional = Vec::new();
    let mut served_repo: Option<&str> = None;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index];
        if let Some(value) = arg.strip_prefix("--repo=") {
            served_repo = Some(value);
        } else if arg == "--repo" || arg == "--allow-command" {
            index += 1;
            let value = args.get(index).copied();
            if arg == "--repo" {
                served_repo = value;
            }
        } else if !arg.starts_with('-') {
            positional.push(arg);
        }
        index += 1;
    }
    if positional != ["mcp", "serve"] {
        return Some(format!(
            "runs `ok {}` rather than `ok mcp serve`",
            args.join(" ")
        ));
    }
    // Clients launch the server from the workspace root, so a missing or relative `--repo`
    // is resolved against this repository, not against the config file's directory.
    let served = served_repo.unwrap_or(".");
    let served_path = repo.join(served);
    let same_repo = match (served_path.canonicalize(), repo.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => served_path == repo,
    };
    (!same_repo).then(|| format!("serves `{served}` rather than this repository"))
}

/// What is at the guidance path before `apply` touches it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SkillFile {
    Missing,
    Managed,
    /// Someone else's file; it is never overwritten.
    Foreign,
}

fn inspect_skill_file(path: &Path) -> anyhow::Result<SkillFile> {
    if !path.exists() {
        return Ok(SkillFile::Missing);
    }
    ensure_safe_target(path)?;
    Ok(if fs::read_to_string(path)?.contains(MANAGED_SKILL_MARKER) {
        SkillFile::Managed
    } else {
        SkillFile::Foreign
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
        index_path: open_kioku_storage::generations::resolve_index_location(repo).sqlite_path(),
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
2. **Preflight** with `plan_change` and `detail: \"preflight\"` before a\n\
   multi-file edit, rename, deletion, or public interface change. Read its\n\
   caveats before editing.\n\
3. **Edit** only within the returned scope unless new evidence justifies an\n\
   expansion.\n\
4. **Verify** the changed files and selected tests before finishing. For\n\
   boundary verification, save a `plan_change` result with `format: \"json\"`\n\
   and pass it to `verify_change`.\n\
5. **Read cheaply.** `plan_change` and `build_context_pack` return Markdown by\n\
   default, which carries the same evidence at a fraction of the context cost.\n\
   Ask for `format: \"json\"` only when the result will be parsed. If a result\n\
   missed something, raise `limit` rather than narrowing the request.\n\n\
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
Before a multi-file edit, rename, deletion, or public API change, call\n\
`plan_change` with `detail: \"preflight\"` and read its caveats. Keep edits\n\
within its returned scope unless new evidence supports expansion. Before finishing, run the selected tests\n\
and verify the changed files. When boundary verification is needed, save a\n\
`plan_change` result with `format: \"json\"` and pass it to `verify_change`.\n\
Otherwise call `plan_change` and `build_context_pack` without `format`: the\n\
Markdown default carries the same evidence at a fraction of the context cost.\n\
Raise `limit` when a result missed something.\n\n\
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
            assert!(guidance.contains("plan_change"));
            assert!(guidance.contains("verify_change"));
            // This rule is written into the user's own repository, so a retired
            // name here fails on first use with nothing to point at the fix.
            for retired in ["preflight_change", "propose_patch", "create_change_contract"] {
                assert!(
                    !guidance.contains(retired),
                    "installed guidance must not name the retired `{retired}`"
                );
            }
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

    fn write_mcp_json(repo: &Path, server: serde_json::Value) -> PathBuf {
        let path = repo.join(".mcp.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({"mcpServers": {"open-kioku": server}}))
                .unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn hand_written_entry_serving_this_repository_is_adopted() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let expected = expected_mcp_server(repo);
        // What `ok mcp install claude` prints and a user pastes: no `--read-only`, an `env`
        // key, and the repository as `.`.
        for args in [
            serde_json::json!(["mcp", "serve", "--repo", "."]),
            serde_json::json!(["mcp", "serve", "--repo", repo.display().to_string(), "--read-only"]),
            serde_json::json!(["--repo", ".", "mcp", "serve"]),
            serde_json::json!(["mcp", "serve"]),
        ] {
            let path = write_mcp_json(
                repo,
                serde_json::json!({"command": "ok", "args": args, "env": {}}),
            );
            let before = fs::read(&path).unwrap();
            assert_eq!(
                inspect_mcp_server_entry(&path, repo, &expected).unwrap(),
                McpServerEntry::Adopted,
                "{args}"
            );
            assert_eq!(fs::read(&path).unwrap(), before, "inspection must not rewrite");
        }
        let path = write_mcp_json(repo, expected.clone());
        assert_eq!(
            inspect_mcp_server_entry(&path, repo, &expected).unwrap(),
            McpServerEntry::Managed
        );
    }

    #[test]
    fn incompatible_entry_names_the_key_and_the_edit() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let expected = expected_mcp_server(repo);
        for (server, problem) in [
            (
                serde_json::json!({"command": "npx", "args": ["open-kioku"]}),
                "launches `npx` instead of `ok`",
            ),
            (
                serde_json::json!({"command": "ok", "args": ["mcp", "serve", "--repo", "/somewhere/else"]}),
                "serves `/somewhere/else` rather than this repository",
            ),
            (
                serde_json::json!({"command": "ok", "args": ["daemon", "start"]}),
                "rather than `ok mcp serve`",
            ),
            (serde_json::json!("ok mcp serve"), "has no string `command`"),
        ] {
            let path = write_mcp_json(repo, server.clone());
            let McpServerEntry::Incompatible(conflict) =
                inspect_mcp_server_entry(&path, repo, &expected).unwrap()
            else {
                panic!("{server} must be incompatible");
            };
            assert!(conflict.problem.contains(problem), "{}", conflict.problem);
            let message = conflict.apply_message(McpClient::Claude, repo);
            assert!(message.contains("`mcpServers.open-kioku`"), "{message}");
            assert!(message.contains("\"--read-only\""), "{message}");
            assert!(message.contains("Nothing was indexed or written"), "{message}");
            assert!(!repo.join(".ok").exists());
        }
    }

    #[test]
    fn check_after_adopt_passes_config_and_never_recommends_a_failing_apply() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        write_mcp_json(
            repo,
            serde_json::json!({"command": "ok", "args": ["mcp", "serve", "--repo", "."], "env": {}}),
        );
        let report = check_agent_setup(McpClient::Claude, repo).unwrap();
        let config = report.checks.iter().find(|check| check.name == "config").unwrap();
        assert_eq!(config.status, "passed");
        assert!(config.detail.contains("existing entry preserved"), "{}", config.detail);
        assert!(!report.ready);
        assert!(report.next_step.contains("--apply"), "{}", report.next_step);

        write_mcp_json(
            repo,
            serde_json::json!({"command": "ok", "args": ["mcp", "serve", "--repo", "/somewhere/else"]}),
        );
        let report = check_agent_setup(McpClient::Claude, repo).unwrap();
        let config = report.checks.iter().find(|check| check.name == "config").unwrap();
        assert_eq!(config.status, "mismatch");
        assert!(config.detail.contains("`mcpServers.open-kioku`"), "{}", config.detail);
        assert!(config.detail.contains("/somewhere/else"), "{}", config.detail);
        assert!(!report.next_step.contains("--apply"), "{}", report.next_step);

        fs::create_dir_all(repo.join(".claude/skills/open-kioku")).unwrap();
        fs::write(repo.join(".claude/skills/open-kioku/SKILL.md"), "# mine\n").unwrap();
        let report = check_agent_setup(McpClient::Claude, repo).unwrap();
        let skill = report.checks.iter().find(|check| check.name == "skill").unwrap();
        assert_eq!(skill.status, "mismatch");
        assert!(!report.next_step.contains("--apply"), "{}", report.next_step);
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
