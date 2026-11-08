use open_kioku_gate::{
    build_dossier, plan_with_expansions, render_markdown, shell_write_targets, ChangeDossier,
    ChangeInput, DossierVerdict, EventKind, GateEvent, GateMode, GateSession, GateStore,
    HookAction, HookInput, HookResponse, PathDecision,
};

#[derive(Subcommand)]
enum GateCommand {
    /// Declare a task: build its plan, and hold every edit to the plan's boundary until
    /// `ok gate finish`.
    #[command(after_help = "Examples:
  ok gate start \"add retry handling to token refresh\"
  ok gate start \"rename PolicyGate\" --mode enforce
  ok gate start \"fix the doctor probe\" --limit 20")]
    Start {
        /// The change to make, in natural language.
        task: String,
        /// How edits outside the boundary are treated: enforce (deny), warn (allow and tell
        /// the agent), or observe (record only). Defaults to `[gate] mode` in ok.toml.
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,
        /// Maximum context results the plan is built from.
        #[arg(long, default_value_t = 12)]
        limit: usize,
        /// Replace an active session instead of refusing to start over it. The new plan is
        /// built first; the old session ends only once the new one can start.
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Show the active session: task, mode, boundary, and the edits reported so far.
    Status,
    /// Decide one path against the active boundary, the way the edit hook would.
    Check {
        /// Repository-relative or absolute path.
        path: PathBuf,
    },
    /// Widen the active boundary by one path, recording the reason in the dossier.
    Expand {
        /// Repository-relative or absolute path to admit.
        path: PathBuf,
        /// Why this file must change for the declared task.
        #[arg(long, value_name = "TEXT")]
        reason: String,
    },
    /// Verify the actual change against the declared boundary, write the Change Dossier,
    /// and end the session.
    #[command(after_help = "Examples:
  ok gate finish
  ok gate finish --format json
  ok gate finish --run-commands --keep")]
    Finish {
        /// Execute the plan's validation commands locally and record their exit codes.
        #[arg(long, default_value_t = false)]
        run_commands: bool,
        /// Output format.
        #[arg(long, value_enum, default_value_t = GateFormat::Markdown)]
        format: GateFormat,
        /// Write the dossier but leave the session active.
        #[arg(long, default_value_t = false)]
        keep: bool,
    },
    /// End the active session without verifying. The event trail stays on disk.
    Abandon,
    /// Run as a coding-agent hook: read the harness's JSON on stdin, answer on stdout.
    Hook {
        /// Which hook event this process serves.
        #[arg(value_enum)]
        event: GateHookEvent,
    },
    /// Write the gate hooks into a client's repository-scoped settings.
    #[command(after_help = "Examples:
  ok gate install claude             # print the proposed .claude/settings.json hooks
  ok gate install claude --apply     # write them (merged, idempotent)")]
    Install {
        /// Coding-agent client to configure.
        #[arg(value_enum)]
        client: GateClient,
        /// Write the configuration. Without this flag, nothing changes.
        #[arg(long, default_value_t = false)]
        apply: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum GateFormat {
    Text,
    Json,
    Markdown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum GateHookEvent {
    /// Before an edit tool or a shell command runs: allow, warn, or deny by the boundary.
    PreTool,
    /// After it ran: tell the agent when the edit was outside the boundary, and record
    /// files a shell command changed.
    PostTool,
    /// When the agent wants to stop: verify, write the dossier, and hold it on a failure.
    Stop,
    /// When the user sends a prompt: say whether a gate session is active.
    Prompt,
    /// When a session starts or resumes: the same.
    SessionStart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum GateClient {
    Claude,
}

#[derive(Debug, Serialize)]
struct GateStartOutput {
    session_id: String,
    task: String,
    mode: GateMode,
    base_rev: Option<String>,
    risk_level: String,
    confidence_summary: String,
    allowed: Vec<String>,
    caution: Vec<String>,
    forbidden_rule_count: usize,
    plan_path: PathBuf,
    session_path: PathBuf,
}

#[derive(Debug, Serialize)]
struct GateStatusOutput {
    active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<GateSession>,
    edits_reported: usize,
    edits_denied: usize,
    edits_warned: usize,
}

#[derive(Debug, Serialize)]
struct GateFinishOutput {
    dossier: ChangeDossier,
    dossier_json: PathBuf,
    dossier_markdown: PathBuf,
    session_ended: bool,
}

fn handle_gate_command(json: bool, repo: &Path, command: GateCommand) -> anyhow::Result<()> {
    // Hooks resolve their repository from the harness's input; every other subcommand works
    // on an absolute root, so an absolute path given to `check` or `expand` is compared
    // against an absolute repository and not against `.`.
    if let GateCommand::Hook { event } = command {
        return gate_hook(repo, event);
    }
    let repo = std::path::absolute(repo)?;
    let repo = repo.as_path();
    match command {
        GateCommand::Start {
            task,
            mode,
            limit,
            force,
        } => gate_start(json, repo, &task, mode.as_deref(), limit, force),
        GateCommand::Status => gate_status(json, repo),
        GateCommand::Check { path } => gate_check(json, repo, &path),
        GateCommand::Expand { path, reason } => gate_expand(json, repo, &path, &reason),
        GateCommand::Finish {
            run_commands,
            format,
            keep,
        } => gate_finish(json, repo, run_commands, format, keep),
        GateCommand::Abandon => gate_abandon(json, repo),
        GateCommand::Hook { .. } => unreachable!("handled above"),
        GateCommand::Install { client, apply } => gate_install(json, repo, client, apply),
    }
}

/// The name the dossier gives the repository: `[repo] name` from ok.toml unless it is the
/// default, else the directory name.
fn gate_repo_name(repo: &Path, config: &OkConfig) -> String {
    Some(config.repo.name.clone())
        .filter(|name| !name.is_empty() && name != "open-kioku-repo")
        .or_else(|| {
            repo.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "repository".into())
}

fn gate_start(
    json: bool,
    repo: &Path,
    task: &str,
    mode: Option<&str>,
    limit: usize,
    force: bool,
) -> anyhow::Result<()> {
    let gate_store = GateStore::for_repo(repo);
    let active = gate_store.active_session()?;
    if let Some(active) = &active {
        if !force {
            anyhow::bail!(
                "a gate session is already active for task \"{}\" (started {}); finish it with \
                 `ok gate finish`, drop it with `ok gate abandon`, or pass --force",
                active.task,
                active.started_at.format("%Y-%m-%d %H:%M UTC")
            );
        }
    }
    let config = OkConfig::load_from_repo(repo)?;
    let mode = match mode {
        Some(value) => GateMode::parse(value).ok_or_else(|| {
            caller_input_error(format!(
                "--mode must be enforce, warn, or observe, not `{value}`"
            ))
        })?,
        None => GateMode::parse(&config.gate.mode).unwrap_or_default(),
    };
    let store = open_store(repo)?;
    require_authoritative_relationships(&store)?;
    let context = build_context_pack_with_config(repo, &store, task, limit, &config)?;
    let index_dir = default_index_dir(repo);
    let search_index = if TantivySearchIndex::exists(&index_dir) {
        Some(TantivySearchIndex::open_or_create(&index_dir)?)
    } else {
        None
    };
    let report = PlanEngine::new(&store as &dyn OkStore)
        .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
        .with_history_store(Some(&store))
        .with_memory_facts(RepoMemoryStore::search_repo(repo, task, 8)?)
        .with_memory_enabled(config.memory.enabled)
        .plan_from_context(task, limit, context)?;
    let plan_json = serde_json::to_vec_pretty(&report)?;
    let mut session = GateSession::new(task, mode, &report, &plan_json);
    session.repo_name = gate_repo_name(repo, &config);
    session.base_rev = open_kioku_git::commit(repo);
    if let Some(base) = session.base_rev.as_deref() {
        session.preexisting_changes = gate_changed_since(repo, base)?;
    }
    // The plan is built, so the new session can start: only now does a replaced session end.
    if let Some(active) = &active {
        gate_store.finish(active)?;
    }
    gate_store.start(&session, &plan_json)?;
    let output = GateStartOutput {
        session_id: session.id.clone(),
        task: session.task.clone(),
        mode,
        base_rev: session.base_rev.clone(),
        risk_level: session.risk_level.clone(),
        confidence_summary: session.confidence_summary.clone(),
        allowed: session.boundary.allowed.clone(),
        caution: session.boundary.caution.clone(),
        forbidden_rule_count: session.boundary.forbidden_rules.len()
            + session.boundary.forbidden.len(),
        plan_path: gate_store.plan_path(),
        session_path: gate_store.session_path(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    println!(
        "Gate session {} started ({} mode)",
        output.session_id,
        mode.as_str()
    );
    println!("Task: {}", output.task);
    println!("Risk {} · {}", output.risk_level, output.confidence_summary);
    println!("Allowed ({}):", output.allowed.len());
    for path in &output.allowed {
        println!("  {path}");
    }
    if !output.caution.is_empty() {
        println!("Caution ({}):", output.caution.len());
        for path in &output.caution {
            println!("  {path}");
        }
    }
    println!(
        "Forbidden rules: {} (secrets, generated code, dependencies, the index)",
        output.forbidden_rule_count
    );
    println!();
    match mode {
        GateMode::Enforce => println!(
            "Edits outside the boundary are denied until widened: ok gate expand <path> --reason \"<why>\""
        ),
        GateMode::Warn => println!(
            "Edits outside the boundary are allowed and reported; verification will fail unless widened: ok gate expand <path> --reason \"<why>\""
        ),
        GateMode::Observe => println!("Observe mode: every edit is recorded, none is denied."),
    }
    println!("When the change is done: ok gate finish");
    Ok(())
}

fn gate_status(json: bool, repo: &Path) -> anyhow::Result<()> {
    let gate_store = GateStore::for_repo(repo);
    let session = gate_store.active_session()?;
    let (events, _) = if session.is_some() {
        gate_store.events()?
    } else {
        (Vec::new(), 0)
    };
    let count = |kinds: &[EventKind]| events.iter().filter(|e| kinds.contains(&e.kind)).count();
    let output = GateStatusOutput {
        active: session.is_some(),
        edits_reported: count(&[
            EventKind::EditAllowed,
            EventKind::EditCaution,
            EventKind::EditWarned,
            EventKind::EditDenied,
        ]),
        edits_denied: count(&[EventKind::EditDenied]),
        edits_warned: count(&[EventKind::EditWarned]),
        session,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    match &output.session {
        None => println!("No active gate session. Start one: ok gate start \"<task>\""),
        Some(session) => {
            println!(
                "Gate session {} ({} mode), started {}",
                session.id,
                session.mode.as_str(),
                session.started_at.format("%Y-%m-%d %H:%M UTC")
            );
            println!("Task: {}", session.task);
            println!(
                "Boundary: {} allowed, {} caution, {} expanded, {} forbidden rule(s)",
                session.boundary.allowed.len(),
                session.boundary.caution.len(),
                session.boundary.expansions.len(),
                session.boundary.forbidden_rules.len() + session.boundary.forbidden.len()
            );
            for expansion in &session.boundary.expansions {
                println!("  expanded {}: {}", expansion.path, expansion.reason);
            }
            println!(
                "Edits reported: {} ({} warned, {} denied)",
                output.edits_reported, output.edits_warned, output.edits_denied
            );
        }
    }
    Ok(())
}

fn require_active_session(gate_store: &GateStore) -> anyhow::Result<GateSession> {
    gate_store.active_session()?.ok_or_else(|| {
        caller_input_error("no active gate session; start one with `ok gate start \"<task>\"`")
    })
}

/// `path` as the deepest existing ancestor's canonical form plus the rest, so a path under a
/// symlinked directory (`/tmp` on macOS, a linked home) compares equal to the canonical
/// repository root even when the file does not exist yet.
fn gate_canonical_lossy(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            let mut out = canonical;
            for component in rest.iter().rev() {
                out.push(component);
            }
            return out;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// `path` relative to `repo`: lexically first, then with both sides canonicalized, so a
/// symlink on either side does not read as "outside the repository".
fn gate_relative(repo: &Path, path: &Path) -> Option<String> {
    open_kioku_gate::repo_relative(repo, path).or_else(|| {
        let repo = gate_canonical_lossy(repo);
        let path = gate_canonical_lossy(path);
        open_kioku_gate::repo_relative(&repo, &path)
    })
}

fn gate_relative_path(repo: &Path, path: &Path) -> anyhow::Result<String> {
    gate_relative(repo, path).ok_or_else(|| {
        caller_input_error(format!(
            "{} is not inside the repository {}",
            path.display(),
            repo.display()
        ))
    })
}

fn gate_check(json: bool, repo: &Path, path: &Path) -> anyhow::Result<()> {
    let gate_store = GateStore::for_repo(repo);
    let session = require_active_session(&gate_store)?;
    let relative = gate_relative_path(repo, path)?;
    let decision = session.decide(&relative);
    if json {
        println!("{}", serde_json::to_string_pretty(&decision)?);
    } else {
        println!(
            "{} [{}] {:?}: {}",
            decision.path,
            decision.class.as_str(),
            decision.action,
            decision.reason
        );
    }
    if decision.action == HookAction::Deny {
        anyhow::bail!("`{}` would be denied", decision.path);
    }
    Ok(())
}

fn gate_expand(json: bool, repo: &Path, path: &Path, reason: &str) -> anyhow::Result<()> {
    let gate_store = GateStore::for_repo(repo);
    let mut session = require_active_session(&gate_store)?;
    let relative = gate_relative_path(repo, path)?;
    gate_store.expand(&mut session, &relative, reason)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&session.boundary.expansions)?
        );
    } else {
        println!(
            "Boundary widened: {relative} ({}). Recorded in the dossier.",
            reason.trim()
        );
    }
    Ok(())
}

/// Every path whose working-tree content differs from `base`: tracked files changed since it,
/// committed or not, and untracked files git does not ignore. The gate's own `.ok/` is
/// never counted.
fn gate_changed_since(repo: &Path, base: &str) -> anyhow::Result<Vec<String>> {
    let mut paths = open_kioku_git::changed_paths_since_commit(repo, base)?
        .iter()
        .map(open_kioku_gate::normalize)
        .filter(|path| !path.starts_with(".ok/"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Untracked files git does not ignore, repository-relative, without the gate's own `.ok/`.
fn gate_untracked(repo: &Path) -> anyhow::Result<Vec<String>> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.quotePath=false"])
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git ls-files --others failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output
        .stdout
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .map(|entry| open_kioku_gate::normalize_str(&String::from_utf8_lossy(entry)))
        .filter(|path| !path.starts_with(".ok/"))
        .collect())
}

/// The unified diff of every tracked change since `base`, committed or not, with the flags
/// `ok verify --git` uses. Renames are paired, so a file moved out of a forbidden directory
/// is judged with both of its paths.
fn gate_git_diff(repo: &Path, base: &str) -> anyhow::Result<String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.quotePath=false"])
        .args([
            "diff",
            "--unified=0",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--find-renames",
            "--submodule=short",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--relative",
            "--end-of-options",
            base,
            "--",
        ])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git diff {base} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

/// Split a unified diff into its `diff --git` entries.
fn gate_diff_entries(diff: &str) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    for line in diff.split_inclusive('\n') {
        if line.starts_with("diff --git ") || entries.is_empty() {
            entries.push(String::new());
        }
        entries
            .last_mut()
            .expect("an entry was just pushed")
            .push_str(line);
    }
    entries.into_iter().filter(|e| !e.trim().is_empty()).collect()
}

/// What the gate knows about the session's change: in a git repository, every tracked change
/// since the session's base revision (committed during the session or not) and every
/// untracked file, less any path that was already dirty when the session started and that
/// no hook reported editing since; elsewhere, the paths the hooks reported.
fn gate_change_input(
    repo: &Path,
    session: &GateSession,
    events: &[GateEvent],
) -> anyhow::Result<ChangeInput> {
    let head_rev = open_kioku_git::commit(repo);
    let reported = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                EventKind::EditAllowed | EventKind::EditCaution | EventKind::EditWarned
            )
        })
        .filter_map(|event| event.path.clone())
        .collect::<BTreeSet<_>>();
    let Some(head) = head_rev.as_deref() else {
        let changed = reported
            .into_iter()
            .filter(|path| repo.join(path).exists())
            .collect::<Vec<_>>();
        return Ok(ChangeInput {
            changed_files: changed,
            untracked_files: Vec::new(),
            preexisting_excluded: Vec::new(),
            unified_diff: None,
            head_rev,
            source: "edits reported by the hooks (not a git repository)".to_string(),
        });
    };
    let mut source = String::from("git diff against the session's base revision plus untracked files, \
                                   less changes that predate the session");
    let base = match session.base_rev.as_deref() {
        Some(base) if base != head => {
            source.push_str(&format!(
                "; {} commit(s) made during the session are included",
                gate_commit_count(repo, base, head)
            ));
            base.to_string()
        }
        Some(base) => base.to_string(),
        None => head.to_string(),
    };
    let diff = match gate_git_diff(repo, &base) {
        Ok(diff) => diff,
        Err(err) if base != head => {
            // The base is gone (a rebase or an amend rewrote it): the current HEAD is the only
            // comparison left, and the dossier says so.
            source = format!(
                "git diff against HEAD plus untracked files; the session's base revision \
                 {} is no longer in the repository ({err}), so commits made during the session \
                 are not included",
                &base[..base.len().min(10)]
            );
            gate_git_diff(repo, head)?
        }
        Err(err) => return Err(err),
    };
    let preexisting = session
        .preexisting_changes
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let excluded = |path: &String| preexisting.contains(path) && !reported.contains(path);

    let mut kept_diff = String::new();
    let mut preexisting_excluded = Vec::new();
    for entry in gate_diff_entries(&diff) {
        let paths = open_kioku_patch::changed_files_from_unified_diff(&entry)
            .iter()
            .map(open_kioku_gate::normalize)
            .collect::<Vec<_>>();
        if !paths.is_empty() && paths.iter().all(excluded) {
            preexisting_excluded.extend(paths);
        } else {
            kept_diff.push_str(&entry);
        }
    }
    let (excluded_untracked, untracked): (Vec<_>, Vec<_>) =
        gate_untracked(repo)?.into_iter().partition(excluded);
    preexisting_excluded.extend(excluded_untracked);
    preexisting_excluded.sort();
    preexisting_excluded.dedup();

    let mut changed = open_kioku_patch::changed_files_from_unified_diff(&kept_diff)
        .iter()
        .map(open_kioku_gate::normalize)
        .filter(|path| !path.starts_with(".ok/"))
        .collect::<BTreeSet<_>>();
    changed.extend(untracked.iter().cloned());
    Ok(ChangeInput {
        changed_files: changed.into_iter().collect(),
        untracked_files: untracked,
        preexisting_excluded,
        unified_diff: (!kept_diff.trim().is_empty()).then_some(kept_diff),
        head_rev,
        source,
    })
}

fn gate_commit_count(repo: &Path, base: &str, head: &str) -> usize {
    ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-list", "--count", &format!("{base}..{head}")])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
        .unwrap_or(0)
}

struct GateRun {
    dossier: ChangeDossier,
    dossier_json: PathBuf,
    dossier_markdown: PathBuf,
}

/// Verify the change against the session and write the dossier. Shared by `finish` and the
/// stop hook, so a dossier the hook wrote is the one `finish` would write.
fn gate_run_verification(
    repo: &Path,
    gate_store: &GateStore,
    session: &GateSession,
    run_commands: bool,
) -> anyhow::Result<GateRun> {
    let (events, malformed) = gate_store.events()?;
    let change = gate_change_input(repo, session, &events)?;
    let plan = plan_with_expansions(gate_store.saved_plan()?, session);
    let verification = if change.changed_files.is_empty() {
        None
    } else {
        let store = open_store(repo)?;
        let index_dir = default_index_dir(repo);
        let search_index = if TantivySearchIndex::exists(&index_dir) {
            Some(TantivySearchIndex::open_or_create(&index_dir)?)
        } else {
            None
        };
        let architecture_policy = load_architecture_policy(repo)?;
        let check_dependency_delta = architecture_policy.is_some();
        let contract_store =
            run_commands.then(|| FsContractStore::new(repo.join(".ok/contracts")));
        Some(
            ChangeVerifier::new(&store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_contract_store(
                    contract_store
                        .as_ref()
                        .map(|store| store as &dyn ContractStore),
                )
                .verify(
                    repo,
                    &plan,
                    VerifyChangeInput {
                        changed_files: change
                            .changed_files
                            .iter()
                            .map(PathBuf::from)
                            .collect(),
                        unified_diff: change.unified_diff.clone(),
                        run_commands,
                        write_attestation: run_commands,
                        check_dependency_delta,
                        architecture_policy,
                        ..VerifyChangeInput::default()
                    },
                )?,
        )
    };
    let repo_name = if session.repo_name.is_empty() {
        gate_repo_name(repo, &OkConfig::default())
    } else {
        session.repo_name.clone()
    };
    let dossier = build_dossier(
        &repo_name,
        session,
        &plan,
        &events,
        malformed,
        &change,
        verification.as_ref(),
    );
    let (dossier_json, dossier_markdown) = gate_store.write_dossier(&dossier)?;
    gate_store.append_event(
        &GateEvent::new(&session.id, EventKind::VerifyRun)
            .with_detail(dossier.verdict.label().to_ascii_lowercase()),
    )?;
    Ok(GateRun {
        dossier,
        dossier_json,
        dossier_markdown,
    })
}

fn gate_finish(
    json: bool,
    repo: &Path,
    run_commands: bool,
    format: GateFormat,
    keep: bool,
) -> anyhow::Result<()> {
    let gate_store = GateStore::for_repo(repo);
    let session = require_active_session(&gate_store)?;
    let run = gate_run_verification(repo, &gate_store, &session, run_commands)?;
    if !keep {
        gate_store.finish(&session)?;
    }
    let output = GateFinishOutput {
        dossier_json: run.dossier_json.clone(),
        dossier_markdown: run.dossier_markdown.clone(),
        session_ended: !keep,
        dossier: run.dossier,
    };
    match (json, format) {
        (true, _) | (_, GateFormat::Json) => {
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        (_, GateFormat::Markdown) => {
            print!("{}", render_markdown(&output.dossier));
            eprintln!(
                "dossier: {}\n         {}",
                output.dossier_json.display(),
                output.dossier_markdown.display()
            );
        }
        (_, GateFormat::Text) => {
            println!("{}", gate_summary_line(&output.dossier));
            println!("dossier: {}", output.dossier_json.display());
        }
    }
    if output.dossier.verdict == DossierVerdict::Fail {
        anyhow::bail!("change dossier verdict is FAIL");
    }
    Ok(())
}

fn gate_abandon(json: bool, repo: &Path) -> anyhow::Result<()> {
    let gate_store = GateStore::for_repo(repo);
    let session = require_active_session(&gate_store)?;
    gate_store.finish(&session)?;
    print_text_or_json(
        json,
        &format!("Gate session {} abandoned; nothing was verified.", session.id),
        &serde_json::json!({"abandoned": session.id}),
    )
}

fn gate_summary_line(dossier: &ChangeDossier) -> String {
    let violations = dossier
        .verification
        .as_ref()
        .map(|v| v.boundary_violations.len())
        .unwrap_or(0);
    format!(
        "Change Dossier {}: {} changed file(s), {} boundary violation(s), {} edit(s) denied, {} warned",
        dossier.verdict.label(),
        dossier.change.changed_files.len(),
        violations,
        dossier.trail.edits_denied,
        dossier.trail.edits_warned
    )
}

/// The repository a hook serves. An explicit `--repo` wins; with the default `.`, the
/// harness's project directory (`CLAUDE_PROJECT_DIR`), then the git root of the `cwd` it
/// reports, then `.` itself. Always absolute.
fn gate_hook_repo(repo: &Path, input: &HookInput) -> anyhow::Result<PathBuf> {
    if repo != Path::new(".") {
        return Ok(std::path::absolute(repo)?);
    }
    if let Ok(dir) = std::env::var("CLAUDE_PROJECT_DIR") {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    if !input.cwd.is_empty() {
        let cwd = PathBuf::from(&input.cwd);
        if let Ok(root) = open_kioku_git::discover_root(&cwd) {
            return Ok(root);
        }
        return Ok(cwd);
    }
    Ok(std::path::absolute(repo)?)
}

fn gate_emit(response: HookResponse) -> anyhow::Result<()> {
    if let Some(value) = response.to_json() {
        println!("{}", serde_json::to_string(&value)?);
    }
    Ok(())
}

fn gate_hook(repo: &Path, event: GateHookEvent) -> anyhow::Result<()> {
    let input = HookInput::parse(std::io::stdin().lock()).unwrap_or_default();
    let repo = gate_hook_repo(repo, &input)?;
    let gate_store = GateStore::for_repo(&repo);
    let session = gate_store.active_session()?;
    match event {
        GateHookEvent::PreTool => gate_hook_pre_tool(&repo, &gate_store, session, &input),
        GateHookEvent::PostTool => gate_hook_post_tool(&repo, &gate_store, session, &input),
        GateHookEvent::Stop => gate_hook_stop(&repo, &gate_store, session, &input),
        GateHookEvent::Prompt | GateHookEvent::SessionStart => {
            let event_name = if event == GateHookEvent::Prompt {
                "UserPromptSubmit"
            } else {
                "SessionStart"
            };
            let text = match &session {
                Some(session) => format!(
                    "Open Kioku gate session is active ({} mode) for task \"{}\": {} allowed \
                     path(s), {} caution, {} widened. Edits are held to that boundary; widen it \
                     with `ok gate expand <path> --reason \"<why>\"`, finish with `ok gate finish`.",
                    session.mode.as_str(),
                    session.task,
                    session.boundary.allowed.len(),
                    session.boundary.caution.len(),
                    session.boundary.expansions.len()
                ),
                None => {
                    let config = OkConfig::load_from_repo(&repo).unwrap_or_default();
                    if config.gate.require_plan {
                        "Open Kioku gate: no session is active and this repository requires \
                         one (`[gate] require_plan = true`). Source edits are denied until \
                         `ok gate start \"<task>\"` declares a plan and boundary."
                            .to_string()
                    } else {
                        "Open Kioku gate: no session is active for this repository. Before \
                         editing source for a task, `ok gate start \"<task>\"` declares a plan \
                         and boundary that every edit is then held to, and `ok gate finish` \
                         writes the verified Change Dossier."
                            .to_string()
                    }
                }
            };
            gate_emit(HookResponse::Context {
                event: event_name.into(),
                text,
            })
        }
    }
}

/// Decide the paths an edit tool writes. A path outside the repository is judged by mode: a
/// session cannot be widened to it.
fn gate_hook_decisions(
    repo: &Path,
    session: &GateSession,
    input: &HookInput,
) -> Vec<PathDecision> {
    input
        .edited_paths()
        .iter()
        .map(|path| match gate_relative(repo, path) {
            Some(relative) => session.decide(&relative),
            None => {
                let display = open_kioku_gate::normalize(path);
                let (action, reason) = match session.mode {
                    GateMode::Observe => (HookAction::Allow, "observe mode".to_string()),
                    GateMode::Warn => (
                        HookAction::Warn,
                        format!("`{display}` is outside the repository the gate session covers"),
                    ),
                    GateMode::Enforce => (
                        HookAction::Deny,
                        format!(
                            "`{display}` is outside the repository the gate session covers; \
                             the boundary cannot be widened to it"
                        ),
                    ),
                };
                PathDecision {
                    path: display,
                    class: open_kioku_gate::PathClass::Outside,
                    action,
                    reason,
                }
            }
        })
        .collect()
}

/// Decide the repository files a shell command names as write targets (redirections, `tee`,
/// `sed -i`, `cp`/`mv` destinations, `touch`, `rm`, `git mv`/`git rm`). A target outside the
/// repository (`/tmp`, `/dev/null`) is not the gate's concern and is skipped; what the
/// command writes that this cannot see, the post-tool hook reads from the tree.
fn gate_shell_decisions(repo: &Path, session: &GateSession, command: &str) -> Vec<PathDecision> {
    shell_write_targets(command)
        .iter()
        .filter_map(|target| {
            let path = Path::new(target);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                repo.join(path)
            };
            gate_relative(repo, &path)
        })
        .map(|relative| {
            let mut decision = session.decide(&relative);
            if decision.action == HookAction::Deny {
                decision.reason.push_str(
                    " Use the Edit or Write tool for repository files so each path is judged \
                     before it changes, or write scratch output outside the repository.",
                );
            }
            decision
        })
        .collect()
}

/// A snapshot id for one shell tool call, so the post-tool hook can pair with the pre-tool one.
fn gate_snapshot_id(input: &HookInput) -> String {
    input
        .tool_use_id
        .clone()
        .filter(|id| !id.is_empty())
        .map(|id| {
            id.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect()
        })
        .unwrap_or_else(|| {
            open_kioku_gate::sha256_hex(input.tool_input.to_string().as_bytes())[..16].to_string()
        })
}

fn gate_hook_pre_tool(
    repo: &Path,
    gate_store: &GateStore,
    session: Option<GateSession>,
    input: &HookInput,
) -> anyhow::Result<()> {
    if !input.is_edit_tool() && !input.is_shell_tool() {
        return Ok(());
    }
    let Some(session) = session else {
        let config = OkConfig::load_from_repo(repo).unwrap_or_default();
        if config.gate.require_plan && input.is_edit_tool() {
            return gate_emit(HookResponse::Deny {
                event: "PreToolUse".into(),
                reason: "This repository requires a gate session before source edits \
                         (`[gate] require_plan = true`). Run `ok gate start \"<task>\"` to \
                         declare the plan and boundary, then retry the edit."
                    .into(),
            });
        }
        return Ok(());
    };
    let decisions = if input.is_edit_tool() {
        gate_hook_decisions(repo, &session, input)
    } else {
        // Remember what was dirty before the command, so the post-tool hook can tell what
        // it changed, however it changed it.
        if let Some(base) = session.base_rev.as_deref() {
            if let Ok(dirty) = gate_changed_since(repo, base) {
                gate_store.write_snapshot(&gate_snapshot_id(input), &dirty)?;
            }
        }
        gate_shell_decisions(repo, &session, input.shell_command().unwrap_or_default())
    };
    for decision in &decisions {
        gate_store.append_event(&GateEvent::for_decision(
            &session.id,
            decision,
            &input.tool_name,
        ))?;
    }
    let denied = decisions
        .iter()
        .filter(|d| d.action == HookAction::Deny)
        .map(|d| d.reason.clone())
        .collect::<Vec<_>>();
    if denied.is_empty() {
        return Ok(());
    }
    gate_emit(HookResponse::Deny {
        event: "PreToolUse".into(),
        reason: format!(
            "Open Kioku gate denied this {}. {}",
            if input.is_shell_tool() {
                "command"
            } else {
                "edit"
            },
            denied.join(" ")
        ),
    })
}

fn gate_hook_post_tool(
    repo: &Path,
    gate_store: &GateStore,
    session: Option<GateSession>,
    input: &HookInput,
) -> anyhow::Result<()> {
    let Some(session) = session else {
        return Ok(());
    };
    if input.is_edit_tool() {
        let warned = gate_hook_decisions(repo, &session, input)
            .into_iter()
            .filter(|d| d.action == HookAction::Warn)
            .map(|d| d.reason)
            .collect::<Vec<_>>();
        if warned.is_empty() {
            return Ok(());
        }
        return gate_emit(HookResponse::Context {
            event: "PostToolUse".into(),
            text: format!("Open Kioku gate: {}", warned.join(" ")),
        });
    }
    if !input.is_shell_tool() {
        return Ok(());
    }
    // Files the command changed that the pre-tool hook could not name: new dirty paths
    // against the snapshot it took. Recorded like any edit; the agent hears about those
    // outside the boundary.
    let Some(base) = session.base_rev.as_deref() else {
        return Ok(());
    };
    let before = gate_store.take_snapshot(&gate_snapshot_id(input))?;
    let Some(before) = before else {
        return Ok(());
    };
    let before = before.into_iter().collect::<BTreeSet<_>>();
    let named = shell_write_targets(input.shell_command().unwrap_or_default())
        .iter()
        .filter_map(|t| gate_relative(repo, &repo.join(t)))
        .collect::<BTreeSet<_>>();
    let mut notices = Vec::new();
    for path in gate_changed_since(repo, base)? {
        if before.contains(&path) || named.contains(&path) {
            continue;
        }
        // The write already happened, so nothing is denied here: what enforce mode would have
        // refused is recorded as warned, and the agent is told.
        let mut decision = session.decide(&path);
        if decision.action == HookAction::Deny {
            decision.action = HookAction::Warn;
        }
        gate_store.append_event(&GateEvent::for_decision(
            &session.id,
            &decision,
            &input.tool_name,
        ))?;
        if decision.action != HookAction::Allow {
            notices.push(format!(
                "the command changed `{}` ({}): {}",
                decision.path,
                decision.class.as_str(),
                decision.reason
            ));
        }
    }
    if notices.is_empty() {
        return Ok(());
    }
    gate_emit(HookResponse::Context {
        event: "PostToolUse".into(),
        text: format!(
            "Open Kioku gate: {} Verification at finish will hold the change to the boundary.",
            notices.join(" ")
        ),
    })
}

fn gate_hook_stop(
    repo: &Path,
    gate_store: &GateStore,
    session: Option<GateSession>,
    input: &HookInput,
) -> anyhow::Result<()> {
    let Some(session) = session else {
        return Ok(());
    };
    let run = gate_run_verification(repo, gate_store, &session, false)?;
    let dossier = &run.dossier;
    let summary = gate_summary_line(dossier);
    if dossier.verdict == DossierVerdict::Fail && !input.stop_hook_active {
        let mut reason = format!(
            "Open Kioku gate: the change does not verify against the declared boundary. {summary}."
        );
        if let Some(verification) = &dossier.verification {
            for finding in verification.boundary_violations.iter().take(8) {
                reason.push_str(&format!(
                    "\n- {} [{}]: {}",
                    finding.path.as_deref().unwrap_or("-"),
                    finding.kind,
                    finding.reason
                ));
            }
        }
        reason.push_str(
            "\nEither revert the edits outside the boundary, or widen it with a reason for each \
             path (`ok gate expand <path> --reason \"<why>\"`), then stop again. The dossier is at ",
        );
        reason.push_str(&run.dossier_markdown.display().to_string());
        reason.push('.');
        return gate_emit(HookResponse::Block { reason });
    }
    gate_emit(HookResponse::Context {
        event: "Stop".into(),
        text: format!(
            "Open Kioku gate: {summary}. Dossier: {}",
            run.dossier_markdown.display()
        ),
    })
}

/// The hook entries `ok gate install claude` writes. Each command is the `ok` binary on PATH,
/// the same assumption the plugin's MCP server entry makes.
fn gate_claude_hooks() -> serde_json::Value {
    let command = |event: &str, timeout: u64| {
        serde_json::json!({
            "type": "command",
            "command": format!("ok gate hook {event}"),
            "timeout": timeout
        })
    };
    serde_json::json!({
        "PreToolUse": [{
            "matcher": "Edit|Write|MultiEdit|NotebookEdit|Bash",
            "hooks": [command("pre-tool", 30)]
        }],
        "PostToolUse": [{
            "matcher": "Edit|Write|MultiEdit|NotebookEdit|Bash",
            "hooks": [command("post-tool", 30)]
        }],
        "Stop": [{
            "hooks": [command("stop", 600)]
        }],
        "UserPromptSubmit": [{
            "hooks": [command("prompt", 10)]
        }],
        "SessionStart": [{
            "hooks": [command("session-start", 10)]
        }]
    })
}

/// Merge the gate hooks into a settings object. An event that already carries a gate hook
/// (any command starting `ok gate hook`) is left as it is, so applying twice changes nothing.
fn merge_gate_hooks(settings: &mut serde_json::Value) -> usize {
    if !settings.is_object() {
        *settings = serde_json::json!({});
    }
    let hooks = settings
        .as_object_mut()
        .expect("settings is an object")
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        *hooks = serde_json::json!({});
    }
    let hooks = hooks.as_object_mut().expect("hooks is an object");
    let mut added = 0usize;
    for (event, entries) in gate_claude_hooks()
        .as_object()
        .expect("gate hooks is an object")
    {
        let existing = hooks
            .entry(event.clone())
            .or_insert_with(|| serde_json::json!([]));
        if !existing.is_array() {
            *existing = serde_json::json!([]);
        }
        let list = existing.as_array_mut().expect("event list is an array");
        let already = list.iter().any(|entry| {
            entry
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|h| {
                    h.iter().any(|hook| {
                        hook.get("command")
                            .and_then(|c| c.as_str())
                            .is_some_and(|c| c.starts_with("ok gate hook"))
                    })
                })
        });
        if already {
            continue;
        }
        for entry in entries.as_array().expect("entries is an array") {
            list.push(entry.clone());
            added += 1;
        }
    }
    added
}

fn gate_install(json: bool, repo: &Path, client: GateClient, apply: bool) -> anyhow::Result<()> {
    let GateClient::Claude = client;
    let path = repo.join(".claude").join("settings.json");
    let mut settings = if path.exists() {
        let text = fs::read_to_string(&path)?;
        serde_json::from_str::<serde_json::Value>(&text).map_err(|err| {
            caller_input_error(format!("{} is not valid JSON: {err}", path.display()))
        })?
    } else {
        serde_json::json!({})
    };
    let added = merge_gate_hooks(&mut settings);
    let rendered = serde_json::to_string_pretty(&settings)?;
    if apply {
        if added > 0 {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, format!("{rendered}\n"))?;
        }
        print_text_or_json(
            json,
            &if added > 0 {
                format!(
                    "Wrote {} gate hook entr{} to {}.\nThey run `ok gate hook <event>`, so `ok` must be on PATH for Claude Code.\nStart a session: ok gate start \"<task>\"",
                    added,
                    if added == 1 { "y" } else { "ies" },
                    path.display()
                )
            } else {
                format!(
                    "{} already carries the gate hooks; nothing changed.",
                    path.display()
                )
            },
            &serde_json::json!({"path": path, "added": added, "applied": true}),
        )
    } else {
        print_text_or_json(
            json,
            &format!(
                "Proposed {} ({} entr{} to add; nothing written without --apply):\n{rendered}",
                path.display(),
                added,
                if added == 1 { "y" } else { "ies" }
            ),
            &serde_json::json!({"path": path, "added": added, "applied": false, "settings": settings}),
        )
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    #[test]
    fn gate_hooks_merge_idempotently_and_keep_foreign_hooks() {
        let mut settings = serde_json::json!({
            "permissions": {"allow": ["Bash(ls)"]},
            "hooks": {
                "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo hi"}]}]
            }
        });
        let added = merge_gate_hooks(&mut settings);
        assert_eq!(added, 5);
        assert_eq!(settings["permissions"]["allow"][0], "Bash(ls)");
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        assert_eq!(merge_gate_hooks(&mut settings), 0);
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn gate_hooks_replace_a_non_object_settings_value() {
        let mut settings = serde_json::json!("junk");
        assert_eq!(merge_gate_hooks(&mut settings), 5);
        assert!(settings["hooks"]["UserPromptSubmit"].is_array());
    }

    #[test]
    fn gate_diff_entries_split_on_each_git_header() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/y b/y\nnew file mode 100644\n--- /dev/null\n+++ b/y\n@@ -0,0 +1 @@\n+y\n";
        let entries = gate_diff_entries(diff);
        assert_eq!(entries.len(), 2);
        assert!(entries[0].starts_with("diff --git a/x"));
        assert!(entries[1].starts_with("diff --git a/y"));
        assert_eq!(entries.concat(), diff);
        assert!(gate_diff_entries("").is_empty());
    }

    #[test]
    fn gate_canonical_lossy_keeps_a_missing_tail() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let missing = dir.path().join("not").join("yet.rs");
        assert_eq!(gate_canonical_lossy(&missing), real.join("not").join("yet.rs"));
    }
}
