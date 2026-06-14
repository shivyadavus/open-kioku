use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use open_kioku_git::commit_history;
use std::fs;
use std::process::Command;

fn benchmark_history_ingest(c: &mut Criterion) {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "--quiet"]);
    git(repo.path(), &["config", "user.email", "bench@example.com"]);
    git(repo.path(), &["config", "user.name", "History Bench"]);
    git(repo.path(), &["config", "commit.gpgsign", "false"]);
    git(repo.path(), &["config", "gc.auto", "0"]);
    for index in 0..50 {
        let path = repo.path().join(format!("src/file_{index}.rs"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!("pub fn value_{index}() -> usize {{ {index} }}\n"),
        )
        .unwrap();
        git(repo.path(), &["add", "."]);
        git(
            repo.path(),
            &["commit", "--quiet", "-m", &format!("change {index}")],
        );
    }

    let mut group = c.benchmark_group("git_history_ingest");
    group.sample_size(10);
    for max_commits in [10usize, 50] {
        group.bench_with_input(
            BenchmarkId::new("commit_history", max_commits),
            &max_commits,
            |b, max_commits| {
                b.iter(|| commit_history(repo.path(), *max_commits).unwrap());
            },
        );
    }
    group.finish();
}

fn git(root: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

criterion_group!(benches, benchmark_history_ingest);
criterion_main!(benches);
