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
    '''    #[serde(default, skip_serializing_if = "Option::is_none")]\n    git_commit: Option<String>,\n    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]\n    index_config: BTreeMap<String, serde_json::Value>,\n''',
    '''    #[serde(default, skip_serializing_if = "Option::is_none")]\n    git_commit: Option<String>,\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    analysis_semantics_fingerprint: Option<String>,\n    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]\n    adapter_versions: BTreeMap<String, String>,\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    proof_policy_version: Option<String>,\n    #[serde(default, skip_serializing_if = "Option::is_none")]\n    index_mode: Option<String>,\n    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]\n    index_config: BTreeMap<String, serde_json::Value>,\n''',
    "reproducibility metadata fields",
)
replace_exact(
    path,
    '''    require_metamorphic_group_per_language_relationship: bool,\n    require_frozen_corpus: bool,\n}\n''',
    '''    require_metamorphic_group_per_language_relationship: bool,\n    require_reproducibility_metadata: bool,\n    require_frozen_corpus: bool,\n}\n''',
    "metadata policy flag",
)
replace_exact(
    path,
    '''    if policy.require_frozen_corpus && corpus.status != RelationshipBenchCorpusStatus::Frozen {\n        failures.push("release gating requires a frozen relationship corpus".to_string());\n    }\n''',
    '''    if policy.require_frozen_corpus && corpus.status != RelationshipBenchCorpusStatus::Frozen {\n        failures.push("release gating requires a frozen relationship corpus".to_string());\n    }\n    if policy.require_reproducibility_metadata {\n        for (name, value) in [\n            ("git_commit", report.run_metadata.git_commit.as_deref()),\n            (\n                "analysis_semantics_fingerprint",\n                report.run_metadata.analysis_semantics_fingerprint.as_deref(),\n            ),\n            (\n                "proof_policy_version",\n                report.run_metadata.proof_policy_version.as_deref(),\n            ),\n            ("index_mode", report.run_metadata.index_mode.as_deref()),\n        ] {\n            if value.map(str::trim).filter(|value| !value.is_empty()).is_none() {\n                failures.push(format!("run metadata is missing required {name}"));\n            }\n        }\n        if report.run_metadata.adapter_versions.is_empty() {\n            failures.push("run metadata is missing required adapter_versions".to_string());\n        }\n    }\n''',
    "metadata release gate",
)
replace_exact(
    path,
    '''            require_metamorphic_group_per_language_relationship: false,\n            require_frozen_corpus: false,\n''',
    '''            require_metamorphic_group_per_language_relationship: false,\n            require_reproducibility_metadata: false,\n            require_frozen_corpus: false,\n''',
    "test policy metadata default",
)
replace_exact(
    path,
    '''          "require_metamorphic_group_per_language_relationship":false,\n          "require_frozen_corpus":false,\n''',
    '''          "require_metamorphic_group_per_language_relationship":false,\n          "require_reproducibility_metadata":false,\n          "require_frozen_corpus":false,\n''',
    "strict policy parser fixture",
)

p = Path(path)
text = p.read_text()
text += r'''

#[cfg(test)]
mod ri3_reproducibility_metadata_tests {
    use super::*;
    use open_kioku_core::{RelationshipAuthority, RelationshipProofKind, SourceRange};

    #[test]
    fn release_gate_fails_closed_when_reproducibility_metadata_is_missing() {
        let range = SourceRange {
            start_line: 10,
            start_column: 4,
            end_line: 10,
            end_column: 17,
        };
        let mut case = relationship_bench_tests::case(
            "metadata-fixture",
            RelationshipBenchExpectedOutcome::MustEmit,
        );
        case.expected_source_range = Some(range.clone());
        case.expected_proof_kinds = BTreeSet::from([RelationshipProofKind::ExactCallSite]);
        let corpus = relationship_bench_tests::corpus(vec![case]);

        let mut relationship = relationship_bench_tests::observed(
            "symbol:target",
            RelationshipAuthority::Authoritative,
        );
        relationship.source_ranges.push(range);
        relationship.proof_kinds = BTreeSet::from([RelationshipProofKind::ExactCallSite]);
        let observations = vec![RelationshipBenchObservation {
            case_id: "metadata-fixture".into(),
            outcome: RelationshipBenchObservedOutcome::Proven,
            candidate_count: 1,
            relationships: vec![relationship],
        }];

        let mut report = score_relationship_bench(&corpus, &observations).unwrap();
        let mut policy = relationship_bench_tests::permissive_test_policy();
        policy.require_reproducibility_metadata = true;
        let gate = evaluate_relationship_bench_gates(&corpus, &report, &policy);
        assert!(!gate.passed);
        assert!(gate.failures.iter().any(|failure| failure.contains("git_commit")));
        assert!(gate
            .failures
            .iter()
            .any(|failure| failure.contains("analysis_semantics_fingerprint")));
        assert!(gate
            .failures
            .iter()
            .any(|failure| failure.contains("adapter_versions")));

        report.run_metadata.git_commit = Some("abc123".into());
        report.run_metadata.analysis_semantics_fingerprint = Some("semantics:v1".into());
        report.run_metadata
            .adapter_versions
            .insert("rust".into(), "adapter:v1".into());
        report.run_metadata.proof_policy_version = Some("ri3.1".into());
        report.run_metadata.index_mode = Some("full".into());
        let gate = evaluate_relationship_bench_gates(&corpus, &report, &policy);
        assert!(gate.passed, "{:?}", gate.failures);
    }
}
'''
p.write_text(text)

thresholds = Path("benchmarks/relationship-thresholds.json")
raw = thresholds.read_text()
old = '  "require_metamorphic_group_per_language_relationship": true,\n  "require_frozen_corpus": true\n'
new = '  "require_metamorphic_group_per_language_relationship": true,\n  "require_reproducibility_metadata": true,\n  "require_frozen_corpus": true\n'
if raw.count(old) != 1:
    raise SystemExit(f"relationship threshold metadata seam changed: {raw.count(old)}")
thresholds.write_text(raw.replace(old, new, 1))
