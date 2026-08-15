use open_kioku_config::{OkConfig, ResolutionMode};
use open_kioku_core::{Confidence, GraphEdgeType};
use open_kioku_ingest::Indexer;

#[test]
fn verifies_multi_language_semantic_resolution_pipeline() {
    let mut config = OkConfig::default();
    config.index.resolution_mode = ResolutionMode::V2;
    config.history.enabled = false;
    config.scip.enabled = false;
    let indexer = Indexer::default();

    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let src_dir = temp_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();

    // 1. Java fixture
    let java_dir = src_dir.join("com/acme");
    std::fs::create_dir_all(&java_dir).unwrap();
    std::fs::write(
        java_dir.join("Service.java"),
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

    // 2. TypeScript fixture
    std::fs::write(
        src_dir.join("math.ts"),
        r#"
export function add(a: number, b: number): number {
    return a + b;
}
export function calculate(): number {
    return add(1, 2);
}
"#,
    )
    .unwrap();

    // 3. Python fixture
    std::fs::write(
        src_dir.join("app.py"),
        r#"
def compute(x):
    return x * 2

def process():
    return compute(10)
"#,
    )
    .unwrap();

    // 4. Rust fixture
    std::fs::write(
        src_dir.join("worker.rs"),
        r#"
pub fn step() -> u32 {
    42
}
pub fn run() -> u32 {
    step()
}
"#,
    )
    .unwrap();

    // 5. Go fixture
    std::fs::write(
        src_dir.join("handler.go"),
        r#"
package main

func helper() int {
    return 1
}
func Handle() int {
    return helper()
}
"#,
    )
    .unwrap();

    let (snapshot, _) = indexer
        .index_repo_with_history(temp_dir.path(), &config)
        .expect("indexing pipeline failed");

    assert!(
        !snapshot.symbols.is_empty(),
        "expected symbols to be extracted across languages"
    );
    assert!(
        !snapshot.call_sites.is_empty(),
        "expected call sites to be extracted across languages"
    );
    assert!(
        snapshot
            .call_sites
            .iter()
            .all(|c| c.caller_symbol_id.is_some()),
        "expected all calls to have attributed caller symbols"
    );

    // Verify V2 resolution populated resolved relationships with exact evidence
    assert!(
        !snapshot.resolved_relationships.is_empty(),
        "expected V2 resolved relationships"
    );
    for rel in &snapshot.resolved_relationships {
        assert_eq!(rel.edge_type, GraphEdgeType::Calls);
        assert!(matches!(
            rel.confidence,
            Confidence::Exact | Confidence::High
        ));
        assert!(
            !rel.evidence.is_empty(),
            "expected non-empty resolution evidence"
        );
        for ev in &rel.evidence {
            assert!(
                ev.file_range.is_some(),
                "expected evidence to carry exact call-site source range"
            );
        }
    }

    // Verify Java specific call resolution: run -> execute
    let java_file = snapshot
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("Service.java"))
        .expect("expected Service.java file in snapshot");

    let java_execute_symbol = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "execute" && s.file_id == java_file.id)
        .expect("expected Service.execute symbol");

    let java_run_symbol = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "run" && s.file_id == java_file.id)
        .expect("expected Service.run symbol");

    let java_call_edge = snapshot
        .resolved_relationships
        .iter()
        .find(|r| r.from == java_run_symbol.id && r.to == java_execute_symbol.id);

    assert!(
        java_call_edge.is_some(),
        "expected resolved Calls edge from Service.run to Service.execute"
    );

    // Verify Shadow diffs and quality report
    assert!(
        !snapshot.resolution_diffs.is_empty(),
        "expected resolution diffs in V2/Shadow mode"
    );
    let quality = snapshot
        .resolution_quality
        .expect("expected resolution quality report");
    assert!(quality.resolved_exact > 0 || quality.resolved_high > 0);
}
