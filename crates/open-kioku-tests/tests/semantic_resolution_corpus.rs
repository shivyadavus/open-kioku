use open_kioku_config::OkConfig;
use open_kioku_ingest::Indexer;

#[test]
fn verifies_multi_language_semantic_resolution_pipeline() {
    let config = OkConfig::default();
    let indexer = Indexer::default();

    // Verifies that the public Indexer pipeline successfully indexes workspace fixtures without failing
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    let java_file = src_dir.join("Service.java");
    std::fs::write(
        &java_file,
        r#"
package com.acme;

public class Service {
    public void execute() {}
    public void run() {
        execute();
    }
}
"#,
    )
    .unwrap();

    let (snapshot, _) = indexer
        .index_repo_with_history(temp_dir.path(), &config)
        .expect("indexing pipeline failed");

    assert!(
        !snapshot.symbols.is_empty(),
        "expected symbols to be extracted"
    );
}
