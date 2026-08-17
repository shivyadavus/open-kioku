#!/usr/bin/env python3
from pathlib import Path
import json


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    return text.replace(old, new)

# CLI argument for deterministic Markdown output.
types_path = Path("crates/open-kioku-cli/src/types.rs")
types = types_path.read_text()
types = replace_once(
    types,
    '''    /// Optional path for the deterministic JSON score report.
    #[arg(long, value_name = "REPORT_JSON")]
    write: Option<PathBuf>,

    /// Versioned JSON release-gate policy. When supplied, gate results are included in the report.
''',
    '''    /// Optional path for the deterministic JSON score report.
    #[arg(long, value_name = "REPORT_JSON")]
    write: Option<PathBuf>,

    /// Optional path for a deterministic Markdown score and capability report.
    #[arg(long, value_name = "REPORT_MD")]
    write_markdown: Option<PathBuf>,

    /// Versioned JSON release-gate policy. When supplied, gate results are included in the report.
''',
    "relationship bench Markdown argument",
)
types_path.write_text(types)

# Deterministic Markdown renderer and write path.
bench_path = Path("crates/open-kioku-cli/src/bench/relationship.rs")
bench = bench_path.read_text()
bench = replace_once(
    bench,
    '''    let rendered = serde_json::to_string_pretty(&report)?;

    if let Some(path) = &args.write {
''',
    '''    let rendered = serde_json::to_string_pretty(&report)?;
    let markdown = render_relationship_bench_markdown(&report);

    if let Some(path) = &args.write {
''',
    "relationship report render anchor",
)
bench = replace_once(
    bench,
    '''    if json {
        println!("{rendered}");
''',
    '''    if let Some(path) = &args.write_markdown {
        fs::write(path, &markdown).with_context(|| {
            format!(
                "failed to write relationship benchmark Markdown report {}",
                path.display()
            )
        })?;
    }

    if json {
        println!("{rendered}");
''',
    "relationship Markdown write",
)
bench = replace_once(
    bench,
    '''        if let Some(path) = &args.write {
            println!("Wrote report to {}", path.display());
        }
''',
    '''        if let Some(path) = &args.write {
            println!("Wrote report to {}", path.display());
        }
        if let Some(path) = &args.write_markdown {
            println!("Wrote Markdown report to {}", path.display());
        }
''',
    "relationship Markdown console notice",
)

renderer = r'''
fn render_relationship_bench_markdown(report: &RelationshipBenchScoreReport) -> String {
    const LANGUAGES: [&str; 5] = [
        "rust",
        "typescript_javascript",
        "python",
        "java",
        "go",
    ];
    const RELATIONSHIPS: [&str; 7] = [
        "CALLS",
        "REFERENCES",
        "USES_TYPE",
        "IMPLEMENTS",
        "EXTENDS",
        "IMPORTS",
        "DEPENDS_ON",
    ];

    let mut out = String::new();
    out.push_str("# Relationship Conformance Report\n\n");
    out.push_str("## Identity\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    out.push_str(&format!("| Schema | `{}` |\n", markdown_cell(&report.schema_version)));
    out.push_str(&format!("| Corpus | `{}` |\n", markdown_cell(&report.corpus_version)));
    out.push_str(&format!("| Corpus status | `{:?}` |\n", report.corpus_status));
    out.push_str(&format!(
        "| Observation digest | `{}` |\n",
        markdown_cell(&report.observation_digest)
    ));
    if let Some(gate) = &report.gate {
        out.push_str(&format!(
            "| Gate | **{}** |\n",
            if gate.passed { "PASS" } else { "FAIL" }
        ));
    }

    out.push_str("\n## Overall quality\n\n");
    out.push_str("| Metric | Value |\n| --- | ---: |\n");
    out.push_str(&format!("| Cases | {} |\n", report.overall.cases));
    out.push_str(&format!("| Authoritative precision | {:.4} |\n", report.overall.precision));
    out.push_str(&format!("| Authoritative recall | {:.4} |\n", report.overall.recall));
    out.push_str(&format!("| False positives | {} |\n", report.overall.false_positives));
    out.push_str(&format!("| False negatives | {} |\n", report.overall.false_negatives));
    out.push_str(&format!(
        "| MustNotEmit false-positive rate | {:.4} |\n",
        report.overall.must_not_emit_false_positive_rate
    ));
    out.push_str(&format!(
        "| Exact-range compliance | {:.4} |\n",
        report.overall.exact_range_compliance
    ));
    out.push_str(&format!("| Proof compliance | {:.4} |\n", report.overall.proof_compliance));
    out.push_str(&format!("| Outcome compliance | {:.4} |\n", report.overall.outcome_compliance));
    out.push_str(&format!(
        "| Metamorphic equivalence | {:.4} ({}/{}) |\n",
        report.metamorphic_equivalence,
        report.metamorphic_equivalent_groups,
        report.metamorphic_groups
    ));

    out.push_str("\n## Capability matrix\n\n");
    out.push_str("Every Tier-1 language × structural-relationship cohort is shown. Zero-case cells are intentional visibility, not implied support.\n\n");
    out.push_str("| Language | Relationship | Cases | TP | FP | FN | Precision | Recall | Exact range | Proof | Outcome |\n");
    out.push_str("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for language in LANGUAGES {
        for relationship in RELATIONSHIPS {
            let key = format!("{language}::{relationship}");
            if let Some(metrics) = report.by_language_relationship.get(&key) {
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} | {} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} |\n",
                    language,
                    relationship,
                    metrics.cases,
                    metrics.true_positives,
                    metrics.false_positives,
                    metrics.false_negatives,
                    metrics.precision,
                    metrics.recall,
                    metrics.exact_range_compliance,
                    metrics.proof_compliance,
                    metrics.outcome_compliance,
                ));
            } else {
                out.push_str(&format!(
                    "| {language} | {relationship} | 0 | 0 | 0 | 0 | — | — | — | — | — |\n"
                ));
            }
        }
    }

    out.push_str("\n## Reproducibility\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    markdown_optional_row(&mut out, "Git commit", report.run_metadata.git_commit.as_deref());
    markdown_optional_row(
        &mut out,
        "Analysis semantics fingerprint",
        report.run_metadata.analysis_semantics_fingerprint.as_deref(),
    );
    markdown_optional_row(
        &mut out,
        "Proof policy version",
        report.run_metadata.proof_policy_version.as_deref(),
    );
    markdown_optional_row(&mut out, "Index mode", report.run_metadata.index_mode.as_deref());
    if report.run_metadata.adapter_versions.is_empty() {
        out.push_str("| Adapter versions | — |\n");
    } else {
        let adapters = report
            .run_metadata
            .adapter_versions
            .iter()
            .map(|(name, version)| format!("{}={}", markdown_cell(name), markdown_cell(version)))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("| Adapter versions | `{adapters}` |\n"));
    }

    if let Some(gate) = &report.gate {
        out.push_str("\n## Gate results\n\n");
        out.push_str(&format!("**{}**\n", if gate.passed { "PASS" } else { "FAIL" }));
        if gate.failures.is_empty() {
            out.push_str("\nNo configured gate failures.\n");
        } else {
            out.push('\n');
            for failure in &gate.failures {
                out.push_str(&format!("- {}\n", markdown_cell(failure)));
            }
        }
    }

    if !report.diagnostics.is_empty() {
        out.push_str("\n## Diagnostics\n\n");
        for diagnostic in &report.diagnostics {
            out.push_str(&format!(
                "- `{}` **{}** — {}\n",
                markdown_cell(&diagnostic.case_id),
                markdown_cell(&diagnostic.kind),
                markdown_cell(&diagnostic.message)
            ));
        }
    }
    out
}

fn markdown_optional_row(out: &mut String, label: &str, value: Option<&str>) {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => out.push_str(&format!("| {} | `{}` |\n", label, markdown_cell(value))),
        None => out.push_str(&format!("| {} | — |\n", label)),
    }
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace(['\r', '\n'], " ").replace('`', "\\`")
}

'''
anchor = "fn validate_relationship_bench_corpus(corpus: &RelationshipBenchCorpus) -> anyhow::Result<()> {\n"
if anchor not in bench:
    raise SystemExit("Markdown renderer anchor missing")
bench = bench.replace(anchor, renderer + anchor, 1)

if "markdown_report_is_deterministic_and_shows_all_capability_cells" not in bench:
    bench += r'''

#[cfg(test)]
mod ri3_relationship_markdown_tests {
    use super::*;

    #[test]
    fn markdown_report_is_deterministic_and_shows_all_capability_cells() {
        let corpus = relationship_bench_tests::corpus(vec![relationship_bench_tests::case(
            "markdown-case",
            RelationshipBenchExpectedOutcome::MustNotEmit,
        )]);
        let report = score_relationship_bench(&corpus, &[]).unwrap();
        let first = render_relationship_bench_markdown(&report);
        let second = render_relationship_bench_markdown(&report);

        assert_eq!(first, second);
        assert!(first.contains("# Relationship Conformance Report"));
        assert!(first.contains("## Capability matrix"));
        assert!(first.contains("| rust | CALLS |"));
        assert!(first.contains("| go | DEPENDS_ON | 0 |"));
        assert_eq!(
            first
                .lines()
                .filter(|line| {
                    line.starts_with("| rust |")
                        || line.starts_with("| typescript_javascript |")
                        || line.starts_with("| python |")
                        || line.starts_with("| java |")
                        || line.starts_with("| go |")
                })
                .count(),
            35
        );
    }
}
'''
bench_path.write_text(bench)

# Small, clearly non-release development corpus. It exercises scorer/gate mechanics across all
# Tier-1 language labels while the live resolver producer and full frozen corpus remain separate.
languages = ["rust", "typescript_javascript", "python", "java", "go"]
cases = []
observations = []
for idx, language in enumerate(languages, start=1):
    source = f"symbol:dev:{language}:caller"
    target = f"symbol:dev:{language}:callee"
    source_range = {
        "start_line": 10 + idx,
        "start_column": 4,
        "end_line": 10 + idx,
        "end_column": 18,
    }
    for variant in ["a", "b"]:
        case_id = f"dev-{language}-calls-positive-{variant}"
        cases.append({
            "id": case_id,
            "fixture_id": f"dev:{language}:calls:positive",
            "split": "development",
            "language": language,
            "relationship": "CALLS",
            "source_symbol_id": source,
            "expected_outcome": "must_emit",
            "expected_target_symbol_id": target,
            "expected_source_range": source_range,
            "expected_proof_kinds": ["exact_call_site", "exact_reference"],
            "candidate_count_expected": 1,
            "metamorphic_group": f"dev:{language}:calls:positive",
            "notes": "development scorer-contract fixture; not release conformance evidence",
        })
        observations.append({
            "case_id": case_id,
            "outcome": "proven",
            "candidate_count": 1,
            "relationships": [{
                "source_symbol_id": source,
                "target_symbol_id": target,
                "relationship": "CALLS",
                "authority": "authoritative",
                "proof_kinds": ["exact_call_site", "exact_reference"],
                "source_ranges": [source_range],
                "resolver_strategies": ["development_fixture"],
            }],
        })
    for variant in ["a", "b"]:
        case_id = f"dev-{language}-calls-negative-{variant}"
        cases.append({
            "id": case_id,
            "fixture_id": f"dev:{language}:calls:negative",
            "split": "development",
            "language": language,
            "relationship": "CALLS",
            "source_symbol_id": source,
            "expected_outcome": "must_not_emit",
            "candidate_count_expected": 0,
            "metamorphic_group": f"dev:{language}:calls:negative",
            "notes": "development scorer-contract fixture; not release conformance evidence",
        })
        observations.append({
            "case_id": case_id,
            "outcome": "unresolved",
            "candidate_count": 0,
            "relationships": [],
        })

corpus = {
    "schema_version": "1.0.0",
    "corpus_version": "dev-scorer-contract-1",
    "status": "development",
    "cases": cases,
}
observation_set = {
    "metadata": {
        "analysis_semantics_fingerprint": "development-synthetic-observations",
        "adapter_versions": {"development_fixture": "1"},
        "proof_policy_version": "development-only",
        "index_mode": "development-synthetic",
        "index_config": {"purpose": "scorer contract CI; not release conformance"},
    },
    "observations": observations,
}
policy = {
    "schema_version": "1.0.0",
    "minimum_cases": 20,
    "minimum_cases_per_language": 4,
    "minimum_cases_per_language_relationship": 0,
    "minimum_negative_fraction": 0.40,
    "minimum_overall_precision": 1.0,
    "minimum_language_relationship_precision": 1.0,
    "maximum_must_not_emit_false_positive_rate": 0.0,
    "minimum_exact_range_compliance": 1.0,
    "minimum_proof_compliance": 1.0,
    "minimum_outcome_compliance": 1.0,
    "minimum_metamorphic_groups": 10,
    "minimum_metamorphic_equivalence": 1.0,
    "require_zero_false_negatives": True,
    "require_positive_and_negative_per_language_relationship": True,
    "require_metamorphic_group_per_language_relationship": True,
    "require_reproducibility_metadata": False,
    "require_frozen_corpus": False,
}
for name, payload in [
    ("benchmarks/relationship-development-corpus.json", corpus),
    ("benchmarks/relationship-development-observations.json", observation_set),
    ("benchmarks/relationship-development-thresholds.json", policy),
]:
    Path(name).write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")

# Run the development scorer contract only once in the OS matrix.
ci_path = Path(".github/workflows/ci.yml")
ci = ci_path.read_text()
ci = replace_once(
    ci,
    '''      - run: cargo test -p open-kioku-tests
      - name: MCP golden snapshot contract
''',
    '''      - run: cargo test -p open-kioku-tests
      - name: relationship scorer development contract
        if: matrix.os == 'ubuntu-latest'
        run: |
          cargo run -q -p open-kioku-cli --bin ok -- relationship-bench \\
            --corpus benchmarks/relationship-development-corpus.json \\
            --observations benchmarks/relationship-development-observations.json \\
            --policy benchmarks/relationship-development-thresholds.json \\
            --enforce-gates \\
            --write target/relationship-development-report.json \\
            --write-markdown target/relationship-development-report.md
          test -s target/relationship-development-report.json
          test -s target/relationship-development-report.md
          grep -q '## Capability matrix' target/relationship-development-report.md
      - name: MCP golden snapshot contract
''',
    "CI scorer development contract",
)
ci_path.write_text(ci)
