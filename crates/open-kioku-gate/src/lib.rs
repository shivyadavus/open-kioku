//! The gate between a coding agent and the merge button.
//!
//! A gate session starts from a saved plan: the plan's recommended change boundary becomes
//! the session's boundary, and every edit the agent's harness reports (through a hook) is
//! classified against it before it lands. When the agent stops, the actual change is verified
//! against the same boundary and the result is written as a *Change Dossier*: one versioned
//! artifact that says what was declared, what was edited, what was blocked, what the
//! verification found, and what is still uncertain.
//!
//! This crate owns the session and dossier schemas, the per-path decision, the hook wire
//! format, and the dossier rendering. It opens no index and runs no verification itself: the
//! CLI hands it a [`open_kioku_core::PlanReport`] to start from and a
//! [`open_kioku_patch::ChangeVerificationReport`] to finish with, so the hot path (one
//! decision per edit) reads one small JSON file and nothing else.

use chrono::{DateTime, Utc};
use open_kioku_core::{BoundaryForbiddenRule, PlanReport, TestTarget};
use open_kioku_errors::{OkError, Result};
use open_kioku_patch::{forbidden_rule_matches, ChangeVerificationReport, VerificationVerdict};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};

/// Schema version carried by every session, event, and dossier this crate writes.
pub const GATE_SCHEMA_VERSION: u32 = 1;
/// Where a repository's gate state lives, relative to the repository root.
pub const GATE_DIR: &str = ".ok/gate";
const SESSION_FILE: &str = "session.json";
const PLAN_FILE: &str = "plan.json";
const EVENTS_FILE: &str = "events.jsonl";
const DOSSIER_DIR: &str = "dossiers";
const SNAPSHOT_DIR: &str = "snapshots";
const LAST_VERIFY_FILE: &str = "last-verify.json";

/// Git's empty tree: the base of a session started in a repository with no commits yet, so
/// every tracked file counts as the session's change.
pub const EMPTY_TREE_REV: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// What the last verification saw and said, keyed by a fingerprint of the trail and the
/// dirty tree. A stop that finds the same fingerprint answers from it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastVerify {
    pub fingerprint: String,
    pub verdict: DossierVerdict,
    pub summary: String,
    pub dossier_markdown: PathBuf,
}

/// How strictly a session holds edits to its boundary.
///
/// A forbidden path (secrets, generated code, the index itself) is denied in every mode but
/// `observe`: a boundary that lets an agent edit `.env` is not a boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateMode {
    /// Deny edits outside the boundary; the agent must widen it with a reason first.
    Enforce,
    /// Allow edits outside the boundary, tell the agent, and let verification judge them.
    #[default]
    Warn,
    /// Record every edit; deny nothing. For measuring what an agent does unconstrained.
    Observe,
}

impl GateMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "enforce" => Some(Self::Enforce),
            "warn" => Some(Self::Warn),
            "observe" => Some(Self::Observe),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enforce => "enforce",
            Self::Warn => "warn",
            Self::Observe => "observe",
        }
    }
}

/// A path the agent added to the boundary during the session, with the reason it gave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryExpansion {
    pub path: String,
    pub reason: String,
    pub at: DateTime<Utc>,
}

/// The boundary a session holds edits to: the plan's, plus any expansions made since.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GateBoundary {
    pub allowed: Vec<String>,
    pub caution: Vec<String>,
    pub forbidden: Vec<String>,
    #[serde(default)]
    pub forbidden_rules: Vec<BoundaryForbiddenRule>,
    #[serde(default)]
    pub expansions: Vec<BoundaryExpansion>,
}

impl GateBoundary {
    pub fn from_plan(plan: &PlanReport) -> Self {
        let boundary = &plan.recommended_change_boundary;
        Self {
            allowed: boundary.allowed_files.iter().map(normalize).collect(),
            caution: boundary.caution_files.iter().map(normalize).collect(),
            forbidden: boundary.forbidden_files.iter().map(normalize).collect(),
            forbidden_rules: boundary.forbidden_rules.clone(),
            expansions: Vec::new(),
        }
    }

    /// Where `path` (repository-relative, `/`-separated) stands against this boundary.
    pub fn classify(&self, path: &str) -> PathClass {
        let path = normalize_str(path);
        if open_kioku_core::is_secret_like_path(Path::new(&path))
            || self.forbidden.iter().any(|f| f == &path)
            || self
                .forbidden_rules
                .iter()
                .any(|rule| forbidden_rule_matches(rule, &path))
        {
            return PathClass::Forbidden;
        }
        if self.allowed.iter().any(|a| a == &path) {
            return PathClass::Allowed;
        }
        if self.expansions.iter().any(|e| e.path == path) {
            return PathClass::Expanded;
        }
        if self.caution.iter().any(|c| c == &path) {
            return PathClass::Caution;
        }
        PathClass::Outside
    }
}

/// The relation of one path to the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathClass {
    Allowed,
    Caution,
    Expanded,
    Forbidden,
    Outside,
}

impl PathClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Caution => "caution",
            Self::Expanded => "expanded",
            Self::Forbidden => "forbidden",
            Self::Outside => "outside",
        }
    }
}

/// What the hook tells the harness to do with an edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookAction {
    Allow,
    /// Allow, and tell the agent the edit is outside what it declared.
    Warn,
    Deny,
}

/// The decision for one path under one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathDecision {
    pub path: String,
    pub class: PathClass,
    pub action: HookAction,
    pub reason: String,
}

/// A gate session: a task, its boundary, and where it started from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateSession {
    pub schema_version: u32,
    pub id: String,
    pub task: String,
    pub mode: GateMode,
    pub started_at: DateTime<Utc>,
    /// `git rev-parse HEAD` when the session started, or [`EMPTY_TREE_REV`] in a repository
    /// with no commits yet; absent outside a git repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_rev: Option<String>,
    pub plan_sha256: String,
    pub risk_level: String,
    pub risk_score: f32,
    pub confidence_summary: String,
    /// The name the dossier gives the repository; set by the CLI from ok.toml or the directory.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo_name: String,
    pub boundary: GateBoundary,
    /// Paths already modified or untracked when the session started. A path here that no
    /// hook reported editing during the session is not the session's change and is left out
    /// of verification; the dossier lists what was left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preexisting_changes: Vec<String>,
}

impl GateSession {
    pub fn new(task: &str, mode: GateMode, plan: &PlanReport, plan_json: &[u8]) -> Self {
        let now = Utc::now();
        Self {
            schema_version: GATE_SCHEMA_VERSION,
            id: session_id(task, now),
            task: task.to_string(),
            mode,
            started_at: now,
            base_rev: None,
            plan_sha256: sha256_hex(plan_json),
            risk_level: plan.risk.level.clone(),
            risk_score: plan.risk.score,
            confidence_summary: plan.confidence_summary.clone(),
            repo_name: String::new(),
            boundary: GateBoundary::from_plan(plan),
            preexisting_changes: Vec::new(),
        }
    }

    /// Decide one edit. The path is repository-relative.
    pub fn decide(&self, path: &str) -> PathDecision {
        let path = normalize_str(path);
        let class = self.boundary.classify(&path);
        let (action, reason) = match (class, self.mode) {
            (_, GateMode::Observe) => (
                HookAction::Allow,
                "observe mode records and never denies".into(),
            ),
            (PathClass::Forbidden, _) => (
                HookAction::Deny,
                format!(
                    "`{path}` is forbidden: it matches the secret-path policy or a forbidden \
                     boundary rule (secrets, key material, generated code, dependencies, or the \
                     index). The gate never admits a forbidden path, and `ok gate expand` refuses \
                     it; change the task if this file truly must change."
                ),
            ),
            (PathClass::Allowed, _) | (PathClass::Expanded, _) => {
                (HookAction::Allow, "inside the declared boundary".into())
            }
            (PathClass::Caution, _) => (
                HookAction::Warn,
                format!(
                    "`{path}` is a caution path: the plan expects it may need touching but did \
                     not commit to it. Verification will warn."
                ),
            ),
            (PathClass::Outside, GateMode::Enforce) => (
                HookAction::Deny,
                format!(
                    "`{path}` is outside the declared boundary for task \"{}\". If this edit is \
                     required, widen the boundary first with a reason: \
                     `ok gate expand {path} --reason \"<why this file must change>\"`, then retry. \
                     The expansion and its reason are recorded in the Change Dossier.",
                    self.task
                ),
            ),
            (PathClass::Outside, GateMode::Warn) => (
                HookAction::Warn,
                format!(
                    "`{path}` is outside the declared boundary for task \"{}\". Verification \
                     will fail unless the boundary is widened: \
                     `ok gate expand {path} --reason \"<why this file must change>\"`.",
                    self.task
                ),
            ),
        };
        PathDecision {
            path,
            class,
            action,
            reason,
        }
    }
}

/// What happened during a session, one record per line in `events.jsonl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    SessionStarted,
    EditAllowed,
    EditCaution,
    EditWarned,
    EditDenied,
    BoundaryExpanded,
    VerifyRun,
    SessionFinished,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateEvent {
    pub schema_version: u32,
    pub at: DateTime<Utc>,
    pub session_id: String,
    pub kind: EventKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl GateEvent {
    pub fn new(session_id: &str, kind: EventKind) -> Self {
        Self {
            schema_version: GATE_SCHEMA_VERSION,
            at: Utc::now(),
            session_id: session_id.to_string(),
            kind,
            path: None,
            tool: None,
            detail: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.tool = Some(tool.into());
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn for_decision(session_id: &str, decision: &PathDecision, tool: &str) -> Self {
        let kind = match (decision.action, decision.class) {
            (HookAction::Deny, _) => EventKind::EditDenied,
            (HookAction::Warn, PathClass::Caution) => EventKind::EditCaution,
            (HookAction::Warn, _) => EventKind::EditWarned,
            (HookAction::Allow, _) => EventKind::EditAllowed,
        };
        Self::new(session_id, kind)
            .with_path(decision.path.clone())
            .with_tool(tool)
            .with_detail(decision.class.as_str())
    }
}

/// The gate's on-disk state for one repository, under `.ok/gate`.
#[derive(Debug, Clone)]
pub struct GateStore {
    root: PathBuf,
}

impl GateStore {
    pub fn for_repo(repo: impl AsRef<Path>) -> Self {
        Self {
            root: repo.as_ref().join(GATE_DIR),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn session_path(&self) -> PathBuf {
        self.root.join(SESSION_FILE)
    }

    pub fn plan_path(&self) -> PathBuf {
        self.root.join(PLAN_FILE)
    }

    pub fn events_path(&self) -> PathBuf {
        self.root.join(EVENTS_FILE)
    }

    pub fn dossier_path(&self, session_id: &str, extension: &str) -> PathBuf {
        self.root
            .join(DOSSIER_DIR)
            .join(format!("{session_id}.{extension}"))
    }

    /// The active session, or `None` when no gate session has been started (or the last one
    /// was finished).
    pub fn active_session(&self) -> Result<Option<GateSession>> {
        let path = self.session_path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        let session: GateSession = serde_json::from_slice(&bytes).map_err(|err| {
            OkError::Storage(format!("{} is not a gate session: {err}", path.display()))
        })?;
        if session.schema_version != GATE_SCHEMA_VERSION {
            return Err(OkError::Storage(format!(
                "{} has gate schema version {}, this build reads {}",
                path.display(),
                session.schema_version,
                GATE_SCHEMA_VERSION
            )));
        }
        Ok(Some(session))
    }

    /// Start a session: write the plan and the session, and reset the event trail.
    pub fn start(&self, session: &GateSession, plan_json: &[u8]) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::write(self.plan_path(), plan_json)?;
        self.write_session(session)?;
        self.archive_events()?;
        fs::write(self.events_path(), b"")?;
        self.append_event(&GateEvent::new(&session.id, EventKind::SessionStarted))
    }

    /// Keep the previous session's trail as `events-<session id>.jsonl` before a new session
    /// starts, so replacing a session with `--force` loses no record.
    fn archive_events(&self) -> Result<()> {
        let path = self.events_path();
        let Ok(metadata) = fs::metadata(&path) else {
            return Ok(());
        };
        if metadata.len() == 0 {
            return Ok(());
        }
        let first = fs::read_to_string(&path)?
            .lines()
            .find_map(|line| serde_json::from_str::<GateEvent>(line).ok())
            .map(|event| event.session_id)
            .unwrap_or_else(|| Utc::now().format("%Y%m%dT%H%M%SZ").to_string());
        let archive = self.root.join(format!("events-{first}.jsonl"));
        if archive.exists() {
            // The same session's trail was archived already: append this remainder to it.
            let mut file = fs::OpenOptions::new().append(true).open(&archive)?;
            file.write_all(&fs::read(&path)?)?;
            fs::remove_file(&path)?;
        } else {
            fs::rename(&path, &archive)?;
        }
        Ok(())
    }

    /// The last verification's fingerprint and outcome, so a stop with nothing changed since
    /// answers from it instead of verifying again.
    pub fn write_last_verify(&self, record: &LastVerify) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::write(
            self.root.join(LAST_VERIFY_FILE),
            serde_json::to_string(record)?,
        )?;
        Ok(())
    }

    pub fn read_last_verify(&self) -> Option<LastVerify> {
        let bytes = fs::read(self.root.join(LAST_VERIFY_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Write the session atomically: a hook reading it while `expand` rewrites it sees the
    /// old session or the new one, never a torn file.
    pub fn write_session(&self, session: &GateSession) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let text = serde_json::to_string_pretty(session)?;
        let tmp = self
            .root
            .join(format!("{SESSION_FILE}.tmp-{}", std::process::id()));
        fs::write(&tmp, text)?;
        fs::rename(&tmp, self.session_path())?;
        Ok(())
    }

    /// Remember the dirty paths before a shell command, keyed by the tool call, so the
    /// post-tool hook can tell what the command changed.
    pub fn write_snapshot(&self, id: &str, dirty: &[String]) -> Result<()> {
        let dir = self.root.join(SNAPSHOT_DIR);
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_string(dirty)?,
        )?;
        Ok(())
    }

    /// The snapshot `write_snapshot` stored under `id`, removed as it is read.
    pub fn take_snapshot(&self, id: &str) -> Result<Option<Vec<String>>> {
        let path = self.root.join(SNAPSHOT_DIR).join(format!("{id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        let _ = fs::remove_file(&path);
        Ok(serde_json::from_slice(&bytes).ok())
    }

    pub fn saved_plan(&self) -> Result<PlanReport> {
        let bytes = fs::read(self.plan_path())?;
        serde_json::from_slice(&bytes).map_err(|err| {
            OkError::Storage(format!(
                "{} is not a saved plan: {err}",
                self.plan_path().display()
            ))
        })
    }

    /// Append one event. Appends are line-atomic on local filesystems, so two hooks firing
    /// for parallel tool calls do not corrupt each other's records.
    pub fn append_event(&self, event: &GateEvent) -> Result<()> {
        fs::create_dir_all(&self.root)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.events_path())?;
        let mut line = serde_json::to_string(event)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        Ok(())
    }

    /// Every event of the active session, in order. A malformed line is skipped and counted
    /// in the dossier's caveats rather than failing the read: the trail is evidence, and a
    /// torn write must not hide the rest of it.
    pub fn events(&self) -> Result<(Vec<GateEvent>, usize)> {
        let path = self.events_path();
        if !path.exists() {
            return Ok((Vec::new(), 0));
        }
        let file = fs::File::open(&path)?;
        let mut events = Vec::new();
        let mut malformed = 0usize;
        for line in std::io::BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<GateEvent>(&line) {
                Ok(event) => events.push(event),
                Err(_) => malformed += 1,
            }
        }
        Ok((events, malformed))
    }

    /// Widen the active session's boundary by one path, recording why.
    pub fn expand(&self, session: &mut GateSession, path: &str, reason: &str) -> Result<()> {
        let path = normalize_str(path);
        if reason.trim().is_empty() {
            return Err(OkError::InvalidInput(
                "a boundary expansion needs a reason; it is recorded in the dossier".into(),
            ));
        }
        if session.boundary.classify(&path) == PathClass::Forbidden {
            return Err(OkError::PolicyDenied(format!(
                "`{path}` matches a forbidden boundary rule and cannot be admitted by expansion"
            )));
        }
        if session.boundary.expansions.iter().any(|e| e.path == path) {
            return Ok(());
        }
        session.boundary.expansions.push(BoundaryExpansion {
            path: path.clone(),
            reason: reason.trim().to_string(),
            at: Utc::now(),
        });
        self.write_session(session)?;
        self.append_event(
            &GateEvent::new(&session.id, EventKind::BoundaryExpanded)
                .with_path(path)
                .with_detail(reason.trim()),
        )
    }

    /// End the active session. The plan, events, and dossiers stay on disk for the record.
    pub fn finish(&self, session: &GateSession) -> Result<()> {
        self.append_event(&GateEvent::new(&session.id, EventKind::SessionFinished))?;
        let path = self.session_path();
        if path.exists() {
            fs::remove_file(path)?;
        }
        // Per-tool-call snapshots and the last-verification record belong to the session.
        let _ = fs::remove_dir_all(self.root.join(SNAPSHOT_DIR));
        let _ = fs::remove_file(self.root.join(LAST_VERIFY_FILE));
        Ok(())
    }

    pub fn write_dossier(&self, dossier: &ChangeDossier) -> Result<(PathBuf, PathBuf)> {
        let dir = self.root.join(DOSSIER_DIR);
        fs::create_dir_all(&dir)?;
        let json_path = self.dossier_path(&dossier.session_id, "json");
        let md_path = self.dossier_path(&dossier.session_id, "md");
        fs::write(&json_path, serde_json::to_string_pretty(dossier)?)?;
        fs::write(&md_path, render_markdown(dossier))?;
        Ok((json_path, md_path))
    }
}

/// A plan with the session's expansions admitted to its boundary, so verification holds the
/// change to what the agent declared *and* what it widened with a reason. The dossier lists
/// every expansion; nothing is admitted silently.
pub fn plan_with_expansions(mut plan: PlanReport, session: &GateSession) -> PlanReport {
    for expansion in &session.boundary.expansions {
        let path = PathBuf::from(&expansion.path);
        if !plan
            .recommended_change_boundary
            .allowed_files
            .iter()
            .any(|p| normalize(p) == expansion.path)
        {
            plan.recommended_change_boundary.allowed_files.push(path);
        }
    }
    plan
}

// ---------------------------------------------------------------------------------------------
// Hook wire format
// ---------------------------------------------------------------------------------------------

/// What a coding-agent harness sends a hook on stdin. The field names are Claude Code's;
/// other harnesses are adapted to them by the CLI.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub hook_event_name: String,
    #[serde(default)]
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: serde_json::Value,
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub stop_hook_active: bool,
}

impl HookInput {
    pub fn parse(reader: impl std::io::Read) -> Result<Self> {
        serde_json::from_reader(reader).map_err(|err| {
            OkError::InvalidInput(format!("hook input is not the expected JSON: {err}"))
        })
    }

    /// Whether this tool writes repository files.
    pub fn is_edit_tool(&self) -> bool {
        matches!(
            self.tool_name.as_str(),
            "Edit" | "Write" | "MultiEdit" | "NotebookEdit"
        )
    }

    /// Whether this tool runs a shell command, which may write files the edit hooks never see.
    pub fn is_shell_tool(&self) -> bool {
        self.tool_name == "Bash"
    }

    /// The shell command a shell tool runs.
    pub fn shell_command(&self) -> Option<&str> {
        self.tool_input.get("command").and_then(|v| v.as_str())
    }

    /// The absolute paths this tool call writes.
    pub fn edited_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for key in ["file_path", "notebook_path"] {
            if let Some(path) = self.tool_input.get(key).and_then(|v| v.as_str()) {
                paths.push(PathBuf::from(path));
            }
        }
        paths
    }
}

/// What the hook writes to stdout. Serialized as Claude Code's `hookSpecificOutput` shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookResponse {
    /// Say nothing; the harness proceeds as it would have.
    Silent,
    /// Refuse the tool call and show the agent why.
    Deny { event: String, reason: String },
    /// Let the call proceed and add context the agent sees.
    Context { event: String, text: String },
    /// Keep the agent working (Stop hooks): it may not finish until `reason` is addressed.
    Block { reason: String },
}

impl HookResponse {
    pub fn to_json(&self) -> Option<serde_json::Value> {
        match self {
            Self::Silent => None,
            Self::Deny { event, reason } => Some(serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": event,
                    "permissionDecision": "deny",
                    "permissionDecisionReason": reason,
                }
            })),
            Self::Context { event, text } => Some(serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": event,
                    "additionalContext": text,
                }
            })),
            Self::Block { reason } => Some(serde_json::json!({
                "decision": "block",
                "reason": reason,
            })),
        }
    }
}

/// `path` relative to `repo`, `/`-separated, or `None` when it lies outside the repository.
/// Both are compared lexically after resolving `.` and `..`; a hook must not touch the
/// filesystem to answer.
pub fn repo_relative(repo: &Path, path: &Path) -> Option<String> {
    let repo = lexical_normalize(repo);
    let path = if path.is_absolute() {
        lexical_normalize(path)
    } else {
        lexical_normalize(&repo.join(path))
    };
    let relative = path.strip_prefix(&repo).ok()?;
    let text = normalize(relative);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The files a shell command names as something it writes: redirection targets (`>`, `>>`,
/// `&>`), `tee` files, `sed -i` files, `cp`/`mv`/`install` destinations, `touch`, `rm`,
/// `truncate`, `git mv` and `git rm` paths. Paths come back absolute, resolved against `cwd`
/// as the command's own `cd`/`pushd` move it. Heuristic and conservative: it reads the
/// command text, not the shell, so a target it cannot resolve (`~`, a variable, a command
/// substitution, a glob) is left out rather than guessed. What it misses, the post-tool hook
/// finds in the tree.
pub fn shell_write_targets(command: &str, cwd: &Path) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    let mut cwd: Option<PathBuf> = Some(lexical_normalize(cwd));
    for segment in split_shell_segments(command) {
        let words = shell_words(&segment)
            .iter()
            .map(|w| trim_group(w))
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>();
        if words.is_empty() {
            continue;
        }
        let mut raw: Vec<String> = Vec::new();
        // Redirections anywhere in the segment: `> f`, `>> f`, `>f`, `2>f`, `&> f`.
        let mut i = 0;
        while i < words.len() {
            let stripped = redirect_operator(&words[i]);
            if matches!(stripped, Some(">") | Some(">>") | Some(">|")) {
                if let Some(next) = words.get(i + 1) {
                    raw.push(next.clone());
                }
                i += 2;
                continue;
            }
            if let Some(rest) = stripped.and_then(|s| {
                s.strip_prefix(">>")
                    .or_else(|| s.strip_prefix(">|"))
                    .or_else(|| s.strip_prefix('>'))
            }) {
                if !rest.is_empty() && !rest.starts_with('&') {
                    raw.push(rest.to_string());
                }
            }
            i += 1;
        }
        // The command word, past `sudo`, `env VAR=x`, `command`, `builtin`, `nohup`, `time`.
        let mut start = 0;
        while start < words.len()
            && (matches!(
                words[start].as_str(),
                "sudo" | "env" | "command" | "builtin" | "nohup" | "time" | "exec"
            ) || words[start].contains('=') && !words[start].starts_with('-'))
        {
            start += 1;
        }
        let Some(name) = words.get(start) else {
            continue;
        };
        let name = name.rsplit('/').next().unwrap_or(name).to_string();
        let args: Vec<&str> = words[start + 1..]
            .iter()
            .map(String::as_str)
            .take_while(|w| redirect_operator(w).is_none())
            .collect();
        let operands: Vec<&str> = args
            .iter()
            .copied()
            .filter(|a| !a.starts_with('-') || *a == "-")
            .collect();
        match name.as_str() {
            "cd" | "pushd" => {
                cwd = match operands.first().copied() {
                    None | Some("-") => None,
                    Some(dir) if dir.starts_with('~') || dir.starts_with('$') => None,
                    Some(dir) if Path::new(dir).is_absolute() => {
                        Some(lexical_normalize(Path::new(dir)))
                    }
                    Some(dir) => cwd.as_ref().map(|c| lexical_normalize(&c.join(dir))),
                };
                continue;
            }
            "tee" | "touch" | "rm" | "truncate" | "unlink" | "mkfifo" => {
                raw.extend(operands.iter().map(|o| o.to_string()))
            }
            "sed"
                if args
                    .iter()
                    .any(|a| a.starts_with("-i") || *a == "--in-place") =>
            {
                // Every operand after the script is a file. The script is the first operand,
                // unless `-e`/`-f` (or their long forms) supplied it, in which case the word
                // after each such flag is the script and every remaining operand is a file.
                let script_flags = ["-e", "-f", "--expression", "--file"];
                let scripted = args.iter().any(|a| {
                    script_flags.contains(a)
                        || a.starts_with("--expression=")
                        || a.starts_with("--file=")
                });
                let mut skip_next = false;
                let mut first_operand_seen = false;
                for arg in &args {
                    if skip_next {
                        skip_next = false;
                        continue;
                    }
                    if script_flags.contains(arg) {
                        skip_next = true;
                        continue;
                    }
                    if arg.starts_with('-') && *arg != "-" {
                        continue;
                    }
                    if !scripted && !first_operand_seen {
                        first_operand_seen = true;
                        continue;
                    }
                    raw.push(arg.to_string());
                }
            }
            "cp" | "mv" | "install" | "ln" => {
                if let Some(dest) = operands.last() {
                    raw.push(dest.to_string());
                }
            }
            "git" => match operands.first().copied() {
                Some("mv") | Some("rm") => raw.extend(operands[1..].iter().map(|o| o.to_string())),
                _ => {}
            },
            _ => {}
        }
        for target in raw {
            if let Some(path) = resolve_target(&target, cwd.as_deref()) {
                targets.push(path);
            }
        }
    }
    targets.sort();
    targets.dedup();
    targets
}

/// The redirection operator a word carries (`>`, `>>`, `>|`, `>f`, `2>f`, `&>f`), with any
/// descriptor or `&` prefix removed; `None` for a word that is not a redirection.
fn redirect_operator(word: &str) -> Option<&str> {
    let s = word.trim_start_matches(|c: char| c.is_ascii_digit());
    let s = s.strip_prefix('&').unwrap_or(s);
    s.starts_with('>').then_some(s)
}

/// Subshell and group punctuation glued to a word: `(echo`, `auth.rs)`, `{`, `}`.
fn trim_group(word: &str) -> String {
    if word.starts_with("$(") {
        return word.to_string();
    }
    word.trim_start_matches(['(', '{'])
        .trim_end_matches([')', '}'])
        .to_string()
}

fn resolve_target(raw: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let value = raw.trim_matches(|c| c == '"' || c == '\'');
    if value.is_empty()
        || value == "-"
        || value.starts_with("/dev/")
        || value.starts_with('$')
        || value.starts_with('~')
        || value.contains("$(")
        || value.contains('`')
        || value.contains(['*', '?', '['])
    {
        return None;
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Some(lexical_normalize(path));
    }
    cwd.map(|c| lexical_normalize(&c.join(path)))
}

/// Split on `;`, `&&`, `||`, `|` and newlines outside quotes. A heredoc body is dropped: its
/// lines are data, not commands.
fn split_shell_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    let mut heredoc: Option<String> = None;
    let mut line = String::new();
    while let Some(c) = chars.next() {
        if let Some(terminator) = &heredoc {
            if c == '\n' {
                if line.trim() == terminator {
                    heredoc = None;
                }
                line.clear();
            } else {
                line.push(c);
            }
            continue;
        }
        match quote {
            Some(q) => {
                current.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    current.push(c);
                }
                '<' if chars.peek() == Some(&'<') => {
                    chars.next();
                    if chars.peek() == Some(&'<') {
                        chars.next();
                        current.push_str("<<<");
                        continue;
                    }
                    // `<<EOF`, `<<-EOF`, `<<'EOF'`, `<< "EOF"`: the terminator word.
                    let mut word = String::new();
                    while let Some(&n) = chars.peek() {
                        if n == ' ' || n == '-' {
                            chars.next();
                            if word.is_empty() {
                                continue;
                            }
                            break;
                        }
                        if n.is_whitespace() || n == ';' || n == '|' || n == '&' || n == '>' {
                            break;
                        }
                        word.push(n);
                        chars.next();
                    }
                    let word = word.trim_matches(|c| c == '\'' || c == '"').to_string();
                    // The rest of this line is still the command; the body starts next line.
                    while let Some(&n) = chars.peek() {
                        if n == '\n' {
                            chars.next();
                            break;
                        }
                        current.push(n);
                        chars.next();
                    }
                    heredoc = Some(word);
                    line.clear();
                }
                ';' | '\n' => {
                    segments.push(std::mem::take(&mut current));
                }
                '|' | '&' => {
                    if chars.peek() == Some(&c) {
                        chars.next();
                    }
                    segments.push(std::mem::take(&mut current));
                }
                _ => current.push(c),
            },
        }
    }
    segments.push(current);
    segments
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect()
}

/// Whitespace-split with quotes honoured and `$(...)` and backtick substitutions kept whole;
/// quotes stay on the word so `resolve_target` can strip them.
fn shell_words(segment: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut substitution_depth = 0usize;
    let mut in_backtick = false;
    let chars: Vec<char> = segment.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            current.push(c);
            if c == q {
                quote = None;
            }
        } else if in_backtick {
            current.push(c);
            if c == '`' {
                in_backtick = false;
            }
        } else if substitution_depth > 0 {
            current.push(c);
            if c == '(' {
                substitution_depth += 1;
            } else if c == ')' {
                substitution_depth -= 1;
            }
        } else if c == '$' && chars.get(i + 1) == Some(&'(') {
            current.push_str("$(");
            substitution_depth = 1;
            i += 1;
        } else if c == '`' {
            in_backtick = true;
            current.push(c);
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            current.push(c);
        } else if c.is_whitespace() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else {
            current.push(c);
        }
        i += 1;
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

// ---------------------------------------------------------------------------------------------
// Change Dossier
// ---------------------------------------------------------------------------------------------

/// The overall verdict of a dossier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DossierVerdict {
    Pass,
    Warn,
    Fail,
}

impl DossierVerdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DossierRepo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_rev: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_rev: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DossierPlan {
    pub risk_level: String,
    pub risk_score: f32,
    pub confidence_summary: String,
    pub allowed_count: usize,
    pub caution_count: usize,
    pub forbidden_rule_count: usize,
    pub validation_targets: Vec<String>,
    pub plan_sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DossierTrail {
    pub events_total: usize,
    pub edits_allowed: usize,
    pub edits_caution: usize,
    pub edits_warned: usize,
    pub edits_denied: usize,
    pub denied_paths: Vec<String>,
    pub warned_paths: Vec<String>,
    pub edited_paths: Vec<String>,
    pub expansions: Vec<BoundaryExpansion>,
    pub malformed_events: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DossierChange {
    pub changed_files: Vec<String>,
    pub untracked_files: Vec<String>,
    /// Modified before the session started and not edited during it: not this change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preexisting_excluded: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_sha256: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DossierFinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub kind: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DossierCommand {
    pub command: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DossierVerification {
    pub verdict: VerificationVerdict,
    pub boundary_violations: Vec<DossierFinding>,
    pub warnings_count: usize,
    pub warnings: Vec<DossierFinding>,
    pub missing_tests: Vec<DossierFinding>,
    pub changed_impact: Vec<DossierFinding>,
    pub changed_symbols: Vec<String>,
    pub recommended_tests: Vec<String>,
    pub command_results: Vec<DossierCommand>,
}

/// One verified record of one agent change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeDossier {
    pub schema_version: u32,
    pub session_id: String,
    pub task: String,
    pub mode: GateMode,
    pub started_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub repo: DossierRepo,
    pub verdict: DossierVerdict,
    pub plan: DossierPlan,
    pub boundary: GateBoundary,
    pub trail: DossierTrail,
    pub change: DossierChange,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<DossierVerification>,
    pub caveats: Vec<String>,
}

/// What the finishing step knows about the actual change.
#[derive(Debug, Clone, Default)]
pub struct ChangeInput {
    pub changed_files: Vec<String>,
    pub untracked_files: Vec<String>,
    pub preexisting_excluded: Vec<String>,
    pub unified_diff: Option<String>,
    pub head_rev: Option<String>,
    pub source: String,
}

/// Build a dossier. `verification` is `None` when there was nothing to verify (no change);
/// the dossier then says so and passes.
pub fn build_dossier(
    repo_name: &str,
    session: &GateSession,
    plan: &PlanReport,
    events: &[GateEvent],
    malformed_events: usize,
    change: &ChangeInput,
    verification: Option<&ChangeVerificationReport>,
) -> ChangeDossier {
    let mut trail = DossierTrail {
        events_total: events.len(),
        malformed_events,
        expansions: session.boundary.expansions.clone(),
        ..DossierTrail::default()
    };
    let mut edited = BTreeSet::new();
    let mut denied = BTreeSet::new();
    let mut warned = BTreeSet::new();
    for event in events {
        match event.kind {
            EventKind::EditAllowed => {
                trail.edits_allowed += 1;
                edited.extend(event.path.clone());
            }
            EventKind::EditCaution => {
                trail.edits_caution += 1;
                edited.extend(event.path.clone());
            }
            EventKind::EditWarned => {
                trail.edits_warned += 1;
                edited.extend(event.path.clone());
                warned.extend(event.path.clone());
            }
            EventKind::EditDenied => {
                trail.edits_denied += 1;
                denied.extend(event.path.clone());
            }
            _ => {}
        }
    }
    trail.edited_paths = edited.into_iter().collect();
    trail.denied_paths = denied.into_iter().collect();
    trail.warned_paths = warned.into_iter().collect();

    let mut caveats = Vec::new();
    if malformed_events > 0 {
        caveats.push(format!(
            "{malformed_events} event record(s) could not be read; the edit trail may be incomplete"
        ));
    }
    if events.iter().all(|e| {
        !matches!(
            e.kind,
            EventKind::EditAllowed
                | EventKind::EditCaution
                | EventKind::EditWarned
                | EventKind::EditDenied
        )
    }) && !change.changed_files.is_empty()
    {
        caveats.push(
            "the change was made without the edit hooks reporting any edit: the trail shows \
             what the harness reported, not what happened"
                .into(),
        );
    }
    if session.base_rev.is_none() {
        caveats.push(
            "not a git repository: the change is what the hooks reported, with no diff".into(),
        );
    }
    if !change.preexisting_excluded.is_empty() {
        caveats.push(format!(
            "{} path(s) were already modified before the session started and no hook reported \
             editing them, so they are not judged as this change: {}",
            change.preexisting_excluded.len(),
            change.preexisting_excluded.join(", ")
        ));
    }
    if session.mode == GateMode::Observe {
        caveats.push("observe mode: nothing was denied during the session".into());
    }
    let changed = change
        .changed_files
        .iter()
        .map(|path| normalize_str(path))
        .collect::<BTreeSet<_>>();
    let warned_absent = trail
        .warned_paths
        .iter()
        .filter(|path| !changed.contains(*path))
        .cloned()
        .collect::<Vec<_>>();
    if !warned_absent.is_empty() {
        caveats.push(format!(
            "{} edit(s) the hook allowed outside the boundary are not in the change (reverted, \
             or under a path git ignores, which cannot reach a commit): {}",
            warned_absent.len(),
            warned_absent.join(", ")
        ));
    }

    let verification_summary = verification.map(|report| DossierVerification {
        verdict: report.verdict,
        boundary_violations: report.boundary_violations.iter().map(finding).collect(),
        warnings_count: report.warnings.len(),
        warnings: report.warnings.iter().take(12).map(finding).collect(),
        missing_tests: report.missing_tests.iter().map(finding).collect(),
        changed_impact: report.changed_impact.iter().map(finding).collect(),
        changed_symbols: report.changed_symbols.clone(),
        recommended_tests: report
            .recommended_tests
            .iter()
            .take(12)
            .map(test_label)
            .collect(),
        command_results: report
            .command_results
            .iter()
            .map(|r| DossierCommand {
                command: r.command.clone(),
                status: r.status.clone(),
                exit_code: r.exit_code,
            })
            .collect(),
    });
    if verification.is_none() {
        caveats.push("no change to verify: no diff and no reported edits".into());
    }

    let verdict = match verification.map(|v| v.verdict) {
        Some(VerificationVerdict::Fail) => DossierVerdict::Fail,
        Some(VerificationVerdict::Warn) => DossierVerdict::Warn,
        Some(VerificationVerdict::Pass) | None => {
            if trail.edits_warned > 0 || trail.edits_denied > 0 {
                DossierVerdict::Warn
            } else {
                DossierVerdict::Pass
            }
        }
    };

    ChangeDossier {
        schema_version: GATE_SCHEMA_VERSION,
        session_id: session.id.clone(),
        task: session.task.clone(),
        mode: session.mode,
        started_at: session.started_at,
        created_at: Utc::now(),
        repo: DossierRepo {
            name: repo_name.to_string(),
            base_rev: session.base_rev.clone(),
            head_rev: change.head_rev.clone(),
        },
        verdict,
        plan: DossierPlan {
            risk_level: session.risk_level.clone(),
            risk_score: session.risk_score,
            confidence_summary: session.confidence_summary.clone(),
            allowed_count: session.boundary.allowed.len(),
            caution_count: session.boundary.caution.len(),
            forbidden_rule_count: session.boundary.forbidden_rules.len()
                + session.boundary.forbidden.len(),
            validation_targets: plan.validation.iter().take(12).map(test_label).collect(),
            plan_sha256: session.plan_sha256.clone(),
        },
        boundary: session.boundary.clone(),
        trail,
        change: DossierChange {
            changed_files: change.changed_files.clone(),
            untracked_files: change.untracked_files.clone(),
            preexisting_excluded: change.preexisting_excluded.clone(),
            diff_sha256: change
                .unified_diff
                .as_deref()
                .map(|d| sha256_hex(d.as_bytes())),
            source: change.source.clone(),
        },
        verification: verification_summary,
        caveats,
    }
}

fn finding(f: &open_kioku_patch::VerificationFinding) -> DossierFinding {
    DossierFinding {
        path: f.path.as_ref().map(normalize),
        kind: f.kind.clone(),
        reason: f.reason.clone(),
    }
}

fn test_label(test: &TestTarget) -> String {
    match &test.command {
        Some(command) if !command.is_empty() => format!("{} (`{}`)", test.name, command),
        _ => test.name.clone(),
    }
}

/// The dossier as Markdown: the shape a pull request comment or a review tool shows. No
/// source snippets; paths, counts, verdicts, and reasons only.
pub fn render_markdown(dossier: &ChangeDossier) -> String {
    let mut out = String::new();
    let icon = match dossier.verdict {
        DossierVerdict::Pass => "✅",
        DossierVerdict::Warn => "⚠️",
        DossierVerdict::Fail => "❌",
    };
    out.push_str(&format!(
        "## {icon} Change Dossier: {}\n\n",
        dossier.verdict.label()
    ));
    out.push_str(&format!("**Task:** {}\n\n", dossier.task));
    out.push_str(&format!(
        "**Repository:** `{}`  ·  base `{}`  ·  head `{}`  ·  gate mode `{}`  ·  session `{}`\n\n",
        dossier.repo.name,
        short(dossier.repo.base_rev.as_deref()),
        short(dossier.repo.head_rev.as_deref()),
        dossier.mode.as_str(),
        dossier.session_id
    ));

    out.push_str("### Declared before editing\n\n");
    out.push_str(&format!(
        "- Risk `{}` ({:.2}); {}\n",
        dossier.plan.risk_level, dossier.plan.risk_score, dossier.plan.confidence_summary
    ));
    out.push_str(&format!(
        "- Boundary: {} allowed, {} caution, {} forbidden rule(s); plan `sha256:{}`\n",
        dossier.plan.allowed_count,
        dossier.plan.caution_count,
        dossier.plan.forbidden_rule_count,
        &dossier.plan.plan_sha256[..12.min(dossier.plan.plan_sha256.len())]
    ));
    if !dossier.boundary.allowed.is_empty() {
        out.push_str("- Allowed: ");
        out.push_str(&join_code(&dossier.boundary.allowed, 10));
        out.push('\n');
    }
    if !dossier.boundary.expansions.is_empty() {
        out.push_str("- Widened during the session:\n");
        for e in &dossier.boundary.expansions {
            out.push_str(&format!("  - `{}`: {}\n", e.path, e.reason));
        }
    }
    out.push('\n');

    out.push_str("### Edits as the harness reported them\n\n");
    let t = &dossier.trail;
    out.push_str(&format!(
        "- {} allowed, {} caution, {} warned, {} denied ({} event(s))\n",
        t.edits_allowed, t.edits_caution, t.edits_warned, t.edits_denied, t.events_total
    ));
    if !t.denied_paths.is_empty() {
        out.push_str(&format!("- Denied: {}\n", join_code(&t.denied_paths, 10)));
    }
    if !t.warned_paths.is_empty() {
        out.push_str(&format!(
            "- Outside the boundary (warned): {}\n",
            join_code(&t.warned_paths, 10)
        ));
    }
    out.push('\n');

    out.push_str("### The actual change\n\n");
    out.push_str(&format!(
        "- {} changed file(s) from {}",
        dossier.change.changed_files.len(),
        dossier.change.source
    ));
    if let Some(sha) = &dossier.change.diff_sha256 {
        out.push_str(&format!("; diff `sha256:{}`", &sha[..12.min(sha.len())]));
    }
    out.push('\n');
    if !dossier.change.changed_files.is_empty() {
        out.push_str(&format!(
            "- Files: {}\n",
            join_code(&dossier.change.changed_files, 15)
        ));
    }
    if !dossier.change.untracked_files.is_empty() {
        out.push_str(&format!(
            "- New (untracked): {}\n",
            join_code(&dossier.change.untracked_files, 10)
        ));
    }
    if !dossier.change.preexisting_excluded.is_empty() {
        out.push_str(&format!(
            "- Left out (modified before the session, not edited during it): {}\n",
            join_code(&dossier.change.preexisting_excluded, 10)
        ));
    }
    out.push('\n');

    out.push_str("### Verification\n\n");
    match &dossier.verification {
        None => out.push_str("- Not run: there was no change to verify.\n"),
        Some(v) => {
            out.push_str(&format!("- Verdict: **{:?}**\n", v.verdict));
            if v.boundary_violations.is_empty() {
                out.push_str("- Boundary: every changed file is inside the declared boundary.\n");
            } else {
                out.push_str("- Boundary violations:\n");
                for f in &v.boundary_violations {
                    out.push_str(&format!(
                        "  - `{}` [{}] {}\n",
                        f.path.as_deref().unwrap_or("-"),
                        f.kind,
                        f.reason
                    ));
                }
            }
            if !v.changed_impact.is_empty() {
                out.push_str(&format!(
                    "- Impacted dependents not updated: {}\n",
                    v.changed_impact.len()
                ));
                for f in v.changed_impact.iter().take(8) {
                    out.push_str(&format!(
                        "  - `{}` {}\n",
                        f.path.as_deref().unwrap_or("-"),
                        f.reason
                    ));
                }
            }
            if !v.missing_tests.is_empty() {
                out.push_str(&format!(
                    "- Recommended tests the plan did not list: {}\n",
                    v.missing_tests.len()
                ));
            }
            if !v.recommended_tests.is_empty() {
                out.push_str(&format!(
                    "- Validation targets: {}\n",
                    v.recommended_tests
                        .iter()
                        .take(6)
                        .map(|t| format!("`{t}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !v.command_results.is_empty() {
                out.push_str("- Commands run:\n");
                for c in &v.command_results {
                    out.push_str(&format!(
                        "  - `{}` → {} ({})\n",
                        c.command,
                        c.status,
                        c.exit_code
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "no exit code".into())
                    ));
                }
            }
            if v.warnings_count > 0 {
                out.push_str(&format!("- Warnings: {}\n", v.warnings_count));
                for f in v.warnings.iter().take(6) {
                    out.push_str(&format!("  - [{}] {}\n", f.kind, f.reason));
                }
            }
        }
    }
    out.push('\n');

    if !dossier.caveats.is_empty() {
        out.push_str("### Caveats\n\n");
        for c in &dossier.caveats {
            out.push_str(&format!("- {c}\n"));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "<sub>Open Kioku gate · dossier schema v{} · {}</sub>\n",
        dossier.schema_version,
        dossier.created_at.format("%Y-%m-%d %H:%M UTC")
    ));
    out
}

fn join_code(items: &[String], limit: usize) -> String {
    let mut text = items
        .iter()
        .take(limit)
        .map(|i| format!("`{i}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > limit {
        text.push_str(&format!(" … and {} more", items.len() - limit));
    }
    text
}

fn short(rev: Option<&str>) -> String {
    rev.map(|r| r.chars().take(10).collect())
        .unwrap_or_else(|| "-".into())
}

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

pub fn normalize(path: impl AsRef<Path>) -> String {
    normalize_str(&path.as_ref().to_string_lossy())
}

pub fn normalize_str(path: &str) -> String {
    let mut text = path.replace('\\', "/");
    while let Some(rest) = text.strip_prefix("./") {
        text = rest.to_string();
    }
    text.trim_end_matches('/').to_string()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn session_id(task: &str, at: DateTime<Utc>) -> String {
    let digest = sha256_hex(format!("{task}\n{}", at.to_rfc3339()).as_bytes());
    format!("gate-{}-{}", at.format("%Y%m%dT%H%M%SZ"), &digest[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{ChangeBoundary, RiskReport};

    fn plan() -> PlanReport {
        let mut plan: PlanReport = serde_json::from_value(serde_json::json!({
            "task": "add retry to token refresh",
            "summary": "",
            "primary_context": [],
            "relevant_symbols": [],
            "impact": {"target": "", "direct_impacts": [], "indirect_impacts": [],
                        "risk_report": {"level": "low", "score": 0.1, "reasons": []}, "evidence": []},
            "validation": [],
            "risk": {"level": "medium", "score": 0.5, "reasons": []},
            "recommended_change_boundary": {"allowed_files": [], "caution_files": [], "forbidden_files": []},
            "recommended_next_steps": [],
            "tool_calls": [],
            "memory_facts": [],
            "evidence": [],
            "confidence_summary": "overall Medium (0.60)"
        }))
        .expect("minimal plan deserializes");
        plan.risk = RiskReport {
            level: "medium".into(),
            score: 0.5,
            reasons: vec![],
        };
        plan.recommended_change_boundary = ChangeBoundary {
            allowed_files: vec!["src/auth.rs".into(), "./src/lib.rs".into()],
            caution_files: vec!["tests/auth_flow.rs".into()],
            forbidden_files: vec!["Cargo.lock".into()],
            forbidden_rules: vec![
                BoundaryForbiddenRule {
                    pattern: ".ok/**".into(),
                    reason: "index".into(),
                    evidence_refs: vec![],
                },
                BoundaryForbiddenRule {
                    pattern: "**/secrets/**".into(),
                    reason: "secrets".into(),
                    evidence_refs: vec![],
                },
            ],
            ..ChangeBoundary::default()
        };
        plan
    }

    fn session(mode: GateMode) -> GateSession {
        let plan = plan();
        GateSession::new(&plan.task, mode, &plan, b"{}")
    }

    #[test]
    fn classification_follows_the_plan_boundary() {
        let s = session(GateMode::Enforce);
        assert_eq!(s.boundary.classify("src/auth.rs"), PathClass::Allowed);
        assert_eq!(s.boundary.classify("./src/lib.rs"), PathClass::Allowed);
        assert_eq!(
            s.boundary.classify("tests/auth_flow.rs"),
            PathClass::Caution
        );
        assert_eq!(s.boundary.classify("Cargo.lock"), PathClass::Forbidden);
        assert_eq!(
            s.boundary.classify(".env"),
            PathClass::Forbidden,
            "the secret-path policy applies even when the plan's rules omit it"
        );
        assert_eq!(
            s.boundary.classify("config/.env.local"),
            PathClass::Forbidden
        );
        assert_eq!(
            s.boundary.classify("certs/server.pem"),
            PathClass::Forbidden
        );
        assert_eq!(
            s.boundary.classify(".ok/gate/session.json"),
            PathClass::Forbidden
        );
        assert_eq!(
            s.boundary.classify("app/secrets/key.pem"),
            PathClass::Forbidden
        );
        assert_eq!(s.boundary.classify("src/other.rs"), PathClass::Outside);
    }

    #[test]
    fn enforce_denies_outside_and_forbidden_and_warns_on_caution() {
        let s = session(GateMode::Enforce);
        assert_eq!(s.decide("src/auth.rs").action, HookAction::Allow);
        assert_eq!(s.decide("src/other.rs").action, HookAction::Deny);
        assert_eq!(s.decide(".env").action, HookAction::Deny);
        assert_eq!(s.decide("tests/auth_flow.rs").action, HookAction::Warn);
        let d = s.decide("src/other.rs");
        assert!(
            d.reason.contains("ok gate expand src/other.rs"),
            "{}",
            d.reason
        );
    }

    #[test]
    fn warn_allows_outside_but_still_denies_forbidden() {
        let s = session(GateMode::Warn);
        assert_eq!(s.decide("src/other.rs").action, HookAction::Warn);
        assert_eq!(s.decide(".env").action, HookAction::Deny);
    }

    #[test]
    fn observe_denies_nothing() {
        let s = session(GateMode::Observe);
        assert_eq!(s.decide(".env").action, HookAction::Allow);
        assert_eq!(s.decide("src/other.rs").action, HookAction::Allow);
    }

    #[test]
    fn expansion_admits_a_path_with_a_reason_and_never_a_forbidden_one() {
        let dir = tempfile::tempdir().unwrap();
        let store = GateStore::for_repo(dir.path());
        let mut s = session(GateMode::Enforce);
        store.start(&s, b"{}").unwrap();
        assert!(store.expand(&mut s, "src/other.rs", "  ").is_err());
        store
            .expand(&mut s, "./src/other.rs", "the retry helper lives here")
            .unwrap();
        assert_eq!(s.decide("src/other.rs").action, HookAction::Allow);
        assert_eq!(s.decide("src/other.rs").class, PathClass::Expanded);
        assert!(store.expand(&mut s, ".env", "need it").is_err());
        // Persisted: a fresh read sees the expansion.
        let reloaded = store.active_session().unwrap().unwrap();
        assert_eq!(reloaded.boundary.expansions.len(), 1);
        let (events, malformed) = store.events().unwrap();
        assert_eq!(malformed, 0);
        assert!(events
            .iter()
            .any(|e| e.kind == EventKind::BoundaryExpanded
                && e.path.as_deref() == Some("src/other.rs")));
        // Verification sees the widened boundary.
        let widened = plan_with_expansions(plan(), &s);
        assert!(widened
            .recommended_change_boundary
            .allowed_files
            .iter()
            .any(|p| p == Path::new("src/other.rs")));
    }

    #[test]
    fn store_round_trips_session_events_and_finish() {
        let dir = tempfile::tempdir().unwrap();
        let store = GateStore::for_repo(dir.path());
        assert!(store.active_session().unwrap().is_none());
        let s = session(GateMode::Warn);
        store.start(&s, br#"{"task":"x"}"#).unwrap();
        let active = store.active_session().unwrap().unwrap();
        assert_eq!(active.id, s.id);
        assert_eq!(active.mode, GateMode::Warn);
        let d = active.decide("src/other.rs");
        store
            .append_event(&GateEvent::for_decision(&active.id, &d, "Edit"))
            .unwrap();
        // A torn line is skipped and counted, not fatal.
        fs::OpenOptions::new()
            .append(true)
            .open(store.events_path())
            .unwrap()
            .write_all(b"{\"not\": \"an event\n")
            .unwrap();
        let (events, malformed) = store.events().unwrap();
        assert_eq!(malformed, 1);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, EventKind::EditWarned);
        store.finish(&active).unwrap();
        assert!(store.active_session().unwrap().is_none());
        assert!(store.plan_path().exists(), "the plan stays for the record");
    }

    #[test]
    fn repo_relative_paths_are_lexical_and_reject_escapes() {
        let repo = Path::new("/work/repo");
        assert_eq!(
            repo_relative(repo, Path::new("/work/repo/src/auth.rs")).as_deref(),
            Some("src/auth.rs")
        );
        assert_eq!(
            repo_relative(repo, Path::new("/work/repo/src/../src/lib.rs")).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(
            repo_relative(repo, Path::new("src/x.rs")).as_deref(),
            Some("src/x.rs")
        );
        assert_eq!(repo_relative(repo, Path::new("/work/other/x.rs")), None);
        assert_eq!(
            repo_relative(repo, Path::new("/work/repo/../repo2/x.rs")),
            None
        );
        assert_eq!(repo_relative(repo, Path::new("/work/repo")), None);
    }

    #[test]
    fn hook_input_reads_claude_code_shape() {
        let input = HookInput::parse(
            br#"{"session_id":"abc","cwd":"/w","hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{"file_path":"/w/src/a.rs","content":"x"}}"#.as_slice(),
        )
        .unwrap();
        assert!(input.is_edit_tool());
        assert_eq!(input.edited_paths(), vec![PathBuf::from("/w/src/a.rs")]);
        let bash =
            HookInput::parse(br#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#.as_slice())
                .unwrap();
        assert!(!bash.is_edit_tool());
        assert!(bash.edited_paths().is_empty());
    }

    #[test]
    fn shell_write_targets_name_redirections_and_file_commands() {
        let cwd = Path::new("/r");
        let t = |c: &str| {
            shell_write_targets(c, cwd)
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(t("echo x > src/a.rs"), vec!["/r/src/a.rs"]);
        assert_eq!(t("cargo test 2>&1 >> out.log"), vec!["/r/out.log"]);
        assert_eq!(t("echo x >'with space.txt'"), vec!["/r/with space.txt"]);
        assert_eq!(t("printf y | tee -a notes.md"), vec!["/r/notes.md"]);
        assert_eq!(
            t("sed -i 's/a/b/' src/x.rs src/y.rs"),
            vec!["/r/src/x.rs", "/r/src/y.rs"]
        );
        assert_eq!(t("sed -i.bak -e 's/a/b/' src/x.rs"), vec!["/r/src/x.rs"]);
        assert_eq!(t("cp a.txt b/c.txt && mv d e"), vec!["/r/b/c.txt", "/r/e"]);
        assert_eq!(
            t("git mv old.rs new.rs; git rm gone.rs"),
            vec!["/r/gone.rs", "/r/new.rs", "/r/old.rs"]
        );
        assert_eq!(t("touch x.rs; rm -f y.rs"), vec!["/r/x.rs", "/r/y.rs"]);
        assert_eq!(t("sudo env FOO=1 tee out.txt"), vec!["/r/out.txt"]);
        assert_eq!(
            t("cp a b &>log"),
            vec!["/r/b", "/r/log"],
            "`&>` is a redirection"
        );
        assert_eq!(
            t("(echo x > src/auth.rs)"),
            vec!["/r/src/auth.rs"],
            "subshell parens"
        );
        // `cd` moves the resolution base, including into and out of the repository.
        assert_eq!(t("cd /tmp && echo x > out.txt"), vec!["/tmp/out.txt"]);
        assert_eq!(
            t("cd crates/foo && echo x > src/lib.rs"),
            vec!["/r/crates/foo/src/lib.rs"]
        );
        assert_eq!(t("cd .. && echo x > y.txt"), vec!["/y.txt"]);
        assert!(
            t("cd ~ && echo x > notes.txt").is_empty(),
            "unknown base: skipped"
        );
        // Not targets: reads, /dev/null, variables, home, substitutions, globs, the heredoc
        // body, quoted operators.
        assert!(t("cat src/a.rs | grep x").is_empty());
        assert!(t("cargo build > /dev/null").is_empty());
        assert!(t("echo x > $OUT").is_empty());
        assert!(t("echo note > ~/notes.txt").is_empty());
        assert!(t("sed -i 's/a/b/' $(git ls-files '*.rs')").is_empty());
        assert!(t("rm -f *.bak").is_empty());
        assert!(t("echo 'a > b'").is_empty());
        assert_eq!(
            t("python3 - <<'EOF'\nopen('src/x.rs','w').write('> y')\nEOF\necho done > z.txt"),
            vec!["/r/z.txt"],
            "the heredoc body is data; the command after it still counts"
        );
        assert_eq!(t("cat <<EOF > gen.rs\nfn x() {}\nEOF"), vec!["/r/gen.rs"]);
    }

    #[test]
    fn a_replaced_session_keeps_its_trail_and_finish_clears_session_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = GateStore::for_repo(dir.path());
        let first = session(GateMode::Enforce);
        store.start(&first, b"{}").unwrap();
        store
            .append_event(&GateEvent::new(&first.id, EventKind::EditDenied).with_path("x.rs"))
            .unwrap();
        store.write_snapshot("t", &[]).unwrap();
        store
            .write_last_verify(&LastVerify {
                fingerprint: "f".into(),
                verdict: DossierVerdict::Pass,
                summary: "s".into(),
                dossier_markdown: PathBuf::from("d.md"),
            })
            .unwrap();
        let second = session(GateMode::Warn);
        store.start(&second, b"{}").unwrap();
        let archive = store.root().join(format!("events-{}.jsonl", first.id));
        assert!(archive.exists(), "the first session's trail is archived");
        assert!(fs::read_to_string(&archive)
            .unwrap()
            .contains("edit_denied"));
        let (events, _) = store.events().unwrap();
        assert_eq!(events.len(), 1, "the new trail starts fresh");
        store.finish(&second).unwrap();
        assert!(!store.root().join("snapshots").exists());
        assert!(store.read_last_verify().is_none());
    }

    #[test]
    fn snapshots_are_taken_once_and_session_writes_leave_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = GateStore::for_repo(dir.path());
        let s = session(GateMode::Enforce);
        store.start(&s, b"{}").unwrap();
        store.write_snapshot("toolu_1", &["a.rs".into()]).unwrap();
        assert_eq!(
            store.take_snapshot("toolu_1").unwrap(),
            Some(vec!["a.rs".to_string()])
        );
        assert_eq!(store.take_snapshot("toolu_1").unwrap(), None);
        let leftovers = fs::read_dir(store.root())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn hook_responses_serialize_to_the_harness_shape() {
        let deny = HookResponse::Deny {
            event: "PreToolUse".into(),
            reason: "no".into(),
        }
        .to_json()
        .unwrap();
        assert_eq!(
            deny["hookSpecificOutput"]["permissionDecision"],
            serde_json::json!("deny")
        );
        let block = HookResponse::Block {
            reason: "fix".into(),
        }
        .to_json()
        .unwrap();
        assert_eq!(block["decision"], serde_json::json!("block"));
        assert!(HookResponse::Silent.to_json().is_none());
    }

    #[test]
    fn dossier_verdict_and_markdown_reflect_trail_and_verification() {
        let s = session(GateMode::Warn);
        let p = plan();
        let d_ok = s.decide("src/auth.rs");
        let d_warn = s.decide("src/other.rs");
        let d_deny = s.decide(".env");
        let events = vec![
            GateEvent::new(&s.id, EventKind::SessionStarted),
            GateEvent::for_decision(&s.id, &d_ok, "Edit"),
            GateEvent::for_decision(&s.id, &d_warn, "Write"),
            GateEvent::for_decision(&s.id, &d_deny, "Write"),
        ];
        let change = ChangeInput {
            changed_files: vec!["src/auth.rs".into(), "src/other.rs".into()],
            untracked_files: vec!["src/other.rs".into()],
            preexisting_excluded: vec!["notes.md".into()],
            unified_diff: Some("diff --git a/src/auth.rs b/src/auth.rs\n".into()),
            head_rev: Some("abcdef1234567890".into()),
            source: "git diff HEAD + untracked".into(),
        };
        let dossier = build_dossier("demo", &s, &p, &events, 0, &change, None);
        assert_eq!(dossier.verdict, DossierVerdict::Warn);
        assert_eq!(dossier.trail.edits_allowed, 1);
        assert_eq!(dossier.trail.edits_warned, 1);
        assert_eq!(dossier.trail.edits_denied, 1);
        assert_eq!(dossier.trail.denied_paths, vec![".env".to_string()]);
        assert!(dossier.caveats.iter().any(|c| c.contains("notes.md")));
        let md = render_markdown(&dossier);
        assert!(md.contains("Change Dossier: WARN"));
        assert!(md.contains("Left out"));
        assert!(md.contains("`.env`"));
        assert!(md.contains("sha256:"));
        assert!(!md.contains("content"), "no source text in the dossier");
        let json = serde_json::to_string(&dossier).unwrap();
        let back: ChangeDossier = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema_version, GATE_SCHEMA_VERSION);
    }

    #[test]
    fn dossier_without_change_passes_and_says_so() {
        let s = session(GateMode::Enforce);
        let p = plan();
        let dossier = build_dossier("demo", &s, &p, &[], 0, &ChangeInput::default(), None);
        assert_eq!(dossier.verdict, DossierVerdict::Pass);
        assert!(dossier
            .caveats
            .iter()
            .any(|c| c.contains("no change to verify")));
        // A warned edit that never reached the tree (reverted, or git-ignored) is named.
        let warn = session(GateMode::Warn);
        let events = vec![GateEvent::for_decision(
            &warn.id,
            &warn.decide("target/x.rs"),
            "Write",
        )];
        let dossier = build_dossier("demo", &warn, &p, &events, 0, &ChangeInput::default(), None);
        assert_eq!(dossier.verdict, DossierVerdict::Warn);
        assert!(dossier
            .caveats
            .iter()
            .any(|c| c.contains("not in the change") && c.contains("target/x.rs")));
    }
}
