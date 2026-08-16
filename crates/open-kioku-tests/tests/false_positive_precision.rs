use open_kioku_config::{OkConfig, ResolutionMode};
use open_kioku_core::GraphEdgeType;
use open_kioku_ingest::Indexer;

#[test]
fn verifies_zero_authoritative_fuzzy_calls_in_adversarial_corpus() {
    let mut config = OkConfig::default();
    config.index.resolution_mode = ResolutionMode::V2;
    let indexer = Indexer::default();

    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // Adversarial setup with 50+ same-name methods named `save` across distinct packages
    for i in 0..50 {
        let pkg_dir = src_dir.join(format!("pkg{i}"));
        std::fs::create_dir_all(&pkg_dir).unwrap();
        let file_path = pkg_dir.join(format!("Class{i}.java"));
        std::fs::write(
            &file_path,
            format!(
                r#"
package com.acme.pkg{i};

public class Class{i} {{
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

    assert_eq!(snapshot.files.len(), 50, "expected 50 indexed files");

    // Check all resolved call edges in V2 mode
    for rel in &snapshot.resolved_relationships {
        if rel.edge_type == GraphEdgeType::Calls {
            let caller_sym = snapshot
                .symbols
                .iter()
                .find(|s| s.id == rel.from)
                .expect("caller symbol found");
            let callee_sym = snapshot
                .symbols
                .iter()
                .find(|s| s.id == rel.to)
                .expect("callee symbol found");

            // Crucial precision assertion: caller and callee MUST belong to the same file/class!
            // No cross-class false positives for `save()` methods.
            assert_eq!(
                caller_sym.file_id, callee_sym.file_id,
                "False positive detected: {} in {:?} resolved to {} in {:?}",
                caller_sym.name, caller_sym.file_id, callee_sym.name, callee_sym.file_id
            );
        }
    }

    // Verify zero cross-class calls exist in authoritative analysis facts
    for fact in &snapshot.analysis_facts {
        if fact.edge_type == GraphEdgeType::Calls {
            if let Some(symbol_id) = &fact.symbol_id {
                let caller_sym = snapshot.symbols.iter().find(|s| &s.id == symbol_id);
                let callee_sym = snapshot.symbols.iter().find(|s| s.id.0 == fact.target);
                if let (Some(caller), Some(callee)) = (caller_sym, callee_sym) {
                    assert_eq!(
                        caller.file_id, callee.file_id,
                        "Authoritative analysis fact has false positive cross-file call: caller {:?} callee {:?}",
                        caller, callee
                    );
                }
            }
        }
    }
}
