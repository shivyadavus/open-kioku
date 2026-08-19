use assert_cmd::cargo::cargo_bin_cmd;
use serde_json::Value;
use std::fs;

#[test]
fn context_json_reports_explicit_abstention_for_no_match_query() {
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

    let output = cargo_bin_cmd!("ok")
        .arg("--repo")
        .arg(repo)
        .args([
            "context",
            "zzqv_no_matching_repository_evidence_7f31b9",
            "--format",
            "json",
        ])
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
