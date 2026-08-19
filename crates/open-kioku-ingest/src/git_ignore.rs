use open_kioku_errors::{OkError, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Returns the candidate paths that Git itself considers ignored below `root`.
///
/// `Some(paths)` means `root` is inside a Git work tree and Git was used as
/// the source of truth. `None` means there is no Git work tree, so callers may
/// fall back to filesystem-style ignore handling.
///
/// A single `git check-ignore --stdin -z` process handles the entire candidate
/// set. This preserves Git's nested `.gitignore`, negation, `.git/info/exclude`,
/// and global-exclude semantics without spawning a process per file. We
/// intentionally do not pass `--no-index`, so tracked files are never reported
/// as ignored merely because an exclude pattern also matches them.
pub(crate) fn ignored_paths(
    root: &Path,
    candidates: &[PathBuf],
) -> Result<Option<HashSet<PathBuf>>> {
    if !has_git_marker(root) {
        return Ok(None);
    }

    let probe = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|err| OkError::Repository(format!("git ignore probe failed: {err}")))?;
    if !probe.status.success() || String::from_utf8_lossy(&probe.stdout).trim() != "true" {
        return Ok(None);
    }
    if candidates.is_empty() {
        return Ok(Some(HashSet::new()));
    }

    let mut by_git_path = HashMap::<String, PathBuf>::with_capacity(candidates.len());
    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| OkError::Repository(format!("git ignore discovery failed: {err}")))?;

    {
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            OkError::Repository("git ignore discovery could not open stdin".into())
        })?;
        for candidate in candidates {
            let value = candidate.to_string_lossy().into_owned();
            by_git_path.insert(value.clone(), candidate.clone());
            stdin
                .write_all(value.as_bytes())
                .and_then(|_| stdin.write_all(&[0]))
                .map_err(|err| {
                    OkError::Repository(format!("git ignore discovery input failed: {err}"))
                })?;
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|err| OkError::Repository(format!("git ignore discovery failed: {err}")))?;
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OkError::Repository(format!(
            "git ignore discovery failed: {}",
            stderr.trim()
        )));
    }

    let mut ignored = HashSet::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let value = String::from_utf8_lossy(raw);
        if let Some(candidate) = by_git_path.get(value.as_ref()) {
            ignored.insert(candidate.clone());
        }
    }
    Ok(Some(ignored))
}

fn has_git_marker(root: &Path) -> bool {
    root.ancestors().any(|ancestor| ancestor.join(".git").exists())
}

#[cfg(test)]
mod tests {
    use super::ignored_paths;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[test]
    fn git_is_authoritative_for_nested_scope_and_tracked_files() {
        let dir = initialized_repo();
        write(dir.path(), "src/Foo.java", "class Foo {}\n");
        write(dir.path(), "src/Bar.java", "class Bar {}\n");
        run(dir.path(), &["add", "src"]);
        run(dir.path(), &["commit", "--quiet", "-m", "tracked sources"]);

        write(
            dir.path(),
            "notes/.gitignore",
            "*\n!README.md\n!.gitignore\n",
        );
        write(dir.path(), "notes/scratch.txt", "scratch\n");
        write(dir.path(), "notes/README.md", "kept\n");

        let candidates = paths(&[
            "src/Foo.java",
            "src/Bar.java",
            "notes/scratch.txt",
            "notes/README.md",
        ]);
        let ignored = ignored_paths(dir.path(), &candidates).unwrap().unwrap();
        assert!(ignored.contains(Path::new("notes/scratch.txt")));
        assert!(!ignored.contains(Path::new("src/Foo.java")));
        assert!(!ignored.contains(Path::new("src/Bar.java")));
        assert!(!ignored.contains(Path::new("notes/README.md")));

        write(dir.path(), ".gitignore", "*.java\n");
        write(dir.path(), "src/New.java", "class New {}\n");
        let candidates = paths(&["src/Foo.java", "src/Bar.java", "src/New.java"]);
        let ignored = ignored_paths(dir.path(), &candidates).unwrap().unwrap();
        assert!(ignored.contains(Path::new("src/New.java")));
        assert!(!ignored.contains(Path::new("src/Foo.java")));
        assert!(!ignored.contains(Path::new("src/Bar.java")));
    }

    #[test]
    fn plain_directory_does_not_require_git() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ignored_paths(dir.path(), &[PathBuf::from("src/Foo.java")])
            .unwrap()
            .is_none());
    }

    fn paths(values: &[&str]) -> Vec<PathBuf> {
        values.iter().map(PathBuf::from).collect()
    }

    fn initialized_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "--quiet"]);
        run(dir.path(), &["config", "user.email", "test@example.com"]);
        run(dir.path(), &["config", "user.name", "Test User"]);
        run(dir.path(), &["config", "commit.gpgsign", "false"]);
        dir
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
