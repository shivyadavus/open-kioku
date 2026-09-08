//! One path from a real repository through indexing, SQLite and the graph into `ImpactEngine`.
//!
//! The unit tests hand-build three nodes and two edges, which cannot catch the failure this
//! guards: `neighbors` is an untyped, unordered window, and a file emits one `DEFINES` edge per
//! symbol, so a source file with enough symbols exhausts the window on its own definitions and
//! its derived siblings never come back. That only appears once real files with real symbol
//! counts are indexed.

use open_kioku_config::OkConfig;
use open_kioku_core::{GraphEdgeType, RelationshipAuthority};
use open_kioku_graph::InMemoryGraph;
use open_kioku_impact::ImpactEngine;
use open_kioku_ingest::Indexer;
use open_kioku_storage::{GraphStore, IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::Path;

/// Symbols on the origin file, chosen to exceed the neighbour window the engine asks for.
const ORIGIN_SYMBOLS: usize = 60;

fn write_repo(root: &Path) {
    std::fs::create_dir_all(root.join("pkg")).unwrap();
    // One function per symbol, so the file emits far more DEFINES edges than the window holds.
    let mut origin = String::from("package pkg\n");
    for index in 0..ORIGIN_SYMBOLS {
        origin.push_str(&format!("func Handler{index}() int {{ return {index} }}\n"));
    }
    std::fs::write(root.join("pkg/router.go"), origin).unwrap();
    std::fs::write(
        root.join("pkg/router_test.go"),
        "package pkg\n\nimport \"testing\"\n\nfunc TestHandler0(t *testing.T) { Handler0() }\n",
    )
    .unwrap();
}

#[test]
fn a_real_index_surfaces_derived_siblings_for_a_file_with_many_symbols() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_repo(root);

    let snapshot = Indexer::default()
        .index_repo(root, &OkConfig::default())
        .unwrap();
    let origin = snapshot
        .files
        .iter()
        .find(|file| file.path.ends_with("router.go"))
        .expect("the origin file is indexed");
    let symbol_count = snapshot
        .symbols
        .iter()
        .filter(|symbol| symbol.file_id == origin.id)
        .count();
    assert!(
        symbol_count >= 40,
        "fixture must exceed the neighbour window to be meaningful, got {symbol_count}"
    );

    let store = SqliteStore::open(root.join("index.sqlite")).unwrap();
    store
        .replace_index(IndexData {
            manifest: &snapshot.manifest,
            files: &snapshot.files,
            symbols: &snapshot.symbols,
            occurrences: &snapshot.occurrences,
            chunks: &snapshot.chunks,
            imports: &snapshot.imports,
            tests: &snapshot.tests,
            analysis_facts: &snapshot.analysis_facts,
            scopes: &snapshot.scopes,
            bindings: &snapshot.bindings,
            call_sites: &snapshot.call_sites,
        })
        .unwrap();
    let graph = InMemoryGraph::from_index_with_resolved_relationships(
        &snapshot.files,
        &snapshot.symbols,
        &snapshot.chunks,
        &snapshot.occurrences,
        &snapshot.imports,
        &snapshot.analysis_facts,
        &snapshot.resolved_relationships,
    );
    let nodes = graph.nodes.into_values().collect::<Vec<_>>();
    store.replace_graph(&nodes, &graph.edges).unwrap();

    // The pairing survived indexing and persistence.
    let derived = store
        .edges_by_type(GraphEdgeType::DerivedFrom, 100, 0)
        .unwrap();
    assert_eq!(derived.len(), 1, "{derived:?}");

    let report = ImpactEngine::new(&store)
        .with_graph_store(Some(&store))
        .for_file(Path::new("pkg/router.go"))
        .unwrap();
    let paired = report
        .possible_impact
        .iter()
        .find(|impact| impact.path.ends_with("router_test.go"))
        .unwrap_or_else(|| {
            panic!(
                "derived sibling missing from impact for a {symbol_count}-symbol file; proven={:?} possible={:?}",
                report.proven_impact, report.possible_impact
            )
        });
    // A naming-convention pairing has no proof and must stay a labeled possibility.
    assert_eq!(paired.authority, RelationshipAuthority::Heuristic);
    assert!(
        !report
            .proven_impact
            .iter()
            .any(|impact| impact.path.ends_with("router_test.go")),
        "{:?}",
        report.proven_impact
    );
}
