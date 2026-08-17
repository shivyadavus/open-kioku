use crate::context::{ResolutionResult, UnresolvedReason};
use crate::evidence::{ResolutionEvidence, ResolutionEvidenceKind};
use open_kioku_core::{
    normalize_relationship_proofs, relationship_authority, Confidence, EvidenceSourceType,
    GraphEdgeType, RelationshipAuthority, RelationshipProof, SymbolId,
};
use std::collections::BTreeMap;

/// One plausible structural target discovered by one or more resolver strategies.
///
/// `confidence` remains retrieval/ranking metadata. Structural truth is decided only from
/// `proofs` through the core relationship-authority policy.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolutionCandidate {
    pub target_symbol_id: SymbolId,
    pub confidence: Confidence,
    pub proofs: Vec<RelationshipProof>,
    pub evidence: Vec<ResolutionEvidence>,
}

impl ResolutionCandidate {
    pub fn new(target_symbol_id: SymbolId, confidence: Confidence) -> Self {
        Self {
            target_symbol_id,
            confidence,
            proofs: Vec::new(),
            evidence: Vec::new(),
        }
    }

    pub fn authority(&self, edge_type: &GraphEdgeType) -> RelationshipAuthority {
        relationship_authority(edge_type, &self.proofs)
    }

    fn merge_from(&mut self, mut other: Self) {
        if other.confidence.score() > self.confidence.score() {
            self.confidence = other.confidence;
        }
        self.proofs.append(&mut other.proofs);
        self.evidence.append(&mut other.evidence);
    }

    fn normalize(mut self) -> Self {
        for proof in &mut self.proofs {
            if proof.target_symbol_id.is_none() {
                proof.target_symbol_id = Some(self.target_symbol_id.clone());
            }
        }
        self.proofs = normalize_relationship_proofs(self.proofs);
        normalize_resolution_evidence(&mut self.evidence);
        self
    }
}

/// Internal result of proof-gated candidate evaluation.
///
/// Ambiguous and unresolved outcomes retain candidates so retrieval/context layers can still use
/// them heuristically without turning them into structural graph truth.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolutionOutcome {
    Proven {
        candidate: ResolutionCandidate,
    },
    Ambiguous {
        candidates: Vec<ResolutionCandidate>,
        reason: String,
    },
    Unresolved {
        candidates: Vec<ResolutionCandidate>,
        reason: String,
    },
    External {
        identity: String,
        evidence: Vec<ResolutionEvidence>,
    },
}

/// Canonicalize candidate order and merge duplicate target identities without using discovery order
/// as a semantic signal.
pub fn normalize_candidates(candidates: Vec<ResolutionCandidate>) -> Vec<ResolutionCandidate> {
    let mut by_target = BTreeMap::<String, ResolutionCandidate>::new();
    for candidate in candidates {
        let key = candidate.target_symbol_id.0.clone();
        match by_target.get_mut(&key) {
            Some(existing) => existing.merge_from(candidate),
            None => {
                by_target.insert(key, candidate);
            }
        }
    }
    by_target
        .into_values()
        .map(ResolutionCandidate::normalize)
        .collect()
}

/// Evaluate a complete candidate set. Candidate rank, generation order, lexical score and confidence
/// are intentionally absent from the authority decision.
pub fn evaluate_candidates(
    edge_type: &GraphEdgeType,
    candidates: Vec<ResolutionCandidate>,
) -> ResolutionOutcome {
    let candidates = normalize_candidates(candidates);
    let authoritative = candidates
        .iter()
        .filter(|candidate| candidate.authority(edge_type) == RelationshipAuthority::Authoritative)
        .map(|candidate| candidate.target_symbol_id.0.clone())
        .collect::<Vec<_>>();

    if authoritative.len() == 1 {
        let target = &authoritative[0];
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.target_symbol_id.0 == *target)
            .expect("authoritative target came from normalized candidate set")
            .clone();
        return ResolutionOutcome::Proven { candidate };
    }

    if authoritative.len() > 1 {
        return ResolutionOutcome::Ambiguous {
            candidates,
            reason: format!(
                "{} candidates satisfy authoritative relationship proof policy",
                authoritative.len()
            ),
        };
    }

    if candidates.len() > 1 {
        return ResolutionOutcome::Ambiguous {
            reason: format!(
                "{} plausible candidates remain but none has unique authoritative proof",
                candidates.len()
            ),
            candidates,
        };
    }

    ResolutionOutcome::Unresolved {
        reason: if candidates.is_empty() {
            "no plausible structural candidate was discovered".into()
        } else {
            "candidate discovered but relationship proof policy did not authorize it".into()
        },
        candidates,
    }
}

impl ResolutionOutcome {
    /// Compatibility adapter for the existing public resolver result while callers migrate to the
    /// richer proof-gated outcome.
    pub fn into_legacy_result(self) -> ResolutionResult {
        match self {
            Self::Proven { candidate } => ResolutionResult::Resolved {
                target: candidate.target_symbol_id,
                confidence: candidate.confidence,
                evidence: candidate.evidence,
            },
            Self::Ambiguous { candidates, reason } => ResolutionResult::Ambiguous {
                candidates: candidates
                    .iter()
                    .map(|candidate| candidate.target_symbol_id.clone())
                    .collect(),
                reason,
                evidence: merged_candidate_evidence(&candidates),
            },
            Self::Unresolved { candidates, reason } => {
                let mut evidence = merged_candidate_evidence(&candidates);
                evidence.push(ResolutionEvidence {
                    kind: ResolutionEvidenceKind::FallbackHeuristic,
                    source_type: EvidenceSourceType::Heuristic,
                    file_range: None,
                    symbol_id: candidates
                        .first()
                        .map(|candidate| candidate.target_symbol_id.clone()),
                    message: reason,
                });
                normalize_resolution_evidence(&mut evidence);
                ResolutionResult::Unresolved {
                    reason: UnresolvedReason::NoCandidate,
                    evidence,
                }
            }
            Self::External { identity, .. } => ResolutionResult::External { package: identity },
        }
    }
}

fn merged_candidate_evidence(candidates: &[ResolutionCandidate]) -> Vec<ResolutionEvidence> {
    let mut evidence = candidates
        .iter()
        .flat_map(|candidate| candidate.evidence.iter().cloned())
        .collect::<Vec<_>>();
    normalize_resolution_evidence(&mut evidence);
    evidence
}

fn normalize_resolution_evidence(evidence: &mut Vec<ResolutionEvidence>) {
    evidence.sort_by_key(evidence_key);
    evidence.dedup();
}

fn evidence_key(
    evidence: &ResolutionEvidence,
) -> (u8, u8, String, Option<u32>, Option<u32>, String, String) {
    let (path, start, end) = evidence
        .file_range
        .as_ref()
        .map(|range| {
            (
                range.path.to_string_lossy().replace('\\', "/"),
                range.line_range.as_ref().map(|line| line.start),
                range.line_range.as_ref().map(|line| line.end),
            )
        })
        .unwrap_or_else(|| (String::new(), None, None));
    (
        evidence_kind_rank(&evidence.kind),
        evidence_source_rank(&evidence.source_type),
        path,
        start,
        end,
        evidence
            .symbol_id
            .as_ref()
            .map(|symbol| symbol.0.clone())
            .unwrap_or_default(),
        evidence.message.clone(),
    )
}

fn evidence_kind_rank(kind: &ResolutionEvidenceKind) -> u8 {
    match kind {
        ResolutionEvidenceKind::LexicalScope => 0,
        ResolutionEvidenceKind::TypedBinding => 1,
        ResolutionEvidenceKind::ExactImport => 2,
        ResolutionEvidenceKind::ExplicitImport => 3,
        ResolutionEvidenceKind::ImplicitSelf => 4,
        ResolutionEvidenceKind::SameFile => 5,
        ResolutionEvidenceKind::InheritedMember => 6,
        ResolutionEvidenceKind::InheritanceGraph => 7,
        ResolutionEvidenceKind::SCIPOccurrence => 8,
        ResolutionEvidenceKind::FallbackHeuristic => 9,
    }
}

fn evidence_source_rank(source: &EvidenceSourceType) -> u8 {
    match source {
        EvidenceSourceType::TreeSitter => 0,
        EvidenceSourceType::Scip => 1,
        EvidenceSourceType::Lsp => 2,
        EvidenceSourceType::Regex => 3,
        EvidenceSourceType::Lexical => 4,
        EvidenceSourceType::Semantic => 5,
        EvidenceSourceType::Runtime => 6,
        EvidenceSourceType::GitHistory => 7,
        EvidenceSourceType::StaticAnalysis => 8,
        EvidenceSourceType::ExternalIntegration => 9,
        EvidenceSourceType::Heuristic => 10,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{RelationshipProof, RelationshipProofKind};

    fn proof(kind: RelationshipProofKind, target: &SymbolId, strategy: &str) -> RelationshipProof {
        let mut proof = RelationshipProof::new(kind, strategy, 1);
        proof.target_symbol_id = Some(target.clone());
        proof
    }

    fn call_candidate(
        target: &str,
        target_proof: RelationshipProofKind,
        strategy: &str,
    ) -> ResolutionCandidate {
        let target = SymbolId::new(target);
        let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
        candidate.proofs.push(proof(
            RelationshipProofKind::ExactCallSite,
            &target,
            "call_site",
        ));
        candidate
            .proofs
            .push(proof(target_proof, &target, strategy));
        candidate
    }

    #[test]
    fn confidence_does_not_turn_a_weak_candidate_into_truth() {
        let target = SymbolId::new("symbol:weak");
        let mut candidate = ResolutionCandidate::new(target.clone(), Confidence::Exact);
        candidate
            .proofs
            .push(proof(RelationshipProofKind::QualifiedName, &target, "name"));

        let outcome = evaluate_candidates(&GraphEdgeType::Calls, vec![candidate]);
        assert!(matches!(outcome, ResolutionOutcome::Unresolved { .. }));
    }

    #[test]
    fn one_authoritative_candidate_overrides_weaker_alternatives() {
        let strong = call_candidate(
            "symbol:strong",
            RelationshipProofKind::ExactReference,
            "exact_reference",
        );
        let weak_target = SymbolId::new("symbol:weak");
        let mut weak = ResolutionCandidate::new(weak_target.clone(), Confidence::Exact);
        weak.proofs.push(proof(
            RelationshipProofKind::QualifiedName,
            &weak_target,
            "qualified_name",
        ));

        let outcome = evaluate_candidates(&GraphEdgeType::Calls, vec![weak, strong]);
        match outcome {
            ResolutionOutcome::Proven { candidate } => {
                assert_eq!(candidate.target_symbol_id.0, "symbol:strong");
            }
            other => panic!("expected one proven target, got {other:?}"),
        }
    }

    #[test]
    fn multiple_authoritative_candidates_remain_ambiguous() {
        let left = call_candidate(
            "symbol:left",
            RelationshipProofKind::ExactReference,
            "left_exact",
        );
        let right = call_candidate(
            "symbol:right",
            RelationshipProofKind::ExactReference,
            "right_exact",
        );

        let outcome = evaluate_candidates(&GraphEdgeType::Calls, vec![right, left]);
        match outcome {
            ResolutionOutcome::Ambiguous { candidates, .. } => {
                assert_eq!(
                    candidates
                        .iter()
                        .map(|candidate| candidate.target_symbol_id.0.as_str())
                        .collect::<Vec<_>>(),
                    vec!["symbol:left", "symbol:right"]
                );
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn normalization_is_independent_of_candidate_generation_order() {
        let target = SymbolId::new("symbol:target");
        let mut lexical = ResolutionCandidate::new(target.clone(), Confidence::High);
        lexical.proofs.push(proof(
            RelationshipProofKind::SameScopeDefinition,
            &target,
            "scope",
        ));
        let mut exact = ResolutionCandidate::new(target.clone(), Confidence::Exact);
        exact.proofs.push(proof(
            RelationshipProofKind::ExactReference,
            &target,
            "exact",
        ));

        let forward = normalize_candidates(vec![lexical.clone(), exact.clone()]);
        let reversed = normalize_candidates(vec![exact, lexical]);
        assert_eq!(forward, reversed);
        assert_eq!(forward.len(), 1);
        assert_eq!(forward[0].confidence, Confidence::Exact);
    }
}
