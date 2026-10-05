use crate::prune::{DirVerdict, DiscoveryPruner};
use ignore::WalkBuilder;
use open_kioku_errors::{OkError, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

/// Returns the paths that Git itself considers ignored below `root`.
///
/// `Some(paths)` means `root` is inside a Git work tree and Git was used as
/// the source of truth. `None` means there is no Git work tree, so callers may
/// fall back to filesystem-style ignore handling.
///
/// Discovery first collects the same lightweight filesystem candidates used by
/// indexing (without descending into the directories `crate::prune` cuts), then
/// sends them through one `git check-ignore --stdin -z` process. This preserves
/// Git's nested `.gitignore`, negation, `.git/info/exclude`, and global-exclude
/// semantics without spawning a process per file. We intentionally do not pass
/// `--no-index`, so tracked files are never reported as ignored merely because
/// an exclude pattern also matches them.
pub(crate) fn ignored_paths(pruner: &DiscoveryPruner) -> Result<Option<HashSet<PathBuf>>> {
    let root = pruner.root();
    if !inside_work_tree(root)? {
        return Ok(None);
    }
    let candidates = filesystem_candidates(pruner);
    check_ignored_candidates(root, &candidates).map(Some)
}

/// Git's verdict on `candidates` (paths relative to `root`) rather than on the files present
/// on disk: an imported index names paths the local checkout may not hold, and whether the
/// local configuration ignores them does not depend on that. Same `None` contract as
/// [`ignored_paths`].
pub(crate) fn ignored_among(
    root: &Path,
    candidates: &[PathBuf],
) -> Result<Option<HashSet<PathBuf>>> {
    if !inside_work_tree(root)? {
        return Ok(None);
    }
    // `git check-ignore` rejects a path outside the work tree (`../x`, `/x`) with a fatal
    // error that answers nothing for the whole batch, so only plain relative paths are sent;
    // no rule of Git's can ignore a path it cannot name anyway.
    let candidates = candidates
        .iter()
        .filter(|path| {
            !path.as_os_str().is_empty()
                && path
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
        })
        .cloned()
        .collect::<Vec<_>>();
    check_ignored_candidates(root, &candidates).map(Some)
}

/// Files Git tracks below `root`, relative to it; `None` outside a Git work tree. Discovery
/// asks only when it pruned a directory, to tell committed source under it from build output.
pub(crate) fn tracked_files(root: &Path) -> Result<Option<Vec<PathBuf>>> {
    if !inside_work_tree(root)? {
        return Ok(None);
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--cached"])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| OkError::Repository(format!("git tracked-file listing failed: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OkError::Repository(format!(
            "git tracked-file listing failed: {}",
            stderr.trim()
        )));
    }
    Ok(Some(
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|raw| !raw.is_empty())
            .map(|raw| PathBuf::from(String::from_utf8_lossy(raw).into_owned()))
            .collect(),
    ))
}

fn inside_work_tree(root: &Path) -> Result<bool> {
    if !has_git_marker(root) {
        return Ok(false);
    }
    let probe = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|err| OkError::Repository(format!("git ignore probe failed: {err}")))?;
    Ok(probe.status.success() && String::from_utf8_lossy(&probe.stdout).trim() == "true")
}

/// Git's verdict on `candidates`. `git check-ignore` fails the whole batch when one path lies
/// inside a submodule ("Pathspec ... is in submodule"), checked out or not. Discovery never
/// sends one from a checked-out submodule (`crate::prune` cuts it), but an imported index can
/// name files under a submodule this checkout has not initialised, and an uninitialised
/// submodule's directory can hold stray files. So on a failure, the paths under the gitlinks
/// Git records are left out of the answer (not ignored: no rule of this repository's reaches
/// them), and the rest are asked once more. The gitlinks are listed only then: a repository with no submodule pays
/// nothing, and no error message is parsed.
fn check_ignored_candidates(root: &Path, candidates: &[PathBuf]) -> Result<HashSet<PathBuf>> {
    match run_check_ignore(root, candidates) {
        Ok(ignored) => Ok(ignored),
        Err(err) => {
            // A listing that fails too says nothing about the first failure: report that one.
            let Ok(gitlinks) = gitlinks(root) else {
                return Err(err);
            };
            if gitlinks.is_empty() {
                return Err(err);
            }
            let outside = candidates
                .iter()
                .filter(|path| !gitlinks.iter().any(|link| path.starts_with(link)))
                .cloned()
                .collect::<Vec<_>>();
            if outside.len() == candidates.len() {
                return Err(err);
            }
            run_check_ignore(root, &outside)
        }
    }
}

/// The submodule paths Git's index records (mode `160000`), relative to `root`.
fn gitlinks(root: &Path) -> Result<Vec<PathBuf>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--stage"])
        .stdin(Stdio::null())
        .output()
        .map_err(|err| OkError::Repository(format!("git submodule listing failed: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OkError::Repository(format!(
            "git submodule listing failed: {}",
            stderr.trim()
        )));
    }
    // Each entry is `<mode> <object> <stage>\t<path>`.
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let entry = entry.strip_prefix(b"160000 ")?;
            let tab = entry.iter().position(|byte| *byte == b'\t')?;
            Some(PathBuf::from(
                String::from_utf8_lossy(&entry[tab + 1..]).into_owned(),
            ))
        })
        .collect())
}

fn run_check_ignore(root: &Path, candidates: &[PathBuf]) -> Result<HashSet<PathBuf>> {
    if candidates.is_empty() {
        return Ok(HashSet::new());
    }

    let mut by_git_path = HashMap::<String, PathBuf>::with_capacity(candidates.len());
    let mut input = Vec::new();
    for candidate in candidates {
        let value = candidate.to_string_lossy().into_owned();
        input.extend_from_slice(value.as_bytes());
        input.push(0);
        by_git_path.insert(value, candidate.clone());
    }

    let mut child = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "--stdin", "-z"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| OkError::Repository(format!("git ignore discovery failed: {err}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| OkError::Repository("git ignore discovery could not open stdin".into()))?;

    // Write concurrently with `wait_with_output`, which drains stdout/stderr.
    // Without this, a large ignored set can fill Git's stdout pipe while the
    // parent is still blocked writing stdin, deadlocking both processes.
    let writer = thread::spawn(move || stdin.write_all(&input));
    let output = child
        .wait_with_output()
        .map_err(|err| OkError::Repository(format!("git ignore discovery failed: {err}")))?;
    let write_result = writer
        .join()
        .map_err(|_| OkError::Repository("git ignore discovery input writer panicked".into()))?;

    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OkError::Repository(format!(
            "git ignore discovery failed: {}",
            stderr.trim()
        )));
    }
    write_result
        .map_err(|err| OkError::Repository(format!("git ignore discovery input failed: {err}")))?;

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
    Ok(ignored)
}

fn filesystem_candidates(pruner: &DiscoveryPruner) -> Vec<PathBuf> {
    let root = pruner.root();
    WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .ignore(false)
        .follow_links(false)
        .filter_entry({
            let pruner = pruner.clone();
            move |entry| {
                let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
                pruner.classify(entry.path(), is_dir) == DirVerdict::Walk
            }
        })
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
    root.ancestors()
        .any(|ancestor| ancestor.join(".git").exists())
}

#[cfg(test)]
mod tests {
    use super::check_ignored_candidates;
    use crate::prune::DiscoveryPruner;
    use std::collections::HashSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn ignored_paths(root: &Path) -> open_kioku_errors::Result<Option<HashSet<PathBuf>>> {
        super::ignored_paths(&DiscoveryPruner::evidence_only(root))
    }

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
    fn batched_check_drains_large_git_output_without_deadlocking() {
        let dir = initialized_repo();
        write(dir.path(), ".gitignore", "*.java\n");
        let candidates = (0..10_000)
            .map(|index| {
                PathBuf::from(format!(
                    "src/generated/VeryLongIgnoredCandidateName{index:05}.java"
                ))
            })
            .collect::<Vec<_>>();

        let ignored = check_ignored_candidates(dir.path(), &candidates).unwrap();
        assert_eq!(ignored.len(), candidates.len());
    }

    /// `git check-ignore` fails a batch naming a path inside a submodule, initialised or not.
    /// Such a path is dropped and the rest still get Git's answer: an imported index can name
    /// files under a submodule this checkout never initialised (#677).
    #[test]
    fn a_path_inside_a_submodule_does_not_fail_the_batch() {
        let dir = initialized_repo();
        write(dir.path(), ".gitignore", "*.log\n");
        // A gitlink, as `git submodule add` records it, without fetching anything.
        run(
            dir.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                "160000,1111111111111111111111111111111111111111,vendor/ledger",
            ],
        );
        let candidates = [
            PathBuf::from("src/main.rs"),
            PathBuf::from("trace.log"),
            PathBuf::from("vendor/ledger/lib.rs"),
            PathBuf::from("vendor/ledger/trace.log"),
        ];
        let ignored = check_ignored_candidates(dir.path(), &candidates).unwrap();
        assert_eq!(ignored, HashSet::from([PathBuf::from("trace.log")]));

        // A failure that no gitlink explains is still an error.
        let unexplained = [PathBuf::from("src/main.rs"), PathBuf::from("../outside.rs")];
        assert!(check_ignored_candidates(dir.path(), &unexplained).is_err());
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
