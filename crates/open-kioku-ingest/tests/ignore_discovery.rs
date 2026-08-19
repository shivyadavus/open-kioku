use open_kioku_config::OkConfig;
use open_kioku_core::{SkipReason, SkipSource};
use open_kioku_ingest::Indexer;
use std::fs;
use std::path::Path;
use std::process::Command;

#[test]
fn nested_gitignore_is_scoped_to_its_directory() {
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
    write(dir.path(), "notes/scratch.java", "class Scratch {}\n");
    write(dir.path(), "notes/README.md", "kept\n");

    let snapshot = index(dir.path());
    let indexed = snapshot
        .files
        .iter()
        .map(|file| file.path.as_path())
        .collect::<Vec<_>>();

    assert_eq!(snapshot.manifest.file_count, 2);
    assert!(indexed.contains(&Path::new("src/Foo.java")));
    assert!(indexed.contains(&Path::new("src/Bar.java")));
    assert!(!indexed.contains(&Path::new("notes/scratch.java")));
    assert!(snapshot.skipped_paths.iter().any(|skipped| {
        skipped.path == Path::new("notes/scratch.java")
            && skipped.reason == SkipReason::Ignored
            && skipped.source == SkipSource::GitIgnore
    }));
}

#[test]
fn tracked_file_is_not_skipped_when_gitignore_pattern_matches_it() {
    let dir = initialized_repo();
    write(dir.path(), "src/Foo.java", "class Foo {}\n");
    run(dir.path(), &["add", "src/Foo.java"]);
    run(dir.path(), &["commit", "--quiet", "-m", "tracked source"]);

    write(dir.path(), ".gitignore", "*.java\n");
    write(dir.path(), "src/Untracked.java", "class Untracked {}\n");

    let tracked_check = Command::new("git")
        .arg("-C")
        .arg(dir.path())
        .args(["check-ignore", "src/Foo.java"])
        .status()
        .unwrap();
    assert!(!tracked_check.success(), "Git must not report the tracked file as ignored");

    let snapshot = index(dir.path());
    assert!(snapshot
        .files
        .iter()
        .any(|file| file.path == Path::new("src/Foo.java")));
    assert!(!snapshot
        .files
        .iter()
        .any(|file| file.path == Path::new("src/Untracked.java")));
    assert!(snapshot.skipped_paths.iter().any(|skipped| {
        skipped.path == Path::new("src/Untracked.java") && skipped.source == SkipSource::GitIgnore
    }));
    assert!(!snapshot.skipped_paths.iter().any(|skipped| {
        skipped.path == Path::new("src/Foo.java") && skipped.source == SkipSource::GitIgnore
    }));
}

#[test]
fn nested_okignore_is_scoped_to_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/Foo.java", "class Foo {}\n");
    write(dir.path(), "notes/.okignore", "*\n");
    write(dir.path(), "notes/scratch.java", "class Scratch {}\n");

    let snapshot = index(dir.path());
    assert_eq!(snapshot.manifest.file_count, 1);
    assert!(snapshot
        .files
        .iter()
        .any(|file| file.path == Path::new("src/Foo.java")));
    assert!(snapshot.skipped_paths.iter().any(|skipped| {
        skipped.path == Path::new("notes/scratch.java")
            && skipped.reason == SkipReason::Ignored
            && skipped.source == SkipSource::OkIgnore
    }));
}

fn index(root: &Path) -> open_kioku_ingest::IndexSnapshot {
    let mut config = OkConfig::default();
    config.scip.enabled = false;
    config.history.enabled = false;
    config.documents.enabled = false;
    Indexer::default().index_repo(root, &config).unwrap()
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
