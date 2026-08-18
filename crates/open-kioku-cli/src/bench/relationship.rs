const RELATIONSHIP_BENCH_SCHEMA_VERSION: &str = "2.0.0";
const RELATIONSHIP_BENCH_POLICY_SCHEMA_VERSION: &str = "2.0.0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RelationshipBenchSplit {
    Development,
    Calibration,
    Holdout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RelationshipBenchLanguage {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Java,
    Go,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RelationshipBenchEndpointKind {
    Symbol,
    File,
    Module,
    Package,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RelationshipBenchEndpoint {
    kind: RelationshipBenchEndpointKind,
    /// Fixture-local selector used only by the live observation producer.
    selector: String,
    /// Stable logical identity used by scoring and metamorphic comparison.
    identity: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RelationshipBenchCapabilityState {
    Authoritative,
    Corroborating,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RelationshipBenchExpectedOutcome {
    MustEmit,
    MustNotEmit,
    MayEmitHeuristicOnly,
    AmbiguousNoAuthoritativeEdge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum RelationshipBenchObservedOutcome {
    Proven,
    Ambiguous,
    #[default]
    Unresolved,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum RelationshipBenchCorpusStatus {
    #[default]
    Development,
    Frozen,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RelationshipBenchCorpus {
    schema_version: String,
    corpus_version: String,
    #[serde(default)]
    status: RelationshipBenchCorpusStatus,
    cases: Vec<RelationshipBenchCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RelationshipBenchCase {
    id: String,
    fixture_id: String,
    split: RelationshipBenchSplit,
    language: RelationshipBenchLanguage,
    relationship: GraphEdgeType,
    capability_state: RelationshipBenchCapabilityState,
    source: RelationshipBenchEndpoint,
    expected_outcome: RelationshipBenchExpectedOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_target: Option<RelationshipBenchEndpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_source_range: Option<open_kioku_core::SourceRange>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    expected_proof_kinds: BTreeSet<open_kioku_core::RelationshipProofKind>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    forbidden_proof_kinds: BTreeSet<open_kioku_core::RelationshipProofKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    candidate_count_expected: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metamorphic_group: Option<String>,
    /// Versioned adversarial/scenario family exercised by the live producer.
    scenario: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RelationshipBenchObservedRelationship {
    source_identity: String,
    target_identity: String,
    relationship: GraphEdgeType,
    authority: open_kioku_core::RelationshipAuthority,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    proof_kinds: BTreeSet<open_kioku_core::RelationshipProofKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_ranges: Vec<open_kioku_core::SourceRange>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    resolver_strategies: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RelationshipBenchObservation {
    case_id: String,
    #[serde(default)]
    outcome: RelationshipBenchObservedOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    candidate_count: Option<usize>,
    #[serde(default)]
    relationships: Vec<RelationshipBenchObservedRelationship>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RelationshipBenchRunMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    analysis_semantics_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    adapter_versions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proof_policy_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index_mode: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    index_config: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RelationshipBenchObservationSet {
    #[serde(default)]
    metadata: RelationshipBenchRunMetadata,
    observations: Vec<RelationshipBenchObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum RelationshipBenchObservationInput {
    Set(RelationshipBenchObservationSet),
    Legacy(Vec<RelationshipBenchObservation>),
}

impl RelationshipBenchObservationInput {
    fn into_parts(self) -> (RelationshipBenchRunMetadata, Vec<RelationshipBenchObservation>) {
        match self {
            Self::Set(set) => (set.metadata, set.observations),
            Self::Legacy(observations) => (RelationshipBenchRunMetadata::default(), observations),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
struct RelationshipBenchMetrics {
    cases: usize,
    positive_cases: usize,
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
    negative_cases: usize,
    negative_cases_with_false_positive: usize,
    must_not_emit_cases: usize,
    must_not_emit_cases_with_false_positive: usize,
    ambiguity_expected_cases: usize,
    ambiguity_collapsed_cases: usize,
    observed_ambiguous_cases: usize,
    observed_unresolved_cases: usize,
    candidate_count_total: usize,
    candidate_count_cases: usize,
    candidate_count_expected_cases: usize,
    candidate_count_matches: usize,
    outcome_cases: usize,
    outcome_matches: usize,
    exact_range_cases: usize,
    exact_range_matches: usize,
    proof_cases: usize,
    proof_matches: usize,
    precision: f64,
    recall: f64,
    f1: f64,
    false_positive_rate: f64,
    false_negative_rate: f64,
    must_not_emit_false_positive_rate: f64,
    ambiguity_rate: f64,
    unresolved_rate: f64,
    average_candidate_count: Option<f64>,
    candidate_count_compliance: Option<f64>,
    outcome_compliance: f64,
    exact_range_compliance: f64,
    proof_compliance: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct RelationshipBenchStrategyMetrics {
    authoritative_emissions: usize,
    correct_authoritative: usize,
    wrong_authoritative: usize,
    precision: f64,
}

#[derive(Debug, Clone, Serialize)]
struct RelationshipBenchDiagnostic {
    case_id: String,
    kind: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_target_identity: Option<String>,
    observed_authoritative_targets: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RelationshipBenchCapabilityReport {
    state: RelationshipBenchCapabilityState,
    cases: usize,
    positive_cases: usize,
    negative_cases: usize,
    authoritative_emissions: usize,
    precision: f64,
    passed: bool,
    failures: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
    minimum_metamorphic_groups: usize,
    minimum_metamorphic_equivalence: f64,
    require_zero_false_negatives: bool,
    require_positive_and_negative_per_authoritative_cohort: bool,
    require_metamorphic_group_per_language_relationship: bool,
    require_non_authoritative_cohorts_fail_closed: bool,
    require_reproducibility_metadata: bool,
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
    schema_version: String,
    corpus_version: String,
    corpus_status: RelationshipBenchCorpusStatus,
    run_metadata: RelationshipBenchRunMetadata,
    observation_digest: String,
    overall: RelationshipBenchMetrics,
    by_language: BTreeMap<String, RelationshipBenchMetrics>,
    by_relationship: BTreeMap<String, RelationshipBenchMetrics>,
    by_language_relationship: BTreeMap<String, RelationshipBenchMetrics>,
    capabilities: BTreeMap<String, RelationshipBenchCapabilityReport>,
    by_resolver_strategy: BTreeMap<String, RelationshipBenchStrategyMetrics>,
    by_proof_kind: BTreeMap<String, RelationshipBenchStrategyMetrics>,
    observed_proof_kind_counts: BTreeMap<String, usize>,
    wrong_target_counts: BTreeMap<String, usize>,
    metamorphic_groups: usize,
    metamorphic_equivalent_groups: usize,
    metamorphic_equivalence: f64,
    diagnostics: Vec<RelationshipBenchDiagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gate: Option<RelationshipBenchGateReport>,
}

fn load_relationship_bench_corpus(path: &Path) -> anyhow::Result<RelationshipBenchCorpus> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read relationship benchmark corpus {}", path.display()))?;
    let corpus: RelationshipBenchCorpus = serde_json::from_str(&raw)
        .with_context(|| format!("invalid relationship benchmark corpus {}", path.display()))?;
    validate_relationship_bench_corpus(&corpus)?;
    Ok(corpus)
}

fn load_relationship_bench_policy(path: &Path) -> anyhow::Result<RelationshipBenchPolicy> {
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
        (
            "minimum_metamorphic_equivalence",
            policy.minimum_metamorphic_equivalence,
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
    if policy.require_reproducibility_metadata {
        for (name, value) in [
            ("git_commit", report.run_metadata.git_commit.as_deref()),
            (
                "analysis_semantics_fingerprint",
                report.run_metadata.analysis_semantics_fingerprint.as_deref(),
            ),
            (
                "proof_policy_version",
                report.run_metadata.proof_policy_version.as_deref(),
            ),
            ("index_mode", report.run_metadata.index_mode.as_deref()),
        ] {
            if value.map(str::trim).filter(|value| !value.is_empty()).is_none() {
                failures.push(format!("run metadata is missing required {name}"));
            }
        }
        if report.run_metadata.adapter_versions.len() < 6 {
            failures.push("run metadata must identify all six Tier-1 language adapters".to_string());
        }
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
    if report.metamorphic_groups < policy.minimum_metamorphic_groups {
        failures.push(format!(
            "corpus has {} metamorphic groups, below required {}",
            report.metamorphic_groups, policy.minimum_metamorphic_groups
        ));
    }
    if report.metamorphic_equivalence < policy.minimum_metamorphic_equivalence {
        failures.push(format!(
            "metamorphic authoritative-edge/proof equivalence {:.4} is below required {:.4}",
            report.metamorphic_equivalence, policy.minimum_metamorphic_equivalence
        ));
    }

    const LANGUAGES: [&str; 6] = ["rust", "typescript", "javascript", "python", "java", "go"];
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
            let Some(metrics) = report.by_language_relationship.get(&key) else {
                failures.push(format!(
                    "cohort {key} has 0 cases, below required {}",
                    policy.minimum_cases_per_language_relationship
                ));
                continue;
            };
            if metrics.cases < policy.minimum_cases_per_language_relationship {
                failures.push(format!(
                    "cohort {key} has {} cases, below required {}",
                    metrics.cases, policy.minimum_cases_per_language_relationship
                ));
                continue;
            }
            if policy.require_metamorphic_group_per_language_relationship
                && !corpus.cases.iter().any(|case| {
                    language_name(case.language) == language
                        && edge_type_name(&case.relationship) == relationship
                        && case.metamorphic_group.is_some()
                })
            {
                failures.push(format!("cohort {key} has no metamorphic group"));
            }
            let Some(capability) = report.capabilities.get(&key) else {
                failures.push(format!("cohort {key} has no capability verdict"));
                continue;
            };
            match capability.state {
                RelationshipBenchCapabilityState::Authoritative => {
                    if policy.require_positive_and_negative_per_authoritative_cohort
                        && (metrics.positive_cases == 0 || metrics.negative_cases == 0)
                    {
                        failures.push(format!(
                            "authoritative cohort {key} must contain both positive and negative/ambiguous cases"
                        ));
                    }
                    if metrics.true_positives + metrics.false_positives == 0 {
                        failures.push(format!(
                            "authoritative cohort {key} emitted no authoritative relationship; precision cannot be release-gated"
                        ));
                    } else if metrics.precision < policy.minimum_language_relationship_precision {
                        failures.push(format!(
                            "cohort {key} authoritative precision {:.4} is below required {:.4}",
                            metrics.precision, policy.minimum_language_relationship_precision
                        ));
                    }
                }
                RelationshipBenchCapabilityState::Corroborating
                | RelationshipBenchCapabilityState::Unsupported => {
                    if metrics.positive_cases != 0 {
                        failures.push(format!(
                            "non-authoritative cohort {key} contains {} MustEmit case(s)",
                            metrics.positive_cases
                        ));
                    }
                    if policy.require_non_authoritative_cohorts_fail_closed
                        && capability.authoritative_emissions != 0
                    {
                        failures.push(format!(
                            "non-authoritative cohort {key} emitted {} authoritative relationship(s)",
                            capability.authoritative_emissions
                        ));
                    }
                }
            }
            failures.extend(
                capability
                    .failures
                    .iter()
                    .map(|failure| format!("cohort {key}: {failure}")),
            );
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

fn run_relationship_bench_command(args: RelationshipBenchArgs, json: bool) -> anyhow::Result<()> {
    let corpus = load_relationship_bench_corpus(&args.corpus)?;
    let (metadata, observations) = if is_live_relationship_observation_path(&args.observations) {
        let live = produce_live_relationship_observations(&corpus)?;
        (live.metadata, live.observations)
    } else {
        let raw = fs::read_to_string(&args.observations).with_context(|| {
            format!(
                "failed to read relationship benchmark observations {}",
                args.observations.display()
            )
        })?;
        let input: RelationshipBenchObservationInput = serde_json::from_str(&raw).with_context(|| {
            format!(
                "invalid relationship benchmark observations {}",
                args.observations.display()
            )
        })?;
        input.into_parts()
    };
    let mut report = score_relationship_bench_with_metadata(&corpus, &observations, metadata)?;
    if let Some(policy_path) = &args.policy {
        let policy = load_relationship_bench_policy(policy_path)?;
        report.gate = Some(evaluate_relationship_bench_gates(&corpus, &report, &policy));
    } else if args.enforce_gates {
        anyhow::bail!("--enforce-gates requires --policy");
    }
    let rendered = serde_json::to_string_pretty(&report)?;

    if let Some(path) = &args.write {
        fs::write(path, &rendered).with_context(|| {
            format!("failed to write relationship benchmark report {}", path.display())
        })?;
        write_relationship_bench_companion_reports(path, &report)?;
    }

    if json {
        println!("{rendered}");
    } else {
        println!(
            "Relationship conformance: {} cases | precision {:.4} | recall {:.4}",
            report.overall.cases, report.overall.precision, report.overall.recall
        );
        println!(
            "MustNotEmit FP {:.4} | exact ranges {:.4} | proofs {:.4} | metamorphic {:.4}",
            report.overall.must_not_emit_false_positive_rate,
            report.overall.exact_range_compliance,
            report.overall.proof_compliance,
            report.metamorphic_equivalence,
        );
        println!("Observation digest: {}", report.observation_digest);
        if let Some(path) = &args.write {
            println!("Wrote report to {} (+ Markdown/capability companions)", path.display());
        }
        if let Some(gate) = &report.gate {
            println!(
                "Release gates: {}{}",
                if gate.passed { "PASS" } else { "FAIL" },
                if gate.failures.is_empty() {
                    String::new()
                } else {
                    format!(" ({} failure(s))", gate.failures.len())
                }
            );
            for failure in gate.failures.iter().take(30) {
                println!("- gate: {failure}");
            }
        }
        if !report.diagnostics.is_empty() {
            println!("Diagnostics: {}", report.diagnostics.len());
            for diagnostic in report.diagnostics.iter().take(30) {
                println!(
                    "- {} [{}] {}",
                    diagnostic.case_id, diagnostic.kind, diagnostic.message
                );
            }
        }
    }
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

fn validate_relationship_bench_corpus(corpus: &RelationshipBenchCorpus) -> anyhow::Result<()> {
    if corpus.schema_version != RELATIONSHIP_BENCH_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported relationship benchmark schema version {}; expected {}",
            corpus.schema_version,
            RELATIONSHIP_BENCH_SCHEMA_VERSION
        );
    }
    if corpus.corpus_version.trim().is_empty() {
        anyhow::bail!("relationship benchmark corpus_version must not be empty");
    }
    if corpus.cases.is_empty() {
        anyhow::bail!("relationship benchmark corpus must contain at least one case");
    }

    let mut ids = BTreeSet::new();
    let mut cohort_capabilities = BTreeMap::<String, RelationshipBenchCapabilityState>::new();
    let mut metamorphic_contracts = BTreeMap::<
        String,
        (
            RelationshipBenchLanguage,
            String,
            RelationshipBenchExpectedOutcome,
            RelationshipBenchCapabilityState,
        ),
    >::new();
    let mut metamorphic_sizes = BTreeMap::<String, usize>::new();
    for case in &corpus.cases {
        if case.id.trim().is_empty() {
            anyhow::bail!("relationship benchmark case id must not be empty");
        }
        if case.fixture_id.trim().is_empty() {
            anyhow::bail!("case {} has an empty fixture_id", case.id);
        }
        if !ids.insert(case.id.clone()) {
            anyhow::bail!("duplicate relationship benchmark case id: {}", case.id);
        }
        validate_endpoint(&case.id, "source", &case.source)?;
        if case.scenario.trim().is_empty() {
            anyhow::bail!("case {} has an empty scenario", case.id);
        }
        if !is_conformance_relationship(&case.relationship) {
            anyhow::bail!(
                "case {} uses unsupported relationship {:?}",
                case.id,
                case.relationship
            );
        }
        let cohort = format!(
            "{}::{}",
            language_name(case.language),
            edge_type_name(&case.relationship)
        );
        if let Some(existing) = cohort_capabilities.get(&cohort) {
            if *existing != case.capability_state {
                anyhow::bail!("cohort {cohort} mixes capability states");
            }
        } else {
            cohort_capabilities.insert(cohort, case.capability_state);
        }
        if case.capability_state != RelationshipBenchCapabilityState::Authoritative
            && case.expected_outcome == RelationshipBenchExpectedOutcome::MustEmit
        {
            anyhow::bail!(
                "non-authoritative case {} cannot require authoritative emission",
                case.id
            );
        }
        if let Some(group) = case.metamorphic_group.as_deref() {
            if group.trim().is_empty() {
                anyhow::bail!("case {} has an empty metamorphic_group", case.id);
            }
            *metamorphic_sizes.entry(group.to_string()).or_default() += 1;
            let contract = (
                case.language,
                edge_type_name(&case.relationship).to_string(),
                case.expected_outcome,
                case.capability_state,
            );
            if let Some(existing) = metamorphic_contracts.get(group) {
                if existing != &contract {
                    anyhow::bail!(
                        "metamorphic group {group} mixes language, relationship, outcome, or capability contracts"
                    );
                }
            } else {
                metamorphic_contracts.insert(group.to_string(), contract);
            }
        }
        match case.expected_outcome {
            RelationshipBenchExpectedOutcome::MustEmit => {
                let Some(target) = &case.expected_target else {
                    anyhow::bail!("MustEmit case {} must declare expected_target", case.id);
                };
                validate_endpoint(&case.id, "expected_target", target)?;
            }
            RelationshipBenchExpectedOutcome::MustNotEmit
            | RelationshipBenchExpectedOutcome::MayEmitHeuristicOnly
            | RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge => {
                if case.expected_target.is_some() {
                    anyhow::bail!("non-emission case {} must not declare expected_target", case.id);
                }
                if case.expected_source_range.is_some() || !case.expected_proof_kinds.is_empty() {
                    anyhow::bail!(
                        "non-emission case {} cannot require source ranges or proof kinds",
                        case.id
                    );
                }
            }
        }
    }
    for (group, size) in metamorphic_sizes {
        if size < 2 {
            anyhow::bail!("metamorphic group {group} must contain at least two cases");
        }
    }
    Ok(())
}

fn validate_endpoint(
    case_id: &str,
    label: &str,
    endpoint: &RelationshipBenchEndpoint,
) -> anyhow::Result<()> {
    if endpoint.selector.trim().is_empty() {
        anyhow::bail!("case {case_id} has an empty {label}.selector");
    }
    if endpoint.identity.trim().is_empty() {
        anyhow::bail!("case {case_id} has an empty {label}.identity");
    }
    Ok(())
}

fn score_relationship_bench_with_metadata(
    corpus: &RelationshipBenchCorpus,
    observations: &[RelationshipBenchObservation],
    run_metadata: RelationshipBenchRunMetadata,
) -> anyhow::Result<RelationshipBenchScoreReport> {
    validate_relationship_bench_corpus(corpus)?;

    let cases_by_id = corpus
        .cases
        .iter()
        .map(|case| (case.id.as_str(), case))
        .collect::<BTreeMap<_, _>>();
    let mut observations_by_id = BTreeMap::<&str, Vec<RelationshipBenchObservedRelationship>>::new();
    for observation in observations {
        if !cases_by_id.contains_key(observation.case_id.as_str()) {
            anyhow::bail!("observation references unknown case id: {}", observation.case_id);
        }
        if observations_by_id.contains_key(observation.case_id.as_str()) {
            anyhow::bail!("duplicate observation for case id: {}", observation.case_id);
        }
        let mut relationships = observation.relationships.clone();
        normalize_observed_relationships(&mut relationships);
        observations_by_id.insert(observation.case_id.as_str(), relationships);
    }
    if observations_by_id.len() != corpus.cases.len() {
        let missing = corpus
            .cases
            .iter()
            .filter(|case| !observations_by_id.contains_key(case.id.as_str()))
            .map(|case| case.id.clone())
            .take(20)
            .collect::<Vec<_>>();
        anyhow::bail!(
            "relationship observations are incomplete: {} of {} cases observed; missing {:?}",
            observations_by_id.len(),
            corpus.cases.len(),
            missing
        );
    }

    let mut overall = RelationshipBenchMetrics::default();
    let mut by_language = BTreeMap::<String, RelationshipBenchMetrics>::new();
    let mut by_relationship = BTreeMap::<String, RelationshipBenchMetrics>::new();
    let mut by_language_relationship = BTreeMap::<String, RelationshipBenchMetrics>::new();
    let mut observed_proof_kind_counts = BTreeMap::<String, usize>::new();
    let mut by_resolver_strategy = BTreeMap::<String, RelationshipBenchStrategyMetrics>::new();
    let mut by_proof_kind = BTreeMap::<String, RelationshipBenchStrategyMetrics>::new();
    let mut wrong_target_counts = BTreeMap::<String, usize>::new();
    let mut metamorphic_observations = BTreeMap::<String, Vec<Vec<ObservedRelationshipKey>>>::new();
    let mut diagnostics = Vec::new();

    let observation_lookup = observations
        .iter()
        .map(|observation| (observation.case_id.as_str(), observation))
        .collect::<BTreeMap<_, _>>();
    let mut cases = corpus.cases.iter().collect::<Vec<_>>();
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    for case in cases {
        let relationships = observations_by_id
            .get(case.id.as_str())
            .map(Vec::as_slice)
            .expect("complete observation set validated above");
        for relationship in relationships {
            for proof_kind in &relationship.proof_kinds {
                *observed_proof_kind_counts
                    .entry(proof_kind_name(proof_kind).to_string())
                    .or_default() += 1;
            }
        }

        let observation = observation_lookup.get(case.id.as_str()).copied();
        let outcome = score_relationship_case(case, observation, relationships);
        if let Some(group) = case.metamorphic_group.as_ref() {
            let authoritative = relationships
                .iter()
                .filter(|relationship| {
                    relationship.authority
                        == open_kioku_core::RelationshipAuthority::Authoritative
                })
                .map(observed_relationship_key)
                .collect::<Vec<_>>();
            metamorphic_observations
                .entry(group.clone())
                .or_default()
                .push(authoritative);
        }
        for relationship in relationships.iter().filter(|relationship| {
            relationship.authority == open_kioku_core::RelationshipAuthority::Authoritative
        }) {
            let correct = authoritative_relationship_matches_case(case, relationship);
            if !correct {
                *wrong_target_counts
                    .entry(relationship.target_identity.clone())
                    .or_default() += 1;
            }
            let strategies = if relationship.resolver_strategies.is_empty() {
                vec!["<unspecified>".to_string()]
            } else {
                relationship.resolver_strategies.iter().cloned().collect()
            };
            for strategy in strategies {
                update_strategy_metrics(by_resolver_strategy.entry(strategy).or_default(), correct);
            }
            for proof_kind in &relationship.proof_kinds {
                update_strategy_metrics(
                    by_proof_kind
                        .entry(proof_kind_name(proof_kind).to_string())
                        .or_default(),
                    correct,
                );
            }
        }
        merge_relationship_metrics(&mut overall, &outcome.metrics);
        merge_relationship_metrics(
            by_language
                .entry(language_name(case.language).to_string())
                .or_default(),
            &outcome.metrics,
        );
        merge_relationship_metrics(
            by_relationship
                .entry(edge_type_name(&case.relationship).to_string())
                .or_default(),
            &outcome.metrics,
        );
        merge_relationship_metrics(
            by_language_relationship
                .entry(format!(
                    "{}::{}",
                    language_name(case.language),
                    edge_type_name(&case.relationship)
                ))
                .or_default(),
            &outcome.metrics,
        );
        diagnostics.extend(outcome.diagnostics);
    }

    finalize_relationship_metrics(&mut overall);
    for metrics in by_language.values_mut() {
        finalize_relationship_metrics(metrics);
    }
    for metrics in by_relationship.values_mut() {
        finalize_relationship_metrics(metrics);
    }
    for metrics in by_language_relationship.values_mut() {
        finalize_relationship_metrics(metrics);
    }
    for metrics in by_resolver_strategy.values_mut() {
        finalize_strategy_metrics(metrics);
    }
    for metrics in by_proof_kind.values_mut() {
        finalize_strategy_metrics(metrics);
    }
    diagnostics.sort_by(|left, right| {
        (&left.case_id, &left.kind, &left.message).cmp(&(&right.case_id, &right.kind, &right.message))
    });

    let metamorphic_groups = metamorphic_observations.len();
    let metamorphic_equivalent_groups = metamorphic_observations
        .values()
        .filter(|variants| {
            variants
                .first()
                .map(|first| variants.iter().all(|variant| variant == first))
                .unwrap_or(false)
        })
        .count();
    let metamorphic_equivalence = relationship_ratio(metamorphic_equivalent_groups, metamorphic_groups);

    let capabilities = build_capability_reports(corpus, &by_language_relationship, observations);

    Ok(RelationshipBenchScoreReport {
        schema_version: corpus.schema_version.clone(),
        corpus_version: corpus.corpus_version.clone(),
        corpus_status: corpus.status,
        run_metadata,
        observation_digest: relationship_observation_digest(observations)?,
        overall,
        by_language,
        by_relationship,
        by_language_relationship,
        capabilities,
        by_resolver_strategy,
        by_proof_kind,
        observed_proof_kind_counts,
        wrong_target_counts,
        metamorphic_groups,
        metamorphic_equivalent_groups,
        metamorphic_equivalence,
        diagnostics,
        gate: None,
    })
}

fn build_capability_reports(
    corpus: &RelationshipBenchCorpus,
    metrics_by_cohort: &BTreeMap<String, RelationshipBenchMetrics>,
    observations: &[RelationshipBenchObservation],
) -> BTreeMap<String, RelationshipBenchCapabilityReport> {
    let obs_by_id = observations
        .iter()
        .map(|observation| (observation.case_id.as_str(), observation))
        .collect::<BTreeMap<_, _>>();
    let mut states = BTreeMap::<String, RelationshipBenchCapabilityState>::new();
    let mut authoritative_emissions = BTreeMap::<String, usize>::new();
    for case in &corpus.cases {
        let key = format!(
            "{}::{}",
            language_name(case.language),
            edge_type_name(&case.relationship)
        );
        states.entry(key.clone()).or_insert(case.capability_state);
        if let Some(observation) = obs_by_id.get(case.id.as_str()) {
            let count = observation
                .relationships
                .iter()
                .filter(|relationship| {
                    relationship.authority
                        == open_kioku_core::RelationshipAuthority::Authoritative
                })
                .count();
            *authoritative_emissions.entry(key).or_default() += count;
        }
    }

    let mut reports = BTreeMap::new();
    for (key, state) in states {
        let metrics = metrics_by_cohort.get(&key).cloned().unwrap_or_default();
        let emissions = authoritative_emissions.get(&key).copied().unwrap_or(0);
        let mut failures = Vec::new();
        match state {
            RelationshipBenchCapabilityState::Authoritative => {
                if metrics.positive_cases == 0 {
                    failures.push("authoritative capability has no positive conformance case".into());
                }
                if metrics.false_negatives > 0 {
                    failures.push(format!(
                        "{} expected authoritative relationship(s) were missing",
                        metrics.false_negatives
                    ));
                }
            }
            RelationshipBenchCapabilityState::Corroborating
            | RelationshipBenchCapabilityState::Unsupported => {
                if metrics.positive_cases > 0 {
                    failures.push("non-authoritative capability contains MustEmit cases".into());
                }
                if emissions > 0 {
                    failures.push(format!(
                        "non-authoritative capability emitted {emissions} authoritative relationship(s)"
                    ));
                }
            }
        }
        reports.insert(
            key,
            RelationshipBenchCapabilityReport {
                state,
                cases: metrics.cases,
                positive_cases: metrics.positive_cases,
                negative_cases: metrics.negative_cases,
                authoritative_emissions: emissions,
                precision: metrics.precision,
                passed: failures.is_empty(),
                failures,
            },
        );
    }
    reports
}

#[derive(Default)]
struct RelationshipCaseScore {
    metrics: RelationshipBenchMetrics,
    diagnostics: Vec<RelationshipBenchDiagnostic>,
}

fn score_relationship_case(
    case: &RelationshipBenchCase,
    observation: Option<&RelationshipBenchObservation>,
    relationships: &[RelationshipBenchObservedRelationship],
) -> RelationshipCaseScore {
    let mut score = RelationshipCaseScore::default();
    score.metrics.cases = 1;
    if let Some(candidate_count) = observation.and_then(|value| value.candidate_count) {
        score.metrics.candidate_count_cases = 1;
        score.metrics.candidate_count_total = candidate_count;
        if let Some(expected) = case.candidate_count_expected {
            score.metrics.candidate_count_expected_cases = 1;
            if candidate_count == expected {
                score.metrics.candidate_count_matches = 1;
            } else {
                score.diagnostics.push(RelationshipBenchDiagnostic {
                    case_id: case.id.clone(),
                    kind: "candidate_count_mismatch".into(),
                    message: format!("expected {expected} candidates but observed {candidate_count}"),
                    expected_target_identity: case
                        .expected_target
                        .as_ref()
                        .map(|target| target.identity.clone()),
                    observed_authoritative_targets: Vec::new(),
                });
            }
        }
    }
    score.metrics.outcome_cases = 1;
    let observed_outcome = observation.map(|value| value.outcome).unwrap_or_default();
    if observed_outcome_matches(case.expected_outcome, observed_outcome) {
        score.metrics.outcome_matches = 1;
    } else {
        score.diagnostics.push(RelationshipBenchDiagnostic {
            case_id: case.id.clone(),
            kind: "resolution_outcome_mismatch".into(),
            message: format!(
                "expected {:?} behavior but observed {:?}",
                case.expected_outcome, observed_outcome
            ),
            expected_target_identity: case
                .expected_target
                .as_ref()
                .map(|target| target.identity.clone()),
            observed_authoritative_targets: Vec::new(),
        });
    }
    if observed_outcome == RelationshipBenchObservedOutcome::Ambiguous {
        score.metrics.observed_ambiguous_cases = 1;
    }
    if observed_outcome == RelationshipBenchObservedOutcome::Unresolved {
        score.metrics.observed_unresolved_cases = 1;
    }
    let authoritative = relationships
        .iter()
        .filter(|relationship| {
            relationship.authority == open_kioku_core::RelationshipAuthority::Authoritative
        })
        .collect::<Vec<_>>();
    let observed_authoritative_targets = authoritative
        .iter()
        .map(|relationship| relationship.target_identity.clone())
        .collect::<Vec<_>>();

    match case.expected_outcome {
        RelationshipBenchExpectedOutcome::MustEmit => {
            score.metrics.positive_cases = 1;
            let expected_target = case
                .expected_target
                .as_ref()
                .expect("validated MustEmit case has a target");
            let correct = authoritative
                .iter()
                .copied()
                .filter(|relationship| {
                    relationship.relationship == case.relationship
                        && relationship.source_identity == case.source.identity
                        && relationship.target_identity == expected_target.identity
                })
                .collect::<Vec<_>>();
            if correct.is_empty() {
                score.metrics.false_negatives = 1;
                score.diagnostics.push(RelationshipBenchDiagnostic {
                    case_id: case.id.clone(),
                    kind: "missing_authoritative_relationship".into(),
                    message: "expected authoritative relationship was not emitted".into(),
                    expected_target_identity: Some(expected_target.identity.clone()),
                    observed_authoritative_targets: observed_authoritative_targets.clone(),
                });
            } else {
                score.metrics.true_positives = 1;
            }

            let wrong_authoritative = authoritative
                .iter()
                .filter(|relationship| {
                    relationship.relationship != case.relationship
                        || relationship.source_identity != case.source.identity
                        || relationship.target_identity != expected_target.identity
                })
                .count();
            score.metrics.false_positives += wrong_authoritative;
            if wrong_authoritative > 0 {
                score.diagnostics.push(RelationshipBenchDiagnostic {
                    case_id: case.id.clone(),
                    kind: "wrong_authoritative_relationship".into(),
                    message: format!(
                        "{} unexpected authoritative relationship(s) were emitted",
                        wrong_authoritative
                    ),
                    expected_target_identity: Some(expected_target.identity.clone()),
                    observed_authoritative_targets: observed_authoritative_targets.clone(),
                });
            }

            if let Some(expected_range) = &case.expected_source_range {
                score.metrics.exact_range_cases = 1;
                if correct.iter().any(|relationship| {
                    relationship
                        .source_ranges
                        .iter()
                        .any(|range| range == expected_range)
                }) {
                    score.metrics.exact_range_matches = 1;
                } else {
                    score.diagnostics.push(RelationshipBenchDiagnostic {
                        case_id: case.id.clone(),
                        kind: "source_range_mismatch".into(),
                        message: "authoritative relationship did not preserve the exact expected source range"
                            .into(),
                        expected_target_identity: Some(expected_target.identity.clone()),
                        observed_authoritative_targets: observed_authoritative_targets.clone(),
                    });
                }
            }

            if !case.expected_proof_kinds.is_empty() || !case.forbidden_proof_kinds.is_empty() {
                score.metrics.proof_cases = 1;
                if correct.iter().any(|relationship| {
                    case.expected_proof_kinds
                        .iter()
                        .all(|kind| relationship.proof_kinds.contains(kind))
                        && case
                            .forbidden_proof_kinds
                            .iter()
                            .all(|kind| !relationship.proof_kinds.contains(kind))
                }) {
                    score.metrics.proof_matches = 1;
                } else {
                    score.diagnostics.push(RelationshipBenchDiagnostic {
                        case_id: case.id.clone(),
                        kind: "proof_kind_mismatch".into(),
                        message: "authoritative relationship violated required/forbidden proof-kind expectations"
                            .into(),
                        expected_target_identity: Some(expected_target.identity.clone()),
                        observed_authoritative_targets,
                    });
                }
            }
        }
        RelationshipBenchExpectedOutcome::MustNotEmit
        | RelationshipBenchExpectedOutcome::MayEmitHeuristicOnly
        | RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge => {
            score.metrics.negative_cases = 1;
            if case.expected_outcome == RelationshipBenchExpectedOutcome::MustNotEmit {
                score.metrics.must_not_emit_cases = 1;
            }
            if case.expected_outcome == RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge {
                score.metrics.ambiguity_expected_cases = 1;
            }
            if !authoritative.is_empty() {
                score.metrics.false_positives = authoritative.len();
                score.metrics.negative_cases_with_false_positive = 1;
                if case.expected_outcome == RelationshipBenchExpectedOutcome::MustNotEmit {
                    score.metrics.must_not_emit_cases_with_false_positive = 1;
                }
                if case.expected_outcome == RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge {
                    score.metrics.ambiguity_collapsed_cases = 1;
                }
                score.diagnostics.push(RelationshipBenchDiagnostic {
                    case_id: case.id.clone(),
                    kind: match case.expected_outcome {
                        RelationshipBenchExpectedOutcome::MustNotEmit => "must_not_emit_violation".into(),
                        RelationshipBenchExpectedOutcome::MayEmitHeuristicOnly => {
                            "heuristic_only_case_became_authoritative".into()
                        }
                        RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge => {
                            "ambiguity_collapsed_to_authoritative_edge".into()
                        }
                        RelationshipBenchExpectedOutcome::MustEmit => unreachable!(),
                    },
                    message: format!(
                        "{} authoritative relationship(s) were emitted for a non-emission case",
                        authoritative.len()
                    ),
                    expected_target_identity: None,
                    observed_authoritative_targets,
                });
            }
        }
    }
    score
}

fn normalize_observed_relationships(relationships: &mut Vec<RelationshipBenchObservedRelationship>) {
    for relationship in relationships.iter_mut() {
        relationship.source_ranges.sort_by(|left, right| {
            (
                left.start_line,
                left.start_column,
                left.end_line,
                left.end_column,
            )
                .cmp(&(
                    right.start_line,
                    right.start_column,
                    right.end_line,
                    right.end_column,
                ))
        });
        relationship.source_ranges.dedup();
    }
    relationships.sort_by_key(observed_relationship_key);
    relationships.dedup();
}

fn relationship_observation_digest(observations: &[RelationshipBenchObservation]) -> anyhow::Result<String> {
    let mut normalized = observations.to_vec();
    for observation in &mut normalized {
        normalize_observed_relationships(&mut observation.relationships);
    }
    normalized.sort_by(|left, right| left.case_id.cmp(&right.case_id));
    let encoded = serde_json::to_vec(&normalized)?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

type ObservedRelationshipKey = (
    String,
    String,
    String,
    u8,
    Vec<String>,
    Vec<String>,
    Vec<(u32, u32, u32, u32)>,
);

fn observed_relationship_key(relationship: &RelationshipBenchObservedRelationship) -> ObservedRelationshipKey {
    (
        relationship.source_identity.clone(),
        relationship.target_identity.clone(),
        edge_type_name(&relationship.relationship).to_string(),
        authority_rank(relationship.authority),
        relationship
            .proof_kinds
            .iter()
            .map(|kind| proof_kind_name(kind).to_string())
            .collect(),
        relationship.resolver_strategies.iter().cloned().collect(),
        relationship
            .source_ranges
            .iter()
            .map(|range| {
                (
                    range.start_line,
                    range.start_column,
                    range.end_line,
                    range.end_column,
                )
            })
            .collect(),
    )
}

fn merge_relationship_metrics(target: &mut RelationshipBenchMetrics, source: &RelationshipBenchMetrics) {
    target.cases += source.cases;
    target.positive_cases += source.positive_cases;
    target.true_positives += source.true_positives;
    target.false_positives += source.false_positives;
    target.false_negatives += source.false_negatives;
    target.negative_cases += source.negative_cases;
    target.negative_cases_with_false_positive += source.negative_cases_with_false_positive;
    target.must_not_emit_cases += source.must_not_emit_cases;
    target.must_not_emit_cases_with_false_positive += source.must_not_emit_cases_with_false_positive;
    target.ambiguity_expected_cases += source.ambiguity_expected_cases;
    target.ambiguity_collapsed_cases += source.ambiguity_collapsed_cases;
    target.observed_ambiguous_cases += source.observed_ambiguous_cases;
    target.observed_unresolved_cases += source.observed_unresolved_cases;
    target.candidate_count_total += source.candidate_count_total;
    target.candidate_count_cases += source.candidate_count_cases;
    target.candidate_count_expected_cases += source.candidate_count_expected_cases;
    target.candidate_count_matches += source.candidate_count_matches;
    target.outcome_cases += source.outcome_cases;
    target.outcome_matches += source.outcome_matches;
    target.exact_range_cases += source.exact_range_cases;
    target.exact_range_matches += source.exact_range_matches;
    target.proof_cases += source.proof_cases;
    target.proof_matches += source.proof_matches;
}

fn finalize_relationship_metrics(metrics: &mut RelationshipBenchMetrics) {
    metrics.precision = relationship_ratio(
        metrics.true_positives,
        metrics.true_positives + metrics.false_positives,
    );
    metrics.recall = relationship_ratio(
        metrics.true_positives,
        metrics.true_positives + metrics.false_negatives,
    );
    metrics.f1 = if metrics.precision + metrics.recall == 0.0 {
        0.0
    } else {
        2.0 * metrics.precision * metrics.recall / (metrics.precision + metrics.recall)
    };
    metrics.false_positive_rate = if metrics.negative_cases == 0 {
        0.0
    } else {
        metrics.negative_cases_with_false_positive as f64 / metrics.negative_cases as f64
    };
    metrics.false_negative_rate = if metrics.positive_cases == 0 {
        0.0
    } else {
        metrics.false_negatives as f64 / metrics.positive_cases as f64
    };
    metrics.must_not_emit_false_positive_rate = if metrics.must_not_emit_cases == 0 {
        0.0
    } else {
        metrics.must_not_emit_cases_with_false_positive as f64 / metrics.must_not_emit_cases as f64
    };
    metrics.ambiguity_rate = relationship_ratio(metrics.observed_ambiguous_cases, metrics.cases);
    metrics.unresolved_rate = relationship_ratio(metrics.observed_unresolved_cases, metrics.cases);
    metrics.average_candidate_count = (metrics.candidate_count_cases > 0).then(|| {
        metrics.candidate_count_total as f64 / metrics.candidate_count_cases as f64
    });
    metrics.candidate_count_compliance = (metrics.candidate_count_expected_cases > 0).then(|| {
        metrics.candidate_count_matches as f64 / metrics.candidate_count_expected_cases as f64
    });
    metrics.outcome_compliance = relationship_ratio(metrics.outcome_matches, metrics.outcome_cases);
    metrics.exact_range_compliance = relationship_ratio(metrics.exact_range_matches, metrics.exact_range_cases);
    metrics.proof_compliance = relationship_ratio(metrics.proof_matches, metrics.proof_cases);
}

fn update_strategy_metrics(metrics: &mut RelationshipBenchStrategyMetrics, correct: bool) {
    metrics.authoritative_emissions += 1;
    if correct {
        metrics.correct_authoritative += 1;
    } else {
        metrics.wrong_authoritative += 1;
    }
}

fn finalize_strategy_metrics(metrics: &mut RelationshipBenchStrategyMetrics) {
    metrics.precision = relationship_ratio(metrics.correct_authoritative, metrics.authoritative_emissions);
}

fn authoritative_relationship_matches_case(
    case: &RelationshipBenchCase,
    relationship: &RelationshipBenchObservedRelationship,
) -> bool {
    case.expected_outcome == RelationshipBenchExpectedOutcome::MustEmit
        && case
            .expected_target
            .as_ref()
            .map(|target| {
                relationship.relationship == case.relationship
                    && relationship.source_identity == case.source.identity
                    && relationship.target_identity == target.identity
            })
            .unwrap_or(false)
}

fn observed_outcome_matches(
    expected: RelationshipBenchExpectedOutcome,
    observed: RelationshipBenchObservedOutcome,
) -> bool {
    match expected {
        RelationshipBenchExpectedOutcome::MustEmit => observed == RelationshipBenchObservedOutcome::Proven,
        RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge => {
            observed == RelationshipBenchObservedOutcome::Ambiguous
        }
        RelationshipBenchExpectedOutcome::MustNotEmit
        | RelationshipBenchExpectedOutcome::MayEmitHeuristicOnly => {
            observed != RelationshipBenchObservedOutcome::Proven
        }
    }
}

fn relationship_ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn is_conformance_relationship(edge_type: &GraphEdgeType) -> bool {
    matches!(
        edge_type,
        GraphEdgeType::Calls
            | GraphEdgeType::References
            | GraphEdgeType::UsesType
            | GraphEdgeType::Implements
            | GraphEdgeType::Extends
            | GraphEdgeType::Imports
            | GraphEdgeType::DependsOn
    )
}

fn language_name(language: RelationshipBenchLanguage) -> &'static str {
    match language {
        RelationshipBenchLanguage::Rust => "rust",
        RelationshipBenchLanguage::TypeScript => "typescript",
        RelationshipBenchLanguage::JavaScript => "javascript",
        RelationshipBenchLanguage::Python => "python",
        RelationshipBenchLanguage::Java => "java",
        RelationshipBenchLanguage::Go => "go",
    }
}

fn edge_type_name(edge_type: &GraphEdgeType) -> &'static str {
    match edge_type {
        GraphEdgeType::Calls => "CALLS",
        GraphEdgeType::References => "REFERENCES",
        GraphEdgeType::UsesType => "USES_TYPE",
        GraphEdgeType::Implements => "IMPLEMENTS",
        GraphEdgeType::Extends => "EXTENDS",
        GraphEdgeType::Imports => "IMPORTS",
        GraphEdgeType::DependsOn => "DEPENDS_ON",
        _ => "OTHER",
    }
}

fn proof_kind_name(kind: &open_kioku_core::RelationshipProofKind) -> &'static str {
    use open_kioku_core::RelationshipProofKind;
    match kind {
        RelationshipProofKind::ExactOccurrence => "exact_occurrence",
        RelationshipProofKind::ExactReference => "exact_reference",
        RelationshipProofKind::ExactCallSite => "exact_call_site",
        RelationshipProofKind::ImportBinding => "import_binding",
        RelationshipProofKind::QualifiedName => "qualified_name",
        RelationshipProofKind::SameScopeDefinition => "same_scope_definition",
        RelationshipProofKind::ContainingType => "containing_type",
        RelationshipProofKind::ReceiverType => "receiver_type",
        RelationshipProofKind::TraitOrInterfaceBinding => "trait_or_interface_binding",
        RelationshipProofKind::InheritanceBinding => "inheritance_binding",
        RelationshipProofKind::ModuleOrPackageBinding => "module_or_package_binding",
        RelationshipProofKind::ExternalExactIndex => "external_exact_index",
    }
}

fn authority_rank(authority: open_kioku_core::RelationshipAuthority) -> u8 {
    match authority {
        open_kioku_core::RelationshipAuthority::Heuristic => 0,
        open_kioku_core::RelationshipAuthority::Corroborating => 1,
        open_kioku_core::RelationshipAuthority::Authoritative => 2,
    }
}

fn render_relationship_bench_markdown(report: &RelationshipBenchScoreReport) -> String {
    let mut out = String::new();
    out.push_str("# Relationship Conformance Report\n\n");
    out.push_str(&format!("- Corpus: `{}` (`{:?}`)\n", report.corpus_version, report.corpus_status));
    out.push_str(&format!("- Cases: {}\n", report.overall.cases));
    out.push_str(&format!("- Authoritative precision: {:.4}\n", report.overall.precision));
    out.push_str(&format!("- Recall: {:.4}\n", report.overall.recall));
    out.push_str(&format!("- MustNotEmit false-positive rate: {:.4}\n", report.overall.must_not_emit_false_positive_rate));
    out.push_str(&format!("- Exact range compliance: {:.4}\n", report.overall.exact_range_compliance));
    out.push_str(&format!("- Proof compliance: {:.4}\n", report.overall.proof_compliance));
    out.push_str(&format!("- Metamorphic authoritative-edge/proof equivalence: {:.4}\n", report.metamorphic_equivalence));
    if let Some(gate) = &report.gate {
        out.push_str(&format!("- Release gate: **{}**\n", if gate.passed { "PASS" } else { "FAIL" }));
    }
    out.push_str("\n## Capability matrix\n\n");
    out.push_str("| Cohort | Expected state | Cases | Positive | Negative | Auth emissions | Precision | Verdict |\n");
    out.push_str("|---|---:|---:|---:|---:|---:|---:|---|\n");
    for (cohort, capability) in &report.capabilities {
        out.push_str(&format!(
            "| `{}` | `{:?}` | {} | {} | {} | {} | {:.4} | {} |\n",
            cohort,
            capability.state,
            capability.cases,
            capability.positive_cases,
            capability.negative_cases,
            capability.authoritative_emissions,
            capability.precision,
            if capability.passed { "PASS" } else { "FAIL" },
        ));
    }
    if let Some(gate) = &report.gate {
        if !gate.failures.is_empty() {
            out.push_str("\n## Gate failures\n\n");
            for failure in &gate.failures {
                out.push_str(&format!("- {failure}\n"));
            }
        }
    }
    if !report.diagnostics.is_empty() {
        out.push_str("\n## Diagnostics\n\n");
        for diagnostic in &report.diagnostics {
            out.push_str(&format!(
                "- `{}` **{}** — {}\n",
                diagnostic.case_id, diagnostic.kind, diagnostic.message
            ));
        }
    }
    out
}

fn write_relationship_bench_companion_reports(
    json_path: &Path,
    report: &RelationshipBenchScoreReport,
) -> anyhow::Result<()> {
    let stem = json_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("relationship-report");
    let parent = json_path.parent().unwrap_or_else(|| Path::new("."));
    let markdown_path = parent.join(format!("{stem}.md"));
    let capability_path = parent.join(format!("{stem}-capabilities.json"));
    fs::write(&markdown_path, render_relationship_bench_markdown(report))?;
    fs::write(
        &capability_path,
        serde_json::to_string_pretty(&report.capabilities)?,
    )?;
    Ok(())
}

#[cfg(test)]
mod relationship_bench_v2_tests {
    use super::*;
    use open_kioku_core::{RelationshipAuthority, RelationshipProofKind, SourceRange};

    fn endpoint(kind: RelationshipBenchEndpointKind, identity: &str) -> RelationshipBenchEndpoint {
        RelationshipBenchEndpoint {
            kind,
            selector: identity.trim_start_matches("symbol:").to_string(),
            identity: identity.to_string(),
        }
    }

    fn case(id: &str, expected_outcome: RelationshipBenchExpectedOutcome) -> RelationshipBenchCase {
        RelationshipBenchCase {
            id: id.into(),
            fixture_id: format!("fixture:{id}"),
            split: RelationshipBenchSplit::Development,
            language: RelationshipBenchLanguage::Rust,
            relationship: GraphEdgeType::Calls,
            capability_state: RelationshipBenchCapabilityState::Authoritative,
            source: endpoint(RelationshipBenchEndpointKind::Symbol, "symbol:caller"),
            expected_outcome,
            expected_target: matches!(expected_outcome, RelationshipBenchExpectedOutcome::MustEmit)
                .then(|| endpoint(RelationshipBenchEndpointKind::Symbol, "symbol:target")),
            expected_source_range: None,
            expected_proof_kinds: BTreeSet::new(),
            forbidden_proof_kinds: BTreeSet::new(),
            candidate_count_expected: None,
            metamorphic_group: None,
            scenario: "unit".into(),
            notes: None,
        }
    }

    fn corpus(cases: Vec<RelationshipBenchCase>) -> RelationshipBenchCorpus {
        RelationshipBenchCorpus {
            schema_version: RELATIONSHIP_BENCH_SCHEMA_VERSION.into(),
            corpus_version: "unit-v2".into(),
            status: RelationshipBenchCorpusStatus::Development,
            cases,
        }
    }

    fn observed(target: &str, authority: RelationshipAuthority) -> RelationshipBenchObservedRelationship {
        RelationshipBenchObservedRelationship {
            source_identity: "symbol:caller".into(),
            target_identity: target.into(),
            relationship: GraphEdgeType::Calls,
            authority,
            proof_kinds: BTreeSet::new(),
            source_ranges: Vec::new(),
            resolver_strategies: BTreeSet::new(),
        }
    }

    fn observation(case_id: &str, outcome: RelationshipBenchObservedOutcome, relationships: Vec<RelationshipBenchObservedRelationship>) -> RelationshipBenchObservation {
        RelationshipBenchObservation {
            case_id: case_id.into(),
            outcome,
            candidate_count: None,
            relationships,
        }
    }

    #[test]
    fn separates_typescript_and_javascript_language_contracts() {
        assert_ne!(
            language_name(RelationshipBenchLanguage::TypeScript),
            language_name(RelationshipBenchLanguage::JavaScript)
        );
    }

    #[test]
    fn supports_non_symbol_relationship_endpoints() {
        let mut import_case = case("import", RelationshipBenchExpectedOutcome::MustEmit);
        import_case.relationship = GraphEdgeType::Imports;
        import_case.source = endpoint(RelationshipBenchEndpointKind::File, "file:src/main.rs");
        import_case.expected_target = Some(endpoint(RelationshipBenchEndpointKind::Module, "module:target"));
        let corpus = corpus(vec![import_case]);
        assert!(validate_relationship_bench_corpus(&corpus).is_ok());
    }

    #[test]
    fn incomplete_observation_sets_fail_closed() {
        let corpus = corpus(vec![
            case("a", RelationshipBenchExpectedOutcome::MustEmit),
            case("b", RelationshipBenchExpectedOutcome::MustNotEmit),
        ]);
        let err = score_relationship_bench_with_metadata(
            &corpus,
            &[observation(
                "a",
                RelationshipBenchObservedOutcome::Proven,
                vec![observed("symbol:target", RelationshipAuthority::Authoritative)],
            )],
            RelationshipBenchRunMetadata::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("incomplete"));
    }

    #[test]
    fn heuristic_evidence_does_not_count_as_structural_false_positive() {
        let corpus = corpus(vec![case(
            "negative",
            RelationshipBenchExpectedOutcome::MustNotEmit,
        )]);
        let report = score_relationship_bench_with_metadata(
            &corpus,
            &[observation(
                "negative",
                RelationshipBenchObservedOutcome::Unresolved,
                vec![observed("symbol:heuristic", RelationshipAuthority::Heuristic)],
            )],
            RelationshipBenchRunMetadata::default(),
        )
        .unwrap();
        assert_eq!(report.overall.false_positives, 0);
    }

    #[test]
    fn metamorphic_equivalence_compares_edge_and_proof_identity_not_only_verdict() {
        let mut left = case("left", RelationshipBenchExpectedOutcome::MustEmit);
        let mut right = case("right", RelationshipBenchExpectedOutcome::MustEmit);
        left.metamorphic_group = Some("group:identity".into());
        right.metamorphic_group = Some("group:identity".into());
        let corpus = corpus(vec![left, right]);

        let mut left_rel = observed("symbol:target", RelationshipAuthority::Authoritative);
        left_rel.proof_kinds = BTreeSet::from([RelationshipProofKind::ExactCallSite]);
        let mut right_rel = observed("symbol:target", RelationshipAuthority::Authoritative);
        right_rel.proof_kinds = BTreeSet::from([RelationshipProofKind::ExactReference]);
        let report = score_relationship_bench_with_metadata(
            &corpus,
            &[
                observation("left", RelationshipBenchObservedOutcome::Proven, vec![left_rel]),
                observation("right", RelationshipBenchObservedOutcome::Proven, vec![right_rel]),
            ],
            RelationshipBenchRunMetadata::default(),
        )
        .unwrap();
        assert_eq!(report.metamorphic_groups, 1);
        assert_eq!(report.metamorphic_equivalent_groups, 0);
        assert_eq!(report.metamorphic_equivalence, 0.0);
    }

    #[test]
    fn exact_range_and_proof_are_scored_independently() {
        let range = SourceRange {
            start_line: 3,
            start_column: 2,
            end_line: 3,
            end_column: 10,
        };
        let mut positive = case("positive", RelationshipBenchExpectedOutcome::MustEmit);
        positive.expected_source_range = Some(range.clone());
        positive.expected_proof_kinds = BTreeSet::from([RelationshipProofKind::ExactCallSite]);
        let corpus = corpus(vec![positive]);
        let mut rel = observed("symbol:target", RelationshipAuthority::Authoritative);
        rel.source_ranges.push(range);
        rel.proof_kinds = BTreeSet::from([RelationshipProofKind::ExactCallSite]);
        let report = score_relationship_bench_with_metadata(
            &corpus,
            &[observation("positive", RelationshipBenchObservedOutcome::Proven, vec![rel])],
            RelationshipBenchRunMetadata::default(),
        )
        .unwrap();
        assert_eq!(report.overall.exact_range_compliance, 1.0);
        assert_eq!(report.overall.proof_compliance, 1.0);
    }

    #[test]
    fn non_authoritative_capability_cannot_contain_must_emit_case() {
        let mut invalid = case("invalid", RelationshipBenchExpectedOutcome::MustEmit);
        invalid.capability_state = RelationshipBenchCapabilityState::Unsupported;
        assert!(validate_relationship_bench_corpus(&corpus(vec![invalid])).is_err());
    }

    #[test]
    fn observation_digest_is_order_independent() {
        let corpus = corpus(vec![
            case("a", RelationshipBenchExpectedOutcome::MustEmit),
            case("b", RelationshipBenchExpectedOutcome::MustNotEmit),
        ]);
        let first = vec![
            observation("b", RelationshipBenchObservedOutcome::Unresolved, Vec::new()),
            observation(
                "a",
                RelationshipBenchObservedOutcome::Proven,
                vec![observed("symbol:target", RelationshipAuthority::Authoritative)],
            ),
        ];
        let mut second = first.clone();
        second.reverse();
        let first_report = score_relationship_bench_with_metadata(
            &corpus,
            &first,
            RelationshipBenchRunMetadata::default(),
        )
        .unwrap();
        let second_report = score_relationship_bench_with_metadata(
            &corpus,
            &second,
            RelationshipBenchRunMetadata::default(),
        )
        .unwrap();
        assert_eq!(first_report.observation_digest, second_report.observation_digest);
    }
}

include!("relationship_live.rs");
