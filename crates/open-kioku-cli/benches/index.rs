use criterion::{criterion_group, criterion_main, Criterion};
use open_kioku_config::OkConfig;
use open_kioku_ingest::Indexer;
use std::fs;
use tempfile::tempdir;

fn benchmark_indexing(c: &mut Criterion) {
    let mut group = c.benchmark_group("indexing");
    group.sample_size(10);

    group.bench_function("index_sample_repo", |b| {
        b.iter_with_setup(
            || {
                let temp = tempdir().unwrap();
                let repo = temp.path().join("repo");
                fs::create_dir_all(&repo).unwrap();
                for i in 0..100 {
                    fs::write(
                        repo.join(format!("file_{}.rs", i)),
                        format!("pub fn function_{}() {{ println!(\"Hello\"); }}", i),
                    )
                    .unwrap();
                }
                OkConfig::write_default(repo.join("ok.toml")).unwrap();
                temp
            },
            |temp| {
                let repo = temp.path().join("repo");
                let config = OkConfig::default();
                let _ = Indexer::default().index_repo(&repo, &config).unwrap();
            },
        );
    });

    group.finish();
}

criterion_group!(benches, benchmark_indexing);
criterion_main!(benches);
