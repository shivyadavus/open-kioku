use open_kioku_gate::{
    build_dossier, plan_with_expansions, plan_with_session_caution, render_markdown,
    shell_write_targets, BackgroundMarker, ChangeDossier, ChangeInput, DossierVerdict, EventKind,
    GateEvent, GateMode, GateSession, GateStore, HookAction, HookInput, HookResponse, LastVerify,
    PathDecision, EMPTY_TREE_REV,
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
        task: Option<String>,
        /// How edits outside the boundary are treated: enforce (deny), warn (allow and tell
        /// the agent), or observe (record only). Defaults to `[gate] mode` in ok.toml.
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,
        /// Maximum context results the plan is built from.
        #[arg(long, default_value_t = 12)]
        limit: usize,
        /// Replace an active session instead of refusing to start over it. The new plan is
        /// built first; the old session ends only once the new one can start, and its event
        /// trail is kept as `.ok/gate/events-<session>.jsonl`.
        #[arg(long, default_value_t = false)]
        force: bool,
        /// Started by the prompt hook: the task and its baseline are read from the starting
        /// marker this nonce names, never from the command line.
        #[arg(long, value_name = "NONCE", hide = true)]
        auto_nonce: Option<String>,
    },
    /// Index the repository for the hooks, in the background, and clear the indexing marker
    /// however it ends.
    #[command(hide = true)]
    AutoIndex,
    /// Judge a finished change with no hooks: plan from the task text, verify every change
    /// since `--base`, print the dossier, exit non-zero on FAIL. For CI on any pull request,
    /// whatever wrote it. A forbidden path fails; a file outside the planned boundary is
    /// listed, and fails only under `--mode enforce`.
    #[command(after_help = "Examples:
  ok gate ci --base origin/main --task \"$PR_TITLE\"
  ok gate ci --base HEAD~1 --task \"add retry handling\" --format json
  ok gate ci --base origin/main --task \"$PR_TITLE\" --write dossier.md")]
    Ci {
        /// The revision the change is read against (a commit, branch, or `origin/main`).
        #[arg(long, value_name = "REV")]
        base: String,
        /// What the change set out to do, in natural language (a pull request's title and body).
        /// It may begin with a dash.
        #[arg(long, value_name = "TEXT", allow_hyphen_values = true)]
        task: String,
        /// `enforce` makes the planned boundary binding: a changed file outside it fails. In
        /// `warn` (the default) and `observe` such a file is listed and the run does not fail.
        /// Without this flag the mode is `[gate] mode` of ok.toml as the base revision has it.
        #[arg(long, value_name = "MODE")]
        mode: Option<String>,
        /// Maximum context results the plan is built from.
        #[arg(long, default_value_t = 12)]
        limit: usize,
        /// Execute the plan's validation commands locally and record their exit codes.
        #[arg(long, default_value_t = false)]
        run_commands: bool,
        /// Output format.
        #[arg(long, value_enum, default_value_t = GateFormat::Markdown)]
        format: GateFormat,
        /// Also write the Markdown dossier to this path (for a pull request comment).
        #[arg(long, value_name = "PATH")]
        write: Option<PathBuf>,
    },
    /// Show the active session: task, mode, boundary, and the edits reported so far.
    Status,
    /// Decide one path against the active boundary, the way the edit hook would.
    Check {
        /// Repository-relative or absolute path. A relative path that exists only from the
        /// current directory is read from there.
        path: PathBuf,
    },
    /// Widen the active boundary by one path, recording the reason in the dossier.
    Expand {
        /// Repository-relative or absolute path to admit. A relative path that exists only
        /// from the current directory is read from there.
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
    /// When the harness conversation ends: write the dossier and end its session.
    SessionEnd,
    /// From git's pre-commit hook: refuse a commit that stages a forbidden path, or, in a
    /// binding `enforce` session, a path outside its boundary. Reads no input.
    PreCommit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum GateClient {
    /// Claude Code: hook entries in `.claude/settings.json`.
    Claude,
    /// Git itself: a `pre-commit` hook, which every agent and every person commits through.
    Git,
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
        if event == GateHookEvent::PreCommit {
            return gate_hook_pre_commit(&gate_command_repo(repo)?);
        }
        return gate_hook(repo, event);
    }
    let repo = gate_command_repo(repo)?;
    let repo = repo.as_path();
    match command {
        GateCommand::Start {
            task,
            mode,
            limit,
            force,
            auto_nonce,
        } => match auto_nonce {
            Some(nonce) => {
                // However planning ends, this run's marker is spent; a newer prompt's is not.
                let result = gate_start_auto(repo, &nonce, limit);
                GateStore::for_repo(repo).clear_starting_if(&nonce);
                result
            }
            None => {
                let task = task.ok_or_else(|| {
                    caller_input_error("a task is required: ok gate start \"<task>\"")
                })?;
                gate_start(json, repo, &task, mode.as_deref(), limit, force)
            }
        },
        GateCommand::AutoIndex => gate_auto_index_run(repo),
        GateCommand::Ci {
            base,
            task,
            mode,
            limit,
            run_commands,
            format,
            write,
        } => gate_ci(
            json,
            repo,
            &base,
            &task,
            mode.as_deref(),
            limit,
            run_commands,
            format,
            write.as_deref(),
        ),
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

/// The repository a non-hook subcommand works on: an explicit `--repo` as given, else the git
/// top level above the current directory (so `ok gate expand` works from the subdirectory a
/// shell left the agent in), else the current directory. Always absolute.
fn gate_command_repo(repo: &Path) -> anyhow::Result<PathBuf> {
    if repo != Path::new(".") {
        return Ok(std::path::absolute(repo)?);
    }
    let cwd = std::env::current_dir()?;
    Ok(gate_git_toplevel(&cwd).unwrap_or(cwd))
}

/// The git working tree's top level above `dir`, when `dir` is inside one.
fn gate_git_toplevel(dir: &Path) -> Option<PathBuf> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// Whether `repo` is inside a git working tree. `discover_root` falls back to the directory
/// itself, so it cannot answer this.
fn gate_in_git(repo: &Path) -> bool {
    gate_git_toplevel(repo).is_some()
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

/// The revision a session's change is read against: `HEAD`, or the empty tree in a git
/// repository with no commits yet. `None` outside a git repository.
fn gate_base_rev(repo: &Path) -> Option<String> {
    open_kioku_git::commit(repo)
        .or_else(|| gate_in_git(repo).then(|| EMPTY_TREE_REV.to_string()))
}

/// The plan `ok plan` would build for `task`, and its JSON.
fn gate_build_plan(
    repo: &Path,
    config: &OkConfig,
    task: &str,
    limit: usize,
) -> anyhow::Result<(PlanReport, Vec<u8>)> {
    let store = open_store(repo)?;
    require_authoritative_relationships(&store)?;
    let context = build_context_pack_with_config(repo, &store, task, limit, config)?;
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
    Ok((report, plan_json))
}

fn gate_mode_from(mode: Option<&str>, config: &OkConfig) -> anyhow::Result<GateMode> {
    match mode {
        Some(value) => GateMode::parse(value).ok_or_else(|| {
            caller_input_error(format!(
                "--mode must be enforce, warn, or observe, not `{value}`"
            ))
        }),
        None => Ok(GateMode::parse(&config.gate.mode).unwrap_or_default()),
    }
}

/// A session for `task` from its plan, with the repository's settings applied.
fn gate_new_session(
    repo: &Path,
    config: &OkConfig,
    task: &str,
    mode: GateMode,
    report: &PlanReport,
    plan_json: &[u8],
) -> GateSession {
    let mut session = GateSession::new(task, mode, report, plan_json);
    session.repo_name = gate_repo_name(repo, config);
    session.boundary.sibling_caution = config.gate.sibling_caution;
    session
}

fn gate_trail_has_edits(events: &[GateEvent]) -> bool {
    events.iter().any(|e| {
        matches!(
            e.kind,
            EventKind::EditAllowed
                | EventKind::EditCaution
                | EventKind::EditWarned
                | EventKind::EditDenied
        )
    })
}

/// End `session` for good: write its dossier first when it did anything, so replacing or
/// closing a session never drops its record. A failed verification is reported and does not
/// keep the session open.
fn gate_close_session(
    repo: &Path,
    gate_store: &GateStore,
    session: &GateSession,
) -> anyhow::Result<()> {
    let (events, _) = gate_store.events().unwrap_or_default();
    let changed = gate_change_input(repo, session, &events)
        .map(|change| !change.changed_files.is_empty())
        .unwrap_or(false);
    // A stop that verified this very state already wrote the dossier: nothing to do again.
    let verified = gate_verify_fingerprint(repo, gate_store, session)
        .ok()
        .zip(gate_store.read_last_verify())
        .is_some_and(|(fingerprint, last)| last.fingerprint == fingerprint);
    if (gate_trail_has_edits(&events) || changed) && !verified {
        if let Err(err) = gate_run_verification(repo, gate_store, session, false) {
            eprintln!(
                "gate: session {} ended without a dossier: {err:#}",
                session.id
            );
        }
    }
    gate_store.finish(session)?;
    Ok(())
}

/// Whether an auto session belongs to the harness conversation `harness` names. Unknown on
/// either side counts as the same: only a known, different conversation replaces a session.
fn gate_same_conversation(session: &GateSession, harness: &str) -> bool {
    match session.harness_session.as_deref() {
        Some(own) if !own.is_empty() && !harness.is_empty() => own == harness,
        _ => true,
    }
}

/// Whether `session` is another conversation's: an auto session started from a prompt of a
/// harness conversation other than the one this hook serves. Such a session is not this
/// conversation's to be judged against, to extend, or to end. A session started by hand
/// binds everyone working in the repository.
fn gate_foreign(session: &GateSession, input: &HookInput) -> bool {
    session.auto && !gate_same_conversation(session, &input.session_id)
}

/// Whether the starting marker is for a prompt of the conversation this hook serves (or of
/// one that cannot be told apart from it).
fn gate_marker_ours(marker: &BackgroundMarker, input: &HookInput) -> bool {
    marker.harness_session.is_empty()
        || input.session_id.is_empty()
        || marker.harness_session == input.session_id
}

/// Whether process `pid` is still running. Where that cannot be told, it is assumed to be:
/// the marker's age then decides.
fn gate_pid_alive(pid: u32) -> bool {
    if Path::new("/proc/self/stat").exists() {
        // The state is the field after the parenthesised command name; `Z` is a process that
        // has exited and only awaits reaping.
        return match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next().map(|state| state != "Z"))
                .unwrap_or(true),
            Err(_) => false,
        };
    }
    #[cfg(unix)]
    {
        ProcessCommand::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(true)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// Whether the background work behind `marker` is over without having cleared it: its
/// process is gone, or, where the process is not known, the marker is older than
/// `unknown_max`. A known, running process is given `known_max`.
fn gate_marker_spent(
    marker: &BackgroundMarker,
    unknown_max: chrono::Duration,
    known_max: chrono::Duration,
) -> bool {
    match marker.pid {
        Some(pid) => !gate_pid_alive(pid) || marker.is_stale(known_max),
        None => marker.is_stale(unknown_max),
    }
}

/// A planner whose process is unknown is given 90 seconds; one that is running, ten minutes.
fn gate_planner_spent(marker: &BackgroundMarker) -> bool {
    gate_marker_spent(
        marker,
        chrono::Duration::seconds(90),
        chrono::Duration::minutes(10),
    )
}

/// For a session started from a prompt, how binding the boundary is follows the repository's
/// configuration as it is now, not as it was when the conversation began: `mode = "enforce"`
/// set mid-conversation binds from the next tool call. A configuration that cannot be read
/// changes nothing here.
fn gate_follow_config(repo: &Path, gate_store: &GateStore, session: &mut GateSession) {
    if !session.auto {
        return;
    }
    let Ok(config) = OkConfig::load_from_repo(repo) else {
        return;
    };
    let mode = GateMode::parse(&config.gate.mode).unwrap_or_default();
    let advisory = mode != GateMode::Enforce;
    let sibling_caution = config.gate.sibling_caution;
    if session.mode != mode
        || session.advisory != advisory
        || session.boundary.sibling_caution != sibling_caution
    {
        session.mode = mode;
        session.advisory = advisory;
        session.boundary.sibling_caution = sibling_caution;
        let _ = gate_store.write_session(session);
    }
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
    // A session asked for by hand wins over one the prompt hook is still planning.
    gate_store.clear_starting();
    let active = gate_store.active_session()?;
    if let Some(active) = &active {
        // An auto session gives way to an explicit start; a manual one needs --force.
        if !force && !active.auto {
            anyhow::bail!(
                "a gate session is already active for task \"{}\" (started {}); finish it with \
                 `ok gate finish`, drop it with `ok gate abandon`, or pass --force",
                active.task,
                active.started_at.format("%Y-%m-%d %H:%M UTC")
            );
        }
    }
    let config = OkConfig::load_from_repo(repo)?;
    let mode = gate_mode_from(mode, &config)?;
    let (report, plan_json) = gate_build_plan(repo, &config, task, limit)?;
    let mut session = gate_new_session(repo, &config, task, mode, &report, &plan_json);
    session.base_rev = gate_base_rev(repo);
    session.preexisting_changes = match &active {
        // Replacing a session: what was dirty before the first session started is still
        // what predates the work; the replaced session's edits are this session's change.
        Some(active) if active.base_rev == session.base_rev => active.preexisting_changes.clone(),
        _ => match session.base_rev.as_deref() {
            Some(base) => gate_changed_since(repo, base)?,
            None => Vec::new(),
        },
    };
    // The plan is built, so the new session can start: only now does a replaced session end.
    // The session that is active now is the one replaced, whichever it was when planning began.
    if let Some(current) = gate_store.active_session()? {
        gate_store.finish(&current)?;
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

/// The planner the prompt hook starts in the background. The task, the conversation and the
/// baseline come from the starting marker `nonce` names; the planner acts only while that
/// marker stands, so a stop, a newer prompt or a manual `ok gate start` cancels it.
///
/// No session: it starts one. An auto session of the same conversation: it widens that
/// session's boundary with this prompt's plan, unless the plan's confidence is Low. An auto
/// session of another conversation: it closes that one, dossier included, and starts anew.
/// A manual session is never touched.
fn gate_start_auto(repo: &Path, nonce: &str, limit: usize) -> anyhow::Result<()> {
    let gate_store = GateStore::for_repo(repo);
    let ours = |store: &GateStore| store.starting().filter(|marker| marker.nonce == nonce);
    let Some(marker) = ours(&gate_store) else {
        println!("gate: the prompt this plan was for is gone; nothing started");
        return Ok(());
    };
    let task = marker.detail.clone();
    let before = gate_store.active_session()?;
    if before.as_ref().is_some_and(|session| !session.auto) {
        println!("gate: a session started by hand is active; the prompt changes nothing");
        return Ok(());
    }
    let config = OkConfig::load_from_repo(repo)?;
    let mode = GateMode::parse(&config.gate.mode).unwrap_or_default();
    let (report, plan_json) = gate_build_plan(repo, &config, &task, limit)?;
    let low_confidence = report.confidence_breakdown.overall_enum == Confidence::Low;

    // Planning took time. Act only if this run's marker still stands and the session that was
    // active when planning began (or its absence) is still what is there.
    if ours(&gate_store).is_none() {
        println!("gate: cancelled while planning; nothing started");
        return Ok(());
    }
    // A plan the planner itself has Low confidence in is no boundary to judge anything by: a
    // greeting or a question about the project plans like that. Nothing starts and nothing is
    // widened; the next prompt is planned on its own.
    if low_confidence {
        println!(
            "gate: the plan for this prompt has Low confidence ({}); no session is started \
             from it and no boundary is changed",
            report.confidence_summary
        );
        return Ok(());
    }
    let now = gate_store.active_session()?;
    match (&before, now) {
        (None, None) => {}
        (Some(was), Some(mut current)) if was.id == current.id => {
            if gate_same_conversation(&current, &marker.harness_session) {
                let boundary = &report.recommended_change_boundary;
                let allowed = boundary
                    .allowed_files
                    .iter()
                    .map(open_kioku_gate::normalize)
                    .collect::<Vec<_>>();
                let caution = boundary
                    .caution_files
                    .iter()
                    .map(open_kioku_gate::normalize)
                    .collect::<Vec<_>>();
                let added = gate_store.extend(&mut current, &task, &allowed, &caution)?;
                println!(
                    "gate: follow-up prompt widened session {} by {added} path(s)",
                    current.id
                );
                return Ok(());
            }
            // Another conversation's session: close it with its record, then start anew.
            gate_close_session(repo, &gate_store, &current)?;
        }
        _ => {
            println!("gate: the session changed while planning; nothing started");
            return Ok(());
        }
    }

    let mut session = gate_new_session(repo, &config, &task, mode, &report, &plan_json);
    session.auto = true;
    // Nobody declared this task: unless the repository says `enforce`, the boundary informs
    // and records, and holds nothing.
    session.advisory = mode != GateMode::Enforce;
    session.harness_session =
        (!marker.harness_session.is_empty()).then(|| marker.harness_session.clone());
    // The baseline is the one the prompt hook captured: an edit made while this plan was
    // being built is the session's change, not something that predates it.
    match marker.base_rev.clone() {
        Some(base) => {
            session.base_rev = Some(base);
            session.preexisting_changes = marker.preexisting.clone();
        }
        None => {
            session.base_rev = gate_base_rev(repo);
            if let Some(base) = session.base_rev.as_deref() {
                session.preexisting_changes = gate_changed_since(repo, base)?;
            }
        }
    }
    gate_store.start(&session, &plan_json)?;
    println!(
        "gate: session {} started from the prompt ({} mode{}): {} allowed, {} caution; {}",
        session.id,
        mode.as_str(),
        if session.advisory { ", advisory" } else { "" },
        session.boundary.allowed.len(),
        session.boundary.caution.len(),
        session.confidence_summary
    );
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
                "Gate session {} ({} mode{}{}), started {}",
                session.id,
                session.mode.as_str(),
                if session.auto { ", started from the prompt" } else { "" },
                if session.advisory { ", advisory" } else { "" },
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

/// A path given to `check` or `expand`. An absolute path is itself. A relative path is read
/// against the repository root, which is how the hooks' messages spell paths, unless it names
/// an existing file only when read against the caller's directory (the agent's shell may have
/// `cd`ed into a subdirectory).
fn gate_argument_path(repo: &Path, path: &Path) -> anyhow::Result<String> {
    if path.is_absolute() {
        return gate_relative_path(repo, path);
    }
    let from_repo = repo.join(path);
    let from_cwd = std::path::absolute(path)?;
    if !from_repo.exists() && from_cwd.exists() {
        if let Some(relative) = gate_relative(repo, &from_cwd) {
            return Ok(relative);
        }
    }
    gate_relative_path(repo, &from_repo)
}

fn gate_check(json: bool, repo: &Path, path: &Path) -> anyhow::Result<()> {
    let gate_store = GateStore::for_repo(repo);
    let session = require_active_session(&gate_store)?;
    let relative = gate_argument_path(repo, path)?;
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
    let relative = gate_argument_path(repo, path)?;
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
    let listed = match open_kioku_git::changed_paths_since_commit_relative(repo, base) {
        Ok(listed) => listed,
        // The base is gone (a rebase or an amend rewrote it): HEAD is the comparison left,
        // as `gate_change_input` reads it.
        Err(err) => match open_kioku_git::commit(repo) {
            Some(head) if head != base => {
                open_kioku_git::changed_paths_since_commit_relative(repo, &head)?
            }
            _ => return Err(err.into()),
        },
    };
    let mut paths = listed
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
    Ok(open_kioku_git::untracked_paths(repo)?
        .iter()
        .map(open_kioku_gate::normalize)
        .filter(|path| !path.starts_with(".ok/"))
        .collect())
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
    let Some(head) = gate_base_rev(repo) else {
        let changed = reported
            .into_iter()
            .filter(|path| repo.join(path).exists())
            .collect::<Vec<_>>();
        return Ok(ChangeInput {
            changed_files: changed,
            untracked_files: Vec::new(),
            preexisting_excluded: Vec::new(),
            unified_diff: None,
            head_rev: None,
            source: "edits reported by the hooks (not a git repository)".to_string(),
        });
    };
    let head_rev = open_kioku_git::commit(repo);
    let mut source = String::from(
        "git diff against the session's base revision plus untracked files, less changes \
         that predate the session",
    );
    let base = match session.base_rev.as_deref() {
        Some(base) if base != head => {
            source.push_str(&format!(
                "; {} commit(s) made during the session are included",
                gate_commit_count(repo, base, &head)
            ));
            base.to_string()
        }
        Some(base) => base.to_string(),
        None => head.clone(),
    };
    let diff = match open_kioku_git::unified_diff_since(repo, &base) {
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
            open_kioku_git::unified_diff_since(repo, &head)?
        }
        Err(err) => return Err(err.into()),
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
        if paths.is_empty() {
            continue;
        }
        // The gate's own files are never the agent's change, even in a repository that
        // tracks part of `.ok/`.
        if paths.iter().all(|path| path.starts_with(".ok/")) {
            continue;
        }
        if paths.iter().all(excluded) {
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
    let plan = gate_store.saved_plan_for(session)?;
    let dossier = gate_verify(repo, session, plan, &events, malformed, run_commands)?;
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

/// The dossier for `session`'s change, verified against `plan` widened by the session's
/// expansions. Writes nothing.
fn gate_verify(
    repo: &Path,
    session: &GateSession,
    plan: PlanReport,
    events: &[GateEvent],
    malformed: usize,
    run_commands: bool,
) -> anyhow::Result<ChangeDossier> {
    let change = gate_change_input(repo, session, events)?;
    let plan = plan_with_expansions(plan, session);
    // What the hook called caution (the plan's own, or a sibling) verification warns about.
    let plan = plan_with_session_caution(plan, session, &change.changed_files);
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
    let mut dossier = build_dossier(
        &repo_name,
        session,
        &plan,
        events,
        malformed,
        &change,
        verification.as_ref(),
    );
    if !change.changed_files.is_empty() {
        if let Some(report) = gate_ripple(repo, session.base_rev.as_deref(), Vec::new()) {
            open_kioku_gate::attach_ripple(&mut dossier, &report);
        }
    }
    Ok(dossier)
}

/// What `[gate] ripple` asks of the hooks. A value that is none of the three is the default.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GateRipple {
    Hold,
    Report,
    Off,
}

fn gate_ripple_setting(repo: &Path) -> GateRipple {
    match OkConfig::load_from_repo(repo)
        .map(|config| config.gate.ripple)
        .unwrap_or_default()
        .as_str()
    {
        "off" => GateRipple::Off,
        "report" => GateRipple::Report,
        _ => GateRipple::Hold,
    }
}

/// The ripple of the working tree against `base` (`HEAD` when absent, or when `base` is no
/// longer in the repository). `None` when the check is off or cannot run here: it informs, and
/// a hook that cannot compute it says nothing rather than fail.
fn gate_ripple(
    repo: &Path,
    base: Option<&str>,
    only_files: Vec<PathBuf>,
) -> Option<open_kioku_patch::ripple::RippleReport> {
    if gate_ripple_setting(repo) == GateRipple::Off {
        return None;
    }
    let base = base
        .filter(|base| gate_rev_exists(repo, base))
        .map(str::to_string);
    ripple_report(repo, base, only_files, true).ok()
}

fn gate_rev_exists(repo: &Path, rev: &str) -> bool {
    ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "-e"])
        .arg(format!("{rev}^{{commit}}"))
        .output()
        .is_ok_and(|output| output.status.success())
}

/// What the hooks have already told one conversation about its ripple, so the same set of
/// broken sites is shown after an edit once and holds a stop once.
#[derive(Debug, Default, Serialize, Deserialize)]
struct GateRippleTold {
    conversation: String,
    #[serde(default)]
    shown: Vec<String>,
    #[serde(default)]
    held: Vec<String>,
}

/// A conversation is held on its ripple at most this many times, whatever changes.
const GATE_RIPPLE_MAX_HOLDS: usize = 3;
/// Conversations whose ripple record is kept.
const GATE_RIPPLE_CONVERSATIONS: usize = 40;

fn gate_ripple_told_path(gate_store: &GateStore) -> PathBuf {
    gate_store.root().join("ripple-told.json")
}

fn gate_ripple_told(gate_store: &GateStore) -> Vec<GateRippleTold> {
    fs::read(gate_ripple_told_path(gate_store))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Runs `change` on this conversation's record and saves it. Returns what `change` returns.
fn gate_ripple_told_update<T>(
    gate_store: &GateStore,
    input: &HookInput,
    change: impl FnOnce(&mut GateRippleTold) -> T,
) -> T {
    let mut all = gate_ripple_told(gate_store);
    let position = match all
        .iter()
        .position(|told| told.conversation == input.session_id)
    {
        Some(position) => position,
        None => {
            all.push(GateRippleTold {
                conversation: input.session_id.clone(),
                ..GateRippleTold::default()
            });
            all.len() - 1
        }
    };
    let out = change(&mut all[position]);
    if all.len() > GATE_RIPPLE_CONVERSATIONS {
        let excess = all.len() - GATE_RIPPLE_CONVERSATIONS;
        all.drain(..excess);
    }
    // Best effort: a record that cannot be written costs a repeated notice, nothing else.
    if let Ok(bytes) = serde_json::to_vec_pretty(&all) {
        let _ = gate_store.ensure_ignored();
        let _ = fs::write(gate_ripple_told_path(gate_store), bytes);
    }
    out
}

/// After an edit: the sites that no longer fit a declaration in the edited files, unless this
/// conversation was already shown exactly these.
fn gate_ripple_after_edit(
    repo: &Path,
    gate_store: &GateStore,
    session: Option<&GateSession>,
    input: &HookInput,
) -> Option<String> {
    let edited = input
        .edited_paths()
        .iter()
        .filter_map(|path| gate_relative(repo, path))
        .map(PathBuf::from)
        // An edit to prose or configuration changes no declaration: nothing is read for it.
        .filter(|path| open_kioku_patch::ripple::reads_path(path))
        .collect::<Vec<_>>();
    if edited.is_empty() {
        return None;
    }
    let base = session.and_then(|session| session.base_rev.as_deref());
    let report = gate_ripple(repo, base, edited)?;
    if report.broken_sites == 0 {
        return None;
    }
    let fingerprint = report.fingerprint();
    let new = gate_ripple_told_update(gate_store, input, |told| {
        if told.shown.contains(&fingerprint) {
            return false;
        }
        told.shown.push(fingerprint.clone());
        true
    });
    new.then(|| {
        format!(
            "Open Kioku: after this edit, {} site(s) elsewhere no longer fit what it changed.\n{}\
             Update them before you finish.",
            report.broken_sites,
            report.to_brief(8)
        )
    })
}

/// At a stop: the reason to hold the agent on its ripple, or a line to add to what the user is
/// told when the sites were reported before or the setting only reports.
enum GateRippleAtStop {
    Hold(String),
    Reported(String),
}

fn gate_ripple_at_stop(
    repo: &Path,
    gate_store: &GateStore,
    session: Option<&GateSession>,
    input: &HookInput,
) -> Option<GateRippleAtStop> {
    let setting = gate_ripple_setting(repo);
    let base = session.and_then(|session| session.base_rev.as_deref());
    let report = gate_ripple(repo, base, Vec::new())?;
    if report.broken_sites == 0 {
        return None;
    }
    let observing = session.is_some_and(|session| session.mode == GateMode::Observe);
    let fingerprint = report.fingerprint();
    let hold = setting == GateRipple::Hold
        && !observing
        && gate_ripple_told_update(gate_store, input, |told| {
            if told.held.contains(&fingerprint) || told.held.len() >= GATE_RIPPLE_MAX_HOLDS {
                return false;
            }
            told.held.push(fingerprint.clone());
            true
        });
    if hold {
        return Some(GateRippleAtStop::Hold(format!(
            "Open Kioku: this change leaves {} site(s) that no longer fit what it changed.\n{}\
             Update each one. If a site is right as it stands, say why in your answer and stop \
             again: the same sites are not held twice.",
            report.broken_sites,
            report.to_brief(12)
        )));
    }
    Some(GateRippleAtStop::Reported(format!(
        "{} site(s) no longer fit what the change altered (`ok ripple` lists them)",
        report.broken_sites
    )))
}

/// `ok gate ci`: a session that never had hooks, judged once against the diff from `base`.
#[allow(clippy::too_many_arguments)]
fn gate_ci(
    json: bool,
    repo: &Path,
    base: &str,
    task: &str,
    mode: Option<&str>,
    limit: usize,
    run_commands: bool,
    format: GateFormat,
    write: Option<&Path>,
) -> anyhow::Result<()> {
    let git = |args: &[&str]| -> Option<String> {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .filter(|text| !text.is_empty())
    };
    let base_commit = format!("{base}^{{commit}}");
    let Some(base_tip) = git(&[
        "rev-parse",
        "--verify",
        "--quiet",
        "--end-of-options",
        &base_commit,
    ]) else {
        return Err(caller_input_error(format!(
            "`{base}` is not a revision of this repository"
        )));
    };
    // The change is what this branch added since it left the base, not what the base gained
    // since: the merge base is the comparison, the base itself when there is none (unrelated
    // histories, or a shallow clone that does not reach it).
    let base_rev = git(&["merge-base", &base_tip, "HEAD"]).unwrap_or(base_tip);
    let config = OkConfig::load_from_repo(repo)?;
    // How binding the judgement is must not be the judged change's own choice: `[gate]` is
    // read from ok.toml as the base revision has it, so a branch cannot make itself advisory
    // (or widen its boundary to siblings) by editing the file. `--mode` is the caller's.
    let base_gate = git(&["show", &format!("{base_rev}:./ok.toml")])
        .and_then(|text| text.parse::<toml::Value>().ok())
        .and_then(|value| value.get("gate").cloned());
    let base_mode = base_gate
        .as_ref()
        .and_then(|gate| gate.get("mode"))
        .and_then(|mode| mode.as_str())
        .and_then(GateMode::parse)
        .unwrap_or_default();
    let base_sibling_caution = base_gate
        .as_ref()
        .and_then(|gate| gate.get("sibling_caution"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let flag_given = mode.is_some();
    let mode = match mode {
        Some(_) => gate_mode_from(mode, &config)?,
        None => base_mode,
    };
    let (plan, plan_json) = gate_build_plan(repo, &config, task, limit)?;
    let mut session = GateSession::new(task, mode, &plan, &plan_json);
    session.repo_name = gate_repo_name(repo, &config);
    session.base_rev = Some(base_rev);
    session.boundary.sibling_caution = base_sibling_caution;
    // The boundary is a plan built from the pull request's words. Nobody declared it and
    // nobody is here to widen it with a reason, so unless the caller asks for `enforce` a file
    // outside it is reported, not failed. Forbidden paths fail either way.
    session.advisory = mode != GateMode::Enforce;
    // A dossier an earlier run wrote into the working tree is not the change. Only a file git
    // does not track is left out this way: naming a file the branch itself added or changed
    // as the dossier path must not take it out of what is judged.
    if let Some(path) = write {
        if let Some(relative) = gate_relative(repo, &std::path::absolute(path)?) {
            let tracked = ProcessCommand::new("git")
                .arg("-C")
                .arg(repo)
                .args(["ls-files", "--error-unmatch", "--"])
                .arg(&relative)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            if tracked {
                return Err(caller_input_error(format!(
                    "--write {relative} names a file this repository tracks; the dossier \
                     would overwrite it. Write it to a path git does not track."
                )));
            }
            session.preexisting_changes.push(relative);
        }
    }
    let mut dossier = gate_verify(repo, &session, plan, &[], 0, run_commands)?;
    let tree_mode = GateMode::parse(&config.gate.mode).unwrap_or_default();
    if !flag_given && tree_mode != base_mode {
        dossier.caveats.push(format!(
            "ok.toml sets `[gate] mode = \"{}\"` in this change and `\"{}\"` at the base \
             revision; the base revision's applies to a change's own judgement",
            tree_mode.as_str(),
            base_mode.as_str()
        ));
    }
    if config.gate.sibling_caution != base_sibling_caution {
        dossier.caveats.push(format!(
            "ok.toml sets `[gate] sibling_caution = {}` in this change and `{}` at the base \
             revision; the base revision's applies to a change's own judgement",
            config.gate.sibling_caution, base_sibling_caution
        ));
    }
    let gate_store = GateStore::for_repo(repo);
    let (dossier_json, dossier_markdown) = gate_store.write_dossier(&dossier)?;
    let markdown = render_markdown(&dossier);
    if let Some(path) = write {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        fs::write(path, &markdown)?;
    }
    let output = GateFinishOutput {
        dossier_json,
        dossier_markdown,
        session_ended: true,
        dossier,
    };
    match (json, format) {
        (true, _) | (_, GateFormat::Json) => {
            println!("{}", serde_json::to_string_pretty(&output)?);
        }
        (_, GateFormat::Markdown) => print!("{markdown}"),
        (_, GateFormat::Text) => println!("{}", gate_summary_line(&output.dossier)),
    }
    if output.dossier.verdict == DossierVerdict::Fail {
        anyhow::bail!("change dossier verdict is FAIL");
    }
    Ok(())
}

/// Run `ok <args>` for `repo` in the background, detached from the hook's process group so
/// the harness's timeout cannot take it down, with its output written to `log` (one run's
/// output: the file is emptied each time, and written in append mode, so an earlier run
/// still going adds whole lines after it rather than writing over this run's). Returns the
/// child's process id.
fn gate_spawn_detached(repo: &Path, args: &[&str], log: &Path) -> anyhow::Result<u32> {
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(log, b"")?;
    let log_file = fs::OpenOptions::new().append(true).open(log)?;
    let mut command = ProcessCommand::new(std::env::current_exe()?);
    // The child runs in the repository, with the repository named, so no positional default
    // of `.` can point it at the hook's own working directory.
    command
        .current_dir(repo)
        .arg("--repo")
        .arg(repo)
        .args(args)
        .env("RUST_BACKTRACE", "0")
        .stdin(std::process::Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    Ok(command.spawn()?.id())
}

/// Whether the repository has an index the gate can plan from.
fn gate_indexed(repo: &Path) -> bool {
    SqliteStore::open_repo_index(repo).ok().flatten().is_some()
}

/// The most files (tracked, and untracked but not ignored: what the indexer walks) a
/// repository may have for the hooks to index it unasked. Indexing
/// is minutes of CPU and gigabytes of memory on a very large repository; past this size it
/// is the user's decision to run `ok index`.
const GATE_AUTO_INDEX_MAX_FILES: usize = 20_000;

/// What the hooks do about a repository with no index.
enum GateAutoIndex {
    /// There is an index: carry on.
    Ready,
    /// Not ready; tell the agent this.
    Notice(String),
    /// Not a place the hooks set up unasked (not the top of a git working tree): say nothing.
    Silent,
}

/// Start indexing in the background when the repository has no index, nothing is indexing
/// it already, it is the top of a git working tree, and it is not too large to index unasked.
fn gate_auto_index(repo: &Path, gate_store: &GateStore) -> anyhow::Result<GateAutoIndex> {
    // Nothing automatic happens anywhere but the top of a git working tree, index or not.
    let is_toplevel = gate_git_toplevel(repo)
        .is_some_and(|top| gate_canonical_lossy(&top) == gate_canonical_lossy(repo));
    if !is_toplevel {
        return Ok(GateAutoIndex::Silent);
    }
    if gate_indexed(repo) {
        gate_store.clear_indexing();
        return Ok(GateAutoIndex::Ready);
    }
    if let Some(marker) = gate_store.indexing() {
        // An indexer that was killed leaves its marker; it is retried, not waited for.
        if !gate_marker_spent(
            &marker,
            chrono::Duration::minutes(30),
            chrono::Duration::hours(6),
        ) {
            return Ok(GateAutoIndex::Notice(
                "Open Kioku gate: the repository is being indexed in the background; the \
                 gate starts on the first prompt after the index is ready."
                    .into(),
            ));
        }
    }
    // What the indexer would walk: tracked files and untracked ones git does not ignore. A
    // repository git cannot list is not indexed unasked.
    let listed = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .output();
    let files = match listed {
        Ok(output) if output.status.success() => {
            output.stdout.iter().filter(|b| **b == 0).count()
        }
        _ => return Ok(GateAutoIndex::Silent),
    };
    if files > GATE_AUTO_INDEX_MAX_FILES {
        return Ok(GateAutoIndex::Notice(format!(
            "Open Kioku gate: this repository has {files} files, more than the hooks index \
             unasked ({GATE_AUTO_INDEX_MAX_FILES}). Run `ok index` once to enable the gate."
        )));
    }
    gate_ignore_index_dir(repo);
    gate_store.mark_indexing(None)?;
    match gate_spawn_detached(
        repo,
        &["gate", "auto-index"],
        &gate_store.root().join("index.log"),
    ) {
        Ok(pid) => gate_store.mark_indexing(Some(pid))?,
        Err(err) => {
            gate_store.clear_indexing();
            return Err(err);
        }
    }
    Ok(GateAutoIndex::Notice(
        "Open Kioku gate: this repository has no index yet, so `ok index` is running in the \
         background (`.ok/gate/index.log`); the gate starts on the first prompt after it is \
         ready."
            .into(),
    ))
}

/// Before the hooks create `.ok/` unasked: if git would show the index as untracked, ignore
/// the directory from inside it. A repository that already ignores `.ok/`, or that has put
/// its own `.gitignore` there, is left as it is.
fn gate_ignore_index_dir(repo: &Path) {
    let ok_dir = repo.join(".ok");
    let ignore = ok_dir.join(".gitignore");
    if ignore.exists() {
        return;
    }
    let ignored = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["check-ignore", "-q", ".ok/index.sqlite"])
        .status()
        .is_ok_and(|status| status.success());
    if !ignored && fs::create_dir_all(&ok_dir).is_ok() {
        let _ = fs::write(ignore, open_kioku_gate::IGNORE_FILE_TEXT);
    }
}

/// `ok gate auto-index`: index, and clear the marker however indexing ends (a panic
/// included), so a failure is retried by the next session rather than reported as "being
/// indexed" for half an hour.
fn gate_auto_index_run(repo: &Path) -> anyhow::Result<()> {
    struct ClearIndexing(GateStore);
    impl Drop for ClearIndexing {
        fn drop(&mut self) {
            self.0.clear_indexing();
        }
    }
    let clear = ClearIndexing(GateStore::for_repo(repo));
    let result = index_repo(repo);
    drop(clear);
    let snapshot = result?;
    println!(
        "Indexed {} files, {} symbols for the gate",
        snapshot.files.len(),
        snapshot.symbols.len()
    );
    Ok(())
}

/// The task a prompt declares, or `None` for a prompt the gate does not plan from: a slash
/// command, a shell (`!`) or memory (`#`) line, or nothing to speak of. How much a prompt
/// says is not judged here; the plan's own confidence decides what the gate does with it.
fn gate_task_from_prompt(prompt: &str) -> Option<String> {
    let text = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.starts_with(['/', '!', '#']) || text.chars().count() < 3 {
        return None;
    }
    Some(text.chars().take(GATE_TASK_MAX_CHARS).collect())
}

/// The most characters of prompt text a plan is built from.
const GATE_TASK_MAX_CHARS: usize = 400;

/// The active session, after waiting briefly for a planner the prompt hook started: a new
/// session being planned, or a follow-up prompt widening the active one.
///
/// The session returned is this conversation's to be judged against: another conversation's
/// prompt-started session is not, and its planner is not waited for.
fn gate_wait_for_session(
    repo: &Path,
    gate_store: &GateStore,
    input: &HookInput,
) -> anyhow::Result<Option<GateSession>> {
    let own = |gate_store: &GateStore| -> anyhow::Result<Option<GateSession>> {
        let mut session = gate_store
            .active_session()?
            .filter(|session| !gate_foreign(session, input));
        if let Some(session) = session.as_mut() {
            gate_follow_config(repo, gate_store, session);
        }
        Ok(session)
    };
    let Some(marker) = gate_store.starting() else {
        return own(gate_store);
    };
    if gate_planner_spent(&marker) {
        // The planner died or hung; its marker must not hold every tool call.
        gate_store.clear_starting_if(&marker.nonce);
        return own(gate_store);
    }
    if !gate_marker_ours(&marker, input) {
        return own(gate_store);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(12);
    while std::time::Instant::now() < deadline {
        if !gate_store
            .starting()
            .is_some_and(|current| current.nonce == marker.nonce)
        {
            break;
        }
        if marker.pid.is_some_and(|pid| !gate_pid_alive(pid)) {
            gate_store.clear_starting_if(&marker.nonce);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Read the session only now: the planner writes it before it clears its marker.
    own(gate_store)
}

/// The newest dossier on disk, for a `finish` that finds no session to finish.
fn gate_latest_dossier(gate_store: &GateStore) -> Option<PathBuf> {
    fs::read_dir(gate_store.root().join("dossiers"))
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

fn gate_finish(
    json: bool,
    repo: &Path,
    run_commands: bool,
    format: GateFormat,
    keep: bool,
) -> anyhow::Result<()> {
    let gate_store = GateStore::for_repo(repo);
    let Some(session) = gate_store.active_session()? else {
        return Err(caller_input_error(match gate_latest_dossier(&gate_store) {
            Some(path) => format!(
                "no active gate session; the last one already wrote its dossier: {}",
                path.display()
            ),
            None => "no active gate session; start one with `ok gate start \"<task>\"`".into(),
        }));
    };
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
    match gate_store.active_session() {
        Ok(Some(session)) => {
            gate_store.finish(&session)?;
            print_text_or_json(
                json,
                &format!("Gate session {} abandoned; nothing was verified.", session.id),
                &serde_json::json!({"abandoned": session.id}),
            )
        }
        Ok(None) => Err(caller_input_error(
            "no active gate session; start one with `ok gate start \"<task>\"`",
        )),
        // A session the hooks refuse to read is exactly what abandon must be able to clear.
        Err(err) => {
            gate_store.abandon_any()?;
            print_text_or_json(
                json,
                &format!("Gate session file could not be read ({err}); it was removed."),
                &serde_json::json!({"abandoned": "unreadable", "error": err.to_string()}),
            )
        }
    }
}

fn gate_summary_line(dossier: &ChangeDossier) -> String {
    let violations = dossier
        .verification
        .as_ref()
        .map(|v| v.boundary_violations.len())
        .unwrap_or(0);
    let mut line = format!(
        "Change Dossier {}: {} changed file(s), {} boundary violation(s), {} edit(s) denied, {} warned",
        dossier.verdict.label(),
        dossier.change.changed_files.len(),
        violations,
        dossier.trail.edits_denied,
        dossier.trail.edits_warned
    );
    if !dossier.change.outside_boundary.is_empty() {
        line.push_str(&format!(
            ", {} outside the planned boundary (advisory)",
            dossier.change.outside_boundary.len()
        ));
    }
    if let Some(ripple) = dossier.ripple.as_ref().filter(|ripple| ripple.broken_sites > 0) {
        line.push_str(&format!(
            ", {} site(s) elsewhere no longer fit what it changed",
            ripple.broken_sites
        ));
    }
    line
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

/// The directory a shell command starts in: the one the harness reports, else the repository.
fn gate_shell_cwd(repo: &Path, input: &HookInput) -> PathBuf {
    if input.cwd.is_empty() {
        repo.to_path_buf()
    } else {
        PathBuf::from(&input.cwd)
    }
}

fn gate_emit(response: HookResponse) -> anyhow::Result<()> {
    if let Some(value) = response.to_json() {
        println!("{}", serde_json::to_string(&value)?);
    }
    Ok(())
}

fn gate_hook(repo: &Path, event: GateHookEvent) -> anyhow::Result<()> {
    let parsed = HookInput::parse(std::io::stdin().lock());
    if event == GateHookEvent::PreTool {
        // The edit hook fails closed: malformed input, an unreadable configuration or a torn
        // session must refuse the tool call (exit 2, which the harness treats as blocking)
        // rather than let an edit through unjudged. The other hooks only inform.
        return match parsed
            .map_err(anyhow::Error::from)
            .and_then(|input| gate_hook_pre_tool_entry(repo, &input))
        {
            Ok(()) => Ok(()),
            Err(err) => {
                eprintln!(
                    "Open Kioku gate: the edit hook could not judge this tool call ({err:#}), so \
                     it is refused. Run `ok gate status` in the repository to see the session; \
                     `ok gate abandon` ends a broken one."
                );
                std::process::exit(2);
            }
        };
    }
    let input = parsed.unwrap_or_default();
    let repo = gate_hook_repo(repo, &input)?;
    let gate_store = GateStore::for_repo(&repo);
    // Every hook serves one conversation. A session another conversation's prompt started is
    // not this one's: it is neither judged against it nor shown it.
    let own_session = |gate_store: &GateStore| -> anyhow::Result<Option<GateSession>> {
        let mut session = gate_store
            .active_session()?
            .filter(|session| !gate_foreign(session, &input));
        if let Some(session) = session.as_mut() {
            gate_follow_config(&repo, gate_store, session);
        }
        Ok(session)
    };
    match event {
        GateHookEvent::PreTool | GateHookEvent::PreCommit => unreachable!("handled above"),
        GateHookEvent::PostTool => {
            gate_hook_post_tool(&repo, &gate_store, own_session(&gate_store)?, &input)
        }
        GateHookEvent::Stop => {
            // A plan still being built for this conversation is waited for, so what the turn
            // edited is judged now. If the planner outlasts the wait it is left to finish:
            // the session it starts carries the prompt's baseline, and the next stop, or the
            // end of the conversation, judges those edits.
            let session = gate_wait_for_session(&repo, &gate_store, &input)?;
            gate_hook_stop(&repo, &gate_store, session, &input)
        }
        GateHookEvent::Prompt | GateHookEvent::SessionStart => {
            gate_hook_prompt(&repo, &gate_store, own_session(&gate_store)?, &input, event)
        }
        GateHookEvent::SessionEnd => {
            // The conversation is over. Its planner, if one is still running, is given the
            // usual wait so that edits already made are judged; past that it has nothing to
            // start. Its auto session ends with its dossier written.
            let session = gate_wait_for_session(&repo, &gate_store, &input)?;
            if let Some(marker) = gate_store.starting() {
                if gate_marker_ours(&marker, &input) {
                    gate_store.clear_starting_if(&marker.nonce);
                }
            }
            match session {
                Some(session) if session.auto => gate_close_session(&repo, &gate_store, &session),
                _ => Ok(()),
            }
        }
    }
}

/// The prompt and session-start hooks: in auto mode, index a repository that has no index
/// and plan from the prompt in the background; otherwise say where the session stands.
fn gate_hook_prompt(
    repo: &Path,
    gate_store: &GateStore,
    session: Option<GateSession>,
    input: &HookInput,
    event: GateHookEvent,
) -> anyhow::Result<()> {
    let event_name = if event == GateHookEvent::Prompt {
        "UserPromptSubmit"
    } else {
        "SessionStart"
    };
    let emit = |text: String| {
        gate_emit(HookResponse::Context {
            event: event_name.into(),
            text,
        })
    };
    let config = match OkConfig::load_from_repo(repo) {
        Ok(config) => config,
        // A configuration that cannot be read is not permission to act: nothing automatic
        // runs until it is fixed.
        Err(err) => {
            return emit(format!(
                "Open Kioku gate: ok.toml cannot be read ({err}); the gate does nothing \
                 automatic until it is fixed."
            ))
        }
    };
    // A session someone started by hand is theirs: nothing automatic touches it.
    let manual_session = session.as_ref().is_some_and(|session| !session.auto);
    if config.gate.auto && !manual_session {
        match gate_auto_index(repo, gate_store)? {
            GateAutoIndex::Ready => {}
            GateAutoIndex::Notice(text) => return emit(text),
            GateAutoIndex::Silent => return Ok(()),
        }
        if event == GateHookEvent::Prompt {
            if let Some(text) = gate_auto_plan(repo, gate_store, session.as_ref(), input)? {
                return emit(text);
            }
        }
    }
    let text = match &session {
        Some(session) => format!(
            "Open Kioku gate session is active ({} mode{}) for task \"{}\": {} allowed \
             path(s), {} caution, {} widened. Edits are judged against that boundary; widen it \
             with `ok gate expand <path> --reason \"<why>\"`{}.",
            session.mode.as_str(),
            if session.advisory { ", advisory" } else { "" },
            session.task.chars().take(160).collect::<String>(),
            session.boundary.allowed.len(),
            session.boundary.caution.len(),
            session.boundary.expansions.len(),
            if session.auto {
                ""
            } else {
                ", finish with `ok gate finish`"
            }
        ),
        None if config.gate.auto => return Ok(()),
        None if config.gate.require_plan => {
            "Open Kioku gate: no session is active and this repository requires \
             one (`[gate] require_plan = true`). Source edits are denied until \
             `ok gate start \"<task>\"` declares a plan and boundary."
                .to_string()
        }
        None => "Open Kioku gate: no session is active for this repository. Before \
                 editing source for a task, `ok gate start \"<task>\"` declares a plan \
                 and boundary that every edit is then held to, and `ok gate finish` \
                 writes the verified Change Dossier."
            .to_string(),
    };
    emit(text)
}

/// Plan from the prompt in the background, and say so. `None` when the prompt is not a task.
/// With no auto session the plan starts one; with one, it widens its boundary (or, for a
/// different conversation, replaces it). The baseline is captured here, at the prompt, so an
/// edit made while the plan is being built belongs to the session.
fn gate_auto_plan(
    repo: &Path,
    gate_store: &GateStore,
    session: Option<&GateSession>,
    input: &HookInput,
) -> anyhow::Result<Option<String>> {
    let Some(task) = input.prompt.as_deref().and_then(gate_task_from_prompt) else {
        return Ok(None);
    };
    // A plan still being built for an earlier prompt of this conversation is superseded: the
    // new marker replaces the old one, whose planner backs out, and the new plan is built
    // from both prompts with the earlier prompt's baseline, so neither prompt's words and no
    // edit made in between is dropped. Another conversation's plan in progress is replaced
    // outright: one repository holds one prompt-started session.
    let superseded = gate_store
        .starting()
        .filter(|marker| !gate_planner_spent(marker) && gate_marker_ours(marker, input));
    let continues = session.is_some_and(|s| gate_same_conversation(s, &input.session_id));
    let task = match &superseded {
        Some(earlier) if !earlier.detail.is_empty() && earlier.detail != task => {
            format!("{} {}", earlier.detail, task)
                .chars()
                .take(GATE_TASK_MAX_CHARS)
                .collect()
        }
        _ => task,
    };
    let mut marker = BackgroundMarker::new(&task);
    marker.harness_session = input.session_id.clone();
    match &superseded {
        Some(earlier) if earlier.base_rev.is_some() => {
            marker.base_rev = earlier.base_rev.clone();
            marker.preexisting = earlier.preexisting.clone();
        }
        _ if !continues => {
            marker.base_rev = gate_base_rev(repo);
            if let Some(base) = marker.base_rev.as_deref() {
                marker.preexisting = gate_changed_since(repo, base).unwrap_or_default();
            }
        }
        _ => {}
    }
    gate_store.mark_starting(&marker)?;
    match gate_spawn_detached(
        repo,
        &["gate", "start", "--auto-nonce", &marker.nonce],
        &gate_store.root().join("start.log"),
    ) {
        Ok(pid) => gate_store.set_starting_pid(&marker.nonce, pid)?,
        Err(err) => {
            gate_store.clear_starting_if(&marker.nonce);
            return Err(err);
        }
    }
    Ok(Some(if continues {
        "Open Kioku gate: this prompt's plan is being added to the session's boundary in \
         the background."
            .to_string()
    } else {
        "Open Kioku gate: planning an edit boundary from this prompt in the background. If \
         the prompt reads as a task, edits are judged against that boundary once it is ready, \
         an edit outside it is reported with `ok gate expand <path> --reason \"<why>\"` as \
         the way to record why it belongs, and a Change Dossier is written when you stop. \
         `ok gate status` shows the session, `.ok/gate/start.log` why there is none."
            .to_string()
    }))
}

fn gate_hook_pre_tool_entry(repo: &Path, input: &HookInput) -> anyhow::Result<()> {
    if !input.is_edit_tool() && !input.is_shell_tool() {
        return Ok(());
    }
    let repo = gate_hook_repo(repo, input)?;
    let gate_store = GateStore::for_repo(&repo);
    let session = gate_wait_for_session(&repo, &gate_store, input)?;
    let planning = gate_store
        .starting()
        .is_some_and(|marker| gate_marker_ours(&marker, input));
    if session.is_none() && planning && input.is_edit_tool() {
        // The planner outlasted the wait. The edit proceeds and verification judges it,
        // unless the repository insists on a plan first.
        let config = OkConfig::load_from_repo(&repo)?;
        if config.gate.require_plan {
            return gate_emit(HookResponse::Deny {
                event: "PreToolUse".into(),
                reason: "Open Kioku gate: the boundary for this prompt is still being planned; \
                         retry the edit in a moment."
                    .into(),
            });
        }
    }
    gate_hook_pre_tool(&repo, &gate_store, session, input)
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

/// The repository files a shell command names as write targets, repository-relative. A
/// target outside the repository (`/tmp`, `/dev/null`) is not the gate's concern.
fn gate_shell_targets(repo: &Path, cwd: &Path, command: &str) -> Vec<String> {
    shell_write_targets(command, cwd)
        .iter()
        .filter_map(|path| gate_relative(repo, path))
        .collect()
}

/// Decide the repository files a shell command names as write targets. What the command
/// writes that this cannot see, the post-tool hook reads from the tree.
fn gate_shell_decisions(
    repo: &Path,
    session: &GateSession,
    cwd: &Path,
    command: &str,
) -> Vec<PathDecision> {
    gate_shell_targets(repo, cwd, command)
        .iter()
        .map(|relative| {
            let mut decision = session.decide(relative);
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

/// The dirty paths since `base`, each with a stamp of its size and modification time, so a
/// file that was already dirty and is written again still reads as changed.
fn gate_dirty_stamps(repo: &Path, base: &str) -> anyhow::Result<BTreeMap<String, String>> {
    Ok(gate_changed_since(repo, base)?
        .into_iter()
        .map(|path| {
            let stamp = fs::metadata(repo.join(&path))
                .map(|m| format!("{}:{:?}", m.len(), m.modified().ok()))
                .unwrap_or_else(|_| "absent".into());
            (path, stamp)
        })
        .collect())
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
    let Some(session) = session else {
        // No session: there is no boundary to hold edits to, but the forbidden-path policy
        // does not depend on one. A secret-like path, a dependency directory or generated
        // output is refused here exactly as it is inside a session, in every mode but
        // `observe`. A configuration that cannot be read is an error, which the caller fails
        // closed on.
        let config = OkConfig::load_from_repo(repo)?;
        let mut targets = input
            .edited_paths()
            .iter()
            .filter_map(|path| gate_relative(repo, path))
            .collect::<Vec<_>>();
        targets.extend(gate_shell_targets(
            repo,
            &gate_shell_cwd(repo, input),
            input.shell_command().unwrap_or_default(),
        ));
        if GateMode::parse(&config.gate.mode).unwrap_or_default() != GateMode::Observe {
            let forbidden = targets
                .iter()
                .filter_map(|path| {
                    open_kioku_gate::policy_forbidden_reason(path)
                        .map(|reason| format!("`{path}` is {reason}"))
                })
                .collect::<Vec<_>>();
            if !forbidden.is_empty() {
                return gate_emit(HookResponse::Deny {
                    event: "PreToolUse".into(),
                    reason: format!(
                        "Open Kioku gate denied this {}. {}. Such a path is forbidden by policy \
                         with or without a gate session, and no boundary can be widened to it. \
                         If it has to change, say so to the user and leave the change to them.",
                        if input.is_shell_tool() {
                            "command"
                        } else {
                            "edit"
                        },
                        forbidden.join("; ")
                    ),
                });
            }
        }
        if !config.gate.require_plan {
            return Ok(());
        }
        let writes_repo_files = !targets.is_empty();
        if !writes_repo_files {
            return Ok(());
        }
        return gate_emit(HookResponse::Deny {
            event: "PreToolUse".into(),
            reason: "This repository requires a gate session before source edits \
                     (`[gate] require_plan = true`). Run `ok gate start \"<task>\"` to \
                     declare the plan and boundary, then retry."
                .into(),
        });
    };
    let decisions = if input.is_edit_tool() {
        gate_hook_decisions(repo, &session, input)
    } else {
        gate_shell_decisions(
            repo,
            &session,
            &gate_shell_cwd(repo, input),
            input.shell_command().unwrap_or_default(),
        )
    };
    // A denial is recorded now, because the tool will not run. An edit that is allowed is
    // recorded by the post-tool hook once it has happened: a tool call the user refuses or
    // that fails must not read as an edit.
    for decision in &decisions {
        if decision.action == HookAction::Deny || input.is_shell_tool() {
            gate_store.append_event(&GateEvent::for_decision(
                &session.id,
                decision,
                &input.tool_name,
            ))?;
        }
    }
    let denied = decisions
        .iter()
        .filter(|d| d.action == HookAction::Deny)
        .map(|d| d.reason.clone())
        .collect::<Vec<_>>();
    if !denied.is_empty() {
        return gate_emit(HookResponse::Deny {
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
        });
    }
    if input.is_shell_tool() {
        // The command will run: remember what was dirty before it, so the post-tool hook can
        // tell what it changed, however it changed it.
        if let Some(base) = session.base_rev.as_deref() {
            let dirty = gate_dirty_stamps(repo, base)?;
            gate_store.write_snapshot(&gate_snapshot_id(input), &dirty)?;
        }
    }
    Ok(())
}

fn gate_hook_post_tool(
    repo: &Path,
    gate_store: &GateStore,
    session: Option<GateSession>,
    input: &HookInput,
) -> anyhow::Result<()> {
    // The ripple of an edit needs no session: it is read from the edit itself.
    let ripple = if input.is_edit_tool() {
        gate_ripple_after_edit(repo, gate_store, session.as_ref(), input)
    } else {
        None
    };
    let Some(session) = session else {
        return match ripple {
            Some(text) => gate_emit(HookResponse::Context {
                event: "PostToolUse".into(),
                text,
            }),
            None => Ok(()),
        };
    };
    if input.is_edit_tool() {
        let decisions = gate_hook_decisions(repo, &session, input);
        for decision in &decisions {
            gate_store.append_event(&GateEvent::for_decision(
                &session.id,
                decision,
                &input.tool_name,
            ))?;
        }
        let warned = decisions
            .into_iter()
            .filter(|d| d.action == HookAction::Warn)
            .map(|d| d.reason)
            .collect::<Vec<_>>();
        let mut notices = Vec::new();
        if !warned.is_empty() {
            notices.push(format!("Open Kioku gate: {}", warned.join(" ")));
        }
        notices.extend(ripple);
        if notices.is_empty() {
            return Ok(());
        }
        return gate_emit(HookResponse::Context {
            event: "PostToolUse".into(),
            text: notices.join("\n"),
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
    let Some(before) = gate_store.take_snapshot(&gate_snapshot_id(input))? else {
        return Ok(());
    };
    let cwd = gate_shell_cwd(repo, input);
    let command = input.shell_command().unwrap_or_default();
    let named = gate_shell_targets(repo, &cwd, command)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut notices = Vec::new();
    // Named targets the pre-tool hook allowed with a warning: the agent is told now, the way
    // an Edit outside the boundary is told, with the remedy.
    for decision in gate_shell_decisions(repo, &session, &cwd, command) {
        if decision.action == HookAction::Warn {
            notices.push(decision.reason);
        }
    }
    for (path, stamp) in gate_dirty_stamps(repo, base)? {
        if named.contains(&path) || before.get(&path) == Some(&stamp) {
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

/// A fingerprint of everything a verification reads: the trail (less verification records)
/// and the dirty tree. Equal fingerprints mean the last dossier still stands.
fn gate_verify_fingerprint(
    repo: &Path,
    gate_store: &GateStore,
    session: &GateSession,
) -> anyhow::Result<String> {
    let (events, malformed) = gate_store.events()?;
    let trail = events
        .iter()
        .filter(|e| e.kind != EventKind::VerifyRun)
        .count();
    let dirty = match session.base_rev.as_deref() {
        Some(base) => gate_changed_since(repo, base)?,
        None => Vec::new(),
    };
    // The session and its boundary are part of what a verdict depends on: another session's
    // record, or one from before the boundary was widened or the mode changed, does not stand.
    let mut text = format!(
        "{}\n{}\n{}\n{trail}\n{malformed}\n",
        session.id,
        session.mode.as_str(),
        serde_json::to_string(&session.boundary)?
    );
    for path in &dirty {
        text.push_str(path);
        text.push('\n');
        if let Ok(metadata) = fs::metadata(repo.join(path)) {
            text.push_str(&format!(
                "{}:{:?}\n",
                metadata.len(),
                metadata.modified().ok()
            ));
        }
    }
    Ok(open_kioku_gate::sha256_hex(text.as_bytes()))
}

fn gate_hook_stop(
    repo: &Path,
    gate_store: &GateStore,
    session: Option<GateSession>,
    input: &HookInput,
) -> anyhow::Result<()> {
    // The ripple of the change needs no session and no plan: it is read from the change.
    let ripple = gate_ripple_at_stop(repo, gate_store, session.as_ref(), input);
    let Some(session) = session else {
        // No session of this conversation. A planner still running is left to finish (the
        // caller waited for it already); another conversation's is none of this stop's.
        return match ripple {
            Some(GateRippleAtStop::Hold(reason)) => gate_emit(HookResponse::Block { reason }),
            Some(GateRippleAtStop::Reported(line)) => gate_emit(HookResponse::SystemMessage {
                text: format!("Open Kioku: {line}."),
            }),
            None => Ok(()),
        };
    };
    if session.auto {
        // Nothing edited and nothing changed: the conversation has not touched the
        // repository yet, so there is nothing to verify and no dossier to write.
        let (events, _) = gate_store.events()?;
        if !gate_trail_has_edits(&events)
            && gate_change_input(repo, &session, &events)?
                .changed_files
                .is_empty()
        {
            return Ok(());
        }
    }
    // A stop with nothing changed since the last verification answers from that verification
    // instead of opening the index again.
    let fingerprint = gate_verify_fingerprint(repo, gate_store, &session)?;
    let last = match gate_store.read_last_verify() {
        Some(last) if last.fingerprint == fingerprint => last,
        _ => {
            let run = gate_run_verification(repo, gate_store, &session, false)?;
            let last = LastVerify {
                fingerprint,
                verdict: run.dossier.verdict,
                summary: gate_summary_line(&run.dossier),
                dossier_markdown: run.dossier_markdown.clone(),
                violations: run
                    .dossier
                    .verification
                    .as_ref()
                    .map(|v| v.boundary_violations.clone())
                    .unwrap_or_default(),
            };
            gate_store.write_last_verify(&last)?;
            last
        }
    };
    // A failing change holds the agent only where the boundary is binding: not in observe
    // mode, and not in an advisory session, whose boundary nobody declared.
    let binding = !session.advisory && session.mode != GateMode::Observe;
    if last.verdict == DossierVerdict::Fail && binding && !input.stop_hook_active {
        let mut reason = format!(
            "Open Kioku gate: the change does not verify against the declared boundary. {}.",
            last.summary
        );
        for finding in last.violations.iter().take(8) {
            reason.push_str(&format!(
                "\n- {} [{}]: {}",
                finding.path.as_deref().unwrap_or("-"),
                finding.kind,
                finding.reason
            ));
        }
        reason.push_str(
            "\nEither revert the edits outside the boundary, or widen it with a reason for each \
             path (`ok gate expand <path> --reason \"<why>\"`), then stop again. The dossier is at ",
        );
        reason.push_str(&last.dossier_markdown.display().to_string());
        reason.push('.');
        if let Some(GateRippleAtStop::Hold(ripple)) = &ripple {
            reason.push_str("\n\n");
            reason.push_str(ripple);
        }
        return gate_emit(HookResponse::Block { reason });
    }
    if let Some(GateRippleAtStop::Hold(reason)) = ripple {
        return gate_emit(HookResponse::Block { reason });
    }
    // Informational only: a message the user sees, which neither blocks nor continues the
    // turn. A session, auto or not, stays open after a stop: a FAIL the agent was held on,
    // and the next prompt of the same conversation, both belong to it.
    gate_emit(HookResponse::SystemMessage {
        text: format!(
            "Open Kioku gate: {}. Dossier: {}",
            last.summary,
            last.dossier_markdown.display()
        ),
    })
}

/// The hook entries `ok gate install claude` writes. Each command is the `ok` binary on PATH,
/// the same assumption the plugin's MCP server entry makes. `hooks/hooks.json`, which the
/// Claude Code plugin ships, carries the same entries; a test holds them equal.
fn gate_claude_hooks() -> serde_json::Value {
    let command = |event: &str, timeout: u64| {
        serde_json::json!({
            "type": "command",
            "command": format!("ok gate hook {event}"),
            "timeout": timeout
        })
    };
    serde_json::json!({
        "SessionStart": [{
            "hooks": [command("session-start", 10)]
        }],
        "UserPromptSubmit": [{
            "hooks": [command("prompt", 10)]
        }],
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
        "SessionEnd": [{
            "hooks": [command("session-end", 60)]
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

/// The line the git hook runs. `ok` is found on PATH, as for every other hook.
const GATE_GIT_HOOK_COMMAND: &str = "ok gate hook pre-commit";

/// `ok gate hook pre-commit`: what git's pre-commit hook asks. Every agent and every person
/// commits through git, so this is the gate for a harness with no hooks of its own.
///
/// A staged path the forbidden policy covers refuses the commit, with or without a session,
/// in every mode but `observe`. A deletion is not judged: removing a committed secret is the
/// remedy, not the offence. In a binding `enforce` session a staged path outside the boundary
/// refuses it too, with the way to widen the boundary; in any other session such a path is
/// named and the commit goes ahead.
///
/// A local hook can be skipped by whoever runs git. The check that cannot be skipped that way
/// is `ok gate ci` on the pull request.
fn gate_hook_pre_commit(repo: &Path) -> anyhow::Result<()> {
    let config = OkConfig::load_from_repo(repo)?;
    if GateMode::parse(&config.gate.mode).unwrap_or_default() == GateMode::Observe {
        return Ok(());
    }
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff", "--cached", "--name-only", "-z", "--diff-filter=ACMRT"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git could not list the staged files: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let staged = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect::<Vec<_>>();
    let gate_store = GateStore::for_repo(repo);
    let mut session = gate_store.active_session().ok().flatten();
    if let Some(session) = session.as_mut() {
        gate_follow_config(repo, &gate_store, session);
    }
    let mut forbidden = Vec::new();
    let mut outside = Vec::new();
    for path in &staged {
        if let Some(reason) = open_kioku_gate::policy_forbidden_reason(path) {
            forbidden.push(format!("  {path}: {reason}"));
            continue;
        }
        let Some(session) = &session else { continue };
        match session.boundary.classify(path) {
            open_kioku_gate::PathClass::Forbidden => {
                forbidden.push(format!("  {path}: forbidden by the plan's rules"));
            }
            open_kioku_gate::PathClass::Outside => outside.push(path.clone()),
            _ => {}
        }
    }
    let binding = session
        .as_ref()
        .is_some_and(|session| !session.advisory && session.mode == GateMode::Enforce);
    if forbidden.is_empty() && (outside.is_empty() || !binding) {
        if let (Some(session), false) = (&session, outside.is_empty()) {
            eprintln!(
                "Open Kioku gate: {} staged file(s) are outside the boundary for task \"{}\" \
                 (reported, not refused): {}",
                outside.len(),
                session.task.chars().take(80).collect::<String>(),
                outside.join(", ")
            );
        }
        return Ok(());
    }
    let mut message = String::from("Open Kioku gate: this commit is refused.\n");
    if !forbidden.is_empty() {
        message.push_str(
            "These staged paths are forbidden by policy (no boundary can be widened to them):\n",
        );
        message.push_str(&forbidden.join("\n"));
        message.push('\n');
    }
    if binding && !outside.is_empty() {
        let session = session.as_ref().expect("binding implies a session");
        message.push_str(&format!(
            "These staged paths are outside the boundary declared for task \"{}\":\n",
            session.task.chars().take(80).collect::<String>()
        ));
        for path in &outside {
            message.push_str(&format!(
                "  {path}: widen it with `ok gate expand {path} --reason \"<why>\"`\n"
            ));
        }
    }
    message.push_str(
        "Unstage them (`git restore --staged <path>`) and commit again. If you are an agent, \
         stop here and tell the user what you wanted to commit and why.",
    );
    eprintln!("{message}");
    std::process::exit(1);
}

/// `ok gate install git`: put the gate in git's pre-commit hook, where git looks for it
/// (`core.hooksPath` included). A hook that is already there and is not the gate's is never
/// overwritten: the line to add to it is printed.
fn gate_install_git(json: bool, repo: &Path, apply: bool) -> anyhow::Result<()> {
    let located = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--git-path", "hooks/pre-commit"])
        .output()?;
    if !located.status.success() {
        return Err(caller_input_error(format!(
            "{} is not inside a git working tree; `ok gate install git` needs one",
            repo.display()
        )));
    }
    let relative = PathBuf::from(String::from_utf8_lossy(&located.stdout).trim());
    let path = if relative.is_absolute() {
        relative
    } else {
        repo.join(relative)
    };
    let script = format!(
        "#!/bin/sh\n# Open Kioku gate: refuse a commit that stages a forbidden path. Installed by\n\
         # `ok gate install git`; `ok` must be on PATH.\nexec {GATE_GIT_HOOK_COMMAND}\n"
    );
    let existing = fs::read_to_string(&path).ok();
    let state = match &existing {
        Some(text) if text.contains(GATE_GIT_HOOK_COMMAND) => "present",
        Some(_) => "foreign",
        None => "absent",
    };
    let text = match (state, apply) {
        ("present", _) => format!(
            "{} already runs the gate; nothing changed.",
            path.display()
        ),
        ("foreign", _) => format!(
            "{} exists and is not the gate's, so it is left as it is. Add this line to it, \
             before anything that should not run on a refused commit:\n  {GATE_GIT_HOOK_COMMAND} || exit 1",
            path.display()
        ),
        (_, false) => format!(
            "Proposed {} (nothing written without --apply):\n{script}",
            path.display()
        ),
        (_, true) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, &script)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            }
            format!(
                "Wrote {}.\nEvery `git commit` in this repository now passes `{GATE_GIT_HOOK_COMMAND}`: a \
                 staged secret-like, dependency or generated path refuses the commit, whoever \
                 or whatever made it. `ok` must be on PATH for git.",
                path.display()
            )
        }
    };
    print_text_or_json(
        json,
        &text,
        &serde_json::json!({
            "path": path,
            "state": state,
            "applied": apply && state == "absent",
        }),
    )
}

fn gate_install(json: bool, repo: &Path, client: GateClient, apply: bool) -> anyhow::Result<()> {
    if client == GateClient::Git {
        return gate_install_git(json, repo, apply);
    }
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
        assert_eq!(added, 6);
        assert_eq!(settings["permissions"]["allow"][0], "Bash(ls)");
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "Bash");
        assert_eq!(merge_gate_hooks(&mut settings), 0);
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn gate_hooks_replace_a_non_object_settings_value() {
        let mut settings = serde_json::json!("junk");
        assert_eq!(merge_gate_hooks(&mut settings), 6);
        assert!(settings["hooks"]["UserPromptSubmit"].is_array());
    }

    #[test]
    fn the_plugins_hooks_file_carries_exactly_the_installers_entries() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hooks/hooks.json");
        let shipped: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            shipped["hooks"],
            gate_claude_hooks(),
            "hooks/hooks.json and `ok gate install claude` must agree"
        );
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
        assert_eq!(
            gate_canonical_lossy(&missing),
            real.join("not").join("yet.rs")
        );
    }
}
