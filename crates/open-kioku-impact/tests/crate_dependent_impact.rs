//! Impact across the crates of a Cargo workspace, from a real repository through indexing and
//! SQLite into `ImpactEngine`.
//!
//! The import resolver answers only paths of the importer's own crate, so `use engine::PlanEngine`
//! in a downstream crate reached neither an exact reference nor a relationship edge, and the
//! dependents of a crate's public API were reported only when a keyword search happened to
//! find them. Keyword search also found the same name in crates that cannot depend on the
//! changed one and in fixture trees no package compiles, and reported those as impacts.

use open_kioku_config::OkConfig;
use open_kioku_core::{ImpactReport, SearchResult};
use open_kioku_impact::ImpactEngine;
use open_kioku_ingest::Indexer;
use open_kioku_storage::{IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::{Path, PathBuf};

const FILES: [(&str, &str); 13] = [
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
    assert!(
        report
            .risk_report
            .reasons
            .iter()
            .any(|reason| reason.starts_with("1 file(s) in 1 package(s) import public items")),
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
    ] {
        assert!(
            !paths.contains(&PathBuf::from(unreachable)),
            "{unreachable} listed: {paths:?}"
        );
    }
    assert!(
        report.risk_report.reasons.iter().any(|reason| reason
            == "3 lexical match(es) left out: Rust files in no package, or in packages that do not depend on `plan-engine`"),
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
