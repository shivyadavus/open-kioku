//! Impact across the crates of a Cargo workspace, from a real repository through indexing and
//! SQLite into `ImpactEngine`.
//!
//! The import resolver used to answer only paths of the importer's own crate, so
//! `use engine::PlanEngine` in a downstream crate reached neither an exact reference nor a
//! relationship edge, and the dependents of a crate's public API were reported only when a keyword
//! search happened to find them. Keyword search also found the same name in crates that cannot
//! depend on the changed one and in fixture trees no package compiles, and reported those as
//! impacts. Impact now reads the package model and import resolutions indexing stored, keyed by
//! manifest path, so a package of the same name in another workspace is another package.

use open_kioku_config::OkConfig;
use open_kioku_core::{GraphEdgeType, ImpactReport, RelationshipAuthority, SearchResult};
use open_kioku_graph::InMemoryGraph;
use open_kioku_impact::ImpactEngine;
use open_kioku_ingest::Indexer;
use open_kioku_storage::{GraphStore, IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::{Path, PathBuf};

const FILES: [(&str, &str); 28] = [
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
    ),
    (
        "crates/engine/Cargo.toml",
        "[package]\nname = \"plan-engine\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "crates/engine/src/lib.rs",
        "pub mod builder;\npub use builder::Builder;\n\npub struct PlanEngine {\n    pub limit: usize,\n}\n\nimpl PlanEngine {\n    pub fn new(limit: usize) -> Self {\n        PlanEngine { limit }\n    }\n}\n",
    ),
    (
        "crates/engine/src/builder.rs",
        "pub struct Builder {\n    pub steps: usize,\n}\n",
    ),
    (
        "crates/app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nplan-engine = { path = \"../engine\" }\n",
    ),
    (
        "crates/app/src/lib.rs",
        "use plan_engine::{Builder, PlanEngine};\n\ninclude!(\"commands.rs\");\n\npub fn steps(builder: &Builder) -> usize {\n    builder.steps\n}\n",
    ),
    // Included into the crate root: it names the imported type without importing it.
    (
        "crates/app/src/commands.rs",
        "pub fn run_plan() -> usize {\n    PlanEngine::new(3).limit\n}\n",
    ),
    (
        "crates/app/src/unrelated.rs",
        "pub fn unrelated() -> usize {\n    7\n}\n",
    ),
    // Imports another crate's `PlanEngine`: its use is that item, not the engine's.
    (
        "crates/app/src/shadow.rs",
        "use other::PlanEngine;\n\npub fn shadow() -> PlanEngine {\n    PlanEngine\n}\n",
    ),
    // Names the word only in a string, a comment and as a method: no use of the type.
    (
        "crates/app/src/mentions.rs",
        "// PlanEngine is built elsewhere.\npub fn mentions(value: &Wrapper) -> &str {\n    value.PlanEngine();\n    \"PlanEngine\"\n}\n",
    ),
    // Depends on the engine under another name and imports it by that name.
    (
        "crates/aliased/Cargo.toml",
        "[package]\nname = \"aliased\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nengine = { package = \"plan-engine\", path = \"../engine\" }\n",
    ),
    (
        "crates/aliased/src/lib.rs",
        "use engine::PlanEngine;\n\npub fn aliased() -> usize {\n    PlanEngine::new(4).limit\n}\n",
    ),
    // Another crate's item of the same name, reached by its path: not the engine's.
    (
        "crates/app/src/settings.rs",
        "pub fn settings() -> usize {\n    other_cfg::PlanEngine::default().limit\n}\n",
    ),
    // The name only inside a multi-line string.
    (
        "crates/app/src/help.rs",
        "pub fn help() -> &'static str {\n    \"usage:\n    PlanEngine::new builds one\n\"\n}\n",
    ),
    // A `'\"'` character literal before a real use.
    (
        "crates/app/src/quoted.rs",
        "pub fn quoted() -> usize {\n    let _quote = '\"'; PlanEngine::new(5).limit\n}\n",
    ),
    // A binary target of the engine package: nothing can import it.
    (
        "crates/engine/src/bin/tool.rs",
        "pub fn tool() -> usize {\n    plan_engine::PlanEngine::new(6).limit\n}\n",
    ),
    // A derive crate: upstream of its users in Cargo, yet the code it emits names the engine.
    (
        "crates/engine_derive/Cargo.toml",
        "[package]\nname = \"engine-derive\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n",
    ),
    (
        "crates/engine_derive/src/lib.rs",
        "pub fn expand() -> String {\n    let tokens = quote! { ::plan_engine::PlanEngine::new(1) };\n    tokens.to_string()\n}\n",
    ),
    // A package that does not depend on the engine, naming the same word.
    (
        "crates/other/Cargo.toml",
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "crates/other/src/lib.rs",
        "pub struct PlanEngine;\n\npub fn describe() -> PlanEngine {\n    PlanEngine\n}\n",
    ),
    // Writes the import but never declares the dependency: it cannot compile against the engine.
    (
        "crates/stray/Cargo.toml",
        "[package]\nname = \"stray\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "crates/stray/src/lib.rs",
        "use plan_engine::PlanEngine;\n\npub fn stray() -> usize {\n    PlanEngine::new(1).limit\n}\n",
    ),
    // Under the virtual workspace root but in no member: no package compiles it.
    (
        "fixtures/demo/src/lib.rs",
        "pub fn demo() -> usize {\n    PlanEngine::new(2).limit\n}\n",
    ),
    // A second workspace with its own `plan-engine`, and a crate depending on that one: the
    // same package name, another package.
    (
        "vendored/Cargo.toml",
        "[workspace]\nmembers = [\"engine\", \"consumer\"]\n",
    ),
    (
        "vendored/engine/Cargo.toml",
        "[package]\nname = \"plan-engine\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
    ),
    (
        "vendored/engine/src/lib.rs",
        "pub struct PlanEngine;\n\nimpl PlanEngine {\n    pub fn new(_limit: usize) -> Self {\n        PlanEngine\n    }\n}\n",
    ),
    (
        "vendored/consumer/Cargo.toml",
        "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nplan-engine = { path = \"../engine\" }\n",
    ),
    (
        "vendored/consumer/src/lib.rs",
        "use plan_engine::PlanEngine;\n\npub fn consume() -> PlanEngine {\n    PlanEngine::new(8)\n}\n",
    ),
];

fn indexed_workspace(root: &Path) -> SqliteStore {
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

fn has_signal(result: &SearchResult, signal: &str) -> bool {
    result
        .score_breakdown
        .iter()
        .any(|component| component.signal == signal)
}

fn listed_paths(report: &ImpactReport) -> Vec<PathBuf> {
    report
        .direct_impacts
        .iter()
        .chain(report.indirect_impacts.iter())
        .map(|result| result.path.clone())
        .collect()
}

#[test]
fn a_downstream_crate_that_imports_a_public_item_is_a_direct_impact() {
    let dir = tempfile::tempdir().unwrap();
    let store = indexed_workspace(dir.path());

    let report = ImpactEngine::new(&store)
        .for_file(Path::new("crates/engine/src/lib.rs"))
        .unwrap();

    let importer = report
        .direct_impacts
        .iter()
        .position(|result| {
            result.path == Path::new("crates/app/src/lib.rs") && has_signal(result, "crate_import")
        })
        .unwrap_or_else(|| panic!("importer missing: {:?}", report.direct_impacts));
    let included = report
        .direct_impacts
        .iter()
        .position(|result| {
            result.path == Path::new("crates/app/src/commands.rs")
                && has_signal(result, "crate_import_use")
        })
        .unwrap_or_else(|| panic!("use site missing: {:?}", report.direct_impacts));
    // Both outrank every keyword match.
    let first_heuristic = report
        .direct_impacts
        .iter()
        .position(|result| {
            !has_signal(result, "crate_import") && !has_signal(result, "crate_import_use")
        })
        .unwrap_or(report.direct_impacts.len());
    assert!(importer < first_heuristic && included < first_heuristic);
    assert!(importer < included);

    let paths = listed_paths(&report);
    assert!(!paths.contains(&PathBuf::from("crates/app/src/unrelated.rs")));
    for not_a_use in [
        "crates/app/src/shadow.rs",
        "crates/app/src/mentions.rs",
        "crates/app/src/settings.rs",
        "crates/app/src/help.rs",
    ] {
        assert!(
            !report
                .direct_impacts
                .iter()
                .any(|result| result.path == Path::new(not_a_use)
                    && has_signal(result, "crate_import_use")),
            "{not_a_use}: {:?}",
            report.direct_impacts
        );
    }
    assert!(
        report.direct_impacts.iter().any(|result| result.path
            == Path::new("crates/app/src/quoted.rs")
            && has_signal(result, "crate_import_use")),
        "a use after a quote character literal is still a use: {:?}",
        report.direct_impacts
    );
    assert!(
        report
            .risk_report
            .reasons
            .iter()
            .any(|reason| reason.starts_with("2 file(s) in 2 package(s) import public items")),
        "{:?}",
        report.risk_report.reasons
    );
}

#[test]
fn an_import_without_a_declared_dependency_is_not_a_crate_import() {
    let dir = tempfile::tempdir().unwrap();
    let store = indexed_workspace(dir.path());

    let report = ImpactEngine::new(&store)
        .for_file(Path::new("crates/engine/src/lib.rs"))
        .unwrap();

    assert!(!report.direct_impacts.iter().any(|result| {
        result.path == Path::new("crates/stray/src/lib.rs") && has_signal(result, "crate_import")
    }));
}

#[test]
fn lexical_matches_no_dependency_path_reaches_are_left_out_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let store = indexed_workspace(dir.path());

    let report = ImpactEngine::new(&store)
        .for_file(Path::new("crates/engine/src/lib.rs"))
        .unwrap();

    let paths = listed_paths(&report);
    for unreachable in [
        "crates/other/src/lib.rs",
        "crates/stray/src/lib.rs",
        "fixtures/demo/src/lib.rs",
        "vendored/engine/src/lib.rs",
        "vendored/consumer/src/lib.rs",
    ] {
        assert!(
            !paths.contains(&PathBuf::from(unreachable)),
            "{unreachable} listed: {paths:?}"
        );
    }
    assert!(
        report.risk_report.reasons.iter().any(|reason| reason
            .starts_with("5 lexical match(es) left out: Rust files in no workspace package, or in packages with no Cargo dependency path to `plan_engine` (`crates/engine`) (e.g. ")),
        "{:?}",
        report.risk_report.reasons
    );
}

#[test]
fn an_item_the_crate_root_reexports_reaches_its_defining_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = indexed_workspace(dir.path());

    let report = ImpactEngine::new(&store)
        .for_file(Path::new("crates/engine/src/builder.rs"))
        .unwrap();

    let importer = report
        .direct_impacts
        .iter()
        .find(|result| {
            result.path == Path::new("crates/app/src/lib.rs") && has_signal(result, "crate_import")
        })
        .unwrap_or_else(|| panic!("re-exported import missing: {:?}", report.direct_impacts));
    assert!(importer
        .evidence
        .iter()
        .any(|line| line.starts_with("imports `plan_engine::Builder`")));
    // The included file names only `PlanEngine`, which `builder.rs` does not define.
    assert!(!report.direct_impacts.iter().any(|result| {
        result.path == Path::new("crates/app/src/commands.rs")
            && has_signal(result, "crate_import_use")
    }));
}

#[test]
fn a_renamed_dependency_is_followed_under_the_name_its_package_uses() {
    let dir = tempfile::tempdir().unwrap();
    let store = indexed_workspace(dir.path());

    let report = ImpactEngine::new(&store)
        .for_file(Path::new("crates/engine/src/lib.rs"))
        .unwrap();

    assert!(
        report.direct_impacts.iter().any(|result| {
            result.path == Path::new("crates/aliased/src/lib.rs")
                && has_signal(result, "crate_import")
        }),
        "{:?}",
        report.direct_impacts
    );
}

#[test]
fn a_proc_macro_crate_that_names_the_changed_crate_is_never_pruned() {
    let dir = tempfile::tempdir().unwrap();
    let store = indexed_workspace(dir.path());

    let report = ImpactEngine::new(&store)
        .for_file(Path::new("crates/engine/src/lib.rs"))
        .unwrap();

    assert!(
        listed_paths(&report).contains(&PathBuf::from("crates/engine_derive/src/lib.rs")),
        "{:?} / {:?}",
        report.direct_impacts,
        report.risk_report.reasons
    );
}

#[test]
fn a_file_crate_import_analysis_cannot_cover_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let store = indexed_workspace(dir.path());

    for (path, cause) in [
        (
            "fixtures/demo/src/lib.rs",
            "downstream crates were not measured: the file is under a Cargo workspace manifest but in none of its members",
        ),
        (
            "crates/engine/src/bin/tool.rs",
            "downstream crates were not measured: the file is not a module of an indexed library crate of `plan_engine` (`crates/engine`)",
        ),
    ] {
        let report = ImpactEngine::new(&store).for_file(Path::new(path)).unwrap();
        assert!(
            report
                .risk_report
                .reasons
                .iter()
                .any(|reason| reason.starts_with(cause)),
            "{path}: {:?}",
            report.risk_report.reasons
        );
    }

    let measured = ImpactEngine::new(&store)
        .for_file(Path::new("crates/engine/src/lib.rs"))
        .unwrap();
    assert!(!measured
        .risk_report
        .reasons
        .iter()
        .any(|reason| reason.starts_with("downstream crates were not measured")));
}

#[test]
fn a_package_of_the_same_name_in_another_workspace_is_not_a_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let store = indexed_workspace(dir.path());

    // `vendored/consumer` depends on `vendored/engine`, which is also named `plan-engine`.
    let report = ImpactEngine::new(&store)
        .for_file(Path::new("crates/engine/src/lib.rs"))
        .unwrap();
    assert!(
        !listed_paths(&report).contains(&PathBuf::from("vendored/consumer/src/lib.rs")),
        "{:?}",
        report.direct_impacts
    );
    let vendored = ImpactEngine::new(&store)
        .for_file(Path::new("vendored/engine/src/lib.rs"))
        .unwrap();
    let importers = vendored
        .direct_impacts
        .iter()
        .filter(|result| has_signal(result, "crate_import"))
        .map(|result| result.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        importers,
        vec![PathBuf::from("vendored/consumer/src/lib.rs")],
        "{:?}",
        vendored.direct_impacts
    );
}

#[test]
fn a_downstream_call_through_an_imported_item_is_proven_impact() {
    let dir = tempfile::tempdir().unwrap();
    let store = indexed_workspace(dir.path());

    let report = ImpactEngine::new(&store)
        .with_graph_store(Some(&store))
        .for_file(Path::new("crates/engine/src/lib.rs"))
        .unwrap();
    // `PlanEngine::new(4)` in `aliased` resolves through `use engine::PlanEngine;` and the
    // renamed dependency to the engine's `new`; the same call in the other workspace's consumer,
    // and in the package that never declares the dependency, does not.
    let calls = report
        .proven_impact
        .iter()
        .filter(|impact| impact.edge_type == GraphEdgeType::Calls)
        .map(|impact| {
            assert_eq!(impact.authority, RelationshipAuthority::Authoritative);
            (impact.path.clone(), impact.symbol.clone())
        })
        .collect::<Vec<_>>();
    assert!(
        calls.contains(&(
            PathBuf::from("crates/aliased/src/lib.rs"),
            Some("crates::aliased::src::lib::aliased".into())
        )),
        "{calls:?} / {:?}",
        report.proven_impact
    );
    for elsewhere in ["vendored/consumer/src/lib.rs", "crates/stray/src/lib.rs"] {
        assert!(
            !report
                .proven_impact
                .iter()
                .chain(&report.possible_impact)
                .any(|impact| impact.path == Path::new(elsewhere)),
            "{elsewhere}: {:?}",
            report.proven_impact
        );
    }
}
