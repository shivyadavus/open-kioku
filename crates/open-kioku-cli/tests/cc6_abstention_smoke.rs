use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::Value;
use std::fs;

const NO_MATCH_QUERY: &str = "zzqv_no_matching_repository_evidence_7f31b9";

fn initialized_fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("temporary repository");
    let repo = temp.path();
    fs::create_dir_all(repo.join("src")).expect("create source directory");
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"cc6-abstention-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write Cargo.toml");
    fs::write(
        repo.join("src/lib.rs"),
        "pub fn alpha_token() -> &'static str { \"alpha\" }\n",
    )
    .expect("write fixture source");

    cargo_bin_cmd!("ok")
        .args(["init"])
        .arg(repo)
        .assert()
        .success();
    cargo_bin_cmd!("ok")
        .args(["index"])
        .arg(repo)
        .args(["--with-scip", "off", "--mode", "fast"])
        .assert()
        .success();

    temp
}

#[test]
fn context_json_reports_explicit_abstention_for_no_match_query() {
    let temp = initialized_fixture();
    let repo = temp.path();

    let output = cargo_bin_cmd!("ok")
        .arg("--repo")
        .arg(repo)
        .args(["context", NO_MATCH_QUERY, "--format", "json"])
        .output()
        .expect("run context command");
    assert!(
        output.status.success(),
        "context command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let pack: Value = serde_json::from_slice(&output.stdout).expect("context JSON");
    let primary = pack
        .pointer("/primary_files")
        .and_then(Value::as_array)
        .expect("primary_files array");
    assert!(
        primary.is_empty(),
        "no-match query must not invent primary context"
    );

    for pointer in [
        "/supporting_files",
        "/dependency_edges",
        "/runtime_signals",
        "/test_candidates",
        "/validation_plan/tests",
        "/recommended_change_boundary/allowed_files",
        "/recommended_change_boundary/caution_files",
        "/recommended_change_boundary/forbidden_files",
    ] {
        let values = pack
            .pointer(pointer)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("{pointer} array"));
        assert!(
            values.is_empty(),
            "abstained no-match context must not synthesize downstream evidence at {pointer}: {values:?}"
        );
    }

    let reason = pack
        .pointer("/retrieval_diagnostics/selection/abstention_reason")
        .and_then(Value::as_str)
        .expect("explicit abstention reason");
    assert!(
        !reason.trim().is_empty(),
        "abstention reason must remain inspectable for no-match queries"
    );
    assert!(
        pack.pointer("/retrieval_diagnostics/selection/retrieval_confidence")
            .is_some(),
        "abstention telemetry must retain retrieval confidence"
    );
}

#[test]
fn rendered_context_formats_keep_no_match_abstention_visible() {
    let temp = initialized_fixture();
    let repo = temp.path();

    for (format, expected_marker) in [
        ("markdown", "Abstention reason:"),
        ("prompt-text", "RETRIEVAL_ABSTENTION_REASON:"),
    ] {
        let output = cargo_bin_cmd!("ok")
            .arg("--repo")
            .arg(repo)
            .args(["context", NO_MATCH_QUERY, "--format", format])
            .output()
            .expect("run rendered context command");
        assert!(
            output.status.success(),
            "context --format {format} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let rendered = String::from_utf8(output.stdout).expect("utf-8 context output");
        assert!(
            rendered.contains(expected_marker),
            "{format} output must explain why retrieval abstained: {rendered}"
        );
        assert!(
            !rendered.contains("alpha_token"),
            "{format} no-match output must not leak unrelated primary context: {rendered}"
        );
    }
}
