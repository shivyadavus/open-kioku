use open_kioku_errors::{OkError, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct CochangeRecord {
    pub path: PathBuf,
    pub cochanged_path: PathBuf,
    pub commit_count: usize,
    pub recency_weight: f32,
    pub test_corun: bool,
    pub commits: Vec<String>,
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
    let root = root.as_ref();
    if !root.join(".git").exists() || max_commits == 0 || max_files_per_commit < 2 {
        return Ok(Vec::new());
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("log")
        .arg(format!("--max-count={max_commits}"))
        .arg("--name-only")
        .arg("--pretty=format:commit:%H")
        .output()
        .map_err(|err| OkError::Repository(format!("git history scan failed: {err}")))?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();
    let mut current_sha: Option<String> = None;
    let mut current_files = Vec::new();
    for line in stdout.lines() {
        if let Some(sha) = line.strip_prefix("commit:") {
            push_commit(&mut commits, current_sha.take(), &mut current_files);
            current_sha = Some(sha.trim().to_string());
        } else {
            let path = line.trim();
            if is_history_path(path) {
                current_files.push(PathBuf::from(path));
            }
        }
    }
    push_commit(&mut commits, current_sha, &mut current_files);

    let mut pairs: HashMap<(PathBuf, PathBuf), CochangeRecord> = HashMap::new();
    for (idx, (sha, mut files)) in commits.into_iter().enumerate() {
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
                    entry.commits.push(sha.clone());
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
    Ok(records)
}

fn push_commit(
    commits: &mut Vec<(String, Vec<PathBuf>)>,
    sha: Option<String>,
    files: &mut Vec<PathBuf>,
) {
    if let Some(sha) = sha {
        commits.push((sha, std::mem::take(files)));
    }
}

fn is_history_path(path: &str) -> bool {
    !path.is_empty()
        && !path.ends_with('/')
        && !path.starts_with(".git/")
        && !path.starts_with(".ok/")
        && !path.contains("=>")
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
    use super::cochange_records;
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

    fn write(root: &std::path::Path, path: &str, content: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn run(root: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}
