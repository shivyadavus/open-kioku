//! A test file holds tests and the helpers, fixtures and lifecycle hooks around them. Only the
//! tests are validation: each fixture repository mixes a runnable test with helpers and hooks
//! its runner never executes as tests, and one holds nothing but those. The index keeps every
//! callable of a test file as test code, but only what the runner discovers may be planned or
//! satisfy validation availability.

use assert_cmd::Command;
use std::collections::BTreeMap;
use std::path::Path;

const TEST: &str = "test_file_symbol";
const HELPER: &str = "test_file_helper";

/// A copy of `fixture`, initialised and indexed. The fixture directories are shared with the
/// lifecycle tests, which index in place, so their runtime state is never copied.
fn indexed_copy(fixture: &str) -> tempfile::TempDir {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if name == ".ok" || name == "ok.toml" {
                continue;
            }
            let target = to.join(&name);
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let repo = tempfile::tempdir().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(fixture),
        repo.path(),
    );
    for args in [["init", "."], ["index", "."]] {
        Command::cargo_bin("ok")
            .unwrap()
            .current_dir(repo.path())
            .args(args)
            .assert()
            .success();
    }
    repo
}

/// Every persisted test target as `path::name`, with its origin.
fn target_origins(repo: &Path) -> BTreeMap<String, String> {
    let conn = rusqlite::Connection::open(repo.join(".ok/index.sqlite")).unwrap();
    let mut statement = conn
        .prepare("SELECT f.path, t.json FROM tests t JOIN files f ON f.id = t.file_id")
        .unwrap();
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap();
    rows.map(|row| {
        let (path, target) = row.unwrap();
        let target: serde_json::Value = serde_json::from_str(&target).unwrap();
        (
            format!("{path}::{}", target["name"].as_str().unwrap()),
            target["origin"].as_str().unwrap_or("symbol").to_string(),
        )
    })
    .collect()
}

fn expected(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(target, origin)| ((*target).to_string(), (*origin).to_string()))
        .collect()
}

fn ok_json(repo: &Path, args: &[&str]) -> serde_json::Value {
    let output = Command::cargo_bin("ok")
        .unwrap()
        .current_dir(repo)
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&output).unwrap_or_else(|err| {
        panic!(
            "{args:?} printed no JSON ({err}): {}",
            String::from_utf8_lossy(&output)
        )
    })
}

/// The names `ok plan` records as validation for `task`.
fn planned_validation(repo: &Path, task: &str) -> Vec<String> {
    let plan = ok_json(repo, &["plan", task, "--format", "json"]);
    plan["validation"]
        .as_array()
        .unwrap_or_else(|| panic!("plan has no validation array: {plan}"))
        .iter()
        .map(|test| test["name"].as_str().unwrap().to_string())
        .collect()
}

/// Indexes `fixture`, checks every persisted target's origin, and checks that the plan for
/// `task` records validation drawn only from runnable tests, `planned` among them.
fn assert_runner_discovery(fixture: &str, targets: &[(&str, &str)], task: &str, planned: &str) {
    let repo = indexed_copy(fixture);
    assert_eq!(target_origins(repo.path()), expected(targets), "{fixture}");
    let helpers = targets
        .iter()
        .filter(|(_, origin)| *origin == HELPER)
        .map(|(target, _)| target.rsplit("::").next().unwrap())
        .collect::<Vec<_>>();
    let validation = planned_validation(repo.path(), task);
    assert!(
        validation.iter().any(|name| name == planned),
        "{fixture}: `{planned}` not planned for `{task}`: {validation:?}"
    );
    for name in &validation {
        assert!(
            !helpers.contains(&name.as_str()),
            "{fixture}: helper `{name}` planned as validation: {validation:?}"
        );
    }
}

#[test]
fn rust_integration_tests_plan_attributed_functions_not_their_helpers() {
    assert_runner_discovery(
        "rust-tests-fixture",
        &[
            ("src/lib.rs::clamp_keeps_small_counts", "symbol"),
            ("src/lib.rs::clamp_bounds_each_case", "symbol"),
            ("tests/eviction.rs::entry_with_hits", HELPER),
            (
                "tests/eviction.rs::evict_coldest_drops_the_least_hit_entry",
                TEST,
            ),
            ("tests/common/mod.rs::seeded_entries", HELPER),
        ],
        "change evict_coldest in eviction to keep the newest entry",
        "evict_coldest_drops_the_least_hit_entry",
    );
}

#[test]
fn python_tests_plan_collected_functions_and_methods_not_fixtures_or_hooks() {
    assert_runner_discovery(
        "python-fixture",
        &[
            ("tests/test_pricing.py::rate", HELPER),
            ("tests/test_pricing.py::make_price", HELPER),
            ("tests/test_pricing.py::test_discount_by_rate", TEST),
            ("tests/test_pricing.py::setUp", HELPER),
            ("tests/test_pricing.py::tearDown", HELPER),
            ("tests/test_pricing.py::test_zero_rate_keeps_price", TEST),
            ("tests/test_pricing.py::assert_discounted", HELPER),
            ("tests/conftest.py::test_numbers", HELPER),
        ],
        "change discount in pricing to round half up",
        "test_zero_rate_keeps_price",
    );
}

#[test]
fn go_tests_plan_test_functions_not_test_main_or_helpers() {
    assert_runner_discovery(
        "go-fixture",
        &[
            ("ledger_test.go::TestMain", HELPER),
            ("ledger_test.go::LedgerFixture", HELPER),
            ("ledger_test.go::TestBalanceSumsEntries", TEST),
        ],
        "change balance to skip negative entries",
        "TestBalanceSumsEntries",
    );
}

#[test]
fn java_tests_plan_annotated_methods_not_lifecycle_or_helper_methods() {
    assert_runner_discovery(
        "java-fixture",
        &[
            ("src/test/java/PublisherTest.java::setUp", HELPER),
            (
                "src/test/java/PublisherTest.java::recordingTemplate",
                HELPER,
            ),
            (
                "src/test/java/PublisherTest.java::publishSendsTheCreatedEvent",
                TEST,
            ),
        ],
        "change publishCreated in Publisher to send the entry key",
        "publishSendsTheCreatedEvent",
    );
}

#[test]
fn typescript_tests_plan_registrations_not_declared_helpers() {
    assert_runner_discovery(
        "typescript-fixture",
        &[
            ("test/index.test.ts::makeName", HELPER),
            (
                "test/index.test.ts::addresses the caller by name",
                "registration_call",
            ),
        ],
        "change greet to trim the caller name",
        "addresses the caller by name",
    );
}

/// Test files, a `testutil` package, a `test-utils` directory and `conftest.py` that hold only
/// helpers, fixtures and lifecycle hooks: the index keeps them as test code, but the repository
/// has no validation, and every surface says so.
#[test]
fn a_repository_of_only_helpers_and_hooks_has_no_validation() {
    let repo = indexed_copy("test-helpers-only-fixture");
    let origins = target_origins(repo.path());
    assert_eq!(
        origins.keys().cloned().collect::<Vec<_>>(),
        expected(&[
            ("journal/journal_test.go::TestMain", HELPER),
            ("journal/journal_test.go::newJournal", HELPER),
            ("src/ledger.test.ts::makeEntry", HELPER),
            ("src/test/java/JournalFixtures.java::emptyJournal", HELPER),
            ("src/test/java/JournalTest.java::setUp", HELPER),
            ("test-utils/repo.ts::makeClient", HELPER),
            ("test-utils/repo.ts::withTempRepo", HELPER),
            ("tests/common/mod.rs::seeded_journal", HELPER),
            ("tests/conftest.py::test_journal", HELPER),
            ("tests/helpers.py::make_entry", HELPER),
            ("tests/helpers.py::test_data", HELPER),
            ("tests/test_journal.py::setUp", HELPER),
            ("tests/test_journal.py::tearDown", HELPER),
            ("testutil/server.go::NewServer", HELPER),
            ("testutil/server.go::TestServer", HELPER),
        ])
        .into_keys()
        .collect::<Vec<_>>()
    );
    assert!(
        origins.values().all(|origin| origin == HELPER),
        "{origins:?}"
    );

    let validation = planned_validation(repo.path(), "change postEntry to reject zero amounts");
    assert!(validation.is_empty(), "{validation:?}");

    let pack = ok_json(
        repo.path(),
        &["--json", "context", "add tests for postEntry zero amounts"],
    );
    let reason = "every indexed test target is a helper or lifecycle hook no test runner executes";
    let caveats = &pack["retrieval_diagnostics"]["caveats"];
    assert!(
        caveats
            .as_array()
            .unwrap()
            .iter()
            .any(|caveat| caveat == reason),
        "{caveats}"
    );
    let availability = pack["confidence_breakdown"]["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|component| component["signal"] == "validation_availability")
        .expect("validation_availability component");
    assert!(
        (availability["raw_value"].as_f64().unwrap() - 0.2).abs() < 1e-6,
        "{availability}"
    );

    // `ok status` counts no runnable test and names the helpers beside that count, so the
    // setup audit can say why validation is unavailable rather than advise indexing tests.
    let status = ok_json(repo.path(), &["--json", "status", "."]);
    assert_eq!(status["quality"]["test_count"], 0, "{status}");
    assert_eq!(
        status["quality"]["excluded_test_targets"],
        serde_json::json!({ "helper": origins.len() }),
        "{status}"
    );
    let audit = Command::cargo_bin("ok")
        .unwrap()
        .current_dir(repo.path())
        .args(["setup", "audit", "."])
        .output()
        .unwrap()
        .stdout;
    let audit = String::from_utf8_lossy(&audit);
    assert!(audit.contains(reason), "{audit}");
}
