from pathlib import Path

path = Path('crates/open-kioku-cli/src/reports/proof.rs')
text = path.read_text()

old = '''    let report_version = required_json_string(value, &["report_version"])?;
    let corpus_id = required_json_string(value, &["corpus_id"])?;
'''
new = '''    let report_version = required_json_string(value, &["report_version"])?;
    if report_version != RETRIEVAL_REPORT_VERSION {
        return Err(format!(
            "retrieval report version `{report_version}` is incompatible with supported report version `{RETRIEVAL_REPORT_VERSION}`"
        ));
    }
    let corpus_id = required_json_string(value, &["corpus_id"])?;
'''
if text.count(old) != 1:
    raise SystemExit(f'report version marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''    let algorithm = json_path(value, &["strategy_identities", "fusion", "algorithm"])
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);
'''
new = '''    let algorithm = required_json_string(
        value,
        &["strategy_identities", "fusion", "algorithm"],
    )?;
'''
if text.count(old) != 1:
    raise SystemExit(f'algorithm marker count={text.count(old)}')
text = text.replace(old, new, 1)

text = text.replace('''        strategy_algorithm: algorithm,
''', '''        strategy_algorithm: Some(algorithm),
''', 1)

old = '''        token_budget_gold_yield_2000: json_path(quality, &["token_budget_gold_yield", "2000"])
            .and_then(serde_json::Value::as_f64),
'''
new = '''        token_budget_gold_yield_2000: optional_json_f64(
            quality,
            &["token_budget_gold_yield", "2000"],
        )?,
'''
if text.count(old) != 1:
    raise SystemExit(f'token yield marker count={text.count(old)}')
text = text.replace(old, new, 1)

marker = '''fn choose_proof_tasks(
'''
helper = '''fn optional_json_f64(
    value: &serde_json::Value,
    path: &[&str],
) -> Result<Option<f64>, String> {
    let Some(raw) = json_path(value, path) else {
        return Ok(None);
    };
    let number = raw.as_f64().ok_or_else(|| {
        format!(
            "retrieval benchmark metric `{}` must be numeric",
            path.join(".")
        )
    })?;
    if !number.is_finite() || !(0.0..=1.0).contains(&number) {
        return Err(format!(
            "retrieval benchmark metric `{}` must be a finite value in [0, 1]",
            path.join(".")
        ));
    }
    Ok(Some(number))
}

fn choose_proof_tasks(
'''
if text.count(marker) != 1:
    raise SystemExit(f'optional metric helper marker count={text.count(marker)}')
text = text.replace(marker, helper, 1)

marker = '''    #[test]
    fn proof_markdown_states_benchmark_metrics_do_not_measure_target_repo() {
'''
tests = '''    #[test]
    fn proof_retrieval_quality_rejects_incompatible_report_versions() {
        let mut report = valid_report();
        report["report_version"] = serde_json::json!("999.0.0");
        let error = parse_proof_retrieval_quality(&report).unwrap_err();
        assert!(error.contains("incompatible with supported report version"));
    }

    #[test]
    fn proof_retrieval_quality_rejects_out_of_range_optional_token_yield() {
        let mut report = valid_report();
        report["strategies"][0]["by_split"]["holdout"]["quality"]
            ["token_budget_gold_yield"]["2000"] = serde_json::json!(-0.1);
        let error = parse_proof_retrieval_quality(&report).unwrap_err();
        assert!(error.contains("token_budget_gold_yield.2000"));
        assert!(error.contains("finite value in [0, 1]"));
    }

    #[test]
    fn proof_markdown_states_benchmark_metrics_do_not_measure_target_repo() {
'''
if text.count(marker) != 1:
    raise SystemExit(f'self-review test marker count={text.count(marker)}')
text = text.replace(marker, tests, 1)

path.write_text(text)
