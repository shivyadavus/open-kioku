//! A file's lexical direct impacts come from searching its own names. Test names must not take
//! those searches: nothing outside a file depends on its `#[test]` functions or the helpers in
//! its `#[cfg(test)] mod tests`, yet they are usually its longest names, and length used to be
//! the only ranking. Indexed for real, so the test targets the parser records have to line up
//! with the symbols the engine ranks.

use open_kioku_config::OkConfig;
use open_kioku_impact::ImpactEngine;
use open_kioku_ingest::Indexer;
use open_kioku_storage::{IndexData, MetadataStore};
use open_kioku_storage_sqlite::SqliteStore;
use std::path::{Path, PathBuf};

const LONG_TEST_NAMES: [&str; 8] = [
    "ordering_breaks_ties_by_path_when_scores_match",
    "ordering_keeps_exact_references_above_lexical_hits",
    "ordering_returns_nothing_for_an_empty_candidate_list",
    "ordering_is_stable_when_called_twice_on_the_same_input",
    "ordering_drops_candidates_below_the_confidence_floor",
    "ordering_orders_by_score_then_by_name_deterministically",
    "ordering_never_returns_more_than_the_requested_limit",
    "ordering_counts_omitted_candidates_beyond_the_limit",
];

fn write_repo(root: &Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    let mut target = String::from(
        "pub fn rank_candidates(scores: &[u32]) -> Vec<u32> {\n    let mut out = scores.to_vec();\n    out.sort();\n    out\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    fn build_candidate_fixture_with_many_duplicated_scores() -> Vec<u32> {\n        vec![3, 1, 2]\n    }\n",
    );
    for name in LONG_TEST_NAMES {
        target.push_str(&format!(
            "\n    #[test]\n    fn {name}() {{\n        assert_eq!(rank_candidates(&build_candidate_fixture_with_many_duplicated_scores()).len(), 3);\n    }}\n"
        ));
    }
    target.push_str("}\n");
    std::fs::write(root.join("src/ranker.rs"), target).unwrap();

    // Names the production function in a string, so it is found only by a lexical search for
    // that name and never through an exact reference.
    std::fs::write(
        root.join("src/dispatch.rs"),
        "pub const HANDLER: &str = \"rank_candidates\";\n",
    )
    .unwrap();
    // Names only test code: a hit here means a test name took a search slot.
    let mut labels = String::new();
    for (index, name) in LONG_TEST_NAMES.iter().enumerate() {
        labels.push_str(&format!("pub const LABEL_{index}: &str = \"{name}\";\n"));
    }
    labels.push_str(
        "pub const FIXTURE: &str = \"build_candidate_fixture_with_many_duplicated_scores\";\n",
    );
    std::fs::write(root.join("src/labels.rs"), labels).unwrap();
}

fn direct_impact_paths(root: &Path) -> Vec<PathBuf> {
    let snapshot = Indexer::default()
        .index_repo(root, &OkConfig::default())
        .unwrap();
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
    let ranker = store
        .get_file_by_path(Path::new("src/ranker.rs"))
        .unwrap()
        .expect("the target file is indexed");
    assert!(
        store.tests_for_files(&[ranker.id]).unwrap().len() >= LONG_TEST_NAMES.len(),
        "the fixture's #[test] functions must be indexed as test targets to be meaningful"
    );
    ImpactEngine::new(&store)
        .for_file(Path::new("src/ranker.rs"))
        .unwrap()
        .direct_impacts
        .into_iter()
        .map(|result| result.path)
        .collect()
}

#[test]
fn long_test_names_do_not_displace_the_production_name_from_impact_search() {
    let dir = tempfile::tempdir().unwrap();
    write_repo(dir.path());

    let direct = direct_impact_paths(dir.path());

    assert!(
        direct.contains(&PathBuf::from("src/dispatch.rs")),
        "the file naming the production function is a direct impact: {direct:?}"
    );
    assert!(
        !direct.contains(&PathBuf::from("src/labels.rs")),
        "a file naming only #[test] functions and cfg(test) helpers is not: {direct:?}"
    );
}
