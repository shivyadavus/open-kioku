from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    return text.replace(old, new, count)

path = Path("crates/open-kioku-cli/src/bench/relationship.rs")
text = path.read_text()

text = replace_exact(
    text,
    '''enum RelationshipBenchExpectedOutcome {
    MustEmit,
    MustNotEmit,
    AmbiguousNoAuthoritativeEdge,
}
''',
    '''enum RelationshipBenchExpectedOutcome {
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
''',
    "relationship outcome protocol",
)

text = replace_exact(
    text,
    '''struct RelationshipBenchCorpus {
    schema_version: String,
    corpus_version: String,
    cases: Vec<RelationshipBenchCase>,
}
''',
    '''struct RelationshipBenchCorpus {
    schema_version: String,
    corpus_version: String,
    #[serde(default)]
    status: RelationshipBenchCorpusStatus,
    cases: Vec<RelationshipBenchCase>,
}
''',
    "corpus status",
)

text = replace_exact(
    text,
    '''struct RelationshipBenchCase {
    id: String,
    split: RelationshipBenchSplit,
''',
    '''struct RelationshipBenchCase {
    id: String,
    fixture_id: String,
    split: RelationshipBenchSplit,
''',
    "case fixture id",
)

text = replace_exact(
    text,
    '''    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    expected_proof_kinds: BTreeSet<open_kioku_core::RelationshipProofKind>,
}
''',
    '''    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    expected_proof_kinds: BTreeSet<open_kioku_core::RelationshipProofKind>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    forbidden_proof_kinds: BTreeSet<open_kioku_core::RelationshipProofKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    candidate_count_expected: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}
''',
    "case protocol fields",
)

text = replace_exact(
    text,
    '''    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_ranges: Vec<open_kioku_core::SourceRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RelationshipBenchObservation {
    case_id: String,
    #[serde(default)]
    relationships: Vec<RelationshipBenchObservedRelationship>,
}
''',
    '''    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_ranges: Vec<open_kioku_core::SourceRange>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    resolver_strategies: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RelationshipBenchObservation {
    case_id: String,
    #[serde(default)]
    outcome: RelationshipBenchObservedOutcome,
    #[serde(default)]
    candidate_count: usize,
    #[serde(default)]
    relationships: Vec<RelationshipBenchObservedRelationship>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RelationshipBenchRunMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_commit: Option<String>,
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
''',
    "observation protocol",
)

text = replace_exact(
    text,
    '''struct RelationshipBenchMetrics {
    cases: usize,
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
    negative_cases: usize,
    negative_cases_with_false_positive: usize,
    exact_range_cases: usize,
    exact_range_matches: usize,
    proof_cases: usize,
    proof_matches: usize,
    precision: f64,
    recall: f64,
    must_not_emit_false_positive_rate: f64,
    exact_range_compliance: f64,
    proof_compliance: f64,
}
''',
    '''struct RelationshipBenchMetrics {
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
    average_candidate_count: f64,
    candidate_count_compliance: f64,
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
''',
    "expanded benchmark metrics",
)

text = replace_exact(
    text,
    '''struct RelationshipBenchScoreReport {
    schema_version: String,
    corpus_version: String,
    observation_digest: String,
    overall: RelationshipBenchMetrics,
    by_language: BTreeMap<String, RelationshipBenchMetrics>,
    by_relationship: BTreeMap<String, RelationshipBenchMetrics>,
    by_language_relationship: BTreeMap<String, RelationshipBenchMetrics>,
    observed_proof_kind_counts: BTreeMap<String, usize>,
    diagnostics: Vec<RelationshipBenchDiagnostic>,
}
''',
    '''struct RelationshipBenchScoreReport {
    schema_version: String,
    corpus_version: String,
    corpus_status: RelationshipBenchCorpusStatus,
    run_metadata: RelationshipBenchRunMetadata,
    observation_digest: String,
    overall: RelationshipBenchMetrics,
    by_language: BTreeMap<String, RelationshipBenchMetrics>,
    by_relationship: BTreeMap<String, RelationshipBenchMetrics>,
    by_language_relationship: BTreeMap<String, RelationshipBenchMetrics>,
    by_resolver_strategy: BTreeMap<String, RelationshipBenchStrategyMetrics>,
    by_proof_kind: BTreeMap<String, RelationshipBenchStrategyMetrics>,
    observed_proof_kind_counts: BTreeMap<String, usize>,
    wrong_target_counts: BTreeMap<String, usize>,
    diagnostics: Vec<RelationshipBenchDiagnostic>,
}
''',
    "expanded score report",
)

old_parse = '''    let observations: Vec<RelationshipBenchObservation> = serde_json::from_str(&raw)
        .with_context(|| {
            format!(
                "invalid relationship benchmark observations {}",
                args.observations.display()
            )
        })?;
    let report = score_relationship_bench(&corpus, &observations)?;
'''
new_parse = '''    let input: RelationshipBenchObservationInput = serde_json::from_str(&raw).with_context(|| {
        format!(
            "invalid relationship benchmark observations {}",
            args.observations.display()
        )
    })?;
    let (metadata, observations) = input.into_parts();
    let report = score_relationship_bench_with_metadata(&corpus, &observations, metadata)?;
'''
text = replace_exact(text, old_parse, new_parse, "observation input wrapper")

text = replace_exact(
    text,
    '''        if case.id.trim().is_empty() {
            anyhow::bail!("relationship benchmark case id must not be empty");
        }
''',
    '''        if case.id.trim().is_empty() {
            anyhow::bail!("relationship benchmark case id must not be empty");
        }
        if case.fixture_id.trim().is_empty() {
            anyhow::bail!("case {} has an empty fixture_id", case.id);
        }
''',
    "fixture validation",
)

text = replace_exact(
    text,
    '''            RelationshipBenchExpectedOutcome::MustNotEmit
            | RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge => {
''',
    '''            RelationshipBenchExpectedOutcome::MustNotEmit
            | RelationshipBenchExpectedOutcome::MayEmitHeuristicOnly
            | RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge => {
''',
    "non-emission validation outcomes",
)

text = replace_exact(
    text,
    '''                if case.expected_source_range.is_some() || !case.expected_proof_kinds.is_empty() {
                    anyhow::bail!(
                        "non-emission case {} cannot require source ranges or proof kinds",
                        case.id
                    );
                }
''',
    '''                if case.expected_source_range.is_some() || !case.expected_proof_kinds.is_empty() {
                    anyhow::bail!(
                        "non-emission case {} cannot require source ranges or proof kinds",
                        case.id
                    );
                }
''',
    "non-emission proof validation",
)

old_score_sig = '''fn score_relationship_bench(
    corpus: &RelationshipBenchCorpus,
    observations: &[RelationshipBenchObservation],
) -> anyhow::Result<RelationshipBenchScoreReport> {
    validate_relationship_bench_corpus(corpus)?;
'''
new_score_sig = '''fn score_relationship_bench(
    corpus: &RelationshipBenchCorpus,
    observations: &[RelationshipBenchObservation],
) -> anyhow::Result<RelationshipBenchScoreReport> {
    score_relationship_bench_with_metadata(
        corpus,
        observations,
        RelationshipBenchRunMetadata::default(),
    )
}

fn score_relationship_bench_with_metadata(
    corpus: &RelationshipBenchCorpus,
    observations: &[RelationshipBenchObservation],
    run_metadata: RelationshipBenchRunMetadata,
) -> anyhow::Result<RelationshipBenchScoreReport> {
    validate_relationship_bench_corpus(corpus)?;
'''
text = replace_exact(text, old_score_sig, new_score_sig, "score metadata wrapper")

text = replace_exact(
    text,
    '''    let mut observed_proof_kind_counts = BTreeMap::<String, usize>::new();
    let mut diagnostics = Vec::new();
''',
    '''    let mut observed_proof_kind_counts = BTreeMap::<String, usize>::new();
    let mut by_resolver_strategy = BTreeMap::<String, RelationshipBenchStrategyMetrics>::new();
    let mut by_proof_kind = BTreeMap::<String, RelationshipBenchStrategyMetrics>::new();
    let mut wrong_target_counts = BTreeMap::<String, usize>::new();
    let mut diagnostics = Vec::new();
''',
    "strategy metric maps",
)

old_case_call = '''        let outcome = score_relationship_case(case, relationships);
        merge_relationship_metrics(&mut overall, &outcome.metrics);
'''
new_case_call = '''        let observation = observations
            .iter()
            .find(|observation| observation.case_id == case.id);
        let outcome = score_relationship_case(case, observation, relationships);
        for relationship in relationships.iter().filter(|relationship| {
            relationship.authority == open_kioku_core::RelationshipAuthority::Authoritative
        }) {
            let correct = authoritative_relationship_matches_case(case, relationship);
            if !correct {
                *wrong_target_counts
                    .entry(relationship.target_symbol_id.0.clone())
                    .or_default() += 1;
            }
            let strategies = if relationship.resolver_strategies.is_empty() {
                vec!["<unspecified>".to_string()]
            } else {
                relationship.resolver_strategies.iter().cloned().collect()
            };
            for strategy in strategies {
                update_strategy_metrics(
                    by_resolver_strategy.entry(strategy).or_default(),
                    correct,
                );
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
'''
text = replace_exact(text, old_case_call, new_case_call, "case observation metrics")

text = replace_exact(
    text,
    '''    diagnostics.sort_by(|left, right| {
        (&left.case_id, &left.kind, &left.message).cmp(&(&right.case_id, &right.kind, &right.message))
    });

    Ok(RelationshipBenchScoreReport {
        schema_version: corpus.schema_version.clone(),
        corpus_version: corpus.corpus_version.clone(),
        observation_digest: relationship_observation_digest(observations)?,
        overall,
        by_language,
        by_relationship,
        by_language_relationship,
        observed_proof_kind_counts,
        diagnostics,
    })
''',
    '''    for metrics in by_resolver_strategy.values_mut() {
        finalize_strategy_metrics(metrics);
    }
    for metrics in by_proof_kind.values_mut() {
        finalize_strategy_metrics(metrics);
    }
    diagnostics.sort_by(|left, right| {
        (&left.case_id, &left.kind, &left.message).cmp(&(&right.case_id, &right.kind, &right.message))
    });

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
        by_resolver_strategy,
        by_proof_kind,
        observed_proof_kind_counts,
        wrong_target_counts,
        diagnostics,
    })
''',
    "score report construction",
)

text = replace_exact(
    text,
    '''fn score_relationship_case(
    case: &RelationshipBenchCase,
    relationships: &[RelationshipBenchObservedRelationship],
) -> RelationshipCaseScore {
    let mut score = RelationshipCaseScore::default();
    score.metrics.cases = 1;
''',
    '''fn score_relationship_case(
    case: &RelationshipBenchCase,
    observation: Option<&RelationshipBenchObservation>,
    relationships: &[RelationshipBenchObservedRelationship],
) -> RelationshipCaseScore {
    let mut score = RelationshipCaseScore::default();
    score.metrics.cases = 1;
    score.metrics.candidate_count_cases = 1;
    let candidate_count = observation.map(|value| value.candidate_count).unwrap_or(0);
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
                expected_target_symbol_id: case.expected_target_symbol_id.clone(),
                observed_authoritative_targets: Vec::new(),
            });
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
            expected_target_symbol_id: case.expected_target_symbol_id.clone(),
            observed_authoritative_targets: Vec::new(),
        });
    }
    if observed_outcome == RelationshipBenchObservedOutcome::Ambiguous {
        score.metrics.observed_ambiguous_cases = 1;
    }
    if observed_outcome == RelationshipBenchObservedOutcome::Unresolved {
        score.metrics.observed_unresolved_cases = 1;
    }
''',
    "per-case outcome metrics",
)

text = replace_exact(
    text,
    '''        RelationshipBenchExpectedOutcome::MustEmit => {
            let expected_target = case
''',
    '''        RelationshipBenchExpectedOutcome::MustEmit => {
            score.metrics.positive_cases = 1;
            let expected_target = case
''',
    "positive case count",
)

old_proof_check = '''            if !case.expected_proof_kinds.is_empty() {
                score.metrics.proof_cases = 1;
                if correct.iter().any(|relationship| {
                    case.expected_proof_kinds
                        .iter()
                        .all(|kind| relationship.proof_kinds.contains(kind))
                }) {
                    score.metrics.proof_matches = 1;
                } else {
                    score.diagnostics.push(RelationshipBenchDiagnostic {
                        case_id: case.id.clone(),
                        kind: "proof_kind_mismatch".into(),
                        message: "authoritative relationship did not contain all required proof kinds"
                            .into(),
                        expected_target_symbol_id: Some(expected_target.clone()),
                        observed_authoritative_targets,
                    });
                }
            }
'''
new_proof_check = '''            if !case.expected_proof_kinds.is_empty() || !case.forbidden_proof_kinds.is_empty() {
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
                        expected_target_symbol_id: Some(expected_target.clone()),
                        observed_authoritative_targets,
                    });
                }
            }
'''
text = replace_exact(text, old_proof_check, new_proof_check, "required/forbidden proof scoring")

old_negative = '''        RelationshipBenchExpectedOutcome::MustNotEmit
        | RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge => {
            score.metrics.negative_cases = 1;
            if !authoritative.is_empty() {
                score.metrics.false_positives = authoritative.len();
                score.metrics.negative_cases_with_false_positive = 1;
                score.diagnostics.push(RelationshipBenchDiagnostic {
                    case_id: case.id.clone(),
                    kind: match case.expected_outcome {
                        RelationshipBenchExpectedOutcome::MustNotEmit => {
                            "must_not_emit_violation".into()
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
                    expected_target_symbol_id: None,
                    observed_authoritative_targets,
                });
            }
        }
'''
new_negative = '''        RelationshipBenchExpectedOutcome::MustNotEmit
        | RelationshipBenchExpectedOutcome::MayEmitHeuristicOnly
        | RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge => {
            score.metrics.negative_cases = 1;
            if case.expected_outcome == RelationshipBenchExpectedOutcome::MustNotEmit {
                score.metrics.must_not_emit_cases = 1;
            }
            if case.expected_outcome
                == RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge
            {
                score.metrics.ambiguity_expected_cases = 1;
            }
            if !authoritative.is_empty() {
                score.metrics.false_positives = authoritative.len();
                score.metrics.negative_cases_with_false_positive = 1;
                if case.expected_outcome == RelationshipBenchExpectedOutcome::MustNotEmit {
                    score.metrics.must_not_emit_cases_with_false_positive = 1;
                }
                if case.expected_outcome
                    == RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge
                {
                    score.metrics.ambiguity_collapsed_cases = 1;
                }
                score.diagnostics.push(RelationshipBenchDiagnostic {
                    case_id: case.id.clone(),
                    kind: match case.expected_outcome {
                        RelationshipBenchExpectedOutcome::MustNotEmit => {
                            "must_not_emit_violation".into()
                        }
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
                    expected_target_symbol_id: None,
                    observed_authoritative_targets,
                });
            }
        }
'''
text = replace_exact(text, old_negative, new_negative, "negative outcome scoring")

text = replace_exact(
    text,
    '''        relationship.source_ranges.dedup();
    }
''',
    '''        relationship.source_ranges.dedup();
    }
''',
    "relationship normalization anchor",
)

# Include resolver strategies in deterministic relationship key.
text = replace_exact(
    text,
    '''    Vec<String>,
    Vec<(u32, u32, u32, u32)>,
);
''',
    '''    Vec<String>,
    Vec<String>,
    Vec<(u32, u32, u32, u32)>,
);
''',
    "relationship key strategy tuple",
)
text = replace_exact(
    text,
    '''        relationship
            .proof_kinds
            .iter()
            .map(|kind| proof_kind_name(kind).to_string())
            .collect(),
        relationship
            .source_ranges
''',
    '''        relationship
            .proof_kinds
            .iter()
            .map(|kind| proof_kind_name(kind).to_string())
            .collect(),
        relationship.resolver_strategies.iter().cloned().collect(),
        relationship
            .source_ranges
''',
    "relationship key strategies",
)

old_merge = '''    target.cases += source.cases;
    target.true_positives += source.true_positives;
    target.false_positives += source.false_positives;
    target.false_negatives += source.false_negatives;
    target.negative_cases += source.negative_cases;
    target.negative_cases_with_false_positive += source.negative_cases_with_false_positive;
    target.exact_range_cases += source.exact_range_cases;
    target.exact_range_matches += source.exact_range_matches;
    target.proof_cases += source.proof_cases;
    target.proof_matches += source.proof_matches;
'''
new_merge = '''    target.cases += source.cases;
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
'''
text = replace_exact(text, old_merge, new_merge, "expanded metric merge")

old_finalize = '''    metrics.recall = relationship_ratio(
        metrics.true_positives,
        metrics.true_positives + metrics.false_negatives,
    );
    metrics.must_not_emit_false_positive_rate = if metrics.negative_cases == 0 {
        0.0
    } else {
        metrics.negative_cases_with_false_positive as f64 / metrics.negative_cases as f64
    };
    metrics.exact_range_compliance = relationship_ratio(metrics.exact_range_matches, metrics.exact_range_cases);
    metrics.proof_compliance = relationship_ratio(metrics.proof_matches, metrics.proof_cases);
}
'''
new_finalize = '''    metrics.recall = relationship_ratio(
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
    metrics.average_candidate_count = if metrics.candidate_count_cases == 0 {
        0.0
    } else {
        metrics.candidate_count_total as f64 / metrics.candidate_count_cases as f64
    };
    metrics.candidate_count_compliance = relationship_ratio(
        metrics.candidate_count_matches,
        metrics.candidate_count_expected_cases,
    );
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
            .expected_target_symbol_id
            .as_ref()
            .map(|target| {
                relationship.relationship == case.relationship
                    && relationship.source_symbol_id == case.source_symbol_id
                    && relationship.target_symbol_id == *target
            })
            .unwrap_or(false)
}

fn observed_outcome_matches(
    expected: RelationshipBenchExpectedOutcome,
    observed: RelationshipBenchObservedOutcome,
) -> bool {
    match expected {
        RelationshipBenchExpectedOutcome::MustEmit => {
            observed == RelationshipBenchObservedOutcome::Proven
        }
        RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge => {
            observed == RelationshipBenchObservedOutcome::Ambiguous
        }
        RelationshipBenchExpectedOutcome::MustNotEmit
        | RelationshipBenchExpectedOutcome::MayEmitHeuristicOnly => {
            observed != RelationshipBenchObservedOutcome::Proven
        }
    }
}
'''
text = replace_exact(text, old_finalize, new_finalize, "expanded metric finalization")

# Test fixtures need the new required/defaultable Rust fields.
text = replace_exact(
    text,
    '''        RelationshipBenchCase {
            id: id.into(),
            split: RelationshipBenchSplit::Development,
''',
    '''        RelationshipBenchCase {
            id: id.into(),
            fixture_id: format!("fixture:{id}"),
            split: RelationshipBenchSplit::Development,
''',
    "case test fixture id",
)
text = replace_exact(
    text,
    '''            expected_source_range: None,
            expected_proof_kinds: BTreeSet::new(),
        }
''',
    '''            expected_source_range: None,
            expected_proof_kinds: BTreeSet::new(),
            forbidden_proof_kinds: BTreeSet::new(),
            candidate_count_expected: None,
            notes: None,
        }
''',
    "case test protocol fields",
)
text = replace_exact(
    text,
    '''            schema_version: RELATIONSHIP_BENCH_SCHEMA_VERSION.into(),
            corpus_version: "dev-1".into(),
            cases,
''',
    '''            schema_version: RELATIONSHIP_BENCH_SCHEMA_VERSION.into(),
            corpus_version: "dev-1".into(),
            status: RelationshipBenchCorpusStatus::Development,
            cases,
''',
    "test corpus status",
)
text = replace_exact(
    text,
    '''            authority,
            proof_kinds: BTreeSet::new(),
            source_ranges: Vec::new(),
        }
''',
    '''            authority,
            proof_kinds: BTreeSet::new(),
            source_ranges: Vec::new(),
            resolver_strategies: BTreeSet::new(),
        }
''',
    "observed relationship strategy field",
)
# Observation literals: set outcome/candidate_count according to each case afterward with defaults.
text = text.replace(
    '''        let observations = vec![RelationshipBenchObservation {
            case_id: "negative".into(),
            relationships:''',
    '''        let observations = vec![RelationshipBenchObservation {
            case_id: "negative".into(),
            outcome: RelationshipBenchObservedOutcome::Unresolved,
            candidate_count: 2,
            relationships:''',
)
text = text.replace(
    '''        let observations = vec![RelationshipBenchObservation {
            case_id: "positive".into(),
            relationships:''',
    '''        let observations = vec![RelationshipBenchObservation {
            case_id: "positive".into(),
            outcome: RelationshipBenchObservedOutcome::Proven,
            candidate_count: 2,
            relationships:''',
)
text = text.replace(
    '''            RelationshipBenchObservation {
                case_id: "b".into(),
                relationships:''',
    '''            RelationshipBenchObservation {
                case_id: "b".into(),
                outcome: RelationshipBenchObservedOutcome::Unresolved,
                candidate_count: 1,
                relationships:''',
)
text = text.replace(
    '''            RelationshipBenchObservation {
                case_id: "a".into(),
                relationships:''',
    '''            RelationshipBenchObservation {
                case_id: "a".into(),
                outcome: RelationshipBenchObservedOutcome::Proven,
                candidate_count: 1,
                relationships:''',
)

path.write_text(text)
