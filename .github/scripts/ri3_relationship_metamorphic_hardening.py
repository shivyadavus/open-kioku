#!/usr/bin/env python3
from pathlib import Path

path = Path("crates/open-kioku-cli/src/bench/relationship.rs")
text = path.read_text()


def replace_once(old: str, new: str, label: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    text = text.replace(old, new)


replace_once(
    "    let mut metamorphic_verdicts = BTreeMap::<String, Vec<bool>>::new();\n",
    "    let mut metamorphic_signatures =\n        BTreeMap::<String, Vec<(String, RelationshipMetamorphicSignature)>>::new();\n",
    "metamorphic accumulator",
)

replace_once(
    '''        if let Some(group) = case.metamorphic_group.as_ref() {
            metamorphic_verdicts
                .entry(group.clone())
                .or_default()
                .push(case_conformance_verdict(&outcome.metrics));
        }
''',
    '''        if let Some(group) = case.metamorphic_group.as_ref() {
            metamorphic_signatures
                .entry(group.clone())
                .or_default()
                .push((
                    case.id.clone(),
                    relationship_metamorphic_signature(observation, relationships),
                ));
        }
''',
    "metamorphic signature capture",
)

replace_once(
    '''    diagnostics.sort_by(|left, right| {
        (&left.case_id, &left.kind, &left.message).cmp(&(&right.case_id, &right.kind, &right.message))
    });
    let metamorphic_groups = metamorphic_verdicts.len();
    let metamorphic_equivalent_groups = metamorphic_verdicts
        .values()
        .filter(|verdicts| {
            verdicts
                .first()
                .map(|first| verdicts.iter().all(|verdict| verdict == first))
                .unwrap_or(false)
        })
        .count();
    let metamorphic_equivalence =
        relationship_ratio(metamorphic_equivalent_groups, metamorphic_groups);
''',
    '''    let metamorphic_groups = metamorphic_signatures.len();
    let mut metamorphic_equivalent_groups = 0usize;
    for (group, variants) in &metamorphic_signatures {
        let equivalent = variants
            .first()
            .map(|(_, first)| variants.iter().all(|(_, signature)| signature == first))
            .unwrap_or(false);
        if equivalent {
            metamorphic_equivalent_groups += 1;
            continue;
        }
        let variant_ids = variants
            .iter()
            .map(|(case_id, _)| case_id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        diagnostics.push(RelationshipBenchDiagnostic {
            case_id: variants
                .first()
                .map(|(case_id, _)| case_id.clone())
                .unwrap_or_else(|| group.clone()),
            kind: "metamorphic_identity_mismatch".into(),
            message: format!(
                "metamorphic group {group} produced different canonical outcomes or authoritative relationship/proof identities across variants: {variant_ids}"
            ),
            expected_target_symbol_id: None,
            observed_authoritative_targets: Vec::new(),
        });
    }
    let metamorphic_equivalence =
        relationship_ratio(metamorphic_equivalent_groups, metamorphic_groups);
    diagnostics.sort_by(|left, right| {
        (&left.case_id, &left.kind, &left.message).cmp(&(&right.case_id, &right.kind, &right.message))
    });
''',
    "metamorphic equivalence calculation",
)

replace_once(
    '''fn case_conformance_verdict(metrics: &RelationshipBenchMetrics) -> bool {
    metrics.false_positives == 0
        && metrics.false_negatives == 0
        && metrics.outcome_matches == metrics.outcome_cases
        && (metrics.candidate_count_expected_cases == 0
            || metrics.candidate_count_matches == metrics.candidate_count_expected_cases)
        && (metrics.exact_range_cases == 0
            || metrics.exact_range_matches == metrics.exact_range_cases)
        && (metrics.proof_cases == 0 || metrics.proof_matches == metrics.proof_cases)
}

''',
    '''#[derive(Debug, Clone, PartialEq, Eq)]
struct RelationshipMetamorphicSignature {
    outcome: RelationshipBenchObservedOutcome,
    authoritative_relationships: Vec<MetamorphicRelationshipKey>,
}

type MetamorphicRelationshipKey = (
    String,
    String,
    String,
    Vec<String>,
    Vec<(u32, u32, u32, u32)>,
);

fn relationship_metamorphic_signature(
    observation: Option<&RelationshipBenchObservation>,
    relationships: &[RelationshipBenchObservedRelationship],
) -> RelationshipMetamorphicSignature {
    let mut authoritative_relationships = relationships
        .iter()
        .filter(|relationship| {
            relationship.authority == open_kioku_core::RelationshipAuthority::Authoritative
        })
        .map(metamorphic_relationship_key)
        .collect::<Vec<_>>();
    authoritative_relationships.sort();
    authoritative_relationships.dedup();
    RelationshipMetamorphicSignature {
        outcome: observation.map(|value| value.outcome).unwrap_or_default(),
        authoritative_relationships,
    }
}

fn metamorphic_relationship_key(
    relationship: &RelationshipBenchObservedRelationship,
) -> MetamorphicRelationshipKey {
    let mut proof_kinds = relationship
        .proof_kinds
        .iter()
        .map(|kind| proof_kind_name(kind).to_string())
        .collect::<Vec<_>>();
    proof_kinds.sort();
    proof_kinds.dedup();
    let mut source_ranges = relationship
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
        .collect::<Vec<_>>();
    source_ranges.sort();
    source_ranges.dedup();
    (
        relationship.source_symbol_id.0.clone(),
        relationship.target_symbol_id.0.clone(),
        edge_type_name(&relationship.relationship).to_string(),
        proof_kinds,
        source_ranges,
    )
}

''',
    "metamorphic signature helpers",
)

if "metamorphic_equivalence_compares_canonical_relationship_and_proof_identity" not in text:
    text += r'''

#[cfg(test)]
mod ri3_metamorphic_identity_tests {
    use super::*;
    use open_kioku_core::{RelationshipAuthority, RelationshipProofKind, SourceRange};

    #[test]
    fn metamorphic_equivalence_compares_canonical_relationship_and_proof_identity() {
        let mut a = relationship_bench_tests::case(
            "identity-a",
            RelationshipBenchExpectedOutcome::MustEmit,
        );
        let mut b = relationship_bench_tests::case(
            "identity-b",
            RelationshipBenchExpectedOutcome::MustEmit,
        );
        a.metamorphic_group = Some("group:identity".into());
        b.metamorphic_group = Some("group:identity".into());
        let corpus = relationship_bench_tests::corpus(vec![a, b]);

        let range = SourceRange {
            start_line: 10,
            start_column: 4,
            end_line: 10,
            end_column: 17,
        };
        let mut first = relationship_bench_tests::observed(
            "symbol:target",
            RelationshipAuthority::Authoritative,
        );
        first.proof_kinds = BTreeSet::from([RelationshipProofKind::ExactCallSite]);
        first.source_ranges.push(range.clone());

        let mut second = relationship_bench_tests::observed(
            "symbol:target",
            RelationshipAuthority::Authoritative,
        );
        second.proof_kinds = BTreeSet::from([
            RelationshipProofKind::ExactCallSite,
            RelationshipProofKind::QualifiedName,
        ]);
        second.source_ranges.push(range);

        let observations = vec![
            RelationshipBenchObservation {
                case_id: "identity-a".into(),
                outcome: RelationshipBenchObservedOutcome::Proven,
                candidate_count: 1,
                relationships: vec![first],
            },
            RelationshipBenchObservation {
                case_id: "identity-b".into(),
                outcome: RelationshipBenchObservedOutcome::Proven,
                candidate_count: 1,
                relationships: vec![second],
            },
        ];
        let report = score_relationship_bench(&corpus, &observations).unwrap();

        assert_eq!(report.overall.false_positives, 0);
        assert_eq!(report.overall.false_negatives, 0);
        assert_eq!(report.metamorphic_groups, 1);
        assert_eq!(report.metamorphic_equivalent_groups, 0);
        assert_eq!(report.metamorphic_equivalence, 0.0);
        assert!(report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == "metamorphic_identity_mismatch"));
    }
}
'''

path.write_text(text)
