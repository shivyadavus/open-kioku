use open_kioku_config::{OkConfig, ResolutionMode};
use open_kioku_core::{identity, Confidence, GraphEdgeType};
use open_kioku_graph::InMemoryGraph;
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

    // 1. Java cross-file package and class import fixture + external unresolved import
    let java_repo_dir = src_dir.join("com/acme/repo");
    let java_svc_dir = src_dir.join("com/acme/service");
    std::fs::create_dir_all(&java_repo_dir).unwrap();
    std::fs::create_dir_all(&java_svc_dir).unwrap();
    std::fs::write(
        java_repo_dir.join("Repository.java"),
        r#"
package com.acme.repo;

public class Repository {
    public void save() {}
}
"#,
    )
    .unwrap();

    std::fs::write(
        java_svc_dir.join("Service.java"),
        r#"
package com.acme.service;

import com.acme.repo.Repository;
import com.vendor.external.UnresolvedClient;

public class Service {
    public void execute() {}
    public void run(Repository repo) {
        execute();
        repo.save();
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

    // 4. Rust fixture with impl-before-struct layout, multiple same-caller same-callee calls,
    //    and disambiguation across multiple types with same-name methods
    std::fs::write(
        src_dir.join("worker.rs"),
        r#"
// impl block placed BEFORE struct definition to test ordering resilience
impl Repo {
    pub fn save(&self) -> u32 {
        42
    }
    pub fn run(&self) -> u32 {
        let first = self.save();
        let second = self.save();
        first + second
    }
}

pub struct Repo;

impl Other {
    pub fn save(&self) -> u32 {
        99
    }
}

pub struct Other;

pub fn standalone_step() -> u32 {
    100
}

pub fn standalone_run() -> u32 {
    standalone_step()
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
            let path_str = ev.file_range.as_ref().unwrap().path.to_string_lossy();
            assert!(
                path_str.contains("src/"),
                "evidence path should be a repository relative file path, got: {path_str}"
            );
        }
    }

    // 1. Assert Java resolution: Service.run -> Service.execute and Service.run -> Repository.save
    let java_svc_file = snapshot
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("Service.java"))
        .expect("expected Service.java file in snapshot");

    let java_repo_file = snapshot
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("Repository.java"))
        .expect("expected Repository.java file in snapshot");

    let java_execute_symbol = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "execute" && s.file_id == java_svc_file.id)
        .expect("expected Service.execute symbol");

    let java_run_symbol = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "run" && s.file_id == java_svc_file.id)
        .expect("expected Service.run symbol");

    let java_repo_save_symbol = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "save" && s.file_id == java_repo_file.id)
        .expect("expected Repository.save symbol");

    let java_self_call_edge = snapshot
        .resolved_relationships
        .iter()
        .find(|r| r.from == java_run_symbol.id && r.to == java_execute_symbol.id);

    assert!(
        java_self_call_edge.is_some(),
        "expected resolved Calls edge from Service.run to Service.execute"
    );

    let java_cross_file_edge = snapshot
        .resolved_relationships
        .iter()
        .find(|r| r.from == java_run_symbol.id && r.to == java_repo_save_symbol.id);

    assert!(
        java_cross_file_edge.is_some(),
        "expected cross-file resolved Calls edge from Service.run to Repository.save"
    );

    // 2. Assert TypeScript resolution: calculate -> add
    let ts_file = snapshot
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("math.ts"))
        .expect("expected math.ts file in snapshot");

    let ts_add_sym = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "add" && s.file_id == ts_file.id)
        .expect("expected add symbol in math.ts");

    let ts_calc_sym = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "calculate" && s.file_id == ts_file.id)
        .expect("expected calculate symbol in math.ts");

    let ts_edge = snapshot
        .resolved_relationships
        .iter()
        .find(|r| r.from == ts_calc_sym.id && r.to == ts_add_sym.id);

    assert!(
        ts_edge.is_some(),
        "expected resolved Calls edge from TS calculate to add"
    );

    // 3. Assert Python resolution: process -> compute
    let py_file = snapshot
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("app.py"))
        .expect("expected app.py file in snapshot");

    let py_compute_sym = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "compute" && s.file_id == py_file.id)
        .expect("expected compute symbol in app.py");

    let py_process_sym = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "process" && s.file_id == py_file.id)
        .expect("expected process symbol in app.py");

    let py_edge = snapshot
        .resolved_relationships
        .iter()
        .find(|r| r.from == py_process_sym.id && r.to == py_compute_sym.id);

    assert!(
        py_edge.is_some(),
        "expected resolved Calls edge from Python process to compute"
    );

    // 4. Assert Go resolution: Handle -> helper
    let go_file = snapshot
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("handler.go"))
        .expect("expected handler.go file in snapshot");

    let go_helper_sym = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "helper" && s.file_id == go_file.id)
        .expect("expected helper symbol in handler.go");

    let go_handle_sym = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "Handle" && s.file_id == go_file.id)
        .expect("expected Handle symbol in handler.go");

    let go_edge = snapshot
        .resolved_relationships
        .iter()
        .find(|r| r.from == go_handle_sym.id && r.to == go_helper_sym.id);

    assert!(
        go_edge.is_some(),
        "expected resolved Calls edge from Go Handle to helper"
    );

    // 5. Assert Rust method resolution with impl-before-struct layout: Repo.run -> Repo.save (NOT Other.save!)
    let rust_file = snapshot
        .files
        .iter()
        .find(|f| f.path.to_string_lossy().contains("worker.rs"))
        .expect("expected worker.rs file in snapshot");

    let rust_repo_struct = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "Repo" && s.file_id == rust_file.id)
        .expect("expected Repo struct symbol");

    let rust_other_struct = snapshot
        .symbols
        .iter()
        .find(|s| s.name == "Other" && s.file_id == rust_file.id)
        .expect("expected Other struct symbol");

    let rust_repo_save = snapshot
        .symbols
        .iter()
        .find(|s| {
            s.name == "save"
                && s.file_id == rust_file.id
                && s.parent_symbol_id.as_ref() == Some(&rust_repo_struct.id)
        })
        .expect("expected Repo.save method symbol properly parented to Repo struct");

    let rust_repo_run = snapshot
        .symbols
        .iter()
        .find(|s| {
            s.name == "run"
                && s.file_id == rust_file.id
                && s.parent_symbol_id.as_ref() == Some(&rust_repo_struct.id)
        })
        .expect("expected Repo.run method symbol properly parented to Repo struct");

    let rust_other_save = snapshot
        .symbols
        .iter()
        .find(|s| {
            s.name == "save"
                && s.file_id == rust_file.id
                && s.parent_symbol_id.as_ref() == Some(&rust_other_struct.id)
        })
        .expect("expected Other.save method symbol properly parented to Other struct");

    let rust_impl_edge = snapshot
        .resolved_relationships
        .iter()
        .find(|r| r.from == rust_repo_run.id && r.to == rust_repo_save.id);

    assert!(
        rust_impl_edge.is_some(),
        "expected resolved Calls edge from Repo.run to Repo.save"
    );

    let rust_wrong_edge = snapshot
        .resolved_relationships
        .iter()
        .find(|r| r.from == rust_repo_run.id && r.to == rust_other_save.id);

    assert!(
        rust_wrong_edge.is_none(),
        "Repo.run must NOT resolve to Other.save"
    );

    // Verify InMemoryGraph contains real direct symbol->symbol Calls edges and preserves multi-call site evidence
    let graph = InMemoryGraph::from_index_with_resolved_relationships(
        &snapshot.files,
        &snapshot.symbols,
        &snapshot.chunks,
        &snapshot.occurrences,
        &snapshot.imports,
        &snapshot.analysis_facts,
        &snapshot.resolved_relationships,
    );

    let from_node = identity::symbol_node_id(rust_repo_run);
    let to_node = identity::symbol_node_id(rust_repo_save);

    let graph_edge = graph
        .edges
        .iter()
        .find(|e| e.edge_type == GraphEdgeType::Calls && e.from == from_node && e.to == to_node);

    assert!(
        graph_edge.is_some(),
        "expected direct symbol-to-symbol Calls graph edge from Repo.run to Repo.save"
    );

    let edge = graph_edge.unwrap();
    let call_sites = edge
        .properties
        .get("call_sites")
        .and_then(|v| v.as_array())
        .expect("expected call_sites property on edge");
    assert!(
        call_sites.len() >= 2,
        "expected all call-site evidence ranges to be preserved for deduplicated edge, found: {:?}",
        call_sites
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
