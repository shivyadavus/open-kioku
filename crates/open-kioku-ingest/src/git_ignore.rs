use open_kioku_errors::{OkError, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Returns the paths that Git itself considers ignored below `root`.
///
/// `Some(paths)` means `root` is inside a Git work tree and Git was used as
/// the source of truth. `None` means there is no Git work tree, so callers may
/// fall back to filesystem-style ignore handling.
///
/// The `git ls-files --others --ignored --exclude-standard` shape is
/// intentional: `--others` prevents tracked files from being reported as
/// ignored even when an exclude pattern matches them, while
/// `--exclude-standard` delegates nested `.gitignore`, `.git/info/exclude`, and
/// global exclude precedence to Git instead of reimplementing it here.
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

    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ])
        .output()
        .map_err(|err| OkError::Repository(format!("git ignore discovery failed: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OkError::Repository(format!(
            "git ignore discovery failed: {}",
            stderr.trim()
        )));
    }

    let mut paths = HashSet::new();
    for raw in output.stdout.split(|byte| *byte == 0) {
        if raw.is_empty() {
            continue;
        }
        let path = String::from_utf8(raw.to_vec()).map_err(|err| {
            OkError::Repository(format!("git ignored path is not valid UTF-8: {err}"))
        })?;
        paths.insert(PathBuf::from(path));
    }
    Ok(Some(paths))
}

fn has_git_marker(root: &Path) -> bool {
    root.ancestors().any(|ancestor| ancestor.join(".git").exists())
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
