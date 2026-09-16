//! A Rust item import followed by a bare call, from a real repository through indexing, SQLite
//! and the graph into `ImpactEngine`.
//!
//! `crate::auth::issue_token(user)` and `use crate::auth; auth::issue_token(user)` were proven
//! impact while `use crate::auth::issue_token; issue_token(user)` reported nothing, because the
//! import's full path was looked up as a module and never split into module and item. Each
//! spelling of the item import is indexed here and must reach `proven_impact` with the same proof
//! set. The sources also name one path twice in a grouped import on one line, which shares the
//! stored import row's key and must still index.

use open_kioku_config::OkConfig;
use open_kioku_core::{GraphEdgeType, RelationshipAuthority, RelationshipProofKind};
use open_kioku_graph::InMemoryGraph;
use open_kioku_impact::ImpactEngine;
use open_kioku_ingest::{IndexSnapshot, Indexer};
use open_kioku_storage::{GraphStore, IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::Path;

const AUTH: &str = "pub struct Token(pub String);\n\npub fn issue_token(user: &str) -> Token {\n    Token(format!(\"tok-{user}\"))\n}\n";

const LIB: &str = "pub mod auth;\npub mod session;\n\npub mod inner {\n    pub struct Error;\n}\n\npub use inner::{Error, Error as InnerError};\n";

fn write_repo(root: &Path, import: &str, call: &str) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(root.join("src/lib.rs"), LIB).unwrap();
    std::fs::write(root.join("src/auth.rs"), AUTH).unwrap();
    std::fs::write(
        root.join("src/session.rs"),
        format!(
            "{import}\nuse std::io::{{Write, Write as _}};\n\npub fn open_session(user: &str) -> String {{\n    let token = {call}(user);\n    token.0\n}}\n"
        ),
    )
    .unwrap();
}

fn index_into_store(root: &Path) -> (SqliteStore, IndexSnapshot) {
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
        .expect("imports naming one path twice on one line must store");
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
    (store, snapshot)
}

#[test]
fn rust_item_import_call_is_proven_impact_of_the_imported_module() {
    let variants = [
        ("use crate::auth::issue_token;", "issue_token"),
        ("use crate::auth::{issue_token, Token};", "issue_token"),
        ("use crate::auth::issue_token as mint;", "mint"),
        (
            "use crate::auth::{issue_token, issue_token as mint};",
            "mint",
        ),
    ];

    for (import, call) in variants {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_repo(root, import, call);
        let (store, snapshot) = index_into_store(root);

        let report = ImpactEngine::new(&store)
            .with_graph_store(Some(&store))
            .for_file(Path::new("src/auth.rs"))
            .unwrap();
        let call_impact = report
            .proven_impact
            .iter()
            .find(|impact| {
                impact.edge_type == GraphEdgeType::Calls && impact.path.ends_with("session.rs")
            })
            .unwrap_or_else(|| {
                panic!(
                    "`{import}`: bare call missing from proven impact; proven={:?} possible={:?}",
                    report.proven_impact, report.possible_impact
                )
            });
        assert_eq!(
            call_impact.symbol.as_deref(),
            Some("src::session::open_session"),
            "`{import}`"
        );
        assert_eq!(
            call_impact.authority,
            RelationshipAuthority::Authoritative,
            "`{import}`"
        );
        for kind in [
            RelationshipProofKind::ExactCallSite,
            RelationshipProofKind::ImportBinding,
            RelationshipProofKind::QualifiedName,
        ] {
            assert!(
                call_impact.proof_kinds.contains(&kind),
                "`{import}` lacks {kind:?}: {:?}",
                call_impact.proof_kinds
            );
        }

        for (file, imported) in [
            ("src/session.rs", "std::io::Write"),
            ("src/lib.rs", "inner::Error"),
        ] {
            let rows = snapshot
                .imports
                .iter()
                .filter(|row| {
                    row.imported == imported
                        && snapshot.files.iter().any(|indexed| {
                            indexed.id == row.file_id && indexed.path.as_path() == Path::new(file)
                        })
                })
                .count();
            assert_eq!(rows, 1, "`{import}`: `{imported}` rows in {file}");
        }
    }
}

#[test]
fn associated_function_call_through_an_imported_type_is_proven_impact() {
    // The import binds the type; the call then resolves to the associated function in its impl.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, content) in [
        (
            "Cargo.toml",
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("src/lib.rs", "pub mod auth;\npub mod session;\n"),
        (
            "src/auth.rs",
            "pub struct Token(pub String);\n\nimpl Token {\n    pub fn parse(raw: &str) -> Token {\n        Token(raw.to_string())\n    }\n}\n",
        ),
        (
            "src/session.rs",
            "use crate::auth::Token;\n\npub fn open_session(raw: &str) -> String {\n    Token::parse(raw).0\n}\n",
        ),
    ] {
        let absolute = root.join(path);
        std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        std::fs::write(absolute, content).unwrap();
    }
    let (store, _) = index_into_store(root);

    let report = ImpactEngine::new(&store)
        .with_graph_store(Some(&store))
        .for_file(Path::new("src/auth.rs"))
        .unwrap();
    let call_impact = report
        .proven_impact
        .iter()
        .find(|impact| {
            impact.edge_type == GraphEdgeType::Calls
                && impact.path.ends_with("session.rs")
                && impact.source.ends_with("parse")
        })
        .unwrap_or_else(|| {
            panic!(
                "`Token::parse` missing from proven impact; proven={:?} possible={:?}",
                report.proven_impact, report.possible_impact
            )
        });
    assert_eq!(
        call_impact.symbol.as_deref(),
        Some("src::session::open_session")
    );
    assert_eq!(call_impact.authority, RelationshipAuthority::Authoritative);
}

#[test]
fn item_and_glob_imports_are_proven_impact_of_the_module_that_declares_them() {
    // `src/lib.rs` declares the modules and re-exports the item; `src/auth.rs` declares it. The
    // import edge belongs to the declaring file, and the crate root must not collect it.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, content) in [
        (
            "Cargo.toml",
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "src/lib.rs",
            "pub mod auth;\npub mod session;\n\npub use auth::issue_token;\n",
        ),
        ("src/auth.rs", AUTH),
        (
            "src/session.rs",
            "use crate::auth::issue_token;\nuse crate::auth::*;\n\npub fn open_session(user: &str) -> String {\n    issue_token(user).0\n}\n",
        ),
    ] {
        let absolute = root.join(path);
        std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        std::fs::write(absolute, content).unwrap();
    }
    let (store, _) = index_into_store(root);
    let impact_of = |path: &str| {
        ImpactEngine::new(&store)
            .with_graph_store(Some(&store))
            .for_file(Path::new(path))
            .unwrap()
    };

    let declaring = impact_of("src/auth.rs");
    let import_impact = declaring
        .proven_impact
        .iter()
        .find(|impact| {
            impact.edge_type == GraphEdgeType::Imports && impact.path.ends_with("session.rs")
        })
        .unwrap_or_else(|| {
            panic!(
                "the importing file is missing from the declaring module's proven impact: {:?}",
                declaring.proven_impact
            )
        });
    assert_eq!(
        import_impact.authority,
        RelationshipAuthority::Authoritative
    );
    assert!(import_impact
        .proof_kinds
        .contains(&RelationshipProofKind::ImportBinding));

    let crate_root = impact_of("src/lib.rs");
    assert!(
        !crate_root.proven_impact.iter().any(|impact| {
            impact.edge_type == GraphEdgeType::Imports && impact.path.ends_with("session.rs")
        }),
        "the crate root declares none of the imported names: {:?}",
        crate_root.proven_impact
    );
}
