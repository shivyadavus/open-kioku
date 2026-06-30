mod ownership;
mod reviewers;

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
    let head = fs::read_to_string(root.as_ref().join(".git/HEAD")).ok()?;
    if !head.starts_with("ref: ") {
        return Some(head.trim().to_string());
    }
    let reference = head.trim().strip_prefix("ref: ")?;
    fs::read_to_string(root.as_ref().join(".git").join(reference))
        .ok()
        .map(|value| value.trim().to_string())
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

pub fn commit_patches(root: impl AsRef<Path>, max_commits: usize) -> Result<Vec<CommitPatch>> {
    let root = root.as_ref();
    if !root.join(".git").exists() || max_commits == 0 {
        return Ok(Vec::new());
    }
    let head = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .map_err(|err| OkError::Repository(format!("git patch scan failed: {err}")))?;
    if !head.status.success() {
        return Ok(Vec::new());
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
    run_diff_name_status(root, &[])
}

pub fn diff_name_status_since(root: impl AsRef<Path>, since: &str) -> Result<Vec<DiffFile>> {
    run_diff_name_status(root, &[since])
}

pub fn cached_diff_name_status(root: impl AsRef<Path>) -> Result<Vec<DiffFile>> {
    run_diff_name_status(root, &["--cached"])
}

pub fn head_diff_name_status(root: impl AsRef<Path>) -> Result<Vec<DiffFile>> {
    run_diff_name_status(root, &["HEAD"])
}

pub fn diff_unified_zero(root: impl AsRef<Path>) -> Result<Vec<DiffFile>> {
    run_diff_unified_zero(root, &[])
}

pub fn diff_unified_zero_since(root: impl AsRef<Path>, since: &str) -> Result<Vec<DiffFile>> {
    run_diff_unified_zero(root, &[since])
}

fn run_diff_unified_zero(root: impl AsRef<Path>, extra_args: &[&str]) -> Result<Vec<DiffFile>> {
    let root = root.as_ref();
    if !root.join(".git").exists() {
        return Ok(Vec::new());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotePath=true"])
        .arg("diff")
        .args(extra_args)
        .args(["--unified=0", "--no-ext-diff", "--no-textconv"])
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

fn run_diff_name_status(root: impl AsRef<Path>, extra_args: &[&str]) -> Result<Vec<DiffFile>> {
    let root = root.as_ref();
    if !root.join(".git").exists() {
        return Ok(Vec::new());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("diff")
        .args(extra_args)
        .args(["--name-status", "--find-renames"])
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

fn parse_commit_patches(raw: &[u8]) -> Result<Vec<CommitPatch>> {
    let mut commits = Vec::new();
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
        let patch = git_text(&record[metadata_end + 1..], "patch")?;
        commits.push(CommitPatch {
            commit_id,
            files: parse_file_patches(&patch)?,
        });
    }
    Ok(commits)
}

fn patch_record_starts(raw: &[u8]) -> Vec<usize> {
    raw.iter()
        .enumerate()
        .filter_map(|(index, byte)| {
            if *byte != COMMIT_RECORD_SEPARATOR {
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
    for line in patch.lines() {
        if line.starts_with("diff --git ") {
            finish(&mut patches, &mut pending);
        } else if let Some(value) = line.strip_prefix("rename from ") {
            pending.previous_path = Some(parse_patch_path(value, None)?);
        } else if let Some(value) = line.strip_prefix("rename to ") {
            pending.path = Some(parse_patch_path(value, None)?);
        } else if let Some(value) = line.strip_prefix("+++ ") {
            if value != "/dev/null" {
                pending.path = Some(parse_patch_path(value, Some("b/"))?);
            }
        } else if line.starts_with("@@ ") {
            if let Some(range) = parse_new_hunk_range(line)? {
                pending.line_ranges.push(range);
            }
        }
    }
    finish(&mut patches, &mut pending);
    Ok(patches)
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
        let status = pending.status.unwrap_or_else(|| {
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
    for line in patch.lines() {
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
        } else if let Some(value) = line.strip_prefix("--- ") {
            if value != "/dev/null" {
                pending.old_path = Some(parse_patch_path(value, Some("a/"))?);
            }
        } else if let Some(value) = line.strip_prefix("+++ ") {
            if value != "/dev/null" {
                pending.new_path = Some(parse_patch_path(value, Some("b/"))?);
            }
        } else if line.starts_with("@@ ") {
            pending.hunks.push(parse_diff_hunk(line)?);
        }
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
        cochange_records, commit_history, commit_patches, parse_commit_patches,
        parse_diff_name_status, parse_file_patches, parse_unified_zero_diff,
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

        let patches = commit_patches(dir.path(), 1).unwrap();

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
             @@ -1,3 +0,0 @@\n",
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
             @@ -8 +6,2 @@\n",
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
              +++ b/a.rs\n\
              @@ -0,0 +1 @@\n\
              +embedded "
            .to_vec();
        raw.push(0x1e);
        raw.extend_from_slice(b" byte\n");

        let patches = parse_commit_patches(&raw).unwrap();

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
}
