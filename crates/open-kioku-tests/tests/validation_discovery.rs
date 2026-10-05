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

    // The plan plans none of them, but says it withheld the test-file callable beside the
    // change rather than claim no test exists: a configured runner may collect it.
    let plan = ok_json(
        repo.path(),
        &[
            "plan",
            "change postEntry to reject zero amounts",
            "--format",
            "json",
        ],
    );
    assert_eq!(plan["validation"], serde_json::json!([]), "{plan}");
    let withheld = "1 test-file callable(s) near this change matched no default runner discovery rule (runner configuration is not read)";
    assert!(
        plan["risk"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason == withheld),
        "{}",
        plan["risk"]["reasons"]
    );
    let steps = plan["recommended_next_steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .any(|step| step.as_str().unwrap().starts_with(&format!(
                "No runnable indexed test was found, but {withheld}"
            ))),
        "{steps:?}"
    );
    assert!(
        !steps.iter().any(|step| step
            .as_str()
            .unwrap()
            .starts_with("No indexed tests were found")),
        "{steps:?}"
    );

    let pack = ok_json(
        repo.path(),
        &["--json", "context", "add tests for postEntry zero amounts"],
    );
    // What the index checked, not a claim about what a runner will do.
    let reason = "every indexed test target is a test-file callable matching no default runner discovery rule (runner configuration is not read)";
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

/// A JUnit 3 test reaches `TestCase` through an abstract base in another file. Main planned
/// `testPostsLegacy`; a rule that required `TestCase` on the class itself withheld it, and the
/// plan then said no tests existed.
#[test]
fn a_junit3_test_inheriting_test_case_through_an_abstract_base_is_planned() {
    assert_runner_discovery(
        "java-junit3-fixture",
        &[
            (
                "src/test/java/com/acme/AbstractLedgerTest.java::ledger",
                HELPER,
            ),
            (
                "src/test/java/com/acme/JournalTest.java::replaysEntries",
                TEST,
            ),
            (
                "src/test/java/com/acme/LedgerLegacyTest.java::testPostsLegacy",
                TEST,
            ),
        ],
        "change Ledger post to reject negative amounts",
        "testPostsLegacy",
    );
}

/// The plan for `task` plans nothing, discloses the withheld callables, and never claims that
/// no tests exist. The fixture may hold runnable targets too (the wrapper fixture's interpolated
/// `it(..)` inside a helper) that the selector does not link to the change.
fn assert_withheld_disclosed(fixture: &str, task: &str) {
    let repo = indexed_copy(fixture);
    let origins = target_origins(repo.path());
    assert!(
        origins.values().any(|origin| origin == HELPER),
        "{fixture}: {origins:?}"
    );
    let plan = ok_json(repo.path(), &["plan", task, "--format", "json"]);
    assert_eq!(plan["validation"], serde_json::json!([]), "{fixture}");
    let disclosed = |text: &str| text.contains("matched no default runner discovery rule");
    assert!(
        plan["risk"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| disclosed(reason.as_str().unwrap())),
        "{fixture}: {}",
        plan["risk"]["reasons"]
    );
    let steps = plan["recommended_next_steps"].as_array().unwrap();
    assert!(
        steps.iter().any(|step| {
            let step = step.as_str().unwrap();
            step.starts_with("No runnable indexed test was found, but") && disclosed(step)
        }),
        "{fixture}: {steps:?}"
    );
    assert!(
        !steps.iter().any(|step| step
            .as_str()
            .unwrap()
            .contains("No indexed tests were found")),
        "{fixture}: {steps:?}"
    );
}

/// A `harness = false` test target runs `main` under a custom harness such as libtest-mimic,
/// which no default rule recognises. The test selector links `tests/mimic.rs` to no changed
/// file, so only the index-wide count can disclose it.
#[test]
fn a_custom_harness_test_reads_as_withheld_not_absent() {
    assert_withheld_disclosed(
        "rust-custom-harness-fixture",
        "change post_entry to reject zero amounts",
    );
}

/// Tests registered through wrapper functions (`itPosts(1, 1)`) are calls of a helper, which no
/// default rule recognises; the plan must say they were withheld.
#[test]
fn wrapper_registered_typescript_tests_read_as_withheld_not_absent() {
    assert_withheld_disclosed(
        "typescript-wrapper-fixture",
        "change postEntry to reject zero amounts",
    );
}

/// `pytest.ini` tells pytest to collect `check_*.py` and `*_tests.py`, `*Suite` classes and
/// `should_*` functions. The index does not interpret those options, so it falls back to the
/// test-path rule there and says so, instead of calling pytest's tests helpers.
#[test]
fn a_pytest_configuration_that_changes_discovery_keeps_its_tests_and_says_why() {
    let repo = indexed_copy("pytest-config-fixture");
    assert_eq!(
        target_origins(repo.path()),
        expected(&[
            ("tests/check_balance.py::should_sum_balance", TEST),
            ("tests/ledger_tests.py::should_post_entry", TEST),
            ("tests/ledger_tests.py::should_balance", TEST),
        ])
    );
    let validation = planned_validation(repo.path(), "change post_entry to reject zero amounts");
    assert!(
        validation.iter().any(|name| name == "should_post_entry"),
        "{validation:?}"
    );

    let pack = ok_json(
        repo.path(),
        &["--json", "context", "add tests for post_entry zero amounts"],
    );
    let caveats = pack["retrieval_diagnostics"]["caveats"].as_array().unwrap();
    assert!(
        !caveats.iter().any(|caveat| caveat
            .as_str()
            .unwrap()
            .starts_with("every indexed test target")),
        "{caveats:?}"
    );

    let status = ok_json(repo.path(), &["--json", "status", "."]);
    assert_eq!(status["quality"]["test_count"], 3, "{status}");
    let notes = status["quality"]["quality_notes"].to_string();
    assert!(
        notes.contains(
            "pytest configuration `pytest.ini` sets python_files, python_classes, python_functions"
        ),
        "{notes}"
    );
}

/// xUnit, NUnit and MSTest projects beside a library: each runner's attributed methods are tests
/// and its lifecycle methods, constructors, fixtures and helpers are not. `BalanceChecks.cs` is
/// on no test path; its `.csproj` references NUnit and the test SDK, which makes it a test.
#[test]
fn csharp_tests_plan_runner_attributed_methods_with_project_scoped_commands() {
    let xunit = "src/Acme.Ledger.Tests/PostingTests.cs";
    let fixture = "src/Acme.Ledger.Tests/LedgerFixture.cs";
    let nunit = "tests/Acme.Ledger.NUnit/PostingCases.cs";
    let mstest = "tests/Acme.Ledger.MSTest/VoidingSuite.cs";
    let checks = "checks/Acme.Ledger.Checks/BalanceChecks.cs";
    let target = |path: &str, name: &str| format!("{path}::{name}");
    let entries = [
        (target(fixture, "InitializeAsync"), HELPER),
        (target(fixture, "DisposeAsync"), HELPER),
        (target(xunit, "PostingTests"), HELPER),
        (target(xunit, "Dispose"), HELPER),
        (target(xunit, "PostCreditRaisesTheBalance"), TEST),
        (target(xunit, "PostDebitLowersTheBalance"), TEST),
        (target(xunit, "PostKeepsCreditsAndDebitsApart"), TEST),
        (target(xunit, "PostBoth"), HELPER),
        (target(xunit, "VoidRestoresTheBalance"), TEST),
        (target(nunit, "OpenLedger"), HELPER),
        (target(nunit, "Reset"), HELPER),
        (target(nunit, "PostCreditAddsEachCase"), TEST),
        (target(nunit, "PostDebitBelowZeroIsAllowed"), TEST),
        (target(nunit, "Clean"), HELPER),
        (target(mstest, "OpenLedger"), HELPER),
        (target(mstest, "Reset"), HELPER),
        (target(mstest, "VoidCreditReversesIt"), TEST),
        (target(mstest, "VoidDebitReversesIt"), TEST),
        (target(mstest, "Clean"), HELPER),
        (target(checks, "BalanceStartsAtZero"), TEST),
    ];
    let targets = entries
        .iter()
        .map(|(target, origin)| (target.as_str(), *origin))
        .collect::<Vec<_>>();
    assert_runner_discovery(
        "csharp-tests-fixture",
        &targets,
        "change Post in Posting to reject zero amounts",
        "PostCreditRaisesTheBalance",
    );

    let repo = indexed_copy("csharp-tests-fixture");
    // `ok tests` names the tests linked to the changed file, each filtered to itself in its own
    // project, the nested class joined with `+` as the runners report it.
    let selected = ok_json(
        repo.path(),
        &["--json", "tests", "--changed", "src/Acme.Ledger/Posting.cs"],
    );
    let commands = selected["tests"]
        .as_array()
        .unwrap_or_else(|| panic!("ok tests lists tests: {selected}"))
        .iter()
        .map(|test| {
            (
                test["name"].as_str().unwrap().to_string(),
                test["command"].as_str().unwrap().to_string(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (name, command) in [
        (
            "VoidRestoresTheBalance",
            "dotnet test src/Acme.Ledger.Tests/Acme.Ledger.Tests.csproj --filter \"FullyQualifiedName~Acme.Ledger.Tests.PostingTests+WhenVoided.VoidRestoresTheBalance\"",
        ),
        (
            "PostCreditAddsEachCase",
            "dotnet test tests/Acme.Ledger.NUnit/Acme.Ledger.NUnit.csproj --filter \"FullyQualifiedName~Acme.Ledger.NUnit.PostingCases.PostCreditAddsEachCase\"",
        ),
    ] {
        assert_eq!(
            commands.get(name).map(String::as_str),
            Some(command),
            "{commands:?}"
        );
    }
    assert!(!commands.contains_key("PostBoth"), "{commands:?}");

    // `ok verify` against the saved plan names the C# tests it still recommends.
    let plan = ok_json(
        repo.path(),
        &[
            "plan",
            "change Post in Posting to reject zero amounts",
            "--format",
            "json",
        ],
    );
    let plan_path = repo.path().join("plan.json");
    std::fs::write(&plan_path, plan.to_string()).unwrap();
    let verified = ok_json(
        repo.path(),
        &[
            "verify",
            "--plan",
            plan_path.to_str().unwrap(),
            "--changed",
            "src/Acme.Ledger/Posting.cs",
            "--format",
            "json",
        ],
    );
    let recommended = verified["recommended_tests"]
        .as_array()
        .unwrap_or_else(|| panic!("verify recommends tests: {verified}"))
        .iter()
        .map(|test| test["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    for name in ["PostCreditRaisesTheBalance", "PostCreditAddsEachCase"] {
        assert!(recommended.contains(&name), "{recommended:?}");
    }
    assert!(
        !recommended
            .iter()
            .any(|name| ["PostBoth", "Reset"].contains(name)),
        "{recommended:?}"
    );
}
