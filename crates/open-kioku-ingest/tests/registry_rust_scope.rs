//! The symbol-registry pass matches a bare Rust name to an item of the same file only where
//! Rust module scoping lets the use site see that item (#526). Other languages keep the plain
//! same-file match.

use open_kioku_config::OkConfig;
use open_kioku_core::{AnalysisFact, GraphEdgeType};
use open_kioku_ingest::{IndexSnapshot, Indexer};

const PACKAGE: &str = "[package]\nname = \"bench\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

fn index(files: &[(&str, &str)]) -> IndexSnapshot {
    let dir = tempfile::tempdir().unwrap();
    for (path, content) in files {
        let absolute = dir.path().join(path);
        std::fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        std::fs::write(absolute, content).unwrap();
    }
    let mut config = OkConfig::default();
    config.scip.enabled = false;
    config.history.enabled = false;
    config.semantic.enabled = false;
    Indexer::default().index_repo(dir.path(), &config).unwrap()
}

fn index_rust(worker: &str) -> IndexSnapshot {
    index(&[
        ("Cargo.toml", PACKAGE),
        ("src/lib.rs", "pub mod worker;\n"),
        ("src/worker.rs", worker),
    ])
}

/// Registry `CALLS` facts from the symbol named `caller` to the symbol named `target`.
fn registry_calls<'s>(
    snapshot: &'s IndexSnapshot,
    caller: &str,
    target: &str,
) -> Vec<&'s AnalysisFact> {
    let symbol = |name: &str| {
        let matches = snapshot
            .symbols
            .iter()
            .filter(|symbol| symbol.name == name)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "expected one symbol named `{name}`");
        matches[0]
    };
    let caller = symbol(caller);
    let target = symbol(target);
    snapshot
        .analysis_facts
        .iter()
        .filter(|fact| {
            fact.source.starts_with("open-kioku-symbol-registry/")
                && fact.edge_type == GraphEdgeType::Calls
                && fact.symbol_id.as_ref() == Some(&caller.id)
                && fact.target == target.qualified_name
        })
        .collect()
}

#[test]
fn rust_mod_block_import_keeps_registry_off_the_parent_item() {
    let snapshot = index_rust(
        "pub fn now() {}\n\n#[cfg(test)]\nmod tests {\n    use mock_clock::now;\n\n    #[test]\n    fn t() {\n        now();\n    }\n}\n",
    );
    let facts = registry_calls(&snapshot, "t", "now");
    assert!(facts.is_empty(), "unexpected registry edge: {facts:?}");
}

#[test]
fn rust_sibling_mod_item_is_not_a_registry_candidate() {
    let snapshot = index_rust(
        "mod clock {\n    pub fn now() {}\n}\n\nmod tests {\n    fn t() {\n        now();\n    }\n}\n",
    );
    let facts = registry_calls(&snapshot, "t", "now");
    assert!(facts.is_empty(), "unexpected registry edge: {facts:?}");
}

#[test]
fn rust_super_glob_keeps_the_registry_same_file_edge() {
    let snapshot = index_rust(
        "pub fn now() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn t() {\n        now();\n    }\n}\n",
    );
    // The glob resolves to this file, so the registry's import match reaches `now` first.
    let facts = registry_calls(&snapshot, "t", "now");
    assert_eq!(facts.len(), 1, "{facts:?}");
}

#[test]
fn rust_import_of_this_file_does_not_reopen_an_out_of_scope_item() {
    // `use super::helper;` resolves to this file; its match must not offer the file's `now`,
    // which `use mock_clock::now;` keeps out of scope.
    let snapshot = index_rust(
        "pub fn now() {}
pub fn helper() {}

mod tests {
    use super::helper;
    use mock_clock::now;

    fn t() {
        helper();
        now();
    }
}
",
    );
    let facts = registry_calls(&snapshot, "t", "now");
    assert!(facts.is_empty(), "unexpected registry edge: {facts:?}");
    assert_eq!(registry_calls(&snapshot, "t", "helper").len(), 1);
}

#[test]
fn rust_explicit_import_beside_super_glob_keeps_registry_off_the_parent_item() {
    // An explicit import shadows the glob, so `now` in `t` is `mock_clock::now`; the glob still
    // names this file, and the registry's import match must not offer the file's `now` through it.
    let snapshot = index_rust(
        "pub fn now() {}\n\nmod tests {\n    use super::*;\n    use mock_clock::now;\n\n    fn t() {\n        now();\n    }\n}\n",
    );
    let facts = registry_calls(&snapshot, "t", "now");
    assert!(facts.is_empty(), "unexpected registry edge: {facts:?}");
}

#[test]
fn rust_same_module_call_keeps_the_registry_same_file_edge() {
    let snapshot = index_rust("pub fn now() {}\n\npub fn t() {\n    now();\n}\n");
    let facts = registry_calls(&snapshot, "t", "now");
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(
        facts[0].source.as_ref(),
        "open-kioku-symbol-registry/same-file"
    );
}

#[test]
fn rust_path_qualified_call_keeps_the_registry_same_file_edge() {
    // Module scoping governs bare names only; `super::now()` names the item by its path.
    let snapshot = index_rust(
        "pub fn now() {}\n\nmod tests {\n    use mock_clock::now;\n\n    fn t() {\n        super::now();\n    }\n}\n",
    );
    let facts = registry_calls(&snapshot, "t", "now");
    assert_eq!(facts.len(), 1, "{facts:?}");
}

#[test]
fn python_same_file_match_ignores_a_nested_import() {
    let snapshot = index(&[(
        "src/worker.py",
        "def now():\n    return 1\n\n\ndef t():\n    from mock_clock import now\n    now()\n",
    )]);
    let facts = registry_calls(&snapshot, "t", "now");
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(
        facts[0].source.as_ref(),
        "open-kioku-symbol-registry/same-file"
    );
}

#[test]
fn java_same_file_match_reaches_into_a_nested_class() {
    let snapshot = index(&[(
        "src/Worker.java",
        "class Worker {\n    static void now() {}\n\n    static class Tests {\n        void t() {\n            now();\n        }\n    }\n}\n",
    )]);
    let facts = registry_calls(&snapshot, "t", "now");
    assert_eq!(facts.len(), 1, "{facts:?}");
    assert_eq!(
        facts[0].source.as_ref(),
        "open-kioku-symbol-registry/same-file"
    );
}

#[test]
fn rust_out_of_scope_item_does_not_leave_a_unique_project_name() {
    // `binding` is not unique in the project; ruling out the test helper must not make
    // `other::binding` the unique-name answer for a call that names neither.
    let snapshot = index(&[
        ("Cargo.toml", PACKAGE),
        ("src/lib.rs", "pub mod other;\npub mod worker;\n"),
        ("src/other.rs", "pub fn binding() {}\n"),
        (
            "src/worker.rs",
            "pub fn caller() {\n    binding();\n}\n\n#[cfg(test)]\nmod tests {\n    fn binding() {}\n}\n",
        ),
    ]);
    let caller = snapshot
        .symbols
        .iter()
        .find(|symbol| symbol.name == "caller")
        .unwrap();
    let facts = snapshot
        .analysis_facts
        .iter()
        .filter(|fact| {
            fact.source.starts_with("open-kioku-symbol-registry/")
                && fact.symbol_id.as_ref() == Some(&caller.id)
                && fact.target.ends_with("binding")
        })
        .collect::<Vec<_>>();
    assert!(facts.is_empty(), "unexpected registry edge: {facts:?}");
}
