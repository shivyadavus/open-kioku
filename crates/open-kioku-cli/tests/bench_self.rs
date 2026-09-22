//! `ok bench self` on a repository whose history is scripted at test time.
//!
//! The fixture holds more files than a pack can present (up to 20 primary and 10 supporting), so
//! a rank assertion here can fail: on a three-file repository every file is in every pack, and a
//! builder returning files in arbitrary order would pass.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

/// Comfortably more than the 30 files a pack can list.
const FILLER_MODULES: usize = 40;

const AUTH_V1: &str = "//! Session tokens for the API client.

/// Refresh an expired session token before the request is retried.
pub fn refresh_expired_session_token(token: &str) -> String {
    format!(\"{token}-refreshed\")
}
";

const AUTH_V2: &str = "//! Session tokens for the API client.

/// Refresh an expired session token before the request is retried, at most `attempts` times.
pub fn refresh_expired_session_token(token: &str, attempts: u32) -> String {
    format!(\"{token}-refreshed-{attempts}\")
}
";

const BILLING_V1: &str = "//! Invoice arithmetic.

/// Round an invoice total to whole cents.
pub fn round_invoice_total(total_cents: f64) -> i64 {
    total_cents.round() as i64
}
";

const BILLING_V2: &str = "//! Invoice arithmetic.

/// Round an invoice total to whole cents, halves away from zero.
pub fn round_invoice_total(total_cents: f64) -> i64 {
    (total_cents.abs() + 0.5).floor().copysign(total_cents) as i64
}
";

/// Files that share a query's vocabulary without being the file the commit changed. With only
/// the three modules below, every pack returned exactly one file, so an assertion about rank
/// could not fail however the ranking behaved - which is what a mutation run exposed.
const COMPETITORS: [(&str, &str); 5] = [
    ("src/session_cache.rs", "//! Session cache.\n\n/// Cache of session tokens awaiting refresh after expiry.\npub struct SessionTokenCache {\n    pub refreshed: u64,\n}\n"),
    ("src/token_store.rs", "//! Token store.\n\n/// Persist a session token and the expiry after which a retry must refresh it.\npub fn store_session_token(token: &str, expiry: u64) -> String {\n    format!(\"{token}:{expiry}\")\n}\n"),
    ("src/retry_policy.rs", "//! Retry policy.\n\n/// Backoff used when a request is retried after a session token refresh.\npub fn retry_after_expiry(attempt: u32) -> u64 {\n    2u64.pow(attempt)\n}\n"),
    ("src/invoice_export.rs", "//! Invoice export.\n\n/// Export invoice totals, rounded to whole cents for the ledger.\npub fn export_invoice_totals(totals: &[i64]) -> i64 {\n    totals.iter().sum()\n}\n"),
    ("src/rounding_rules.rs", "//! Rounding rules.\n\n/// Rounding rules for invoice totals: half away from zero, half to even.\npub fn round_half_to_even(value: f64) -> i64 {\n    value.round() as i64\n}\n"),
];

const REPORT: &str = "//! Monthly usage report rendering.

pub fn render_usage_report(rows: &[(String, u64)]) -> String {
    rows.iter().map(|(name, count)| format!(\"{name}: {count}\\n\")).collect()
}
";

/// Filler with vocabulary of its own, so it competes for a place in the pack without sharing
/// terms with either query.
fn filler_module(index: usize) -> String {
    format!(
        "//! Geometry helper {index}.\n\npub fn scale_polygon_{index}(edges: &[f64]) -> Vec<f64> {{\n    edges.iter().map(|edge| edge * {index}.0).collect()\n}}\n"
    )
}

fn git(repo: &Path, args: &[&str], date: &str) {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "user.name=Open Kioku Tests",
            "-c",
            "user.email=tests@open-kioku.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn commit(repo: &Path, files: &[(String, String)], subject: &str, date: &str) {
    for (path, contents) in files {
        let path = repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    git(repo, &["add", "--all"], date);
    git(repo, &["commit", "--quiet", "-m", subject], date);
}

fn scripted_repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "--quiet"], "2026-01-01T09:00:00+00:00");

    let mut initial = vec![
        (".gitignore".to_owned(), ".ok/\nok.toml\n".to_owned()),
        ("src/auth.rs".to_owned(), AUTH_V1.to_owned()),
        ("src/billing.rs".to_owned(), BILLING_V1.to_owned()),
        ("src/report.rs".to_owned(), REPORT.to_owned()),
    ];
    for (path, contents) in COMPETITORS {
        initial.push((path.to_owned(), contents.to_owned()));
    }
    for index in 0..FILLER_MODULES {
        initial.push((format!("src/geometry_{index:02}.rs"), filler_module(index)));
    }
    commit(
        repo,
        &initial,
        "add session, invoice, usage report and geometry modules",
        "2026-01-01T09:00:00+00:00",
    );
    commit(
        repo,
        &[("src/auth.rs".to_owned(), AUTH_V2.to_owned())],
        "retry session token refresh after expiry",
        "2026-01-02T09:00:00+00:00",
    );
    commit(
        repo,
        &[("src/billing.rs".to_owned(), BILLING_V2.to_owned())],
        "round invoice totals half away from zero",
        "2026-01-03T09:00:00+00:00",
    );
    dir
}

/// Every entry under `root`, the root included, with its kind, length, and modification time.
fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, (bool, u64, SystemTime)> {
    let describe = |metadata: fs::Metadata| {
        (
            metadata.is_dir(),
            metadata.len(),
            metadata.modified().unwrap(),
        )
    };
    let mut entries = BTreeMap::new();
    entries.insert(PathBuf::new(), describe(fs::metadata(root).unwrap()));
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                pending.push(path.clone());
            }
            entries.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                describe(metadata),
            );
        }
    }
    entries
}

fn run_bench_self(repo: &Path, temp: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ok"))
        .args(["--json", "bench", "self"])
        .arg(repo)
        .args(extra)
        .env("TMPDIR", temp)
        .output()
        .expect("ok should run")
}

fn bench_self(repo: &Path, temp: &Path, extra: &[&str]) -> serde_json::Value {
    let output = run_bench_self(repo, temp, extra);
    assert!(
        output.status.success(),
        "ok bench self {extra:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("the report should be JSON")
}

fn registered_worktrees(repo: &Path) -> usize {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("git should run");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("worktree "))
        .count()
}

fn stale_registration_listed(repo: &Path) -> bool {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("git should run");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.starts_with("worktree ") && line.ends_with("/stale-user-checkout"))
}

fn leftover_bench_directories(temp: &Path) -> Vec<String> {
    fs::read_dir(temp)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("ok-bench-self-"))
        .collect()
}

#[test]
fn bench_self_scores_scripted_history_deterministically_without_touching_the_repository_index() {
    let dir = scripted_repository();
    let repo = dir.path();
    let ok_dir = repo.join(".ok");
    fs::create_dir_all(ok_dir.join("generations")).unwrap();
    fs::write(
        ok_dir.join("index.sqlite"),
        b"not a database: bench self must not open it",
    )
    .unwrap();
    fs::write(ok_dir.join("abstention-policy.json"), b"{}").unwrap();
    fs::write(ok_dir.join("generations").join("active.json"), b"{}").unwrap();
    let before = snapshot_tree(&ok_dir);
    let temp = tempfile::tempdir().unwrap();

    // A stale registration the user left behind must survive: bench self never prunes.
    let elsewhere = tempfile::tempdir().unwrap();
    let stale = elsewhere.path().join("stale-user-checkout");
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            "--quiet",
            stale.to_str().unwrap(),
            "HEAD",
        ],
        "2026-01-04T09:00:00+00:00",
    );
    fs::remove_dir_all(&stale).unwrap();
    assert_eq!(registered_worktrees(repo), 2);

    let gated = ["--commits", "3", "--min-cases", "1"];
    let first = bench_self(repo, temp.path(), &gated);
    let second = bench_self(repo, temp.path(), &gated);
    assert_eq!(
        first, second,
        "two runs over the same history must report identically"
    );

    assert_eq!(
        snapshot_tree(&ok_dir),
        before,
        "the repository's .ok/ must be left exactly as it was"
    );
    assert_eq!(
        registered_worktrees(repo),
        2,
        "every base checkout must be removed, and only those"
    );
    assert!(
        stale_registration_listed(repo),
        "the user's stale registration was removed"
    );
    assert!(
        leftover_bench_directories(temp.path()).is_empty(),
        "temporary directories left behind: {:?}",
        leftover_bench_directories(temp.path())
    );

    assert_eq!(first["paths_redacted"], true);
    assert_eq!(first["network"], "denied");
    assert_eq!(first["report_version"], 2);
    // The root commit has no parent to index and only adds files.
    assert_eq!(first["selection"]["scanned_commits"], 3);
    assert_eq!(first["selection"]["selected_commits"], 2);
    assert_eq!(first["selection"]["skipped"]["root_commit"], 1);
    // Walking HEAD means the working branch's own commits are cases; the artifact says so.
    assert_eq!(first["selection"]["walked_repository_head"], true);
    assert!(first["caveats"]
        .as_array()
        .unwrap()
        .iter()
        .any(|caveat| caveat
            .as_str()
            .unwrap()
            .contains("commits on the working branch are cases")));
    assert_eq!(first["cases_scored"], 2);
    assert_eq!(first["cases_gold_not_indexed"], 0);
    assert_eq!(first["cases_errored"], 0);
    assert_eq!(first["cases_with_scope_boost_on_gold"], 0);

    // Identity, not position: recorded as digests while paths are redacted.
    assert!(first["repository"]["digest"]
        .as_str()
        .is_some_and(|digest| digest.starts_with("sha256:")));
    assert!(first["repository"]["head_digest"]
        .as_str()
        .is_some_and(|digest| digest.starts_with("sha256:")));
    assert!(first["repository"]["head"].is_null());
    assert!(first["case_set_digest"]
        .as_str()
        .is_some_and(|digest| digest.starts_with("sha256:")));
    // The settings behind the numbers, which otherwise live only in a git-ignored ok.toml.
    assert!(first["configuration"]["ranking"]["text_relevance"].is_number());
    assert!(first["configuration"]["history"]["max_commits"].is_number());
    assert_eq!(first["configuration"]["deny_network"], true);
    assert_eq!(first["configuration"]["semantic_enabled"], false);

    let cases = first["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 2);
    for case in cases {
        assert_eq!(case["status"], "scored", "{case}");
        assert!(case["commit"].is_null(), "{case}");
        assert!(case["query"].is_null(), "{case}");
        assert!(case["error"].is_null(), "{case}");
        assert_eq!(case["commit_scope_boost_on_gold"], false, "{case}");
        assert!(
            case["case_id"]
                .as_str()
                .is_some_and(|id| id.starts_with("sha256:")),
            "{case}"
        );
        let gold = case["gold"].as_array().unwrap();
        assert_eq!(gold.len(), 1, "{case}");
        assert_eq!(gold[0]["path"], "src/**/*.rs", "{case}");
        // A retrieval claim that can fail. The pack must hold competitors - files sharing the
        // query's vocabulary - or any ordering passes; and the modified file must be in the top
        // two of them, which no reversal of a pack this deep can satisfy.
        let returned = case["returned_files"].as_u64().unwrap_or(0);
        assert!(
            returned >= 3,
            "the pack returned {returned} files, so no assertion about rank could fail; the fixture's competitor files must be reaching the pack. Case: {case}"
        );
        // An ordering pin, and deliberately not a flattering one: on this fixture the competitor
        // files are short and almost pure query vocabulary, and retrieval ranks the file the
        // commit actually changed below them. That is recorded here, not endorsed - the quality
        // claim lives in a corpus run over a real repository. Pinning the position is what makes
        // the assertion able to fail at all: any reordering of the ranked list changes it, which
        // a `ranked.reverse()` mutation confirmed. Tuning the fixture until the modified file
        // ranked first would have restored an assertion that passes by construction.
        let expected = match case["position"].as_u64() {
            Some(0) => (3u64, 3u64),
            Some(1) => (3, 4),
            other => panic!("unexpected case position {other:?}"),
        };
        assert_eq!(
            (case["rank"].as_u64().unwrap_or(0), returned),
            expected,
            "the modified file's rank or its pack size moved, so retrieval ordering changed. Case: {case}"
        );
        assert_eq!(case["rank"], gold[0]["rank"], "{case}");
        assert_eq!(case["gold_recall"], 1.0, "{case}");
        assert!(returned <= 30, "a pack lists at most 30 files: {case}");
    }

    // Both metric sets, each carrying its own denominator.
    assert_eq!(first["metrics"]["cases"], 2);
    assert_eq!(first["metrics"]["R@20"], 1.0);
    assert_eq!(first["metrics"]["gold_recall@20"], 1.0);
    assert_eq!(first["metrics_coverage_adjusted"]["cases"], 2);
    assert!(first["gate"]["metrics_suppressed"].is_null());
    assert_eq!(first["gate"]["min_cases"], 1);

    let caveats = first["caveats"].as_array().unwrap();
    assert!(
        caveats
            .iter()
            .any(|caveat| caveat.as_str().unwrap().contains("moves R@k by")),
        "the sample's resolution belongs in the artifact: {caveats:?}"
    );
    assert!(caveats
        .iter()
        .any(|caveat| caveat.as_str().unwrap().contains("case_id")));

    let revealed = bench_self(
        repo,
        temp.path(),
        &["--commits", "3", "--min-cases", "1", "--reveal-paths"],
    );
    let revealed_cases = revealed["cases"].as_array().unwrap();
    assert_eq!(revealed["paths_redacted"], false);
    assert_eq!(
        revealed_cases
            .iter()
            .map(|case| (
                case["query"].as_str().unwrap(),
                case["gold"][0]["path"].as_str().unwrap()
            ))
            .collect::<Vec<_>>(),
        [
            ("round invoice totals half away from zero", "src/billing.rs"),
            ("retry session token refresh after expiry", "src/auth.rs"),
        ],
        "cases are listed newest first"
    );
    for (redacted, revealed) in cases.iter().zip(revealed_cases) {
        assert_eq!(revealed["commit"].as_str().map(str::len), Some(40));
        assert_eq!(revealed["case_id"], revealed["commit"]);
        assert_eq!(redacted["rank"], revealed["rank"]);
        assert_eq!(redacted["task_family"], revealed["task_family"]);
    }
    assert_eq!(snapshot_tree(&ok_dir), before);
    assert_eq!(registered_worktrees(repo), 2);
    assert!(stale_registration_listed(repo));
}

/// Without a floor, a run that can answer only a fraction of its cases still prints a headline
/// over the few that remain. The default floor withholds it and says which condition tripped.
#[test]
fn bench_self_withholds_metrics_below_the_floor_and_says_why() {
    let dir = scripted_repository();
    let repo = dir.path();
    let temp = tempfile::tempdir().unwrap();

    let output = run_bench_self(repo, temp.path(), &["--commits", "3"]);
    assert!(
        !output.status.success(),
        "a run below the floor must not exit 0"
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("the report should still be JSON");
    assert_eq!(report["cases_scored"], 2);
    assert!(report["metrics"].is_null());
    assert!(report["metrics_coverage_adjusted"].is_null());
    let reason = report["gate"]["metrics_suppressed"].as_str().unwrap();
    assert!(reason.contains("--min-cases floor of 10"), "{reason}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("published no metrics"),
        "the exit status should be explained: {stderr}"
    );
    assert!(report["caveats"]
        .as_array()
        .unwrap()
        .iter()
        .any(|caveat| caveat
            .as_str()
            .unwrap()
            .contains("no metrics were published")));
    assert!(leftover_bench_directories(temp.path()).is_empty());
}

/// Two builds cannot be compared by running each against its own HEAD: building one adds a
/// commit, which shifts every position and evicts the oldest case. `--rev` pins the base, and
/// `case_id` joins the cases that survive.
#[test]
fn bench_self_pins_a_base_with_rev_and_keeps_case_identity_across_selections() {
    let dir = scripted_repository();
    let repo = dir.path();
    let temp = tempfile::tempdir().unwrap();

    let full = bench_self(repo, temp.path(), &["--commits", "3", "--min-cases", "1"]);
    let pinned = bench_self(
        repo,
        temp.path(),
        &["--commits", "3", "--min-cases", "1", "--rev", "HEAD~1"],
    );

    let full_cases = full["cases"].as_array().unwrap();
    let pinned_cases = pinned["cases"].as_array().unwrap();
    assert_eq!(full_cases.len(), 2);
    assert_eq!(pinned_cases.len(), 1, "HEAD~1 drops the newest commit");
    assert_ne!(full["case_set_digest"], pinned["case_set_digest"]);
    assert_ne!(
        full["repository"]["head_digest"],
        pinned["repository"]["head_digest"]
    );
    assert_eq!(
        full["repository"]["digest"], pinned["repository"]["digest"],
        "the same repository, whatever the base"
    );

    // Position 1 in one report and position 0 in the other are the same case; joining on
    // position would report both as changed.
    assert_eq!(full_cases[1]["case_id"], pinned_cases[0]["case_id"]);
    assert_ne!(full_cases[0]["position"], full_cases[1]["position"]);
    assert_eq!(pinned_cases[0]["position"], 0);
    // Pinned with --rev, so the run no longer measures whatever the working branch carries.
    assert_eq!(full["selection"]["walked_repository_head"], true);
    assert_eq!(pinned["selection"]["walked_repository_head"], false);
    assert_eq!(full_cases[1]["rank"], pinned_cases[0]["rank"]);

    assert!(leftover_bench_directories(temp.path()).is_empty());
    assert_eq!(registered_worktrees(repo), 1);
}
