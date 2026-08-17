from pathlib import Path


def replace_exact(path: str, old: str, new: str, label: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    p.write_text(text.replace(old, new, count))


relationship = "crates/open-kioku-cli/src/bench/relationship.rs"
replace_exact(
    relationship,
    'const RELATIONSHIP_BENCH_SCHEMA_VERSION: &str = "1.0.0";\n',
    'const RELATIONSHIP_BENCH_SCHEMA_VERSION: &str = "1.0.0";\nconst RELATIONSHIP_BENCH_POLICY_SCHEMA_VERSION: &str = "1.0.0";\n',
    "policy schema constant",
)

replace_exact(
    relationship,
    '''#[derive(Debug, Clone, Serialize)]
struct RelationshipBenchScoreReport {
''',
    '''#[derive(Debug, Clone, Deserialize)]
struct RelationshipBenchPolicy {
    schema_version: String,
    minimum_cases: usize,
    minimum_cases_per_language: usize,
    minimum_cases_per_language_relationship: usize,
    minimum_negative_fraction: f64,
    minimum_overall_precision: f64,
    minimum_language_relationship_precision: f64,
    maximum_must_not_emit_false_positive_rate: f64,
    minimum_exact_range_compliance: f64,
    minimum_proof_compliance: f64,
    minimum_outcome_compliance: f64,
    require_zero_false_negatives: bool,
    require_positive_and_negative_per_language_relationship: bool,
    require_frozen_corpus: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RelationshipBenchGateReport {
    policy_schema_version: String,
    passed: bool,
    failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RelationshipBenchScoreReport {
''',
    "gate policy structs",
)

replace_exact(
    relationship,
    '''    wrong_target_counts: BTreeMap<String, usize>,
    diagnostics: Vec<RelationshipBenchDiagnostic>,
}
''',
    '''    wrong_target_counts: BTreeMap<String, usize>,
    diagnostics: Vec<RelationshipBenchDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gate: Option<RelationshipBenchGateReport>,
}
''',
    "gate report field",
)

replace_exact(
    relationship,
    '''fn run_relationship_bench_command(
    args: RelationshipBenchArgs,
    json: bool,
) -> anyhow::Result<()> {
''',
    '''fn load_relationship_bench_policy(path: &Path) -> anyhow::Result<RelationshipBenchPolicy> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read relationship benchmark policy {}", path.display()))?;
    let policy: RelationshipBenchPolicy = serde_json::from_str(&raw)
        .with_context(|| format!("invalid relationship benchmark policy {}", path.display()))?;
    if policy.schema_version != RELATIONSHIP_BENCH_POLICY_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported relationship benchmark policy schema version {}; expected {}",
            policy.schema_version,
            RELATIONSHIP_BENCH_POLICY_SCHEMA_VERSION
        );
    }
    for (name, value) in [
        ("minimum_negative_fraction", policy.minimum_negative_fraction),
        ("minimum_overall_precision", policy.minimum_overall_precision),
        (
            "minimum_language_relationship_precision",
            policy.minimum_language_relationship_precision,
        ),
        (
            "maximum_must_not_emit_false_positive_rate",
            policy.maximum_must_not_emit_false_positive_rate,
        ),
        (
            "minimum_exact_range_compliance",
            policy.minimum_exact_range_compliance,
        ),
        ("minimum_proof_compliance", policy.minimum_proof_compliance),
        (
            "minimum_outcome_compliance",
            policy.minimum_outcome_compliance,
        ),
    ] {
        if !(0.0..=1.0).contains(&value) {
            anyhow::bail!("relationship benchmark policy {name} must be between 0 and 1");
        }
    }
    Ok(policy)
}

fn evaluate_relationship_bench_gates(
    corpus: &RelationshipBenchCorpus,
    report: &RelationshipBenchScoreReport,
    policy: &RelationshipBenchPolicy,
) -> RelationshipBenchGateReport {
    let mut failures = Vec::new();
    if policy.require_frozen_corpus && corpus.status != RelationshipBenchCorpusStatus::Frozen {
        failures.push("release gating requires a frozen relationship corpus".to_string());
    }
    if report.overall.cases < policy.minimum_cases {
        failures.push(format!(
            "corpus has {} cases, below required {}",
            report.overall.cases, policy.minimum_cases
        ));
    }
    let negative_fraction = if report.overall.cases == 0 {
        0.0
    } else {
        report.overall.negative_cases as f64 / report.overall.cases as f64
    };
    if negative_fraction < policy.minimum_negative_fraction {
        failures.push(format!(
            "negative/ambiguous fraction {:.4} is below required {:.4}",
            negative_fraction, policy.minimum_negative_fraction
        ));
    }
    if report.overall.precision < policy.minimum_overall_precision {
        failures.push(format!(
            "overall authoritative precision {:.4} is below required {:.4}",
            report.overall.precision, policy.minimum_overall_precision
        ));
    }
    if policy.require_zero_false_negatives && report.overall.false_negatives != 0 {
        failures.push(format!(
            "{} required authoritative relationship(s) were missing",
            report.overall.false_negatives
        ));
    }
    if report.overall.must_not_emit_cases == 0 {
        failures.push("corpus contains no MustNotEmit cases".to_string());
    } else if report.overall.must_not_emit_false_positive_rate
        > policy.maximum_must_not_emit_false_positive_rate
    {
        failures.push(format!(
            "MustNotEmit false-positive rate {:.4} exceeds allowed {:.4}",
            report.overall.must_not_emit_false_positive_rate,
            policy.maximum_must_not_emit_false_positive_rate
        ));
    }
    if report.overall.exact_range_cases == 0 {
        failures.push("corpus contains no exact source-range assertions".to_string());
    } else if report.overall.exact_range_compliance < policy.minimum_exact_range_compliance {
        failures.push(format!(
            "exact source-range compliance {:.4} is below required {:.4}",
            report.overall.exact_range_compliance, policy.minimum_exact_range_compliance
        ));
    }
    if report.overall.proof_cases == 0 {
        failures.push("corpus contains no proof-kind assertions".to_string());
    } else if report.overall.proof_compliance < policy.minimum_proof_compliance {
        failures.push(format!(
            "proof compliance {:.4} is below required {:.4}",
            report.overall.proof_compliance, policy.minimum_proof_compliance
        ));
    }
    if report.overall.outcome_compliance < policy.minimum_outcome_compliance {
        failures.push(format!(
            "resolution-outcome compliance {:.4} is below required {:.4}",
            report.overall.outcome_compliance, policy.minimum_outcome_compliance
        ));
    }

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
    for language in LANGUAGES {
        let cases = report
            .by_language
            .get(language)
            .map(|metrics| metrics.cases)
            .unwrap_or(0);
        if cases < policy.minimum_cases_per_language {
            failures.push(format!(
                "language {language} has {cases} cases, below required {}",
                policy.minimum_cases_per_language
            ));
        }
        for relationship in RELATIONSHIPS {
            let key = format!("{language}::{relationship}");
            let metrics = report.by_language_relationship.get(&key);
            let cases = metrics.map(|value| value.cases).unwrap_or(0);
            if cases < policy.minimum_cases_per_language_relationship {
                failures.push(format!(
                    "cohort {key} has {cases} cases, below required {}",
                    policy.minimum_cases_per_language_relationship
                ));
                continue;
            }
            let metrics = metrics.expect("cohort with cases must have metrics");
            if policy.require_positive_and_negative_per_language_relationship
                && (metrics.positive_cases == 0 || metrics.negative_cases == 0)
            {
                failures.push(format!(
                    "cohort {key} must contain both positive and negative/ambiguous cases"
                ));
            }
            if metrics.true_positives + metrics.false_positives == 0 {
                failures.push(format!(
                    "cohort {key} emitted no authoritative relationship; precision cannot be release-gated"
                ));
            } else if metrics.precision < policy.minimum_language_relationship_precision {
                failures.push(format!(
                    "cohort {key} authoritative precision {:.4} is below required {:.4}",
                    metrics.precision, policy.minimum_language_relationship_precision
                ));
            }
        }
    }
    failures.sort();
    failures.dedup();
    RelationshipBenchGateReport {
        policy_schema_version: policy.schema_version.clone(),
        passed: failures.is_empty(),
        failures,
    }
}

fn run_relationship_bench_command(
    args: RelationshipBenchArgs,
    json: bool,
) -> anyhow::Result<()> {
''',
    "gate policy functions",
)

replace_exact(
    relationship,
    '''    let (metadata, observations) = input.into_parts();
    let report = score_relationship_bench_with_metadata(&corpus, &observations, metadata)?;
    let rendered = serde_json::to_string_pretty(&report)?;
''',
    '''    let (metadata, observations) = input.into_parts();
    let mut report = score_relationship_bench_with_metadata(&corpus, &observations, metadata)?;
    if let Some(policy_path) = &args.policy {
        let policy = load_relationship_bench_policy(policy_path)?;
        report.gate = Some(evaluate_relationship_bench_gates(&corpus, &report, &policy));
    } else if args.enforce_gates {
        anyhow::bail!("--enforce-gates requires --policy");
    }
    let rendered = serde_json::to_string_pretty(&report)?;
''',
    "gate evaluation in command",
)

replace_exact(
    relationship,
    '''        if !report.diagnostics.is_empty() {
            println!("Diagnostics: {}", report.diagnostics.len());
''',
    '''        if let Some(gate) = &report.gate {
            println!(
                "Release gates: {}{}",
                if gate.passed { "PASS" } else { "FAIL" },
                if gate.failures.is_empty() {
                    String::new()
                } else {
                    format!(" ({} failure(s))", gate.failures.len())
                }
            );
            for failure in gate.failures.iter().take(20) {
                println!("- gate: {failure}");
            }
        }
        if !report.diagnostics.is_empty() {
            println!("Diagnostics: {}", report.diagnostics.len());
''',
    "gate text rendering",
)

replace_exact(
    relationship,
    '''    }
    Ok(())
}

fn validate_relationship_bench_corpus''',
    '''    }
    if args.enforce_gates {
        let gate = report
            .gate
            .as_ref()
            .expect("enforced relationship benchmark has a gate report");
        if !gate.passed {
            anyhow::bail!(
                "relationship benchmark failed {} release gate(s): {}",
                gate.failures.len(),
                gate.failures.join("; ")
            );
        }
    }
    Ok(())
}

fn validate_relationship_bench_corpus''',
    "gate enforcement",
)

replace_exact(
    relationship,
    '''        wrong_target_counts,
        diagnostics,
    })
}
''',
    '''        wrong_target_counts,
        diagnostics,
        gate: None,
    })
}
''',
    "score report gate default",
)

# Add release-gate regression tests before the module closes.
p = Path(relationship)
text = p.read_text()
anchor = '''    #[test]
    fn scoring_and_digest_are_independent_of_input_order() {
'''
if text.count(anchor) != 1:
    raise SystemExit(f"relationship test anchor changed: expected 1, observed {text.count(anchor)}")
insert = '''    fn permissive_test_policy() -> RelationshipBenchPolicy {
        RelationshipBenchPolicy {
            schema_version: RELATIONSHIP_BENCH_POLICY_SCHEMA_VERSION.into(),
            minimum_cases: 0,
            minimum_cases_per_language: 0,
            minimum_cases_per_language_relationship: 0,
            minimum_negative_fraction: 0.0,
            minimum_overall_precision: 0.0,
            minimum_language_relationship_precision: 0.0,
            maximum_must_not_emit_false_positive_rate: 1.0,
            minimum_exact_range_compliance: 0.0,
            minimum_proof_compliance: 0.0,
            minimum_outcome_compliance: 0.0,
            require_zero_false_negatives: false,
            require_positive_and_negative_per_language_relationship: false,
            require_frozen_corpus: false,
        }
    }

    #[test]
    fn release_gate_does_not_treat_no_emission_precision_as_evidence() {
        let corpus = corpus(vec![case(
            "negative-only",
            RelationshipBenchExpectedOutcome::MustNotEmit,
        )]);
        let observations = vec![RelationshipBenchObservation {
            case_id: "negative-only".into(),
            outcome: RelationshipBenchObservedOutcome::Unresolved,
            candidate_count: 0,
            relationships: Vec::new(),
        }];
        let report = score_relationship_bench(&corpus, &observations).unwrap();
        assert_eq!(report.overall.precision, 1.0);
        let gate = evaluate_relationship_bench_gates(&corpus, &report, &permissive_test_policy());
        assert!(!gate.passed);
        assert!(gate.failures.iter().any(|failure| {
            failure.contains("precision cannot be release-gated")
        }));
    }

    #[test]
    fn release_gate_fails_when_a_required_relationship_is_missing() {
        let corpus = corpus(vec![case(
            "missing-positive",
            RelationshipBenchExpectedOutcome::MustEmit,
        )]);
        let observations = vec![RelationshipBenchObservation {
            case_id: "missing-positive".into(),
            outcome: RelationshipBenchObservedOutcome::Unresolved,
            candidate_count: 1,
            relationships: Vec::new(),
        }];
        let report = score_relationship_bench(&corpus, &observations).unwrap();
        let mut policy = permissive_test_policy();
        policy.require_zero_false_negatives = true;
        let gate = evaluate_relationship_bench_gates(&corpus, &report, &policy);
        assert!(!gate.passed);
        assert!(gate.failures.iter().any(|failure| failure.contains("required authoritative")));
    }

'''
text = text.replace(anchor, insert + anchor, 1)
p.write_text(text)

# CLI flags are additive: scoring remains usable without a release policy; enforcement is explicit.
replace_exact(
    "crates/open-kioku-cli/src/types.rs",
    '''    /// Optional path for the deterministic JSON score report.
    #[arg(long, value_name = "REPORT_JSON")]
    write: Option<PathBuf>,
}
''',
    '''    /// Optional path for the deterministic JSON score report.
    #[arg(long, value_name = "REPORT_JSON")]
    write: Option<PathBuf>,

    /// Versioned JSON release-gate policy. When supplied, gate results are included in the report.
    #[arg(long, value_name = "POLICY_JSON")]
    policy: Option<PathBuf>,

    /// Exit non-zero unless every configured release gate passes.
    #[arg(long, default_value_t = false)]
    enforce_gates: bool,
}
''',
    "relationship bench gate args",
)
