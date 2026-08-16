use criterion::{criterion_group, criterion_main, Criterion};
use open_kioku_config::OkConfig;
use open_kioku_core::IndexMode;
use open_kioku_ingest::Indexer;
use std::fs;
use tempfile::{tempdir, TempDir};

fn benchmark_repo() -> TempDir {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("examples")).unwrap();
    fs::create_dir_all(repo.join("testdata")).unwrap();
    fs::create_dir_all(repo.join("docs")).unwrap();
    for i in 0..100 {
        fs::write(
            repo.join("src").join(format!("file_{i}.rs")),
            format!("pub fn function_{i}() {{ println!(\"Hello\"); }}"),
        )
        .unwrap();
    }
    for i in 0..40 {
        fs::write(
            repo.join("examples").join(format!("example_{i}.rs")),
            format!("pub fn example_{i}() {{}}"),
        )
        .unwrap();
        fs::write(
            repo.join("testdata").join(format!("fixture_{i}.rs")),
            format!("pub fn fixture_{i}() {{}}"),
        )
        .unwrap();
    }
    for i in 0..20 {
        fs::write(
            repo.join("docs").join(format!("guide_{i}.md")),
            format!("# Guide {i}\nintro\n## Workflow\nStep {i} uses deterministic context.\n"),
        )
        .unwrap();
    }
    temp
}

fn benchmark_config() -> OkConfig {
    let mut config = OkConfig::default();
    config.history.enabled = false;
    config.scip.enabled = false;
    config.semantic.enabled = false;
    config
}

fn benchmark_indexing(c: &mut Criterion) {
    let mut group = c.benchmark_group("indexing");
    group.sample_size(10);

    group.bench_function("index_sample_repo", |b| {
        b.iter_with_setup(benchmark_repo, |temp| {
            let repo = temp.path().join("repo");
            let _ = Indexer::default()
                .index_repo(&repo, &benchmark_config())
                .unwrap();
        });
    });

    group.bench_function("index_mode_fast_with_document_corpus", |b| {
        b.iter_with_setup(benchmark_repo, |temp| {
            let repo = temp.path().join("repo");
            let snapshot = Indexer::default()
                .index_repo_with_mode(&repo, &benchmark_config(), IndexMode::Fast)
                .unwrap();
            assert!(!snapshot.document_sections.is_empty());
        });
    });

    group.bench_function("index_mode_full_with_document_corpus", |b| {
        b.iter_with_setup(benchmark_repo, |temp| {
            let repo = temp.path().join("repo");
            let snapshot = Indexer::default()
                .index_repo_with_mode(&repo, &benchmark_config(), IndexMode::Full)
                .unwrap();
            assert!(!snapshot.document_sections.is_empty());
        });
    });

    group.finish();
}

criterion_group!(benches, benchmark_indexing);
criterion_main!(benches);
