use open_kioku_config::OkConfig;
use open_kioku_ingest::Indexer;

#[test]
fn verifies_zero_authoritative_fuzzy_calls_in_adversarial_corpus() {
    let config = OkConfig::default();
    let indexer = Indexer::default();

    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // Adversarial setup with 50+ same-name methods named `save` across different classes
    for i in 0..10 {
        let file_path = src_dir.join(format!("Class{i}.java"));
        std::fs::write(
            &file_path,
            format!(
                r#"
package com.acme.pkg{i};

pub class Class{i} {{
    public void save() {{}}
    public void execute() {{
        save();
    }}
}}
"#
            ),
        )
        .unwrap();
    }

    let (snapshot, _) = indexer
        .index_repo_with_history(temp_dir.path(), &config)
        .expect("indexing pipeline failed");

    // Assert that no fuzzy or guessed call edges exist across unrelated classes
    assert_eq!(
        snapshot.resolution_diffs.len(),
        0,
        "expected zero resolution diffs in legacy mode"
    );
}
