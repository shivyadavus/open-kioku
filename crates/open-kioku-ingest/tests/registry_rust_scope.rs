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

/// A call whose import the resolver's scoping rule cannot place keeps the registry's edge: the
/// rule fails closed for proof, but dropping a candidate on it would cost a valid call its only
/// caller edge.
fn assert_unplaced_import_keeps_edge(worker: &str, caller: &str, target: &str) {
    let snapshot = index_rust(worker);
    let facts = registry_calls(&snapshot, caller, target);
    assert_eq!(facts.len(), 1, "{facts:?}");
}

#[test]
fn rust_import_through_a_sibling_module_keeps_the_registry_edge() {
    assert_unplaced_import_keeps_edge(
        "#[cfg(test)]\nmod helpers {\n    pub fn make() {}\n}\n\n#[cfg(test)]\nmod tests {\n    use super::helpers::make;\n    fn t() {\n        make();\n    }\n}\n",
        "t",
        "make",
    );
}

#[test]
fn rust_self_path_import_keeps_the_registry_edge() {
    assert_unplaced_import_keeps_edge(
        "mod inner {\n    pub fn target_fn() {}\n}\nuse self::inner::target_fn;\n\npub fn caller() {\n    target_fn();\n}\n",
        "caller",
        "target_fn",
    );
}

#[test]
fn rust_self_path_glob_keeps_the_registry_edge() {
    assert_unplaced_import_keeps_edge(
        "mod inner {\n    pub fn target_fn() {}\n}\nuse self::inner::*;\n\npub fn caller() {\n    target_fn();\n}\n",
        "caller",
        "target_fn",
    );
}

#[test]
fn rust_unprefixed_module_reexport_keeps_the_registry_edge() {
    assert_unplaced_import_keeps_edge(
        "mod inner {\n    pub fn target_fn() {}\n}\npub use inner::target_fn;\n\npub fn caller() {\n    target_fn();\n}\n",
        "caller",
        "target_fn",
    );
}

#[test]
fn rust_crate_path_glob_of_this_file_keeps_the_registry_edge() {
    assert_unplaced_import_keeps_edge(
        "pub fn target_fn() {}\n\n#[cfg(test)]\nmod tests {\n    use crate::worker::*;\n    fn t() {\n        target_fn();\n    }\n}\n",
        "t",
        "target_fn",
    );
}

#[test]
fn rust_super_glob_inside_a_function_keeps_the_registry_edge() {
    assert_unplaced_import_keeps_edge(
        "pub fn target_fn() {}\n\n#[cfg(test)]\nmod tests {\n    fn t() {\n        use super::*;\n        target_fn();\n    }\n}\n",
        "t",
        "target_fn",
    );
}

#[test]
fn rust_crate_glob_in_the_crate_root_keeps_the_registry_edge() {
    let snapshot = index(&[
        ("Cargo.toml", PACKAGE),
        (
            "src/lib.rs",
            "pub fn target_fn() {}\n\n#[cfg(test)]\nmod tests {\n    use crate::*;\n    fn t() {\n        target_fn();\n    }\n}\n",
        ),
    ]);
    let facts = registry_calls(&snapshot, "t", "target_fn");
    assert_eq!(facts.len(), 1, "{facts:?}");
}

#[test]
fn rust_member_call_does_not_take_the_ruled_out_items_place_on_its_line() {
    // `path` in `let path = dir.path()` is a local; the ruled-out same-file `path` must not
    // come back as the target of `dir.path()` on the same line.
    let snapshot = index_rust(
        "pub struct Store;\n\nimpl Store {\n    pub fn path(&self) {}\n}\n\n#[cfg(test)]\nmod tests {\n    use mock_fs::path;\n\n    fn t(dir: Dir) {\n        let path = dir.path();\n    }\n}\n",
    );
    let facts = registry_calls(&snapshot, "t", "path");
    assert!(facts.is_empty(), "unexpected registry edge: {facts:?}");
}

#[test]
fn rust_ruling_out_some_same_file_items_does_not_pick_the_rest() {
    // Two same-file `helper`s: the `mod tests` one is out of reach from `caller`, but that does
    // not show that `caller` means the other; the name stays ambiguous as before.
    let snapshot = index_rust(
        "pub struct Store;\n\nimpl Store {\n    pub fn helper(&self) {}\n}\n\npub fn caller(dir: Dir) {\n    let helper = dir.helper();\n}\n\n#[cfg(test)]\nmod tests {\n    fn helper() {}\n}\n",
    );
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
                && fact.target.ends_with("helper")
        })
        .collect::<Vec<_>>();
    assert!(facts.is_empty(), "unexpected registry edge: {facts:?}");
}

#[test]
fn rust_module_declaration_names_keep_their_registry_references() {
    // A `mod` item's scope covers its own name; `m2` in `pub mod m2;` is not a use inside `m2`.
    let with = index(&[
        ("Cargo.toml", PACKAGE),
        ("src/lib.rs", "pub mod m1; pub mod m2;\n"),
        ("src/m1.rs", "pub fn one() {}\n"),
        ("src/m2.rs", "pub fn two() {}\n"),
    ]);
    let m1 = with
        .symbols
        .iter()
        .find(|symbol| symbol.name == "m1")
        .unwrap();
    let m2 = with
        .symbols
        .iter()
        .find(|symbol| symbol.name == "m2")
        .unwrap();
    assert!(with.analysis_facts.iter().any(|fact| {
        fact.source.starts_with("open-kioku-symbol-registry/")
            && fact.symbol_id.as_ref() == Some(&m1.id)
            && fact.target == m2.qualified_name
    }));
}

#[test]
fn rust_super_import_of_a_parent_import_keeps_the_registry_edge() {
    // `super::target_fn` binds to the parent's `use inner::target_fn;`, the item in `inner`.
    assert_unplaced_import_keeps_edge(
        "mod inner {\n    pub fn target_fn() {}\n}\nuse inner::target_fn;\n\n#[cfg(test)]\nmod tests {\n    use super::target_fn;\n    fn t() {\n        target_fn();\n    }\n}\n",
        "t",
        "target_fn",
    );
}

#[test]
fn rust_super_import_of_a_parent_glob_keeps_the_registry_edge() {
    assert_unplaced_import_keeps_edge(
        "mod inner {\n    pub fn target_fn() {}\n}\npub use inner::*;\n\n#[cfg(test)]\nmod tests {\n    use super::target_fn;\n    fn t() {\n        target_fn();\n    }\n}\n",
        "t",
        "target_fn",
    );
}

#[test]
fn rust_grouped_super_import_of_a_parent_import_keeps_the_registry_edge() {
    assert_unplaced_import_keeps_edge(
        "mod inner {\n    pub fn target_fn() {}\n}\nuse inner::target_fn;\n\nmod tests {\n    use super::{self as parent, target_fn};\n    fn t() {\n        target_fn();\n    }\n}\n",
        "t",
        "target_fn",
    );
}
