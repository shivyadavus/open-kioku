from pathlib import Path


def replace_exact(path: str, old: str, new: str, label: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    p.write_text(text.replace(old, new, count))


path = "crates/open-kioku-cli/src/bench/relationship.rs"

replace_exact(
    path,
    '''    #[serde(default, skip_serializing_if = "Option::is_none")]\n    candidate_count_expected: Option<usize>,\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    notes: Option<String>,\n''',
    '''    #[serde(default, skip_serializing_if = "Option::is_none")]\n    candidate_count_expected: Option<usize>,\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    metamorphic_group: Option<String>,\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    notes: Option<String>,\n''',
    "metamorphic case field",
)

replace_exact(
    path,
    '''#[derive(Debug, Clone, Deserialize)]\nstruct RelationshipBenchPolicy {\n''',
    '''#[derive(Debug, Clone, Deserialize)]\n#[serde(deny_unknown_fields)]\nstruct RelationshipBenchPolicy {\n''',
    "strict policy schema",
)

replace_exact(
    path,
    '''    minimum_outcome_compliance: f64,\n    require_zero_false_negatives: bool,\n    require_positive_and_negative_per_language_relationship: bool,\n    require_frozen_corpus: bool,\n''',
    '''    minimum_outcome_compliance: f64,\n    minimum_metamorphic_groups: usize,\n    minimum_metamorphic_equivalence: f64,\n    require_zero_false_negatives: bool,\n    require_positive_and_negative_per_language_relationship: bool,\n    require_metamorphic_group_per_language_relationship: bool,\n    require_frozen_corpus: bool,\n''',
    "metamorphic policy fields",
)

replace_exact(
    path,
    '''    wrong_target_counts: BTreeMap<String, usize>,\n    diagnostics: Vec<RelationshipBenchDiagnostic>,\n''',
    '''    wrong_target_counts: BTreeMap<String, usize>,\n    metamorphic_groups: usize,\n    metamorphic_equivalent_groups: usize,\n    metamorphic_equivalence: f64,\n    diagnostics: Vec<RelationshipBenchDiagnostic>,\n''',
    "metamorphic report fields",
)

replace_exact(
    path,
    '''        (\n            "minimum_outcome_compliance",\n            policy.minimum_outcome_compliance,\n        ),\n''',
    '''        (\n            "minimum_outcome_compliance",\n            policy.minimum_outcome_compliance,\n        ),\n        (\n            "minimum_metamorphic_equivalence",\n            policy.minimum_metamorphic_equivalence,\n        ),\n''',
    "metamorphic policy ratio validation",
)

replace_exact(
    path,
    '''    if report.overall.outcome_compliance < policy.minimum_outcome_compliance {\n        failures.push(format!(\n            "resolution-outcome compliance {:.4} is below required {:.4}",\n            report.overall.outcome_compliance, policy.minimum_outcome_compliance\n        ));\n    }\n\n    const LANGUAGES: [&str; 5] = [\n''',
    '''    if report.overall.outcome_compliance < policy.minimum_outcome_compliance {\n        failures.push(format!(\n            "resolution-outcome compliance {:.4} is below required {:.4}",\n            report.overall.outcome_compliance, policy.minimum_outcome_compliance\n        ));\n    }\n    if report.metamorphic_groups < policy.minimum_metamorphic_groups {\n        failures.push(format!(\n            "corpus has {} metamorphic groups, below required {}",\n            report.metamorphic_groups, policy.minimum_metamorphic_groups\n        ));\n    }\n    if report.metamorphic_equivalence < policy.minimum_metamorphic_equivalence {\n        failures.push(format!(\n            "metamorphic equivalence {:.4} is below required {:.4}",\n            report.metamorphic_equivalence, policy.minimum_metamorphic_equivalence\n        ));\n    }\n\n    const LANGUAGES: [&str; 5] = [\n''',
    "global metamorphic gates",
)

replace_exact(
    path,
    '''            if policy.require_positive_and_negative_per_language_relationship\n                && (metrics.positive_cases == 0 || metrics.negative_cases == 0)\n            {\n                failures.push(format!(\n                    "cohort {key} must contain both positive and negative/ambiguous cases"\n                ));\n            }\n            if metrics.true_positives + metrics.false_positives == 0 {\n''',
    '''            if policy.require_positive_and_negative_per_language_relationship\n                && (metrics.positive_cases == 0 || metrics.negative_cases == 0)\n            {\n                failures.push(format!(\n                    "cohort {key} must contain both positive and negative/ambiguous cases"\n                ));\n            }\n            if policy.require_metamorphic_group_per_language_relationship\n                && !corpus.cases.iter().any(|case| {\n                    language_name(case.language) == language\n                        && edge_type_name(&case.relationship) == relationship\n                        && case.metamorphic_group.is_some()\n                })\n            {\n                failures.push(format!("cohort {key} has no metamorphic group"));\n            }\n            if metrics.true_positives + metrics.false_positives == 0 {\n''',
    "cohort metamorphic gate",
)

replace_exact(
    path,
    '''    let mut ids = BTreeSet::new();\n    for case in &corpus.cases {\n''',
    '''    let mut ids = BTreeSet::new();\n    let mut metamorphic_contracts = BTreeMap::<\n        String,\n        (RelationshipBenchLanguage, String, RelationshipBenchExpectedOutcome),\n    >::new();\n    let mut metamorphic_sizes = BTreeMap::<String, usize>::new();\n    for case in &corpus.cases {\n''',
    "metamorphic corpus validation state",
)

replace_exact(
    path,
    '''        match case.expected_outcome {\n''',
    '''        if let Some(group) = case.metamorphic_group.as_deref() {\n            if group.trim().is_empty() {\n                anyhow::bail!("case {} has an empty metamorphic_group", case.id);\n            }\n            *metamorphic_sizes.entry(group.to_string()).or_default() += 1;\n            let contract = (\n                case.language,\n                edge_type_name(&case.relationship).to_string(),\n                case.expected_outcome,\n            );\n            if let Some(existing) = metamorphic_contracts.get(group) {\n                if existing != &contract {\n                    anyhow::bail!(\n                        "metamorphic group {group} mixes language, relationship, or expected outcome contracts"\n                    );\n                }\n            } else {\n                metamorphic_contracts.insert(group.to_string(), contract);\n            }\n        }\n        match case.expected_outcome {\n''',
    "metamorphic case contract validation",
)

replace_exact(
    path,
    '''    }\n    Ok(())\n}\n\n#[cfg(test)]\nfn score_relationship_bench(\n''',
    '''    }\n    for (group, size) in metamorphic_sizes {\n        if size < 2 {\n            anyhow::bail!("metamorphic group {group} must contain at least two cases");\n        }\n    }\n    Ok(())\n}\n\n#[cfg(test)]\nfn score_relationship_bench(\n''',
    "metamorphic group cardinality validation",
)

replace_exact(
    path,
    '''    let mut wrong_target_counts = BTreeMap::<String, usize>::new();\n    let mut diagnostics = Vec::new();\n\n    let mut cases = corpus.cases.iter().collect::<Vec<_>>();\n''',
    '''    let mut wrong_target_counts = BTreeMap::<String, usize>::new();\n    let mut metamorphic_verdicts = BTreeMap::<String, Vec<bool>>::new();\n    let mut diagnostics = Vec::new();\n\n    let mut cases = corpus.cases.iter().collect::<Vec<_>>();\n''',
    "metamorphic scoring state",
)

replace_exact(
    path,
    '''        let outcome = score_relationship_case(case, observation, relationships);\n        for relationship in relationships.iter().filter(|relationship| {\n''',
    '''        let outcome = score_relationship_case(case, observation, relationships);\n        if let Some(group) = case.metamorphic_group.as_ref() {\n            metamorphic_verdicts\n                .entry(group.clone())\n                .or_default()\n                .push(case_conformance_verdict(&outcome.metrics));\n        }\n        for relationship in relationships.iter().filter(|relationship| {\n''',
    "record metamorphic case verdict",
)

replace_exact(
    path,
    '''    diagnostics.sort_by(|left, right| {\n        (&left.case_id, &left.kind, &left.message).cmp(&(&right.case_id, &right.kind, &right.message))\n    });\n\n    Ok(RelationshipBenchScoreReport {\n''',
    '''    diagnostics.sort_by(|left, right| {\n        (&left.case_id, &left.kind, &left.message).cmp(&(&right.case_id, &right.kind, &right.message))\n    });\n    let metamorphic_groups = metamorphic_verdicts.len();\n    let metamorphic_equivalent_groups = metamorphic_verdicts\n        .values()\n        .filter(|verdicts| {\n            verdicts\n                .first()\n                .map(|first| verdicts.iter().all(|verdict| verdict == first))\n                .unwrap_or(false)\n        })\n        .count();\n    let metamorphic_equivalence =\n        relationship_ratio(metamorphic_equivalent_groups, metamorphic_groups);\n\n    Ok(RelationshipBenchScoreReport {\n''',
    "compute metamorphic equivalence",
)

replace_exact(
    path,
    '''        observed_proof_kind_counts,\n        wrong_target_counts,\n        diagnostics,\n''',
    '''        observed_proof_kind_counts,\n        wrong_target_counts,\n        metamorphic_groups,\n        metamorphic_equivalent_groups,\n        metamorphic_equivalence,\n        diagnostics,\n''',
    "emit metamorphic report",
)

replace_exact(
    path,
    '''fn update_strategy_metrics(metrics: &mut RelationshipBenchStrategyMetrics, correct: bool) {\n''',
    '''fn case_conformance_verdict(metrics: &RelationshipBenchMetrics) -> bool {\n    metrics.false_positives == 0\n        && metrics.false_negatives == 0\n        && metrics.outcome_matches == metrics.outcome_cases\n        && (metrics.candidate_count_expected_cases == 0\n            || metrics.candidate_count_matches == metrics.candidate_count_expected_cases)\n        && (metrics.exact_range_cases == 0\n            || metrics.exact_range_matches == metrics.exact_range_cases)\n        && (metrics.proof_cases == 0 || metrics.proof_matches == metrics.proof_cases)\n}\n\nfn update_strategy_metrics(metrics: &mut RelationshipBenchStrategyMetrics, correct: bool) {\n''',
    "metamorphic verdict helper",
)

replace_exact(
    path,
    '''    fn case(\n        id: &str,\n        expected_outcome: RelationshipBenchExpectedOutcome,\n    ) -> RelationshipBenchCase {\n''',
    '''    pub(super) fn case(\n        id: &str,\n        expected_outcome: RelationshipBenchExpectedOutcome,\n    ) -> RelationshipBenchCase {\n''',
    "test case helper visibility",
)
replace_exact(
    path,
    '''    fn corpus(cases: Vec<RelationshipBenchCase>) -> RelationshipBenchCorpus {\n''',
    '''    pub(super) fn corpus(cases: Vec<RelationshipBenchCase>) -> RelationshipBenchCorpus {\n''',
    "test corpus helper visibility",
)
replace_exact(
    path,
    '''    fn observed(target: &str, authority: RelationshipAuthority) -> RelationshipBenchObservedRelationship {\n''',
    '''    pub(super) fn observed(\n        target: &str,\n        authority: RelationshipAuthority,\n    ) -> RelationshipBenchObservedRelationship {\n''',
    "test observation helper visibility",
)
replace_exact(
    path,
    '''    fn permissive_test_policy() -> RelationshipBenchPolicy {\n''',
    '''    pub(super) fn permissive_test_policy() -> RelationshipBenchPolicy {\n''',
    "test policy helper visibility",
)

replace_exact(
    path,
    '''            candidate_count_expected: None,\n            notes: None,\n''',
    '''            candidate_count_expected: None,\n            metamorphic_group: None,\n            notes: None,\n''',
    "test case metamorphic default",
)

replace_exact(
    path,
    '''            minimum_outcome_compliance: 0.0,\n            require_zero_false_negatives: false,\n            require_positive_and_negative_per_language_relationship: false,\n            require_frozen_corpus: false,\n''',
    '''            minimum_outcome_compliance: 0.0,\n            minimum_metamorphic_groups: 0,\n            minimum_metamorphic_equivalence: 0.0,\n            require_zero_false_negatives: false,\n            require_positive_and_negative_per_language_relationship: false,\n            require_metamorphic_group_per_language_relationship: false,\n            require_frozen_corpus: false,\n''',
    "test policy metamorphic defaults",
)

p = Path(path)
text = p.read_text()
text += '''\n\n#[cfg(test)]\nmod ri3_metamorphic_bench_tests {\n    use super::*;\n\n    #[test]\n    fn policy_rejects_unknown_threshold_fields() {\n        let raw = r#"{\n          "schema_version":"1.0.0",\n          "minimum_cases":0,\n          "minimum_cases_per_language":0,\n          "minimum_cases_per_language_relationship":0,\n          "minimum_negative_fraction":0.0,\n          "minimum_overall_precision":0.0,\n          "minimum_language_relationship_precision":0.0,\n          "maximum_must_not_emit_false_positive_rate":1.0,\n          "minimum_exact_range_compliance":0.0,\n          "minimum_proof_compliance":0.0,\n          "minimum_outcome_compliance":0.0,\n          "minimum_metamorphic_groups":0,\n          "minimum_metamorphic_equivalence":0.0,\n          "require_zero_false_negatives":false,\n          "require_positive_and_negative_per_language_relationship":false,\n          "require_metamorphic_group_per_language_relationship":false,\n          "require_frozen_corpus":false,\n          "future_unwired_threshold":1\n        }"#;\n        assert!(serde_json::from_str::<RelationshipBenchPolicy>(raw).is_err());\n    }\n\n    #[test]\n    fn corpus_rejects_singleton_metamorphic_group() {\n        let mut c = relationship_bench_tests::case(\n            "singleton",\n            RelationshipBenchExpectedOutcome::MustNotEmit,\n        );\n        c.metamorphic_group = Some("group:singleton".into());\n        let corpus = relationship_bench_tests::corpus(vec![c]);\n        assert!(validate_relationship_bench_corpus(&corpus).is_err());\n    }\n\n    #[test]\n    fn gate_enforces_metamorphic_thresholds_from_policy() {\n        let mut a = relationship_bench_tests::case(\n            "meta-a",\n            RelationshipBenchExpectedOutcome::MustNotEmit,\n        );\n        let mut b = relationship_bench_tests::case(\n            "meta-b",\n            RelationshipBenchExpectedOutcome::MustNotEmit,\n        );\n        a.metamorphic_group = Some("group:stable".into());\n        b.metamorphic_group = Some("group:stable".into());\n        let corpus = relationship_bench_tests::corpus(vec![a, b]);\n        let observations = vec![\n            RelationshipBenchObservation {\n                case_id: "meta-a".into(),\n                outcome: RelationshipBenchObservedOutcome::Unresolved,\n                candidate_count: 0,\n                relationships: Vec::new(),\n            },\n            RelationshipBenchObservation {\n                case_id: "meta-b".into(),\n                outcome: RelationshipBenchObservedOutcome::Proven,\n                candidate_count: 1,\n                relationships: vec![relationship_bench_tests::observed(\n                    "symbol:wrong",\n                    open_kioku_core::RelationshipAuthority::Authoritative,\n                )],\n            },\n        ];\n        let report = score_relationship_bench(&corpus, &observations).unwrap();\n        assert_eq!(report.metamorphic_groups, 1);\n        assert_eq!(report.metamorphic_equivalent_groups, 0);\n        assert_eq!(report.metamorphic_equivalence, 0.0);\n        let mut policy = relationship_bench_tests::permissive_test_policy();\n        policy.minimum_metamorphic_groups = 1;\n        policy.minimum_metamorphic_equivalence = 1.0;\n        let gate = evaluate_relationship_bench_gates(&corpus, &report, &policy);\n        assert!(!gate.passed);\n        assert!(gate\n            .failures\n            .iter()\n            .any(|failure| failure.contains("metamorphic equivalence")));\n    }\n}\n'''
p.write_text(text)
