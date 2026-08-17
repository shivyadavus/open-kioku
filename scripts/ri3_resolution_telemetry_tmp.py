from pathlib import Path


def replace_exact(path: str, old: str, new: str, label: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    observed = text.count(old)
    if observed != count:
        raise SystemExit(f"{label} seam changed: expected {count}, observed {observed}")
    p.write_text(text.replace(old, new, count))


# Preserve complete evaluation cardinality on a proven winner so telemetry does not lose weak
# alternatives merely because the outcome has one structural target.
replace_exact(
    "crates/open-kioku-resolution/src/pipeline.rs",
    '''    pub proofs: Vec<RelationshipProof>,\n    pub evidence: Vec<ResolutionEvidence>,\n}\n''',
    '''    pub proofs: Vec<RelationshipProof>,\n    pub evidence: Vec<ResolutionEvidence>,\n    /// Complete normalized target count considered by the evaluation that selected this candidate.\n    pub candidates_considered: usize,\n    /// Non-authoritative candidates retained by the evaluation alongside this proven target.\n    pub heuristic_candidates_retained: usize,\n}\n''',
    "candidate telemetry fields",
)
replace_exact(
    "crates/open-kioku-resolution/src/pipeline.rs",
    '''            proofs: Vec::new(),\n            evidence: Vec::new(),\n        }\n''',
    '''            proofs: Vec::new(),\n            evidence: Vec::new(),\n            candidates_considered: 1,\n            heuristic_candidates_retained: 0,\n        }\n''',
    "candidate telemetry defaults",
)
replace_exact(
    "crates/open-kioku-resolution/src/pipeline.rs",
    '''    let authoritative = candidates\n        .iter()\n        .filter(|candidate| candidate.authority(edge_type) == RelationshipAuthority::Authoritative)\n''',
    '''    let candidates_considered = candidates.len();\n    let heuristic_candidates_retained = candidates\n        .iter()\n        .filter(|candidate| candidate.authority(edge_type) != RelationshipAuthority::Authoritative)\n        .count();\n    let authoritative = candidates\n        .iter()\n        .filter(|candidate| candidate.authority(edge_type) == RelationshipAuthority::Authoritative)\n''',
    "evaluation cardinality",
)
replace_exact(
    "crates/open-kioku-resolution/src/pipeline.rs",
    '''        let candidate = candidates\n            .iter()\n            .find(|candidate| candidate.target_symbol_id.0 == *target)\n            .expect("authoritative target came from normalized candidate set")\n            .clone();\n        return ResolutionOutcome::Proven { candidate };\n''',
    '''        let mut candidate = candidates\n            .iter()\n            .find(|candidate| candidate.target_symbol_id.0 == *target)\n            .expect("authoritative target came from normalized candidate set")\n            .clone();\n        candidate.candidates_considered = candidates_considered;\n        candidate.heuristic_candidates_retained = heuristic_candidates_retained;\n        return ResolutionOutcome::Proven { candidate };\n''',
    "proven candidate cardinality",
)

# Add additive, backwards-compatible per-relationship telemetry to the existing quality report.
replace_exact(
    "crates/open-kioku-ingest/src/lib.rs",
    '''#[derive(Debug, Clone, Default, Serialize, Deserialize)]\npub struct ResolutionQualityReport {\n    pub call_sites: usize,\n''',
    '''#[derive(Debug, Clone, Default, Serialize, Deserialize)]\npub struct RelationshipResolutionQuality {\n    pub candidates_considered: usize,\n    pub proven: usize,\n    pub ambiguous: usize,\n    pub unresolved: usize,\n    pub external: usize,\n    pub heuristic_candidates_retained: usize,\n    #[serde(default)]\n    pub proof_kind_counts: BTreeMap<String, usize>,\n    #[serde(default)]\n    pub resolver_strategy_counts: BTreeMap<String, usize>,\n}\n\n#[derive(Debug, Clone, Default, Serialize, Deserialize)]\npub struct ResolutionQualityReport {\n    pub call_sites: usize,\n''',
    "relationship telemetry struct",
)
replace_exact(
    "crates/open-kioku-ingest/src/lib.rs",
    '''    pub disagreement: usize,\n}\n\n#[derive(Debug, Clone)]\npub struct IndexProgress {\n''',
    '''    pub disagreement: usize,\n    #[serde(default)]\n    pub by_relationship: BTreeMap<String, RelationshipResolutionQuality>,\n}\n\nimpl ResolutionQualityReport {\n    fn record_outcome(\n        &mut self,\n        edge_type: &GraphEdgeType,\n        outcome: &open_kioku_resolution::ResolutionOutcome,\n    ) {\n        let key = relationship_metric_key(edge_type);\n        let metrics = self.by_relationship.entry(key).or_default();\n        match outcome {\n            open_kioku_resolution::ResolutionOutcome::Proven { candidate } => {\n                metrics.candidates_considered += candidate.candidates_considered;\n                metrics.proven += 1;\n                metrics.heuristic_candidates_retained += candidate.heuristic_candidates_retained;\n                record_candidate_evidence(metrics, candidate);\n            }\n            open_kioku_resolution::ResolutionOutcome::Ambiguous { candidates, .. } => {\n                metrics.candidates_considered += candidates.len();\n                metrics.ambiguous += 1;\n                metrics.heuristic_candidates_retained += candidates\n                    .iter()\n                    .filter(|candidate| {\n                        candidate.authority(edge_type)\n                            != open_kioku_core::RelationshipAuthority::Authoritative\n                    })\n                    .count();\n                for candidate in candidates {\n                    record_candidate_evidence(metrics, candidate);\n                }\n            }\n            open_kioku_resolution::ResolutionOutcome::Unresolved { candidates, .. } => {\n                metrics.candidates_considered += candidates.len();\n                metrics.unresolved += 1;\n                metrics.heuristic_candidates_retained += candidates.len();\n                for candidate in candidates {\n                    record_candidate_evidence(metrics, candidate);\n                }\n            }\n            open_kioku_resolution::ResolutionOutcome::External { .. } => {\n                metrics.external += 1;\n            }\n        }\n    }\n\n    fn record_reference_occurrence(&mut self, occurrence: &SymbolOccurrence) {\n        if occurrence.is_definition {\n            return;\n        }\n        let metrics = self\n            .by_relationship\n            .entry(relationship_metric_key(&GraphEdgeType::References))\n            .or_default();\n        metrics.candidates_considered += 1;\n        if occurrence.provenance == EvidenceSourceType::Scip\n            && occurrence.confidence == Confidence::Exact\n            && occurrence.source_range.is_some()\n        {\n            metrics.proven += 1;\n            *metrics\n                .proof_kind_counts\n                .entry("exact_occurrence".into())\n                .or_default() += 1;\n            *metrics\n                .resolver_strategy_counts\n                .entry("scip_exact_occurrence".into())\n                .or_default() += 1;\n        } else {\n            metrics.unresolved += 1;\n            metrics.heuristic_candidates_retained += 1;\n        }\n    }\n}\n\nfn relationship_metric_key(edge_type: &GraphEdgeType) -> String {\n    serde_json::to_value(edge_type)\n        .ok()\n        .and_then(|value| value.as_str().map(ToOwned::to_owned))\n        .unwrap_or_else(|| format!("{edge_type:?}"))\n}\n\nfn proof_kind_metric_key(kind: &open_kioku_core::RelationshipProofKind) -> String {\n    serde_json::to_value(kind)\n        .ok()\n        .and_then(|value| value.as_str().map(ToOwned::to_owned))\n        .unwrap_or_else(|| format!("{kind:?}"))\n}\n\nfn record_candidate_evidence(\n    metrics: &mut RelationshipResolutionQuality,\n    candidate: &open_kioku_resolution::ResolutionCandidate,\n) {\n    let mut proof_kinds = BTreeSet::new();\n    let mut strategies = BTreeSet::new();\n    for proof in &candidate.proofs {\n        proof_kinds.insert(proof_kind_metric_key(&proof.kind));\n        if !proof.resolver_strategy.is_empty() {\n            strategies.insert(proof.resolver_strategy.clone());\n        }\n    }\n    for kind in proof_kinds {\n        *metrics.proof_kind_counts.entry(kind).or_default() += 1;\n    }\n    for strategy in strategies {\n        *metrics.resolver_strategy_counts.entry(strategy).or_default() += 1;\n    }\n}\n\n#[derive(Debug, Clone)]\npub struct IndexProgress {\n''',
    "relationship telemetry helpers",
)

# REFERENCES are produced outside ResolutionOutcome, so account for exact/proofless occurrences at
# the same ingest boundary instead of leaving that relationship family invisible in telemetry.
replace_exact(
    "crates/open-kioku-ingest/src/lib.rs",
    '''        let mut quality_report = ResolutionQualityReport::default();\n        let mut resolved_relationships = Vec::new();\n\n        let file_lookup: HashMap<FileId, &File> = files.iter().map(|f| (f.id.clone(), f)).collect();\n''',
    '''        let mut quality_report = ResolutionQualityReport::default();\n        for occurrence in &occurrences {\n            quality_report.record_reference_occurrence(occurrence);\n        }\n        let mut resolved_relationships = Vec::new();\n\n        let file_lookup: HashMap<FileId, &File> = files.iter().map(|f| (f.id.clone(), f)).collect();\n''',
    "reference telemetry initialization",
)

# Record every CALLS outcome before legacy comparison mutates aggregate counters.
replace_exact(
    "crates/open-kioku-ingest/src/lib.rs",
    '''                        let v2_outcome = open_kioku_resolution::resolve_call_outcome(call, &ctx);\n                        let semantic_target = match &v2_outcome {\n''',
    '''                        let v2_outcome = open_kioku_resolution::resolve_call_outcome(call, &ctx);\n                        quality_report.record_outcome(&GraphEdgeType::Calls, &v2_outcome);\n                        let semantic_target = match &v2_outcome {\n''',
    "call outcome telemetry",
)

# Record inheritance and declared-type outcomes before Proven-only graph emission.
replace_exact(
    "crates/open-kioku-ingest/src/lib.rs",
    '''                let (edge_type, outcome) =\n                    open_kioku_resolution::resolve_inheritance_relationship_outcome(site, &ctx);\n                if let open_kioku_resolution::ResolutionOutcome::Proven { candidate } = outcome {\n''',
    '''                let (edge_type, outcome) =\n                    open_kioku_resolution::resolve_inheritance_relationship_outcome(site, &ctx);\n                quality_report.record_outcome(&edge_type, &outcome);\n                if let open_kioku_resolution::ResolutionOutcome::Proven { candidate } = outcome {\n''',
    "inheritance outcome telemetry",
)
replace_exact(
    "crates/open-kioku-ingest/src/lib.rs",
    '''                let Some((source, outcome)) =\n                    open_kioku_resolution::resolve_declared_type_use_outcome(binding, &ctx)\n                else {\n                    continue;\n                };\n                if let open_kioku_resolution::ResolutionOutcome::Proven { candidate } = outcome {\n''',
    '''                let Some((source, outcome)) =\n                    open_kioku_resolution::resolve_declared_type_use_outcome(binding, &ctx)\n                else {\n                    continue;\n                };\n                quality_report.record_outcome(&GraphEdgeType::UsesType, &outcome);\n                if let open_kioku_resolution::ResolutionOutcome::Proven { candidate } = outcome {\n''',
    "type-use outcome telemetry",
)

# Add focused candidate-cardinality test at the resolution layer. This protects the strong-vs-weak
# case that motivated the telemetry metadata.
p = Path("crates/open-kioku-resolution/src/pipeline.rs")
text = p.read_text()
text += r'''

#[cfg(test)]
mod ri3_telemetry_tests {
    use super::*;
    use open_kioku_core::{RelationshipProofKind, RelationshipProof};

    fn candidate(id: &str, authoritative: bool) -> ResolutionCandidate {
        let mut candidate = ResolutionCandidate::new(SymbolId::new(id), Confidence::High);
        if authoritative {
            candidate.proofs.push(RelationshipProof::new(
                RelationshipProofKind::ExactReference,
                "exact_reference",
                1,
            ));
        } else {
            candidate.proofs.push(RelationshipProof::new(
                RelationshipProofKind::QualifiedName,
                "qualified_only",
                1,
            ));
        }
        candidate
    }

    #[test]
    fn proven_outcome_retains_complete_candidate_cardinality() {
        let outcome = evaluate_candidates(
            &GraphEdgeType::References,
            vec![candidate("symbol:weak", false), candidate("symbol:strong", true)],
        );
        let ResolutionOutcome::Proven { candidate } = outcome else {
            panic!("expected one authoritative winner");
        };
        assert_eq!(candidate.candidates_considered, 2);
        assert_eq!(candidate.heuristic_candidates_retained, 1);
    }
}
'''
p.write_text(text)
