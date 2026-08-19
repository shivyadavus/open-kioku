use ignore::WalkBuilder;
use open_kioku_errors::{OkError, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Returns the paths that Git itself considers ignored below `root`.
///
/// `Some(paths)` means `root` is inside a Git work tree and Git was used as
/// the source of truth. `None` means there is no Git work tree, so callers may
/// fall back to filesystem-style ignore handling.
///
/// Discovery first collects the same lightweight filesystem candidates used by
/// indexing (without descending into known heavy build/cache directories), then
/// sends them through one `git check-ignore --stdin -z` process. This preserves
/// Git's nested `.gitignore`, negation, `.git/info/exclude`, and global-exclude
/// semantics without spawning a process per file. We intentionally do not pass
/// `--no-index`, so tracked files are never reported as ignored merely because
/// an exclude pattern also matches them.
pub(crate) fn ignored_paths(root: &Path) -> Result<Option<HashSet<PathBuf>>> {
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

    let candidates = filesystem_candidates(root);
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
        for candidate in &candidates {
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

fn filesystem_candidates(root: &Path) -> Vec<PathBuf> {
    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .ignore(false)
        .follow_links(false)
        .filter_entry(|entry| !is_heavy_discovery_dir(entry.path()))
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_type()
                .is_some_and(|kind| kind.is_file() || kind.is_symlink())
        })
        .map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .to_path_buf()
        })
        .collect()
}

fn has_git_marker(root: &Path) -> bool {
    root.ancestors().any(|ancestor| ancestor.join(".git").exists())
}

fn is_heavy_discovery_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git" | ".ok" | "target" | "node_modules" | "dist" | "build" | ".venv"
    )
}

#[cfg(test)]
mod tests {
    use super::ignored_paths;
    use std::fs;
    use std::path::Path;
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

        let ignored = ignored_paths(dir.path()).unwrap().unwrap();
        assert!(ignored.contains(Path::new("notes/scratch.txt")));
        assert!(!ignored.contains(Path::new("src/Foo.java")));
        assert!(!ignored.contains(Path::new("src/Bar.java")));
        assert!(!ignored.contains(Path::new("notes/README.md")));

        write(dir.path(), ".gitignore", "*.java\n");
        write(dir.path(), "src/New.java", "class New {}\n");
        let ignored = ignored_paths(dir.path()).unwrap().unwrap();
        assert!(ignored.contains(Path::new("src/New.java")));
        assert!(!ignored.contains(Path::new("src/Foo.java")));
        assert!(!ignored.contains(Path::new("src/Bar.java")));
    }

    #[test]
    fn plain_directory_does_not_require_git() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ignored_paths(dir.path()).unwrap().is_none());
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
