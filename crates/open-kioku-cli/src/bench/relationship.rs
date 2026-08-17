const RELATIONSHIP_BENCH_SCHEMA_VERSION: &str = "1.0.0";

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
    TypeScriptJavascript,
    Python,
    Java,
    Go,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RelationshipBenchExpectedOutcome {
    MustEmit,
    MustNotEmit,
    AmbiguousNoAuthoritativeEdge,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RelationshipBenchCorpus {
    schema_version: String,
    corpus_version: String,
    cases: Vec<RelationshipBenchCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RelationshipBenchCase {
    id: String,
    split: RelationshipBenchSplit,
    language: RelationshipBenchLanguage,
    relationship: GraphEdgeType,
    source_symbol_id: SymbolId,
    expected_outcome: RelationshipBenchExpectedOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_target_symbol_id: Option<SymbolId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_source_range: Option<open_kioku_core::SourceRange>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    expected_proof_kinds: BTreeSet<open_kioku_core::RelationshipProofKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RelationshipBenchObservedRelationship {
    source_symbol_id: SymbolId,
    target_symbol_id: SymbolId,
    relationship: GraphEdgeType,
    authority: open_kioku_core::RelationshipAuthority,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    proof_kinds: BTreeSet<open_kioku_core::RelationshipProofKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    source_ranges: Vec<open_kioku_core::SourceRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RelationshipBenchObservation {
    case_id: String,
    #[serde(default)]
    relationships: Vec<RelationshipBenchObservedRelationship>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct RelationshipBenchMetrics {
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

#[derive(Debug, Clone, Serialize)]
struct RelationshipBenchDiagnostic {
    case_id: String,
    kind: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_target_symbol_id: Option<SymbolId>,
    observed_authoritative_targets: Vec<SymbolId>,
}

#[derive(Debug, Clone, Serialize)]
struct RelationshipBenchScoreReport {
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

fn load_relationship_bench_corpus(path: &Path) -> anyhow::Result<RelationshipBenchCorpus> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read relationship benchmark corpus {}", path.display()))?;
    let corpus: RelationshipBenchCorpus = serde_json::from_str(&raw)
        .with_context(|| format!("invalid relationship benchmark corpus {}", path.display()))?;
    validate_relationship_bench_corpus(&corpus)?;
    Ok(corpus)
}

fn run_relationship_bench_command(
    args: RelationshipBenchArgs,
    json: bool,
) -> anyhow::Result<()> {
    let corpus = load_relationship_bench_corpus(&args.corpus)?;
    let raw = fs::read_to_string(&args.observations).with_context(|| {
        format!(
            "failed to read relationship benchmark observations {}",
            args.observations.display()
        )
    })?;
    let observations: Vec<RelationshipBenchObservation> = serde_json::from_str(&raw)
        .with_context(|| {
            format!(
                "invalid relationship benchmark observations {}",
                args.observations.display()
            )
        })?;
    let report = score_relationship_bench(&corpus, &observations)?;
    let rendered = serde_json::to_string_pretty(&report)?;

    if let Some(path) = &args.write {
        fs::write(path, &rendered).with_context(|| {
            format!(
                "failed to write relationship benchmark report {}",
                path.display()
            )
        })?;
    }

    if json {
        println!("{rendered}");
    } else {
        println!(
            "Relationship conformance: {} cases | precision {:.4} | recall {:.4}",
            report.overall.cases, report.overall.precision, report.overall.recall
        );
        println!(
            "MustNotEmit/ambiguous FP rate {:.4} | exact ranges {:.4} | proofs {:.4}",
            report.overall.must_not_emit_false_positive_rate,
            report.overall.exact_range_compliance,
            report.overall.proof_compliance
        );
        println!("Observation digest: {}", report.observation_digest);
        if let Some(path) = &args.write {
            println!("Wrote report to {}", path.display());
        }
        if !report.diagnostics.is_empty() {
            println!("Diagnostics: {}", report.diagnostics.len());
            for diagnostic in report.diagnostics.iter().take(20) {
                println!(
                    "- {} [{}] {}",
                    diagnostic.case_id, diagnostic.kind, diagnostic.message
                );
            }
            if report.diagnostics.len() > 20 {
                println!("- ... {} more", report.diagnostics.len() - 20);
            }
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
    for case in &corpus.cases {
        if case.id.trim().is_empty() {
            anyhow::bail!("relationship benchmark case id must not be empty");
        }
        if !ids.insert(case.id.clone()) {
            anyhow::bail!("duplicate relationship benchmark case id: {}", case.id);
        }
        if case.source_symbol_id.0.trim().is_empty() {
            anyhow::bail!("case {} has an empty source_symbol_id", case.id);
        }
        if !is_conformance_relationship(&case.relationship) {
            anyhow::bail!(
                "case {} uses unsupported relationship {:?}",
                case.id,
                case.relationship
            );
        }
        match case.expected_outcome {
            RelationshipBenchExpectedOutcome::MustEmit => {
                let Some(target) = &case.expected_target_symbol_id else {
                    anyhow::bail!("MustEmit case {} must declare expected_target_symbol_id", case.id);
                };
                if target.0.trim().is_empty() {
                    anyhow::bail!("case {} has an empty expected_target_symbol_id", case.id);
                }
            }
            RelationshipBenchExpectedOutcome::MustNotEmit
            | RelationshipBenchExpectedOutcome::AmbiguousNoAuthoritativeEdge => {
                if case.expected_target_symbol_id.is_some() {
                    anyhow::bail!(
                        "non-emission case {} must not declare expected_target_symbol_id",
                        case.id
                    );
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
    Ok(())
}

fn score_relationship_bench(
    corpus: &RelationshipBenchCorpus,
    observations: &[RelationshipBenchObservation],
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

    let mut overall = RelationshipBenchMetrics::default();
    let mut by_language = BTreeMap::<String, RelationshipBenchMetrics>::new();
    let mut by_relationship = BTreeMap::<String, RelationshipBenchMetrics>::new();
    let mut by_language_relationship = BTreeMap::<String, RelationshipBenchMetrics>::new();
    let mut observed_proof_kind_counts = BTreeMap::<String, usize>::new();
    let mut diagnostics = Vec::new();

    let mut cases = corpus.cases.iter().collect::<Vec<_>>();
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    for case in cases {
        let relationships = observations_by_id
            .get(case.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for relationship in relationships {
            for proof_kind in &relationship.proof_kinds {
                *observed_proof_kind_counts
                    .entry(proof_kind_name(proof_kind).to_string())
                    .or_default() += 1;
            }
        }

        let outcome = score_relationship_case(case, relationships);
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
    diagnostics.sort_by(|left, right| {
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
}

#[derive(Default)]
struct RelationshipCaseScore {
    metrics: RelationshipBenchMetrics,
    diagnostics: Vec<RelationshipBenchDiagnostic>,
}

fn score_relationship_case(
    case: &RelationshipBenchCase,
    relationships: &[RelationshipBenchObservedRelationship],
) -> RelationshipCaseScore {
    let mut score = RelationshipCaseScore::default();
    score.metrics.cases = 1;
    let authoritative = relationships
        .iter()
        .filter(|relationship| {
            relationship.authority == open_kioku_core::RelationshipAuthority::Authoritative
        })
        .collect::<Vec<_>>();
    let observed_authoritative_targets = authoritative
        .iter()
        .map(|relationship| relationship.target_symbol_id.clone())
        .collect::<Vec<_>>();

    match case.expected_outcome {
        RelationshipBenchExpectedOutcome::MustEmit => {
            let expected_target = case
                .expected_target_symbol_id
                .as_ref()
                .expect("validated MustEmit case has a target");
            let correct = authoritative
                .iter()
                .copied()
                .filter(|relationship| {
                    relationship.relationship == case.relationship
                        && relationship.source_symbol_id == case.source_symbol_id
                        && relationship.target_symbol_id == *expected_target
                })
                .collect::<Vec<_>>();
            if correct.is_empty() {
                score.metrics.false_negatives = 1;
                score.diagnostics.push(RelationshipBenchDiagnostic {
                    case_id: case.id.clone(),
                    kind: "missing_authoritative_relationship".into(),
                    message: "expected authoritative relationship was not emitted".into(),
                    expected_target_symbol_id: Some(expected_target.clone()),
                    observed_authoritative_targets: observed_authoritative_targets.clone(),
                });
            } else {
                score.metrics.true_positives = 1;
            }

            let wrong_authoritative = authoritative
                .iter()
                .filter(|relationship| {
                    relationship.relationship != case.relationship
                        || relationship.source_symbol_id != case.source_symbol_id
                        || relationship.target_symbol_id != *expected_target
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
                    expected_target_symbol_id: Some(expected_target.clone()),
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
                        expected_target_symbol_id: Some(expected_target.clone()),
                        observed_authoritative_targets: observed_authoritative_targets.clone(),
                    });
                }
            }

            if !case.expected_proof_kinds.is_empty() {
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
        }
        RelationshipBenchExpectedOutcome::MustNotEmit
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

fn relationship_observation_digest(
    observations: &[RelationshipBenchObservation],
) -> anyhow::Result<String> {
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
    Vec<(u32, u32, u32, u32)>,
);

fn observed_relationship_key(
    relationship: &RelationshipBenchObservedRelationship,
) -> ObservedRelationshipKey {
    (
        relationship.source_symbol_id.0.clone(),
        relationship.target_symbol_id.0.clone(),
        edge_type_name(&relationship.relationship).to_string(),
        authority_rank(relationship.authority),
        relationship
            .proof_kinds
            .iter()
            .map(|kind| proof_kind_name(kind).to_string())
            .collect(),
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
    target.true_positives += source.true_positives;
    target.false_positives += source.false_positives;
    target.false_negatives += source.false_negatives;
    target.negative_cases += source.negative_cases;
    target.negative_cases_with_false_positive += source.negative_cases_with_false_positive;
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
    metrics.must_not_emit_false_positive_rate = if metrics.negative_cases == 0 {
        0.0
    } else {
        metrics.negative_cases_with_false_positive as f64 / metrics.negative_cases as f64
    };
    metrics.exact_range_compliance = relationship_ratio(metrics.exact_range_matches, metrics.exact_range_cases);
    metrics.proof_compliance = relationship_ratio(metrics.proof_matches, metrics.proof_cases);
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
        RelationshipBenchLanguage::TypeScriptJavascript => "typescript_javascript",
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

#[cfg(test)]
mod relationship_bench_tests {
    use super::*;
    use open_kioku_core::{RelationshipAuthority, RelationshipProofKind, SourceRange};

    fn case(
        id: &str,
        expected_outcome: RelationshipBenchExpectedOutcome,
    ) -> RelationshipBenchCase {
        RelationshipBenchCase {
            id: id.into(),
            split: RelationshipBenchSplit::Development,
            language: RelationshipBenchLanguage::Rust,
            relationship: GraphEdgeType::Calls,
            source_symbol_id: SymbolId::new("symbol:caller"),
            expected_outcome,
            expected_target_symbol_id: matches!(
                expected_outcome,
                RelationshipBenchExpectedOutcome::MustEmit
            )
            .then(|| SymbolId::new("symbol:target")),
            expected_source_range: None,
            expected_proof_kinds: BTreeSet::new(),
        }
    }

    fn corpus(cases: Vec<RelationshipBenchCase>) -> RelationshipBenchCorpus {
        RelationshipBenchCorpus {
            schema_version: RELATIONSHIP_BENCH_SCHEMA_VERSION.into(),
            corpus_version: "dev-1".into(),
            cases,
        }
    }

    fn observed(target: &str, authority: RelationshipAuthority) -> RelationshipBenchObservedRelationship {
        RelationshipBenchObservedRelationship {
            source_symbol_id: SymbolId::new("symbol:caller"),
            target_symbol_id: SymbolId::new(target),
            relationship: GraphEdgeType::Calls,
            authority,
            proof_kinds: BTreeSet::new(),
            source_ranges: Vec::new(),
        }
    }

    #[test]
    fn corpus_rejects_duplicate_case_ids() {
        let corpus = corpus(vec![
            case("duplicate", RelationshipBenchExpectedOutcome::MustEmit),
            case("duplicate", RelationshipBenchExpectedOutcome::MustEmit),
        ]);
        assert!(validate_relationship_bench_corpus(&corpus).is_err());
    }

    #[test]
    fn must_not_emit_counts_only_authoritative_edges_as_structural_false_positives() {
        let corpus = corpus(vec![case(
            "negative",
            RelationshipBenchExpectedOutcome::MustNotEmit,
        )]);
        let observations = vec![RelationshipBenchObservation {
            case_id: "negative".into(),
            relationships: vec![
                observed("symbol:heuristic", RelationshipAuthority::Heuristic),
                observed("symbol:wrong", RelationshipAuthority::Authoritative),
            ],
        }];
        let report = score_relationship_bench(&corpus, &observations).unwrap();
        assert_eq!(report.overall.false_positives, 1);
        assert_eq!(report.overall.negative_cases_with_false_positive, 1);
        assert_eq!(report.overall.must_not_emit_false_positive_rate, 1.0);
    }

    #[test]
    fn correct_target_wrong_target_range_and_proofs_are_scored_independently() {
        let range = SourceRange {
            start_line: 10,
            start_column: 4,
            end_line: 10,
            end_column: 17,
        };
        let mut benchmark_case = case("positive", RelationshipBenchExpectedOutcome::MustEmit);
        benchmark_case.expected_source_range = Some(range.clone());
        benchmark_case.expected_proof_kinds = BTreeSet::from([
            RelationshipProofKind::ExactCallSite,
            RelationshipProofKind::ExactReference,
        ]);
        let corpus = corpus(vec![benchmark_case]);

        let mut correct = observed("symbol:target", RelationshipAuthority::Authoritative);
        correct.source_ranges.push(range);
        correct.proof_kinds = BTreeSet::from([
            RelationshipProofKind::ExactCallSite,
            RelationshipProofKind::ExactReference,
        ]);
        let observations = vec![RelationshipBenchObservation {
            case_id: "positive".into(),
            relationships: vec![
                correct,
                observed("symbol:wrong", RelationshipAuthority::Authoritative),
            ],
        }];

        let report = score_relationship_bench(&corpus, &observations).unwrap();
        assert_eq!(report.overall.true_positives, 1);
        assert_eq!(report.overall.false_positives, 1);
        assert_eq!(report.overall.false_negatives, 0);
        assert_eq!(report.overall.exact_range_compliance, 1.0);
        assert_eq!(report.overall.proof_compliance, 1.0);
        assert_eq!(report.overall.precision, 0.5);
        assert_eq!(report.overall.recall, 1.0);
    }

    #[test]
    fn scoring_and_digest_are_independent_of_input_order() {
        let corpus = corpus(vec![
            case("a", RelationshipBenchExpectedOutcome::MustEmit),
            case("b", RelationshipBenchExpectedOutcome::MustNotEmit),
        ]);
        let first = vec![
            RelationshipBenchObservation {
                case_id: "b".into(),
                relationships: vec![observed(
                    "symbol:heuristic",
                    RelationshipAuthority::Heuristic,
                )],
            },
            RelationshipBenchObservation {
                case_id: "a".into(),
                relationships: vec![observed(
                    "symbol:target",
                    RelationshipAuthority::Authoritative,
                )],
            },
        ];
        let mut second = first.clone();
        second.reverse();

        let first_report = score_relationship_bench(&corpus, &first).unwrap();
        let second_report = score_relationship_bench(&corpus, &second).unwrap();
        assert_eq!(first_report.observation_digest, second_report.observation_digest);
        assert_eq!(first_report.overall.true_positives, second_report.overall.true_positives);
        assert_eq!(first_report.overall.false_positives, second_report.overall.false_positives);
    }
}
