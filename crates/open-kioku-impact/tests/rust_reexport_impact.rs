//! Rust calls through `use` re-exports, from a real repository through indexing, SQLite and the
//! graph into `ImpactEngine` (#476).
//!
//! `pub use auth::issue_token;` in the crate root made `use crate::issue_token; issue_token(user)`
//! and `crate::issue_token(user)` report no impact on `src/auth.rs`: the root only re-exports the
//! name, and neither the import binding nor the qualified call path followed the re-export. Both
//! must reach `proven_impact` with the proof set a direct item import gets, as must a call
//! through a glob re-export of a prelude. A name two globs bring in stays out of it.

use open_kioku_config::OkConfig;
use open_kioku_core::{GraphEdgeType, ImpactReport, RelationshipAuthority, RelationshipProofKind};
use open_kioku_graph::InMemoryGraph;
use open_kioku_impact::ImpactEngine;
use open_kioku_ingest::Indexer;
use open_kioku_storage::{GraphStore, IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::Path;

const FILES: [(&str, &str); 9] = [
    (
        "Cargo.toml",
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "src/lib.rs",
        "pub mod api;\npub mod auth;\npub mod facade;\npub mod ledger;\npub mod prelude;\npub mod session;\npub mod store;\n\npub use auth::issue_token;\n",
    ),
    (
        "src/auth.rs",
        "pub struct Token(pub String);\n\npub fn issue_token(user: &str) -> Token {\n    Token(user.to_string())\n}\n",
    ),
    ("src/prelude.rs", "pub use crate::auth::*;\n"),
    (
        "src/store.rs",
        "pub fn open(name: &str) -> String {\n    format!(\"store:{name}\")\n}\n",
    ),
    (
        "src/ledger.rs",
        "pub fn open(name: &str) -> String {\n    format!(\"ledger:{name}\")\n}\n",
    ),
    (
        "src/facade.rs",
        "pub use crate::ledger::*;\npub use crate::store::*;\n",
    ),
    (
        "src/api.rs",
        concat!(
            "use crate::issue_token;\n\n",
            "pub fn login(user: &str) -> String {\n    issue_token(user).0\n}\n\n",
            "pub fn login_qualified(user: &str) -> String {\n    crate::issue_token(user).0\n}\n\n",
            "pub fn login_prelude(user: &str) -> String {\n    crate::prelude::issue_token(user).0\n}\n",
        ),
    ),
    (
        "src/session.rs",
        "pub fn open_either(name: &str) -> String {\n    crate::facade::open(name)\n}\n",
    ),
];

fn index_into_store(root: &Path) -> SqliteStore {
    for (path, content) in FILES {
        let absolute = root.join(path);
        std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        std::fs::write(absolute, content).unwrap();
    }
    let mut config = OkConfig::default();
    config.scip.enabled = false;
    config.history.enabled = false;
    config.semantic.enabled = false;
    let snapshot = Indexer::default().index_repo(root, &config).unwrap();
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
    store
}

fn impact_of(store: &SqliteStore, file: &str) -> ImpactReport {
    ImpactEngine::new(store)
        .with_graph_store(Some(store))
        .for_file(Path::new(file))
        .unwrap()
}

#[test]
fn rust_calls_through_use_reexports_are_proven_impact_of_the_defining_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = index_into_store(dir.path());

    let report = impact_of(&store, "src/auth.rs");
    for caller in ["login", "login_qualified", "login_prelude"] {
        let symbol = format!("src::api::{caller}");
        let call_impact = report
            .proven_impact
            .iter()
            .find(|impact| {
                impact.edge_type == GraphEdgeType::Calls
                    && impact.symbol.as_deref() == Some(symbol.as_str())
            })
            .unwrap_or_else(|| {
                panic!(
                    "`{caller}` missing from proven impact; proven={:?} possible={:?}",
                    report.proven_impact, report.possible_impact
                )
            });
        assert_eq!(
            call_impact.authority,
            RelationshipAuthority::Authoritative,
            "`{caller}`"
        );
        assert!(
            call_impact
                .proof_kinds
                .contains(&RelationshipProofKind::ExactCallSite)
                && call_impact
                    .proof_kinds
                    .contains(&RelationshipProofKind::QualifiedName),
            "`{caller}`: {:?}",
            call_impact.proof_kinds
        );
    }
    let login = report
        .proven_impact
        .iter()
        .find(|impact| impact.symbol.as_deref() == Some("src::api::login"))
        .unwrap();
    assert!(
        login
            .proof_kinds
            .contains(&RelationshipProofKind::ImportBinding),
        "the bare call is proven through its import: {:?}",
        login.proof_kinds
    );

    // `facade` brings in an `open` from each of two globs, so the call proves neither.
    for file in ["src/store.rs", "src/ledger.rs"] {
        let report = impact_of(&store, file);
        assert!(
            !report.proven_impact.iter().any(|impact| {
                impact.edge_type == GraphEdgeType::Calls
                    && impact.symbol.as_deref() == Some("src::session::open_either")
            }),
            "{file}: {:?}",
            report.proven_impact
        );
    }
}
