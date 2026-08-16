from pathlib import Path

# Add an explicit benchmark-artifact input and typed proof summary.
path = Path('crates/open-kioku-cli/src/types.rs')
text = path.read_text()
old = '''    /// Include repository-relative paths instead of redacted path shapes.\n    #[arg(long, default_value_t = false)]\n    reveal_paths: bool,\n\n    /// Shorthand for --format html.\n'''
new = '''    /// Include repository-relative paths instead of redacted path shapes.\n    #[arg(long, default_value_t = false)]\n    reveal_paths: bool,\n\n    /// Summarize a previously generated frozen retrieval benchmark report.\n    ///\n    /// These metrics remain explicitly separate from measurements of the repository passed to\n    /// `ok prove`; the report is treated as benchmark evidence, not private-repository quality.\n    #[arg(long, value_name = "RETRIEVAL_REPORT_JSON")]\n    retrieval_report: Option<PathBuf>,\n\n    /// Shorthand for --format html.\n'''
if text.count(old) != 1:
    raise SystemExit(f'ProveArgs marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''struct ProofReport {\n    repo: String,\n    generated_by: &'static str,\n    privacy: ProofPrivacy,\n    summary: ProofSummary,\n    languages: BTreeMap<String, usize>,\n'''
new = '''struct ProofReport {\n    repo: String,\n    generated_by: &'static str,\n    privacy: ProofPrivacy,\n    summary: ProofSummary,\n    retrieval_quality: ProofRetrievalQuality,\n    languages: BTreeMap<String, usize>,\n'''
if text.count(old) != 1:
    raise SystemExit(f'ProofReport marker count={text.count(old)}')
text = text.replace(old, new, 1)

marker = '''#[derive(Serialize)]\nstruct ProofTaskReport {\n'''
insert = '''#[derive(Debug, Clone, Serialize)]\nstruct ProofRetrievalQuality {\n    available: bool,\n    scope: &'static str,\n    applies_to_repository: bool,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    corpus_id: Option<String>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    corpus_revision: Option<String>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    cases_sha256: Option<String>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    report_version: Option<String>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    strategy: Option<String>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    strategy_algorithm: Option<String>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    split: Option<String>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    recall_at_10: Option<f64>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    mean_reciprocal_rank: Option<f64>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    file_f1_at_10: Option<f64>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    no_gold_false_positive_rate: Option<f64>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    token_budget_gold_yield_2000: Option<f64>,\n    caveats: Vec<String>,\n}\n\n#[derive(Serialize)]\nstruct ProofTaskReport {\n'''
if text.count(marker) != 1:
    raise SystemExit(f'ProofRetrievalQuality insertion marker count={text.count(marker)}')
text = text.replace(marker, insert, 1)
path.write_text(text)

# Parse a frozen retrieval report into proof metadata without ever claiming it measures the target repo.
path = Path('crates/open-kioku-cli/src/reports/proof.rs')
text = path.read_text()
old = '''    let limit = args.limit.clamp(1, 100);\n    let snapshot = index_repo(&repo)?;\n'''
new = '''    let limit = args.limit.clamp(1, 100);\n    let retrieval_quality = proof_retrieval_quality(args.retrieval_report.as_deref());\n    let snapshot = index_repo(&repo)?;\n'''
if text.count(old) != 1:
    raise SystemExit(f'run_proof retrieval marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        summary: ProofSummary {\n            indexed_files: snapshot.manifest.file_count,\n            indexed_symbols: snapshot.manifest.symbol_count,\n            indexed_chunks: snapshot.manifest.chunk_count,\n            tasks_scored,\n            average_score: round1(average_score),\n            min_score: scores.iter().min().copied().unwrap_or(0),\n            max_score: scores.iter().max().copied().unwrap_or(0),\n            pass_rate_70: round1(pass_rate_70),\n        },\n        languages,\n'''
new = '''        summary: ProofSummary {\n            indexed_files: snapshot.manifest.file_count,\n            indexed_symbols: snapshot.manifest.symbol_count,\n            indexed_chunks: snapshot.manifest.chunk_count,\n            tasks_scored,\n            average_score: round1(average_score),\n            min_score: scores.iter().min().copied().unwrap_or(0),\n            max_score: scores.iter().max().copied().unwrap_or(0),\n            pass_rate_70: round1(pass_rate_70),\n        },\n        retrieval_quality,\n        languages,\n'''
if text.count(old) != 1:
    raise SystemExit(f'ProofReport construction marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''            "Use --task to evaluate product-specific workflows and --reveal-paths when repository-relative paths are safe to share.",\n        ],\n'''
new = '''            "Use --task to evaluate product-specific workflows and --reveal-paths when repository-relative paths are safe to share.",\n            "Retrieval benchmark metrics, when supplied, describe the frozen benchmark artifact only and are never presented as measurements of the private repository.",\n        ],\n'''
if text.count(old) != 1:
    raise SystemExit(f'proof note marker count={text.count(old)}')
text = text.replace(old, new, 1)

marker = '''fn choose_proof_tasks(\n'''
helper = r'''fn proof_retrieval_quality(report_path: Option<&Path>) -> ProofRetrievalQuality {
    let Some(report_path) = report_path else {
        return unavailable_proof_retrieval_quality(
            "no frozen retrieval benchmark report was supplied; repository proof scores are not retrieval benchmark metrics",
        );
    };
    let raw = match fs::read_to_string(report_path) {
        Ok(raw) => raw,
        Err(err) => {
            return unavailable_proof_retrieval_quality(format!(
                "retrieval benchmark report could not be read: {err}"
            ));
        }
    };
    let value = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(value) => value,
        Err(err) => {
            return unavailable_proof_retrieval_quality(format!(
                "retrieval benchmark report is not valid JSON: {err}"
            ));
        }
    };
    parse_proof_retrieval_quality(&value)
        .unwrap_or_else(unavailable_proof_retrieval_quality)
}

fn parse_proof_retrieval_quality(
    value: &serde_json::Value,
) -> Result<ProofRetrievalQuality, String> {
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "retrieval benchmark report is missing schema_version".to_string())?;
    if schema_version != RETRIEVAL_BENCH_SCHEMA_VERSION {
        return Err(format!(
            "retrieval benchmark schema `{schema_version}` is incompatible with supported schema `{RETRIEVAL_BENCH_SCHEMA_VERSION}`"
        ));
    }
    let report_version = required_json_string(value, &["report_version"])?;
    let corpus_id = required_json_string(value, &["corpus_id"])?;
    let corpus_revision = required_json_string(value, &["provenance", "corpus_revision"])?;
    let cases_sha256 = required_json_string(value, &["provenance", "cases_sha256"])?;
    let fixtures_verified = json_path(value, &["provenance", "frozen_fixture_revisions_verified"])
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            "retrieval benchmark report is missing frozen fixture verification status".to_string()
        })?;
    if !fixtures_verified {
        return Err(
            "retrieval benchmark report did not verify frozen fixture revisions; metrics are not proof-grade"
                .into(),
        );
    }

    let strategies = value
        .get("strategies")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "retrieval benchmark report is missing strategies".to_string())?;
    let fusion = strategies
        .iter()
        .find(|strategy| strategy.get("strategy").and_then(serde_json::Value::as_str) == Some("fusion"))
        .ok_or_else(|| "retrieval benchmark report is missing the fusion strategy".to_string())?;
    let quality = json_path(fusion, &["by_split", "holdout", "quality"])
        .ok_or_else(|| "retrieval benchmark report is missing fusion holdout quality".to_string())?;

    let algorithm = json_path(value, &["strategy_identities", "fusion", "algorithm"])
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string);

    Ok(ProofRetrievalQuality {
        available: true,
        scope: "frozen_retrieval_benchmark_artifact",
        applies_to_repository: false,
        corpus_id: Some(corpus_id),
        corpus_revision: Some(corpus_revision),
        cases_sha256: Some(cases_sha256),
        report_version: Some(report_version),
        strategy: Some("fusion".into()),
        strategy_algorithm: algorithm,
        split: Some("holdout".into()),
        recall_at_10: Some(required_json_f64(quality, &["recall_at_10"])?),
        mean_reciprocal_rank: Some(required_json_f64(
            quality,
            &["mean_reciprocal_rank"],
        )?),
        file_f1_at_10: Some(required_json_f64(quality, &["file_f1_at_10"])?),
        no_gold_false_positive_rate: Some(required_json_f64(
            quality,
            &["no_gold_false_positive_rate"],
        )?),
        token_budget_gold_yield_2000: json_path(quality, &["token_budget_gold_yield", "2000"])
            .and_then(serde_json::Value::as_f64),
        caveats: vec![
            "metrics come from a frozen retrieval benchmark artifact and are not measurements of the repository evaluated by ok prove"
                .into(),
        ],
    })
}

fn unavailable_proof_retrieval_quality(reason: impl Into<String>) -> ProofRetrievalQuality {
    ProofRetrievalQuality {
        available: false,
        scope: "frozen_retrieval_benchmark_artifact",
        applies_to_repository: false,
        corpus_id: None,
        corpus_revision: None,
        cases_sha256: None,
        report_version: None,
        strategy: None,
        strategy_algorithm: None,
        split: None,
        recall_at_10: None,
        mean_reciprocal_rank: None,
        file_f1_at_10: None,
        no_gold_false_positive_rate: None,
        token_budget_gold_yield_2000: None,
        caveats: vec![reason.into()],
    }
}

fn json_path<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    path.iter().try_fold(value, |current, key| current.get(*key))
}

fn required_json_string(value: &serde_json::Value, path: &[&str]) -> Result<String, String> {
    json_path(value, path)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("retrieval benchmark report is missing `{}`", path.join(".")))
}

fn required_json_f64(value: &serde_json::Value, path: &[&str]) -> Result<f64, String> {
    let number = json_path(value, path)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| format!("retrieval benchmark report is missing `{}`", path.join(".")))?;
    if !number.is_finite() || !(0.0..=1.0).contains(&number) {
        return Err(format!(
            "retrieval benchmark metric `{}` must be a finite value in [0, 1]",
            path.join(".")
        ));
    }
    Ok(number)
}

fn choose_proof_tasks(
'''
if text.count(marker) != 1:
    raise SystemExit(f'proof helper insertion marker count={text.count(marker)}')
text = text.replace(marker, helper, 1)

old = '''    out.push_str("\\n## Languages\\n\\n");\n'''
new = '''    push_proof_retrieval_quality_markdown(&mut out, &report.retrieval_quality);\n\n    out.push_str("\\n## Languages\\n\\n");\n'''
if text.count(old) != 1:
    raise SystemExit(f'markdown retrieval insertion marker count={text.count(old)}')
text = text.replace(old, new, 1)

marker = '''fn escape_table_cell(value: &str) -> String {\n'''
render_helper = r'''fn push_proof_retrieval_quality_markdown(out: &mut String, quality: &ProofRetrievalQuality) {
    out.push_str("\n## Retrieval Quality\n\n");
    out.push_str(
        "Scope: frozen benchmark artifact only. These metrics do **not** measure the repository evaluated by `ok prove`.\n\n",
    );
    if !quality.available {
        out.push_str("Status: `unavailable`\n\n");
        for caveat in &quality.caveats {
            out.push_str(&format!("- {}\n", escape_table_cell(caveat)));
        }
        return;
    }

    out.push_str("| Metric | Value |\n| --- | ---: |\n");
    if let Some(corpus_id) = &quality.corpus_id {
        out.push_str(&format!("| Corpus | `{}` |\n", escape_table_cell(corpus_id)));
    }
    if let Some(revision) = &quality.corpus_revision {
        out.push_str(&format!("| Corpus revision | `{}` |\n", escape_table_cell(revision)));
    }
    if let Some(value) = quality.recall_at_10 {
        out.push_str(&format!("| Fusion holdout Recall@10 | {:.3} |\n", value));
    }
    if let Some(value) = quality.mean_reciprocal_rank {
        out.push_str(&format!("| Fusion holdout MRR | {:.3} |\n", value));
    }
    if let Some(value) = quality.file_f1_at_10 {
        out.push_str(&format!("| Fusion holdout file F1@10 | {:.3} |\n", value));
    }
    if let Some(value) = quality.no_gold_false_positive_rate {
        out.push_str(&format!("| Fusion holdout no-gold FP rate | {:.3} |\n", value));
    }
    if let Some(value) = quality.token_budget_gold_yield_2000 {
        out.push_str(&format!("| Gold yield @ 2k tokens | {:.3} |\n", value));
    }
    if !quality.caveats.is_empty() {
        out.push_str("\n");
        for caveat in &quality.caveats {
            out.push_str(&format!("- {}\n", escape_table_cell(caveat)));
        }
    }
}

fn escape_table_cell(value: &str) -> String {
'''
if text.count(marker) != 1:
    raise SystemExit(f'proof markdown helper marker count={text.count(marker)}')
text = text.replace(marker, render_helper, 1)

# Add adversarial parser tests in a uniquely named module (lib.rs already owns `tests`).
text += r'''

#[cfg(test)]
mod proof_retrieval_quality_tests {
    use super::*;

    fn valid_report() -> serde_json::Value {
        serde_json::json!({
            "schema_version": RETRIEVAL_BENCH_SCHEMA_VERSION,
            "report_version": "1.2.0",
            "provenance": {
                "corpus_revision": "fixture-rev-1",
                "cases_sha256": "abc123",
                "frozen_fixture_revisions_verified": true
            },
            "corpus_id": "open-kioku-retrieval-v1",
            "strategy_identities": {
                "fusion": { "algorithm": "reciprocal_rank_fusion" }
            },
            "strategies": [{
                "strategy": "fusion",
                "by_split": {
                    "holdout": {
                        "quality": {
                            "recall_at_10": 0.9,
                            "mean_reciprocal_rank": 0.8,
                            "file_f1_at_10": 0.7,
                            "no_gold_false_positive_rate": 0.2,
                            "token_budget_gold_yield": { "2000": 0.6 }
                        }
                    }
                }
            }]
        })
    }

    #[test]
    fn proof_retrieval_quality_keeps_benchmark_scope_separate_from_private_repo() {
        let quality = parse_proof_retrieval_quality(&valid_report()).unwrap();
        assert!(quality.available);
        assert!(!quality.applies_to_repository);
        assert_eq!(quality.scope, "frozen_retrieval_benchmark_artifact");
        assert_eq!(quality.recall_at_10, Some(0.9));
        assert_eq!(quality.token_budget_gold_yield_2000, Some(0.6));
        assert!(quality
            .caveats
            .iter()
            .any(|caveat| caveat.contains("not measurements of the repository")));
    }

    #[test]
    fn proof_retrieval_quality_fails_closed_when_fixture_revisions_are_unverified() {
        let mut report = valid_report();
        report["provenance"]["frozen_fixture_revisions_verified"] = serde_json::json!(false);
        let error = parse_proof_retrieval_quality(&report).unwrap_err();
        assert!(error.contains("did not verify frozen fixture revisions"));

        let unavailable = unavailable_proof_retrieval_quality(error);
        assert!(!unavailable.available);
        assert_eq!(unavailable.recall_at_10, None);
        assert_eq!(unavailable.mean_reciprocal_rank, None);
    }

    #[test]
    fn proof_retrieval_quality_rejects_out_of_range_metrics() {
        let mut report = valid_report();
        report["strategies"][0]["by_split"]["holdout"]["quality"]["recall_at_10"] =
            serde_json::json!(1.1);
        let error = parse_proof_retrieval_quality(&report).unwrap_err();
        assert!(error.contains("must be a finite value in [0, 1]"));
    }

    #[test]
    fn proof_markdown_states_benchmark_metrics_do_not_measure_target_repo() {
        let quality = parse_proof_retrieval_quality(&valid_report()).unwrap();
        let mut markdown = String::new();
        push_proof_retrieval_quality_markdown(&mut markdown, &quality);
        assert!(markdown.contains("do **not** measure the repository evaluated by `ok prove`"));
        assert!(markdown.contains("Fusion holdout Recall@10"));
    }
}
'''
path.write_text(text)

# Surface the same scope distinction in HTML proof output.
path = Path('crates/open-kioku-cli/src/reports/trust.rs')
text = path.read_text()
old = '''    out.push_str("</tbody></table></section>");\n    html_list_section(&mut out, "Reproduce", &report.reproduce);\n'''
new = '''    out.push_str("</tbody></table></section>");\n    out.push_str("<section class=\\"panel\\"><h2>Retrieval Quality</h2>");\n    out.push_str("<p><strong>Scope:</strong> frozen benchmark artifact only. These metrics do not measure the repository evaluated by <code>ok prove</code>.</p>");\n    if report.retrieval_quality.available {\n        out.push_str("<table><tbody>");\n        if let Some(value) = report.retrieval_quality.recall_at_10 {\n            out.push_str(&format!("<tr><th>Fusion holdout Recall@10</th><td>{value:.3}</td></tr>"));\n        }\n        if let Some(value) = report.retrieval_quality.mean_reciprocal_rank {\n            out.push_str(&format!("<tr><th>Fusion holdout MRR</th><td>{value:.3}</td></tr>"));\n        }\n        if let Some(value) = report.retrieval_quality.file_f1_at_10 {\n            out.push_str(&format!("<tr><th>Fusion holdout file F1@10</th><td>{value:.3}</td></tr>"));\n        }\n        if let Some(value) = report.retrieval_quality.no_gold_false_positive_rate {\n            out.push_str(&format!("<tr><th>Fusion holdout no-gold FP rate</th><td>{value:.3}</td></tr>"));\n        }\n        out.push_str("</tbody></table>");\n    } else {\n        out.push_str("<p>Status: <code>unavailable</code></p>");\n    }\n    if !report.retrieval_quality.caveats.is_empty() {\n        out.push_str("<ul>");\n        for caveat in &report.retrieval_quality.caveats {\n            out.push_str(&format!("<li>{}</li>", escape_html(caveat)));\n        }\n        out.push_str("</ul>");\n    }\n    out.push_str("</section>");\n    html_list_section(&mut out, "Reproduce", &report.reproduce);\n'''
if text.count(old) != 1:
    raise SystemExit(f'proof html marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)
