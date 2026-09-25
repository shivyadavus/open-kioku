mod ownership;
mod reviewers;
pub mod unified_diff;

use chrono::{DateTime, Utc};
use open_kioku_core::{
    GitChangeKind, GitCommitId, GitCommitRecord, GitFileTouch, HistoryRecordId, LineRange, Owner,
};
use open_kioku_errors::{OkError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use unified_diff::{file_header_name, DiffLine, HunkScanner, MalformedDiff};

const COMMIT_RECORD_SEPARATOR: u8 = 0x1e;
const GIT_COMMIT_FORMAT: &str =
    "--format=%x1e%H%x00%P%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%s%x00%B%x00";

pub use ownership::{ownership_for_path, OwnershipInput};
pub use reviewers::{suggest_reviewers, ReviewerSuggestionInput};

#[derive(Debug, Clone, PartialEq)]
pub struct CommitHistory {
    pub commits: Vec<GitCommitRecord>,
    pub file_touches: Vec<GitFileTouch>,
}

impl CommitHistory {
    pub fn empty() -> Self {
        Self {
            commits: Vec::new(),
            file_touches: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CochangeRecord {
    pub path: PathBuf,
    pub cochanged_path: PathBuf,
    pub commit_count: usize,
    pub recency_weight: f32,
    pub test_corun: bool,
    pub commits: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPatch {
    pub commit_id: GitCommitId,
    pub files: Vec<FilePatch>,
}

/// The patches of a history scan, and the commits whose patch could not be read. A skipped
/// commit contributes no line ranges, so its per-symbol touches are missing rather than wrong.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommitPatchScan {
    pub commits: Vec<CommitPatch>,
    pub skipped: Vec<SkippedCommitPatch>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedCommitPatch {
    pub commit_id: GitCommitId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    pub path: PathBuf,
    pub previous_path: Option<PathBuf>,
    pub line_ranges: Vec<LineRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFile {
    pub old_path: Option<PathBuf>,
    pub new_path: Option<PathBuf>,
    pub status: GitChangeKind,
    pub rename_score: Option<u8>,
    pub hunks: Vec<DiffHunk>,
}

impl DiffFile {
    pub fn changed_line_ranges(&self) -> Vec<LineRange> {
        self.hunks
            .iter()
            .filter_map(|hunk| hunk.new_range.clone())
            .collect()
    }

    /// Paths whose content this change adds, modifies or removes: both sides of a rename,
    /// the removed path of a deletion, and only the destination of a copy.
    pub fn changed_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::with_capacity(2);
        if let Some(new_path) = &self.new_path {
            paths.push(new_path.clone());
        }
        if let Some(old_path) = &self.old_path {
            let removed = self.new_path.is_none() || self.status == GitChangeKind::Renamed;
            if removed && self.new_path.as_ref() != Some(old_path) {
                paths.push(old_path.clone());
            }
        }
        paths
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    pub old_range: Option<LineRange>,
    pub new_range: Option<LineRange>,
}

pub fn discover_root(start: impl AsRef<Path>) -> Result<PathBuf> {
    let mut current = start.as_ref().canonicalize()?;
    loop {
        if current.join(".git").exists() || current.join("ok.toml").exists() {
            return Ok(current);
        }
        if !current.pop() {
            return Ok(start.as_ref().canonicalize()?);
        }
    }
}

pub fn branch(root: impl AsRef<Path>) -> Option<String> {
    let head = fs::read_to_string(root.as_ref().join(".git/HEAD")).ok()?;
    if let Some(value) = head.strip_prefix("ref: refs/heads/") {
        return Some(value.trim().to_string());
    }
    None
}

pub fn commit(root: impl AsRef<Path>) -> Option<String> {
    let root = root.as_ref();
    loose_head_commit(root).or_else(|| {
        // A branch whose ref was packed (`git gc`, a fresh clone) and a linked worktree, whose
        // `.git` is a file, have no loose ref to read; Git itself resolves both.
        root.join(".git")
            .exists()
            .then(|| rev_parse_commit(root, "HEAD"))
            .flatten()
    })
}

fn loose_head_commit(root: &Path) -> Option<String> {
    let head = fs::read_to_string(root.join(".git/HEAD")).ok()?;
    if !head.starts_with("ref: ") {
        return Some(head.trim().to_string());
    }
    let reference = head.trim().strip_prefix("ref: ")?;
    fs::read_to_string(root.join(".git").join(reference))
        .ok()
        .map(|value| value.trim().to_string())
}

/// The full id of the commit `revision` names in the repository at `root`, if it is one.
fn rev_parse_commit(root: &Path, revision: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{revision}^{{commit}}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// How a commit recorded elsewhere (an index snapshot's) relates to the local `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionRelation {
    /// It is `HEAD`.
    Same,
    /// It shares history with `HEAD`. `ahead` counts the commits it has that `HEAD` does not,
    /// `behind` the commits `HEAD` has that it does not; an ancestor of `HEAD` has `ahead: 0`.
    Related { ahead: usize, behind: usize },
    /// Both are commits of this repository and share no history.
    Unrelated,
    /// It is not a commit this repository holds, so nothing about it can be verified here.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionComparison {
    /// The local `HEAD`, fully resolved.
    pub head: String,
    /// The compared commit, fully resolved when the repository holds it.
    pub commit: String,
    pub relation: RevisionRelation,
}

/// Compare `commit` with the `HEAD` of the repository at `root`. `None` when `root` has no
/// resolvable `HEAD` (not a Git work tree, or no commit yet), so no comparison is possible.
pub fn compare_with_head(
    root: impl AsRef<Path>,
    commit: &str,
) -> Result<Option<RevisionComparison>> {
    let root = root.as_ref();
    let Some(head) = rev_parse_commit(root, "HEAD") else {
        return Ok(None);
    };
    let unknown = |commit: &str| RevisionComparison {
        head: head.clone(),
        commit: commit.to_string(),
        relation: RevisionRelation::Unknown,
    };
    // Only an object id is looked up: a recorded value such as `unknown`, or a ref name that
    // happens to exist locally, must not resolve to some local commit.
    let is_object_id =
        (7..=64).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !is_object_id {
        return Ok(Some(unknown(commit)));
    }
    let Some(resolved) = rev_parse_commit(root, commit) else {
        return Ok(Some(unknown(commit)));
    };
    if resolved == head {
        return Ok(Some(RevisionComparison {
            head,
            commit: resolved,
            relation: RevisionRelation::Same,
        }));
    }
    let merge_base = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["merge-base", "--end-of-options", &resolved, &head])
        .output()
        .map_err(|err| OkError::Repository(format!("git merge-base failed: {err}")))?;
    // Exit status 1 with no output is Git's answer "no common ancestor"; anything else that
    // is not success is a failure to answer, not an answer.
    match merge_base.status.code() {
        Some(0) => {}
        Some(1) if merge_base.stdout.is_empty() => {
            return Ok(Some(RevisionComparison {
                head,
                commit: resolved,
                relation: RevisionRelation::Unrelated,
            }));
        }
        _ => {
            return Err(OkError::Repository(format!(
                "git merge-base failed: {}",
                String::from_utf8_lossy(&merge_base.stderr).trim()
            )))
        }
    }
    let counts = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-list", "--left-right", "--count", "--end-of-options"])
        .arg(format!("{resolved}...{head}"))
        .output()
        .map_err(|err| OkError::Repository(format!("git rev-list failed: {err}")))?;
    if !counts.status.success() {
        return Err(OkError::Repository(format!(
            "git rev-list failed: {}",
            String::from_utf8_lossy(&counts.stderr).trim()
        )));
    }
    let text = git_text(&counts.stdout, "rev-list count")?;
    let mut fields = text.split_whitespace().map(str::parse::<usize>);
    let (Some(Ok(ahead)), Some(Ok(behind))) = (fields.next(), fields.next()) else {
        return Err(OkError::Repository(format!(
            "git rev-list returned an unexpected count: {}",
            text.trim()
        )));
    };
    Ok(Some(RevisionComparison {
        head,
        commit: resolved,
        relation: RevisionRelation::Related { ahead, behind },
    }))
}

/// Tracked paths whose content in the working tree differs from `commit`: committed changes
/// since it and uncommitted ones alike. Untracked files are not listed.
pub fn changed_paths_since_commit(root: impl AsRef<Path>, commit: &str) -> Result<Vec<PathBuf>> {
    let root = root.as_ref();
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.quotePath=false",
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
        ])
        .args([
            "--no-ext-diff",
            "--no-textconv",
            "--end-of-options",
            commit,
            "--",
        ])
        .output()
        .map_err(|err| OkError::Repository(format!("git diff --name-only failed: {err}")))?;
    if !output.status.success() {
        return Err(OkError::Repository(format!(
            "git diff --name-only failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .map(|raw| PathBuf::from(String::from_utf8_lossy(raw).into_owned()))
        .collect())
}

pub fn require_repo(root: impl AsRef<Path>) -> Result<PathBuf> {
    let root = discover_root(root)?;
    if !root.exists() {
        return Err(OkError::Repository(format!(
            "repository root does not exist: {}",
            root.display()
        )));
    }
    Ok(root)
}

pub fn cochange_records(
    root: impl AsRef<Path>,
    max_commits: usize,
    max_files_per_commit: usize,
) -> Result<Vec<CochangeRecord>> {
    let history = commit_history(root, max_commits)?;
    Ok(cochange_records_from_history(
        &history,
        max_files_per_commit,
    ))
}

pub fn commit_history(root: impl AsRef<Path>, max_commits: usize) -> Result<CommitHistory> {
    let root = root.as_ref();
    if !root.join(".git").exists() || max_commits == 0 {
        return Ok(CommitHistory::empty());
    }
    let head = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map_err(|err| OkError::Repository(format!("git history scan failed: {err}")))?;
    if !head.status.success() {
        return Ok(CommitHistory::empty());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("log")
        .arg(format!("--max-count={max_commits}"))
        .args([
            "--no-show-signature",
            "--no-color",
            "--no-decorate",
            "--encoding=UTF-8",
            "--date=iso-strict",
            "--find-renames",
            GIT_COMMIT_FORMAT,
            "--name-status",
            "-z",
        ])
        .output()
        .map_err(|err| OkError::Repository(format!("git history scan failed: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OkError::Repository(format!(
            "git history scan failed: {}",
            stderr.trim()
        )));
    }
    parse_commit_history(&output.stdout)
}

pub fn commit_patches(root: impl AsRef<Path>, max_commits: usize) -> Result<CommitPatchScan> {
    let root = root.as_ref();
    if !root.join(".git").exists() || max_commits == 0 {
        return Ok(CommitPatchScan::default());
    }
    let head = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map_err(|err| OkError::Repository(format!("git patch scan failed: {err}")))?;
    if !head.status.success() {
        return Ok(CommitPatchScan::default());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=true"])
        .arg("log")
        .arg(format!("--max-count={max_commits}"))
        .args([
            "--no-show-signature",
            "--no-color",
            "--no-decorate",
            "--encoding=UTF-8",
            "--find-renames",
            "--format=%x1e%H%x00",
            "--patch",
            "--unified=0",
            "--no-ext-diff",
            "--no-textconv",
            // Pinned so `diff.noprefix`, `diff.mnemonicPrefix` or `diff.srcPrefix` cannot change
            // the recorded paths.
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ])
        .output()
        .map_err(|err| OkError::Repository(format!("git patch scan failed: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OkError::Repository(format!(
            "git patch scan failed: {}",
            stderr.trim()
        )));
    }
    parse_commit_patches(&output.stdout)
}

pub fn diff_name_status(root: impl AsRef<Path>) -> Result<Vec<DiffFile>> {
    run_diff_name_status(root, &[], None)
}

pub fn diff_name_status_since(root: impl AsRef<Path>, since: &str) -> Result<Vec<DiffFile>> {
    run_diff_name_status(root, &[], Some(since))
}

pub fn cached_diff_name_status(root: impl AsRef<Path>) -> Result<Vec<DiffFile>> {
    run_diff_name_status(root, &["--cached"], None)
}

pub fn head_diff_name_status(root: impl AsRef<Path>) -> Result<Vec<DiffFile>> {
    run_diff_name_status(root, &[], Some("HEAD"))
}

pub fn diff_unified_zero(root: impl AsRef<Path>) -> Result<Vec<DiffFile>> {
    run_diff_unified_zero(root, None)
}

pub fn diff_unified_zero_since(root: impl AsRef<Path>, since: &str) -> Result<Vec<DiffFile>> {
    run_diff_unified_zero(root, Some(since))
}

/// The revision (or range) a caller supplied is placed after `--end-of-options`, so a value
/// such as `--output=<path>` reaches git as a revision it cannot resolve rather than as an
/// option it obeys. Git rejects an option after that terminator, which is why the fixed flags
/// go first.
fn revision_args(revision: Option<&str>) -> Vec<&str> {
    match revision {
        Some(revision) => vec!["--end-of-options", revision],
        None => Vec::new(),
    }
}

fn run_diff_unified_zero(root: impl AsRef<Path>, revision: Option<&str>) -> Result<Vec<DiffFile>> {
    let root = root.as_ref();
    if !root.join(".git").exists() {
        return Ok(Vec::new());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=true"])
        .arg("diff")
        .args([
            "--unified=0",
            "--no-ext-diff",
            "--no-textconv",
            // `color.diff=always` would wrap every line in escape codes the parser cannot read.
            "--no-color",
            "--find-renames",
            // Pinned so `diff.noprefix`, `diff.mnemonicPrefix` or `diff.srcPrefix` cannot make
            // one path read as two, which the parser would take for a rename.
            "--src-prefix=a/",
            "--dst-prefix=b/",
        ])
        .args(revision_args(revision))
        .output()
        .map_err(|err| OkError::Repository(format!("git diff failed: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OkError::Repository(format!(
            "git diff failed: {}",
            stderr.trim()
        )));
    }
    parse_unified_zero_diff(&git_text(&output.stdout, "diff output")?)
}

fn run_diff_name_status(
    root: impl AsRef<Path>,
    flags: &[&str],
    revision: Option<&str>,
) -> Result<Vec<DiffFile>> {
    let root = root.as_ref();
    if !root.join(".git").exists() {
        return Ok(Vec::new());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("diff")
        .args(flags)
        .args(["--name-status", "--find-renames"])
        .args(revision_args(revision))
        .output()
        .map_err(|err| OkError::Repository(format!("git diff --name-status failed: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OkError::Repository(format!(
            "git diff --name-status failed: {}",
            stderr.trim()
        )));
    }
    parse_diff_name_status(&git_text(&output.stdout, "diff name-status output")?)
}

pub fn cochange_records_from_history(
    history: &CommitHistory,
    max_files_per_commit: usize,
) -> Vec<CochangeRecord> {
    if max_files_per_commit < 2 {
        return Vec::new();
    }
    let mut files_by_commit = HashMap::<&str, Vec<PathBuf>>::new();
    for touch in &history.file_touches {
        files_by_commit
            .entry(touch.commit_id.0.as_str())
            .or_default()
            .push(touch.path.clone());
    }
    let mut pairs: HashMap<(PathBuf, PathBuf), CochangeRecord> = HashMap::new();
    for (idx, commit) in history.commits.iter().enumerate() {
        let mut files = files_by_commit
            .remove(commit.id.0.as_str())
            .unwrap_or_default();
        files.sort();
        files.dedup();
        if files.len() < 2 || files.len() > max_files_per_commit {
            continue;
        }
        let recency_weight = 1.0 / (1.0 + idx as f32 / 25.0);
        for left in &files {
            for right in &files {
                if left == right {
                    continue;
                }
                let key = (left.clone(), right.clone());
                let entry = pairs.entry(key).or_insert_with(|| CochangeRecord {
                    path: left.clone(),
                    cochanged_path: right.clone(),
                    commit_count: 0,
                    recency_weight: 0.0,
                    test_corun: is_test_path(right),
                    commits: Vec::new(),
                });
                entry.commit_count += 1;
                entry.recency_weight += recency_weight;
                entry.test_corun |= is_test_path(right);
                if entry.commits.len() < 5 {
                    entry.commits.push(commit.id.0.clone());
                }
            }
        }
    }
    let mut records = pairs.into_values().collect::<Vec<_>>();
    records.sort_by(|a, b| {
        b.recency_weight
            .partial_cmp(&a.recency_weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.commit_count.cmp(&a.commit_count))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.cochanged_path.cmp(&b.cochanged_path))
    });
    records
}

fn parse_commit_history(raw: &[u8]) -> Result<CommitHistory> {
    let mut history = CommitHistory::empty();
    for record in raw
        .split(|byte| *byte == COMMIT_RECORD_SEPARATOR)
        .filter(|record| !record.is_empty())
    {
        let fields = record.splitn(11, |byte| *byte == 0).collect::<Vec<_>>();
        if fields.len() != 11 {
            return Err(OkError::Repository(format!(
                "git history record has {} fields; expected commit metadata and file statuses",
                fields.len()
            )));
        }
        let sha = git_text(fields[0], "commit id")?;
        let parent_ids = git_text(fields[1], "parent commit ids")?
            .split_whitespace()
            .map(|id| GitCommitId::new(id.to_string()))
            .collect::<Vec<_>>();
        let author = owner(
            git_text(fields[2], "author name")?,
            git_text(fields[3], "author email")?,
            "author",
        )?;
        let authored_at = git_timestamp(fields[4], "authored timestamp")?;
        let committer = owner(
            git_text(fields[5], "committer name")?,
            git_text(fields[6], "committer email")?,
            "committer",
        )?;
        let committed_at = git_timestamp(fields[7], "committed timestamp")?;
        let mut summary = git_text(fields[8], "commit summary")?;
        let message = git_text(fields[9], "commit message")?
            .trim_end_matches(['\r', '\n'])
            .to_string();
        if summary.trim().is_empty() {
            summary = message.lines().next().unwrap_or_default().to_string();
        }
        let commit_id = GitCommitId::new(sha);
        let mut touches = parse_file_touches(fields[10], &commit_id, committed_at)?;
        let file_count = touches.len();
        history.commits.push(GitCommitRecord {
            id: commit_id,
            parent_ids,
            author,
            committer: Some(committer),
            authored_at,
            committed_at,
            summary,
            message,
            file_count,
        });
        history.file_touches.append(&mut touches);
    }
    Ok(history)
}

fn parse_commit_patches(raw: &[u8]) -> Result<CommitPatchScan> {
    let mut scan = CommitPatchScan::default();
    let starts = patch_record_starts(raw);
    if starts.is_empty() && !raw.is_empty() {
        return Err(OkError::Repository(
            "git patch output is missing a commit record".into(),
        ));
    }
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(raw.len());
        let record = &raw[start + 1..end];
        let Some(metadata_end) = record.iter().position(|byte| *byte == 0) else {
            return Err(OkError::Repository(
                "git patch record is missing its commit delimiter".into(),
            ));
        };
        let commit_id = GitCommitId::new(git_text(&record[..metadata_end], "commit id")?);
        let patch = String::from_utf8_lossy(&record[metadata_end + 1..]).into_owned();
        // One commit whose patch cannot be read costs that commit's line ranges, not the scan.
        match parse_file_patches(&patch) {
            Ok(files) => scan.commits.push(CommitPatch { commit_id, files }),
            Err(err) => scan.skipped.push(SkippedCommitPatch {
                commit_id,
                reason: err.to_string(),
            }),
        }
    }
    Ok(scan)
}

fn patch_record_starts(raw: &[u8]) -> Vec<usize> {
    raw.iter()
        .enumerate()
        .filter_map(|(index, byte)| {
            // A record starts a line: every patch line inside one begins with a header word
            // or a `+`, `-`, space or `\` marker, so a separator byte mid-line is content.
            if *byte != COMMIT_RECORD_SEPARATOR || (index > 0 && raw.get(index - 1) != Some(&b'\n'))
            {
                return None;
            }
            let commit_start = index + 1;
            [40, 64].into_iter().find_map(|length| {
                let commit_end = commit_start + length;
                (raw.get(commit_end) == Some(&0)
                    && raw
                        .get(commit_start..commit_end)
                        .is_some_and(|commit| commit.iter().all(u8::is_ascii_hexdigit)))
                .then_some(index)
            })
        })
        .collect()
}

fn parse_file_patches(patch: &str) -> Result<Vec<FilePatch>> {
    #[derive(Default)]
    struct PendingPatch {
        path: Option<PathBuf>,
        previous_path: Option<PathBuf>,
        line_ranges: Vec<LineRange>,
    }

    fn finish(patches: &mut Vec<FilePatch>, pending: &mut PendingPatch) {
        if let Some(path) = pending.path.take() {
            patches.push(FilePatch {
                path,
                previous_path: pending.previous_path.take(),
                line_ranges: std::mem::take(&mut pending.line_ranges),
            });
        } else {
            pending.previous_path = None;
            pending.line_ranges.clear();
        }
    }

    let mut patches = Vec::new();
    let mut pending = PendingPatch::default();
    let mut scanner = HunkScanner::new();
    for line in patch.lines() {
        let kind = scanner.scan(line);
        if let Some(malformed) = scanner.malformation() {
            return Err(malformed_patch(pending.path.as_deref(), malformed));
        }
        match kind {
            DiffLine::Content => {}
            DiffLine::HunkHeader(_) => {
                if let Some(range) = parse_new_hunk_range(line)? {
                    pending.line_ranges.push(range);
                }
            }
            DiffLine::Header => {
                if line.starts_with("diff --git ") {
                    finish(&mut patches, &mut pending);
                } else if let Some(value) = line.strip_prefix("rename from ") {
                    pending.previous_path = Some(parse_patch_path(value, None)?);
                } else if let Some(value) = line.strip_prefix("rename to ") {
                    pending.path = Some(parse_patch_path(value, None)?);
                } else if let Some(value) = line.strip_prefix("+++ ") {
                    if value != "/dev/null" {
                        pending.path = Some(parse_marker_path(value, "b/")?);
                    }
                }
            }
        }
    }
    scanner
        .finish()
        .map_err(|malformed| malformed_patch(pending.path.as_deref(), &malformed))?;
    finish(&mut patches, &mut pending);
    Ok(patches)
}

/// A git diff whose hunk bodies disagree with their headers. Git does not write one, so the
/// output is not what the parser takes it for and no path read from it can be trusted.
fn malformed_patch(path: Option<&Path>, malformed: &MalformedDiff) -> OkError {
    let entry = path
        .map(|path| format!(" in the entry for `{}`", path.display()))
        .unwrap_or_default();
    OkError::Repository(format!(
        "git diff output is malformed{entry} at {malformed}"
    ))
}

fn parse_diff_name_status(raw: &str) -> Result<Vec<DiffFile>> {
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut fields = line.split('\t').collect::<Vec<_>>();
            if fields.len() < 2 {
                fields = line.split_whitespace().collect();
            }
            let status = fields.first().copied().unwrap_or_default();
            if fields.len() < 2 {
                return Err(OkError::Repository(format!(
                    "git diff name-status entry is missing a path: `{line}`"
                )));
            }
            let kind = change_kind(status.as_bytes());
            let rename_score = status
                .strip_prefix('R')
                .or_else(|| status.strip_prefix('C'))
                .and_then(|score| score.parse::<u8>().ok());
            match kind {
                GitChangeKind::Renamed | GitChangeKind::Copied => {
                    if fields.len() < 3 {
                        return Err(OkError::Repository(format!(
                            "git diff name-status rename is missing paths: `{line}`"
                        )));
                    }
                    Ok(DiffFile {
                        old_path: Some(parse_patch_path(fields[1], None)?),
                        new_path: Some(parse_patch_path(fields[2], None)?),
                        status: kind,
                        rename_score,
                        hunks: Vec::new(),
                    })
                }
                GitChangeKind::Deleted => Ok(DiffFile {
                    old_path: Some(parse_patch_path(fields[1], None)?),
                    new_path: None,
                    status: kind,
                    rename_score,
                    hunks: Vec::new(),
                }),
                _ => Ok(DiffFile {
                    old_path: None,
                    new_path: Some(parse_patch_path(fields[1], None)?),
                    status: kind,
                    rename_score,
                    hunks: Vec::new(),
                }),
            }
        })
        .collect()
}

fn parse_unified_zero_diff(patch: &str) -> Result<Vec<DiffFile>> {
    #[derive(Default)]
    struct PendingDiff {
        old_path: Option<PathBuf>,
        new_path: Option<PathBuf>,
        status: Option<GitChangeKind>,
        rename_score: Option<u8>,
        hunks: Vec<DiffHunk>,
    }

    fn finish(files: &mut Vec<DiffFile>, pending: &mut PendingDiff) {
        if pending.old_path.is_none() && pending.new_path.is_none() {
            pending.hunks.clear();
            pending.status = None;
            pending.rename_score = None;
            return;
        }
        let status = pending.status.take().unwrap_or_else(|| {
            if pending.old_path.is_none() {
                GitChangeKind::Added
            } else if pending.new_path.is_none() {
                GitChangeKind::Deleted
            } else if pending.old_path != pending.new_path {
                GitChangeKind::Renamed
            } else {
                GitChangeKind::Modified
            }
        });
        files.push(DiffFile {
            old_path: pending.old_path.take(),
            new_path: pending.new_path.take(),
            status,
            rename_score: pending.rename_score.take(),
            hunks: std::mem::take(&mut pending.hunks),
        });
    }

    let mut files = Vec::new();
    let mut pending = PendingDiff::default();
    let mut scanner = HunkScanner::new();
    for line in patch.lines() {
        let kind = scanner.scan(line);
        if let Some(malformed) = scanner.malformation() {
            let path = pending.new_path.as_deref().or(pending.old_path.as_deref());
            return Err(malformed_patch(path, malformed));
        }
        match kind {
            // A hunk's content lines are never headers: a removed `-- x` or an added `++ y`
            // reads as `--- x` or `+++ y` and names no path.
            DiffLine::Content => {}
            DiffLine::HunkHeader(_) => pending.hunks.push(parse_diff_hunk(line)?),
            DiffLine::Header => {
                if line.starts_with("diff --git ") {
                    finish(&mut files, &mut pending);
                } else if line.starts_with("new file mode ") {
                    pending.status = Some(GitChangeKind::Added);
                } else if line.starts_with("deleted file mode ") {
                    pending.status = Some(GitChangeKind::Deleted);
                } else if let Some(score) = line.strip_prefix("similarity index ") {
                    pending.rename_score = score.trim_end_matches('%').parse::<u8>().ok();
                } else if let Some(value) = line.strip_prefix("rename from ") {
                    pending.old_path = Some(parse_patch_path(value, None)?);
                    pending.status = Some(GitChangeKind::Renamed);
                } else if let Some(value) = line.strip_prefix("rename to ") {
                    pending.new_path = Some(parse_patch_path(value, None)?);
                    pending.status = Some(GitChangeKind::Renamed);
                } else if let Some(value) = line.strip_prefix("copy from ") {
                    pending.old_path = Some(parse_patch_path(value, None)?);
                    pending.status = Some(GitChangeKind::Copied);
                } else if let Some(value) = line.strip_prefix("copy to ") {
                    pending.new_path = Some(parse_patch_path(value, None)?);
                    pending.status = Some(GitChangeKind::Copied);
                } else if let Some(value) = line.strip_prefix("--- ") {
                    // `rename from`/`copy from` already name the pre-edit path exactly.
                    if value != "/dev/null" && pending.old_path.is_none() {
                        pending.old_path = Some(parse_marker_path(value, "a/")?);
                    }
                } else if let Some(value) = line.strip_prefix("+++ ") {
                    if value != "/dev/null" && pending.new_path.is_none() {
                        pending.new_path = Some(parse_marker_path(value, "b/")?);
                    }
                }
            }
        }
    }
    if let Err(malformed) = scanner.finish() {
        let path = pending.new_path.as_deref().or(pending.old_path.as_deref());
        return Err(malformed_patch(path, &malformed));
    }
    finish(&mut files, &mut pending);
    Ok(files)
}

fn parse_diff_hunk(header: &str) -> Result<DiffHunk> {
    let old = header
        .split_whitespace()
        .find(|part| part.starts_with('-'))
        .ok_or_else(|| OkError::Repository(format!("git diff hunk is malformed: `{header}`")))?;
    let new = header
        .split_whitespace()
        .find(|part| part.starts_with('+'))
        .ok_or_else(|| OkError::Repository(format!("git diff hunk is malformed: `{header}`")))?;
    Ok(DiffHunk {
        old_range: parse_hunk_range(old.trim_start_matches('-'))?,
        new_range: parse_hunk_range(new.trim_start_matches('+'))?,
    })
}

fn parse_hunk_range(value: &str) -> Result<Option<LineRange>> {
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    let start = start.parse::<u32>().map_err(|err| {
        OkError::Repository(format!("git diff hunk start `{start}` is invalid: {err}"))
    })?;
    let count = count.parse::<u32>().map_err(|err| {
        OkError::Repository(format!("git diff hunk count `{count}` is invalid: {err}"))
    })?;
    if count == 0 {
        return Ok(None);
    }
    Ok(Some(LineRange {
        start,
        end: start.saturating_add(count - 1),
    }))
}

fn parse_new_hunk_range(header: &str) -> Result<Option<LineRange>> {
    let marker = header
        .split_whitespace()
        .find(|part| part.starts_with('+'))
        .ok_or_else(|| OkError::Repository(format!("git patch hunk is malformed: `{header}`")))?;
    let value = marker.trim_start_matches('+');
    let (start, count) = value.split_once(',').unwrap_or((value, "1"));
    let start = start.parse::<u32>().map_err(|err| {
        OkError::Repository(format!("git patch hunk start `{start}` is invalid: {err}"))
    })?;
    let count = count.parse::<u32>().map_err(|err| {
        OkError::Repository(format!("git patch hunk count `{count}` is invalid: {err}"))
    })?;
    if count == 0 {
        return Ok(None);
    }
    Ok(Some(LineRange {
        start,
        end: start.saturating_add(count - 1),
    }))
}

/// The path of a `--- ` or `+++ ` header, without the tab git appends to a name holding a
/// space or the prefix pinned on the git invocation.
fn parse_marker_path(value: &str, prefix: &str) -> Result<PathBuf> {
    parse_patch_path(file_header_name(value), Some(prefix))
}

fn parse_patch_path(value: &str, prefix: Option<&str>) -> Result<PathBuf> {
    let decoded = if value.starts_with('"') {
        decode_git_quoted_path(value)?
    } else {
        value.to_string()
    };
    let decoded = prefix
        .and_then(|prefix| decoded.strip_prefix(prefix))
        .unwrap_or(&decoded);
    Ok(PathBuf::from(decoded))
}

fn decode_git_quoted_path(value: &str) -> Result<String> {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return Err(OkError::Repository(format!(
            "git patch path has invalid quoting: `{value}`"
        )));
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.as_bytes().iter().copied().peekable();
    while let Some(byte) = chars.next() {
        if byte != b'\\' {
            bytes.push(byte);
            continue;
        }
        let escaped = chars.next().ok_or_else(|| {
            OkError::Repository(format!("git patch path has a trailing escape: `{value}`"))
        })?;
        match escaped {
            b'\\' | b'"' => bytes.push(escaped),
            b'a' => bytes.push(0x07),
            b'b' => bytes.push(0x08),
            b't' => bytes.push(b'\t'),
            b'n' => bytes.push(b'\n'),
            b'v' => bytes.push(0x0b),
            b'f' => bytes.push(0x0c),
            b'r' => bytes.push(b'\r'),
            b'0'..=b'7' => {
                let mut octal = vec![escaped];
                for _ in 0..2 {
                    if chars.peek().is_some_and(|byte| matches!(byte, b'0'..=b'7')) {
                        octal.push(chars.next().expect("peeked octal byte"));
                    } else {
                        break;
                    }
                }
                let decoded = std::str::from_utf8(&octal)
                    .ok()
                    .and_then(|value| u8::from_str_radix(value, 8).ok())
                    .ok_or_else(|| {
                        OkError::Repository("git patch path contains invalid octal escape".into())
                    })?;
                bytes.push(decoded);
            }
            other => bytes.push(other),
        }
    }
    String::from_utf8(bytes)
        .map_err(|err| OkError::Repository(format!("git patch path is not UTF-8: {err}")))
}

fn parse_file_touches(
    raw: &[u8],
    commit_id: &GitCommitId,
    touched_at: DateTime<Utc>,
) -> Result<Vec<GitFileTouch>> {
    let mut tokens = raw.split(|byte| *byte == 0);
    let mut touches = Vec::new();
    while let Some(status) = next_status(&mut tokens) {
        let change_kind = change_kind(status);
        let rename_or_copy = matches!(change_kind, GitChangeKind::Renamed | GitChangeKind::Copied);
        let first_path = next_path(&mut tokens, commit_id, status)?;
        let (path, previous_path) = if rename_or_copy {
            let current_path = next_path(&mut tokens, commit_id, status)?;
            (current_path, Some(first_path))
        } else {
            (first_path, None)
        };
        let id = HistoryRecordId::new(format!("file-touch:{}:{}", commit_id.0, touches.len()));
        touches.push(GitFileTouch {
            id,
            commit_id: commit_id.clone(),
            path,
            previous_path,
            change_kind,
            additions: None,
            deletions: None,
            touched_at,
        });
    }
    Ok(touches)
}

fn next_status<'a>(tokens: &mut impl Iterator<Item = &'a [u8]>) -> Option<&'a [u8]> {
    tokens
        .map(trim_status_prefix)
        .find(|token| !token.is_empty())
}

fn next_path<'a>(
    tokens: &mut impl Iterator<Item = &'a [u8]>,
    commit_id: &GitCommitId,
    status: &[u8],
) -> Result<PathBuf> {
    let path = tokens.find(|token| !token.is_empty()).ok_or_else(|| {
        OkError::Repository(format!(
            "git history record for commit `{commit_id}` is missing a path after status `{}`",
            String::from_utf8_lossy(status)
        ))
    })?;
    Ok(PathBuf::from(git_text(path, "changed path")?))
}

fn trim_status_prefix(mut value: &[u8]) -> &[u8] {
    while value
        .first()
        .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
    {
        value = &value[1..];
    }
    value
}

fn change_kind(status: &[u8]) -> GitChangeKind {
    match status.first().copied() {
        Some(b'A') => GitChangeKind::Added,
        Some(b'M') => GitChangeKind::Modified,
        Some(b'D') => GitChangeKind::Deleted,
        Some(b'R') => GitChangeKind::Renamed,
        Some(b'C') => GitChangeKind::Copied,
        Some(b'T') => GitChangeKind::TypeChanged,
        _ => GitChangeKind::Unknown,
    }
}

fn owner(name: String, email: String, role: &str) -> Result<Owner> {
    let name = name.trim().to_string();
    let email = email.trim().to_string();
    let name = if name.is_empty() { email.clone() } else { name };
    if name.is_empty() {
        return Err(OkError::Repository(format!(
            "git history {role} identity is empty"
        )));
    }
    Ok(Owner {
        name,
        email: (!email.is_empty()).then_some(email),
    })
}

fn git_timestamp(raw: &[u8], field: &str) -> Result<DateTime<Utc>> {
    let value = git_text(raw, field)?;
    DateTime::parse_from_rfc3339(&value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|err| {
            OkError::Repository(format!("git history {field} `{value}` is invalid: {err}"))
        })
}

fn git_text(raw: &[u8], field: &str) -> Result<String> {
    String::from_utf8(raw.to_vec()).map_err(|err| {
        OkError::Repository(format!("git history {field} is not valid UTF-8: {err}"))
    })
}

fn is_test_path(path: &Path) -> bool {
    let value = path.to_string_lossy().to_ascii_lowercase();
    value.contains("/test/")
        || value.contains("/tests/")
        || value.ends_with("_test.rs")
        || value.ends_with("_test.go")
        || value.ends_with(".test.ts")
        || value.ends_with(".spec.ts")
        || value.ends_with("test.java")
        || value.ends_with("tests.java")
}

#[cfg(test)]
mod tests {
    use super::{
        changed_paths_since_commit, cochange_records, commit, commit_history, commit_patches,
        compare_with_head, diff_name_status_since, diff_unified_zero_since, parse_commit_patches,
        parse_diff_name_status, parse_file_patches, parse_unified_zero_diff, RevisionRelation,
    };
    use open_kioku_core::GitChangeKind;
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn cochange_records_apply_recency_and_test_corun() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init"]);
        run(dir.path(), &["config", "user.email", "test@example.com"]);
        run(dir.path(), &["config", "user.name", "Test User"]);

        write(dir.path(), "src/old.rs", "fn old() {}\n");
        write(
            dir.path(),
            "tests/old_test.rs",
            "#[test] fn old_test() {}\n",
        );
        run(dir.path(), &["add", "."]);
        run(dir.path(), &["commit", "-m", "old pair"]);

        write(dir.path(), "src/new.rs", "fn new() {}\n");
        write(
            dir.path(),
            "tests/new_test.rs",
            "#[test] fn new_test() {}\n",
        );
        run(dir.path(), &["add", "."]);
        run(dir.path(), &["commit", "-m", "new pair"]);

        let records = cochange_records(dir.path(), 20, 10).unwrap();
        let new_pair = records
            .iter()
            .find(|record| {
                record.path == std::path::Path::new("src/new.rs")
                    && record.cochanged_path == std::path::Path::new("tests/new_test.rs")
            })
            .unwrap();
        let old_pair = records
            .iter()
            .find(|record| {
                record.path == std::path::Path::new("src/old.rs")
                    && record.cochanged_path == std::path::Path::new("tests/old_test.rs")
            })
            .unwrap();

        assert!(new_pair.test_corun);
        assert!(new_pair.recency_weight > old_pair.recency_weight);
        assert_eq!(new_pair.commit_count, 1);
    }

    #[test]
    fn commit_history_respects_window_and_keeps_every_file_touch() {
        let dir = initialized_repo();
        write(dir.path(), "src/old.rs", "fn old() {}\n");
        commit_all(dir.path(), "old");
        write(dir.path(), "src/a.rs", "fn a() {}\n");
        write(dir.path(), "src/b.rs", "fn b() {}\n");
        write(dir.path(), "tests/a_test.rs", "#[test] fn a() {}\n");
        commit_all(dir.path(), "multi-file change");

        let history = commit_history(dir.path(), 1).unwrap();

        assert_eq!(history.commits.len(), 1);
        assert_eq!(history.commits[0].summary, "multi-file change");
        assert_eq!(history.commits[0].author.name, "Test User");
        assert_eq!(
            history.commits[0].author.email.as_deref(),
            Some("test@example.com")
        );
        assert_eq!(history.commits[0].file_count, 3);
        assert_eq!(history.file_touches.len(), 3);
        assert!(history
            .file_touches
            .iter()
            .all(|touch| touch.commit_id == history.commits[0].id));
    }

    #[test]
    fn commit_history_captures_renames() {
        let dir = initialized_repo();
        write(dir.path(), "src/old.rs", "fn renamed() {}\n");
        commit_all(dir.path(), "add old path");
        run(dir.path(), &["mv", "src/old.rs", "src/new.rs"]);
        commit_all(dir.path(), "rename path");

        let history = commit_history(dir.path(), 1).unwrap();
        let touch = history.file_touches.first().unwrap();

        assert_eq!(touch.change_kind, GitChangeKind::Renamed);
        assert_eq!(
            touch.previous_path.as_deref(),
            Some(Path::new("src/old.rs"))
        );
        assert_eq!(touch.path, Path::new("src/new.rs"));
    }

    #[test]
    fn commit_history_handles_empty_and_shallow_repositories() {
        let empty = initialized_repo();
        assert_eq!(
            commit_history(empty.path(), 10).unwrap(),
            super::CommitHistory::empty()
        );

        let origin = initialized_repo();
        write(origin.path(), "src/one.rs", "fn one() {}\n");
        commit_all(origin.path(), "one");
        write(origin.path(), "src/two.rs", "fn two() {}\n");
        commit_all(origin.path(), "two");

        let clone_parent = tempfile::tempdir().unwrap();
        let shallow = clone_parent.path().join("shallow");
        let source = format!("file://{}", origin.path().canonicalize().unwrap().display());
        let status = Command::new("git")
            .args(["clone", "--quiet", "--depth", "1"])
            .arg(source)
            .arg(&shallow)
            .status()
            .unwrap();
        assert!(status.success());

        let history = commit_history(&shallow, 10).unwrap();
        assert_eq!(history.commits.len(), 1);
        assert_eq!(history.commits[0].summary, "two");
    }

    #[test]
    fn commit_patches_capture_zero_context_line_ranges_and_renames() {
        let dir = initialized_repo();
        write(
            dir.path(),
            "src/old.rs",
            "fn alpha() {\n    one();\n}\n\nfn beta() {\n    two();\n}\n",
        );
        commit_all(dir.path(), "add symbols");
        run(dir.path(), &["mv", "src/old.rs", "src/new.rs"]);
        write(
            dir.path(),
            "src/new.rs",
            "fn alpha() {\n    changed();\n}\n\nfn beta() {\n    two();\n    added();\n}\n",
        );
        commit_all(dir.path(), "rename and modify");

        let patches = commit_patches(dir.path(), 1).unwrap().commits;

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].files.len(), 1);
        let file = &patches[0].files[0];
        assert_eq!(file.path, Path::new("src/new.rs"));
        assert_eq!(file.previous_path.as_deref(), Some(Path::new("src/old.rs")));
        assert_eq!(
            file.line_ranges,
            vec![
                open_kioku_core::LineRange { start: 2, end: 2 },
                open_kioku_core::LineRange { start: 7, end: 7 }
            ]
        );
    }

    #[test]
    fn commit_patches_never_read_hunk_content_as_a_path() {
        let dir = initialized_repo();
        write(dir.path(), "src/lib.rs", "fn one() {}\n");
        commit_all(dir.path(), "one");
        write(
            dir.path(),
            "src/lib.rs",
            "fn one() {}\n++ b/not/a/path.rs\n-- a/nor/this.rs\n",
        );
        commit_all(dir.path(), "header-like content");

        let patches = commit_patches(dir.path(), 1).unwrap().commits;

        assert_eq!(patches.len(), 1);
        let files = patches[0]
            .files
            .iter()
            .map(|file| (file.path.clone(), file.line_ranges.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            files,
            vec![(
                Path::new("src/lib.rs").to_path_buf(),
                vec![open_kioku_core::LineRange { start: 2, end: 3 }]
            )]
        );
    }

    #[test]
    fn commit_patch_paths_ignore_local_prefix_config() {
        let dir = initialized_repo();
        write(dir.path(), "b/lib.rs", "fn one() {}\n");
        commit_all(dir.path(), "one");
        run(dir.path(), &["config", "diff.noprefix", "true"]);
        run(dir.path(), &["config", "diff.mnemonicPrefix", "true"]);
        run(dir.path(), &["config", "diff.srcPrefix", "x/"]);
        run(dir.path(), &["config", "diff.dstPrefix", "y/"]);
        write(dir.path(), "b/lib.rs", "fn one() {}\nfn two() {}\n");
        commit_all(dir.path(), "two");

        let patches = commit_patches(dir.path(), 1).unwrap().commits;

        let paths = patches[0]
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(paths, vec![Path::new("b/lib.rs").to_path_buf()]);
    }

    #[test]
    fn a_record_separator_inside_a_text_diff_does_not_split_the_commit() {
        let dir = initialized_repo();
        write(dir.path(), ".gitattributes", "*.dat diff\n");
        write(dir.path(), "src/lib.rs", "fn one() {}\n");
        commit_all(dir.path(), "one");
        write(
            dir.path(),
            "blob.dat",
            "head\n\u{1e}0123456789abcdef0123456789abcdef01234567\0tail\nmore\n",
        );
        write(dir.path(), "src/lib.rs", "fn one() {}\nfn two() {}\n");
        commit_all(dir.path(), "two");

        let scan = commit_patches(dir.path(), 1).unwrap();

        assert!(scan.skipped.is_empty(), "{:?}", scan.skipped);
        assert_eq!(scan.commits.len(), 1);
        let paths = scan.commits[0]
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                Path::new("blob.dat").to_path_buf(),
                Path::new("src/lib.rs").to_path_buf()
            ]
        );
    }

    #[test]
    fn an_unreadable_commit_patch_is_skipped_and_the_rest_are_kept() {
        let raw = b"\x1e1111111111111111111111111111111111111111\x00\n\
              diff --git a/a.rs b/a.rs\n\
              --- a/a.rs\n\
              +++ b/a.rs\n\
              @@ -1 +1,3 @@\n\
              -a\n\
              +b\n\
              \x1e2222222222222222222222222222222222222222\x00\n\
              diff --git a/b.rs b/b.rs\n\
              --- a/b.rs\n\
              +++ b/b.rs\n\
              @@ -1 +1 @@\n\
              -a\n\
              +b\n";

        let scan = parse_commit_patches(raw).unwrap();

        assert_eq!(scan.commits.len(), 1);
        assert_eq!(scan.commits[0].commit_id.0, "2".repeat(40));
        assert_eq!(scan.skipped.len(), 1);
        assert_eq!(scan.skipped[0].commit_id.0, "1".repeat(40));
        assert!(
            scan.skipped[0].reason.contains("entry for `a.rs`"),
            "{}",
            scan.skipped[0].reason
        );
    }

    #[test]
    fn patch_paths_holding_a_space_drop_the_tab_git_appends() {
        let dir = initialized_repo();
        write(dir.path(), "src/sp ace.rs", "fn one() {}\n");
        commit_all(dir.path(), "one");
        write(dir.path(), "src/sp ace.rs", "fn one() {}\nfn two() {}\n");
        commit_all(dir.path(), "two");

        let patches = commit_patches(dir.path(), 1).unwrap().commits;
        assert_eq!(patches[0].files[0].path, Path::new("src/sp ace.rs"));

        write(dir.path(), "src/sp ace.rs", "fn one() {}\n");
        let changed = diff_unified_zero_since(dir.path(), "HEAD").unwrap();
        assert_eq!(
            changed[0].changed_paths(),
            vec![std::path::PathBuf::from("src/sp ace.rs")]
        );
    }

    #[test]
    fn patch_parser_reads_header_like_hunk_content_as_content() {
        let patches = parse_file_patches(
            "diff --git a/src/a.rs b/src/a.rs\n\
             --- a/src/a.rs\n\
             +++ b/src/a.rs\n\
             @@ -1 +1,2 @@\n\
             --- a/src/other.rs\n\
             +++ b/src/other.rs\n\
             +++ b/src/also_not_a_path.rs\n",
        )
        .unwrap();

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].path, Path::new("src/a.rs"));
    }

    #[test]
    fn patch_parser_reports_hunks_that_disagree_with_their_headers() {
        let over = "diff --git a/src/a.rs b/src/a.rs\n\
                    --- a/src/a.rs\n\
                    +++ b/src/a.rs\n\
                    @@ -1 +1,3 @@\n\
                    -a\n\
                    +b\n\
                    diff --git a/src/b.rs b/src/b.rs\n";
        let under = "diff --git a/src/a.rs b/src/a.rs\n\
                     --- a/src/a.rs\n\
                     +++ b/src/a.rs\n\
                     @@ -1 +1 @@\n\
                     -a\n\
                     +b\n\
                     +++ b/src/b.rs\n";
        let unparsed = "diff --git a/src/a.rs b/src/a.rs\n\
                        --- a/src/a.rs\n\
                        +++ b/src/a.rs\n\
                        @@ -1 +1,x @@\n";
        for patch in [over, under, unparsed] {
            let err = parse_file_patches(patch).unwrap_err().to_string();
            assert!(
                err.contains("malformed in the entry for `src/a.rs`"),
                "{err}"
            );
            let err = parse_unified_zero_diff(patch).unwrap_err().to_string();
            assert!(
                err.contains("malformed in the entry for `src/a.rs`"),
                "{err}"
            );
        }
    }

    #[test]
    fn diff_name_status_parser_captures_added_modified_deleted_and_renamed() {
        let files = parse_diff_name_status(
            "A\tsrc/new.rs\n\
             M\tsrc/lib.rs\n\
             D\tsrc/old.rs\n\
             R087\tsrc/before.rs\tsrc/after.rs\n",
        )
        .unwrap();

        assert_eq!(files.len(), 4);
        assert_eq!(files[0].status, GitChangeKind::Added);
        assert_eq!(files[0].new_path.as_deref(), Some(Path::new("src/new.rs")));
        assert_eq!(files[1].status, GitChangeKind::Modified);
        assert_eq!(files[2].status, GitChangeKind::Deleted);
        assert_eq!(files[2].old_path.as_deref(), Some(Path::new("src/old.rs")));
        assert_eq!(files[3].status, GitChangeKind::Renamed);
        assert_eq!(files[3].rename_score, Some(87));
        assert_eq!(
            files[3].old_path.as_deref(),
            Some(Path::new("src/before.rs"))
        );
        assert_eq!(
            files[3].new_path.as_deref(),
            Some(Path::new("src/after.rs"))
        );
    }

    #[test]
    fn changed_paths_name_both_sides_of_a_rename_and_only_the_destination_of_a_copy() {
        let files = parse_unified_zero_diff(
            "diff --git a/src/old.rs b/src/new.rs\n\
             similarity index 100%\n\
             rename from src/old.rs\n\
             rename to src/new.rs\n\
             diff --git a/src/template.rs b/src/copy.rs\n\
             similarity index 100%\n\
             copy from src/template.rs\n\
             copy to src/copy.rs\n\
             diff --git a/src/gone.rs b/src/gone.rs\n\
             deleted file mode 100644\n\
             --- a/src/gone.rs\n\
             +++ /dev/null\n\
             @@ -1 +0,0 @@\n\
             -gone();\n\
             diff --git a/src/lib.rs b/src/lib.rs\n\
             --- a/src/lib.rs\n\
             +++ b/src/lib.rs\n\
             @@ -1 +1 @@\n\
             -old();\n\
             +new();\n",
        )
        .unwrap();

        let changed = files
            .iter()
            .map(|file| (file.status, file.changed_paths()))
            .collect::<Vec<_>>();
        let paths = |paths: &[&str]| {
            paths
                .iter()
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            changed,
            vec![
                (GitChangeKind::Renamed, paths(&["src/new.rs", "src/old.rs"])),
                (GitChangeKind::Copied, paths(&["src/copy.rs"])),
                (GitChangeKind::Deleted, paths(&["src/gone.rs"])),
                (GitChangeKind::Modified, paths(&["src/lib.rs"])),
            ]
        );
    }

    #[test]
    fn unified_zero_diff_hunk_lines_that_look_like_file_headers_name_no_path() {
        let files = parse_unified_zero_diff(
            "diff --git a/db/schema.sql b/db/schema.sql\n\
             --- a/db/schema.sql\n\
             +++ b/db/schema.sql\n\
             @@ -3 +3 @@\n\
             --- legacy index\n\
             +++ replacement index\n\
             diff --git a/db/old.sql b/db/new.sql\n\
             similarity index 91%\n\
             rename from db/old.sql\n\
             rename to db/new.sql\n\
             --- a/db/old.sql\n\
             +++ b/db/new.sql\n\
             @@ -1 +1 @@\n\
             --- dropped view\n\
             +++ kept view\n",
        )
        .unwrap();

        let changed = files
            .iter()
            .map(|file| (file.status, file.changed_paths(), file.hunks.len()))
            .collect::<Vec<_>>();
        let paths = |paths: &[&str]| {
            paths
                .iter()
                .map(std::path::PathBuf::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            changed,
            vec![
                (GitChangeKind::Modified, paths(&["db/schema.sql"]), 1),
                (
                    GitChangeKind::Renamed,
                    paths(&["db/new.sql", "db/old.sql"]),
                    1
                ),
            ]
        );
    }

    #[test]
    fn unified_zero_diff_paths_ignore_local_prefix_and_color_config() {
        let dir = initialized_repo();
        write(dir.path(), "src/lib.rs", "fn one() {}\n");
        commit_all(dir.path(), "one");
        run(dir.path(), &["config", "diff.mnemonicPrefix", "true"]);
        run(dir.path(), &["config", "diff.srcPrefix", "old/"]);
        run(dir.path(), &["config", "diff.dstPrefix", "new/"]);
        run(dir.path(), &["config", "color.diff", "always"]);
        write(dir.path(), "src/lib.rs", "fn one() {}\nfn two() {}\n");

        let changed = diff_unified_zero_since(dir.path(), "HEAD").unwrap();

        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0].status, GitChangeKind::Modified, "{changed:?}");
        assert_eq!(
            changed[0].changed_paths(),
            vec![std::path::PathBuf::from("src/lib.rs")]
        );
    }

    #[test]
    fn unified_zero_diff_hunk_counts_decide_where_content_ends() {
        let files = parse_unified_zero_diff(
            "diff --git a/src/a.rs b/src/a.rs\n\
             --- a/src/a.rs\n\
             +++ b/src/a.rs\n\
             @@ -1 +1 @@\n\
             --- one\n\
             \\ No newline at end of file\n\
             +++ two\n\
             \\ No newline at end of file\n\
             diff --git a/src/b.rs b/src/b.rs\n\
             --- a/src/b.rs\n\
             +++ b/src/b.rs\n\
             @@ -2,2 +2 @@\n\
             --- gone\n\
             --- also gone\n\
             +++ kept\n",
        )
        .unwrap();

        let changed = files
            .iter()
            .map(|file| (file.status, file.changed_paths(), file.hunks.len()))
            .collect::<Vec<_>>();
        assert_eq!(
            changed,
            vec![
                (
                    GitChangeKind::Modified,
                    vec![std::path::PathBuf::from("src/a.rs")],
                    1
                ),
                (
                    GitChangeKind::Modified,
                    vec![std::path::PathBuf::from("src/b.rs")],
                    1
                ),
            ]
        );
    }

    #[test]
    fn unified_zero_diff_parser_captures_old_new_hunks_and_changed_ranges() {
        let files = parse_unified_zero_diff(
            "diff --git a/src/old.rs b/src/new.rs\n\
             similarity index 92%\n\
             rename from src/old.rs\n\
             rename to src/new.rs\n\
             --- a/src/old.rs\n\
             +++ b/src/new.rs\n\
             @@ -2 +2 @@\n\
             -old();\n\
             +new();\n\
             @@ -8,0 +9,2 @@\n\
             +added();\n\
             +again();\n\
             diff --git a/src/deleted.rs b/src/deleted.rs\n\
             deleted file mode 100644\n\
             --- a/src/deleted.rs\n\
             +++ /dev/null\n\
             @@ -1,3 +0,0 @@\n\
             -one();\n\
             -two();\n\
             -three();\n",
        )
        .unwrap();

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].status, GitChangeKind::Renamed);
        assert_eq!(files[0].rename_score, Some(92));
        assert_eq!(files[0].old_path.as_deref(), Some(Path::new("src/old.rs")));
        assert_eq!(files[0].new_path.as_deref(), Some(Path::new("src/new.rs")));
        assert_eq!(
            files[0].hunks,
            vec![
                super::DiffHunk {
                    old_range: Some(open_kioku_core::LineRange { start: 2, end: 2 }),
                    new_range: Some(open_kioku_core::LineRange { start: 2, end: 2 }),
                },
                super::DiffHunk {
                    old_range: None,
                    new_range: Some(open_kioku_core::LineRange { start: 9, end: 10 }),
                }
            ]
        );
        assert_eq!(
            files[0].changed_line_ranges(),
            vec![
                open_kioku_core::LineRange { start: 2, end: 2 },
                open_kioku_core::LineRange { start: 9, end: 10 }
            ]
        );
        assert_eq!(files[1].status, GitChangeKind::Deleted);
        assert_eq!(
            files[1].hunks[0].old_range,
            Some(open_kioku_core::LineRange { start: 1, end: 3 })
        );
        assert_eq!(files[1].hunks[0].new_range, None);
    }

    #[test]
    fn patch_parser_decodes_quoted_paths_and_ignores_deletion_ranges() {
        let patches = parse_file_patches(
            "diff --git \"a/src/space\\040name.rs\" \"b/src/space\\040name.rs\"\n\
             --- \"a/src/space\\040name.rs\"\n\
             +++ \"b/src/space\\040name.rs\"\n\
             @@ -3,2 +3,0 @@\n\
             -gone();\n\
             -gone_too();\n\
             @@ -8 +6,2 @@\n\
             -old();\n\
             +new();\n\
             +added();\n",
        )
        .unwrap();

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].path, Path::new("src/space name.rs"));
        assert_eq!(
            patches[0].line_ranges,
            vec![open_kioku_core::LineRange { start: 6, end: 7 }]
        );
    }

    #[test]
    fn patch_parser_ignores_record_separator_bytes_inside_diff_content() {
        let mut raw = b"\x1e0123456789abcdef0123456789abcdef01234567\x00diff --git a/a.rs b/a.rs\n\
              --- /dev/null\n\
              +++ b/a.rs\n\
              @@ -0,0 +1 @@\n\
              +embedded "
            .to_vec();
        raw.push(0x1e);
        raw.extend_from_slice(b" byte\n");

        let patches = parse_commit_patches(&raw).unwrap().commits;

        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].files.len(), 1);
        assert_eq!(patches[0].files[0].path, Path::new("a.rs"));
    }

    fn initialized_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "--quiet"]);
        run(dir.path(), &["config", "user.email", "test@example.com"]);
        run(dir.path(), &["config", "user.name", "Test User"]);
        run(dir.path(), &["config", "commit.gpgsign", "false"]);
        dir
    }

    fn commit_all(root: &Path, message: &str) {
        run(root, &["add", "."]);
        run(root, &["commit", "--quiet", "-m", message]);
    }

    fn write(root: &Path, path: &str, content: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn run(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    /// A `since` revision reaches git after `--end-of-options`: an option-shaped value is a
    /// revision git cannot resolve, never an instruction it obeys.
    #[test]
    fn since_revision_is_never_parsed_as_a_git_option() {
        let dir = initialized_repo();
        write(dir.path(), "src/one.rs", "fn one() {}\n");
        commit_all(dir.path(), "one");
        write(dir.path(), "src/one.rs", "fn one() {}\nfn two() {}\n");
        commit_all(dir.path(), "two");

        let outside = tempfile::tempdir().unwrap();
        let sink = outside.path().join("diff.txt");
        let since = format!("--output={}", sink.display());
        assert!(
            diff_unified_zero_since(dir.path(), &since).is_err(),
            "an option-shaped revision must be rejected"
        );
        assert!(
            diff_name_status_since(dir.path(), &since).is_err(),
            "an option-shaped revision must be rejected"
        );
        assert!(
            !sink.exists(),
            "git must not have written {}",
            sink.display()
        );

        let changed = diff_unified_zero_since(dir.path(), "HEAD~1").unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(
            changed[0].new_path.as_deref(),
            Some(Path::new("src/one.rs"))
        );
        assert_eq!(
            diff_name_status_since(dir.path(), "HEAD~1").unwrap().len(),
            1
        );
    }

    fn head(root: &Path) -> String {
        commit(root).expect("a committed repository has a HEAD")
    }

    #[test]
    fn a_commit_is_related_to_head_by_its_shared_history() {
        let dir = initialized_repo();
        write(dir.path(), "src/one.rs", "fn one() {}\n");
        commit_all(dir.path(), "one");
        let first = head(dir.path());
        write(dir.path(), "src/one.rs", "fn one() {}\nfn two() {}\n");
        commit_all(dir.path(), "two");
        write(dir.path(), "src/three.rs", "fn three() {}\n");
        commit_all(dir.path(), "three");

        let same = compare_with_head(dir.path(), &head(dir.path()))
            .unwrap()
            .unwrap();
        assert_eq!(same.relation, RevisionRelation::Same);
        let behind = compare_with_head(dir.path(), &first).unwrap().unwrap();
        assert_eq!(
            behind.relation,
            RevisionRelation::Related {
                ahead: 0,
                behind: 2
            }
        );
        // Abbreviated ids resolve to the full one.
        let short = compare_with_head(dir.path(), &first[..10])
            .unwrap()
            .unwrap();
        assert_eq!(short.commit, first);
        assert_eq!(
            changed_paths_since_commit(dir.path(), &first)
                .unwrap()
                .len(),
            2
        );
        write(dir.path(), "src/three.rs", "fn three() { }\n");
        assert_eq!(
            changed_paths_since_commit(dir.path(), &head(dir.path()))
                .unwrap()
                .len(),
            1,
            "uncommitted changes count"
        );
    }

    #[test]
    fn an_absent_unrecorded_or_unrelated_commit_is_never_related_to_head() {
        let dir = initialized_repo();
        write(dir.path(), "a.rs", "fn a() {}\n");
        commit_all(dir.path(), "a");
        let other = initialized_repo();
        write(other.path(), "b.rs", "fn b() {}\n");
        commit_all(other.path(), "b");
        let foreign = head(other.path());

        for value in [foreign.as_str(), "unknown", "HEAD", "--all", "deadbeef"] {
            let comparison = compare_with_head(dir.path(), value).unwrap().unwrap();
            assert_eq!(comparison.relation, RevisionRelation::Unknown, "{value}");
        }
        run(
            dir.path(),
            &["fetch", "--quiet", &other.path().to_string_lossy(), "HEAD"],
        );
        let unrelated = compare_with_head(dir.path(), &foreign).unwrap().unwrap();
        assert_eq!(unrelated.relation, RevisionRelation::Unrelated);

        let bare = tempfile::tempdir().unwrap();
        assert!(compare_with_head(bare.path(), &foreign).unwrap().is_none());
    }

    #[test]
    fn head_resolves_through_packed_refs() {
        let dir = initialized_repo();
        write(dir.path(), "a.rs", "fn a() {}\n");
        commit_all(dir.path(), "a");
        let loose = head(dir.path());
        run(dir.path(), &["pack-refs", "--all"]);
        assert_eq!(commit(dir.path()), Some(loose));
    }
}
