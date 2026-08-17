//! Typed structural relationship proof and authority contract.
//!
//! Core owns both the serialized proof vocabulary and the single effective-authority policy shared
//! by graph storage, query APIs, and downstream consumers. Parsers may produce proof facts, but they
//! do not independently decide whether a structural graph relationship is trusted.

use crate::identity::symbol_id_node_id;
use crate::{EvidenceId, FileRange, GraphEdge, GraphEdgeType, SymbolId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Structured property key used to persist relationship proofs on existing graph edges.
///
/// The existing `GraphEdge::properties` extension point is intentionally retained to avoid a
/// source-breaking public struct-field addition while exposing a typed, first-class API.
pub const RELATIONSHIP_PROOFS_PROPERTY: &str = "relationship_proofs";

/// A typed fact that can contribute to proving a structural repository relationship.
///
/// Fuzzy-name, semantic-similarity, and candidate-rank signals are intentionally absent. They may
/// be useful retrieval signals, but they are not structural proof.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipProofKind {
    ExactOccurrence,
    ExactReference,
    ExactCallSite,
    ImportBinding,
    QualifiedName,
    SameScopeDefinition,
    ContainingType,
    ReceiverType,
    TraitOrInterfaceBinding,
    InheritanceBinding,
    ModuleOrPackageBinding,
    ExternalExactIndex,
}

/// Whether a relationship may be consumed as structural truth.
///
/// Ordering is deliberate so callers can express minimum authority directly.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipAuthority {
    #[default]
    Heuristic,
    Corroborating,
    Authoritative,
}

impl RelationshipProofKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExactOccurrence => "exact_occurrence",
            Self::ExactReference => "exact_reference",
            Self::ExactCallSite => "exact_call_site",
            Self::ImportBinding => "import_binding",
            Self::QualifiedName => "qualified_name",
            Self::SameScopeDefinition => "same_scope_definition",
            Self::ContainingType => "containing_type",
            Self::ReceiverType => "receiver_type",
            Self::TraitOrInterfaceBinding => "trait_or_interface_binding",
            Self::InheritanceBinding => "inheritance_binding",
            Self::ModuleOrPackageBinding => "module_or_package_binding",
            Self::ExternalExactIndex => "external_exact_index",
        }
    }

    /// Maximum authority this proof kind can contribute before relationship-specific policy runs.
    /// This is an intrinsic safety ceiling, not an edge-authorization decision.
    pub fn maximum_authority(self) -> RelationshipAuthority {
        match self {
            Self::ExactOccurrence
            | Self::ExactReference
            | Self::ExactCallSite
            | Self::ExternalExactIndex => RelationshipAuthority::Authoritative,
            Self::ImportBinding
            | Self::QualifiedName
            | Self::SameScopeDefinition
            | Self::ContainingType
            | Self::ReceiverType
            | Self::TraitOrInterfaceBinding
            | Self::InheritanceBinding
            | Self::ModuleOrPackageBinding => RelationshipAuthority::Corroborating,
        }
    }
}

/// Inspectable proof attached to a candidate or emitted graph relationship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipProof {
    pub kind: RelationshipProofKind,
    /// Proof-local classification. Effective edge authority is always recomputed through
    /// [`relationship_authority`] and never trusts this field on its own.
    #[serde(default)]
    pub authority: RelationshipAuthority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_range: Option<FileRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_symbol_id: Option<SymbolId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_symbol_id: Option<SymbolId>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub resolver_strategy: String,
    /// Viable target count when this proof was produced. Authoritative paths require uniqueness.
    #[serde(default)]
    pub candidate_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambiguity: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<EvidenceId>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, serde_json::Value>,
}

impl RelationshipProof {
    pub fn new(
        kind: RelationshipProofKind,
        resolver_strategy: impl Into<String>,
        candidate_count: usize,
    ) -> Self {
        Self {
            kind,
            authority: kind.maximum_authority(),
            source_range: None,
            source_symbol_id: None,
            target_symbol_id: None,
            resolver_strategy: resolver_strategy.into(),
            candidate_count,
            ambiguity: Vec::new(),
            evidence_ids: Vec::new(),
            details: BTreeMap::new(),
        }
    }

    /// Canonicalize set-like fields and cap untrusted serialized authority at the kind ceiling.
    pub fn normalize(&mut self) {
        self.ambiguity.sort();
        self.ambiguity.dedup();
        self.evidence_ids.sort();
        self.evidence_ids.dedup();
        self.authority = self.authority.min(self.kind.maximum_authority());
    }

    pub fn is_unique(&self) -> bool {
        self.candidate_count == 1 && self.ambiguity.is_empty()
    }
}

fn normalized_effective_authority(proof: &RelationshipProof) -> RelationshipAuthority {
    proof.authority.min(proof.kind.maximum_authority())
}

fn has_unique(proofs: &[RelationshipProof], kind: RelationshipProofKind) -> bool {
    proofs.iter().any(|proof| {
        proof.kind == kind
            && proof.is_unique()
            && normalized_effective_authority(proof) >= RelationshipAuthority::Corroborating
    })
}

fn has_unique_exact_target(proofs: &[RelationshipProof]) -> bool {
    proofs.iter().any(|proof| {
        matches!(
            proof.kind,
            RelationshipProofKind::ExactOccurrence
                | RelationshipProofKind::ExactReference
                | RelationshipProofKind::ExternalExactIndex
        ) && proof.is_unique()
            && normalized_effective_authority(proof) == RelationshipAuthority::Authoritative
    })
}

fn fallback_authority(proofs: &[RelationshipProof]) -> RelationshipAuthority {
    proofs
        .iter()
        .filter(|proof| proof.is_unique())
        .map(normalized_effective_authority)
        .max()
        .unwrap_or(RelationshipAuthority::Heuristic)
        .min(RelationshipAuthority::Corroborating)
}

fn proof_identities_are_coherent(proofs: &[RelationshipProof]) -> bool {
    let mut expected_source: Option<&SymbolId> = None;
    let mut expected_target: Option<&SymbolId> = None;
    for proof in proofs {
        if let Some(source) = proof.source_symbol_id.as_ref() {
            match expected_source {
                Some(expected) if expected != source => return false,
                None => expected_source = Some(source),
                _ => {}
            }
        }
        if let Some(target) = proof.target_symbol_id.as_ref() {
            match expected_target {
                Some(expected) if expected != target => return false,
                None => expected_target = Some(target),
                _ => {}
            }
        }
    }
    true
}

/// Compute effective relationship authority from typed proofs using one fail-closed policy.
///
/// Candidate ordering, confidence, fuzzy/name similarity, and semantic scores are intentionally not
/// inputs. A proof marked `authoritative` in serialized data cannot self-promote a weaker proof kind.
pub fn relationship_authority(
    edge_type: &GraphEdgeType,
    proofs: &[RelationshipProof],
) -> RelationshipAuthority {
    if proofs.is_empty() || !proof_identities_are_coherent(proofs) {
        return RelationshipAuthority::Heuristic;
    }

    let exact_target = has_unique_exact_target(proofs);
    let exact_call_site = has_unique(proofs, RelationshipProofKind::ExactCallSite);
    let import_binding = has_unique(proofs, RelationshipProofKind::ImportBinding);
    let qualified_name = has_unique(proofs, RelationshipProofKind::QualifiedName);
    let same_scope = has_unique(proofs, RelationshipProofKind::SameScopeDefinition);
    let containing_type = has_unique(proofs, RelationshipProofKind::ContainingType);
    let receiver_type = has_unique(proofs, RelationshipProofKind::ReceiverType);
    let trait_binding = has_unique(proofs, RelationshipProofKind::TraitOrInterfaceBinding);
    let inheritance_binding = has_unique(proofs, RelationshipProofKind::InheritanceBinding);
    let module_binding = has_unique(proofs, RelationshipProofKind::ModuleOrPackageBinding);
    let external_exact = has_unique(proofs, RelationshipProofKind::ExternalExactIndex);

    let authoritative = match edge_type {
        GraphEdgeType::References => {
            exact_target
                || (import_binding && (qualified_name || same_scope))
                || (qualified_name && same_scope)
        }
        GraphEdgeType::UsesType => {
            exact_target
                || same_scope
                || import_binding
                || qualified_name
                || (receiver_type && (qualified_name || same_scope))
        }
        GraphEdgeType::Calls => {
            exact_call_site
                && (exact_target
                    || same_scope
                    || (receiver_type && (qualified_name || containing_type))
                    || (import_binding && (qualified_name || same_scope))
                    || (inheritance_binding && (receiver_type || containing_type)))
        }
        GraphEdgeType::Implements => {
            inheritance_binding
                && (trait_binding || exact_target || qualified_name || same_scope || import_binding)
        }
        GraphEdgeType::Extends => {
            inheritance_binding && (exact_target || qualified_name || same_scope || import_binding)
        }
        GraphEdgeType::Imports => import_binding || module_binding || external_exact,
        GraphEdgeType::DependsOn => module_binding || import_binding || external_exact,
        _ => false,
    };

    if authoritative {
        RelationshipAuthority::Authoritative
    } else {
        fallback_authority(proofs)
    }
}

/// Canonicalize a proof set for deterministic storage and output.
pub fn normalize_relationship_proofs(mut proofs: Vec<RelationshipProof>) -> Vec<RelationshipProof> {
    for proof in &mut proofs {
        proof.normalize();
    }
    proofs.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.authority.cmp(&right.authority))
            .then_with(|| left.resolver_strategy.cmp(&right.resolver_strategy))
            .then_with(|| left.candidate_count.cmp(&right.candidate_count))
            .then_with(|| left.source_symbol_id.cmp(&right.source_symbol_id))
            .then_with(|| left.target_symbol_id.cmp(&right.target_symbol_id))
            .then_with(|| {
                source_range_key(&left.source_range).cmp(&source_range_key(&right.source_range))
            })
            .then_with(|| left.ambiguity.cmp(&right.ambiguity))
            .then_with(|| left.evidence_ids.cmp(&right.evidence_ids))
            .then_with(|| {
                serde_json::to_string(&left.details)
                    .unwrap_or_default()
                    .cmp(&serde_json::to_string(&right.details).unwrap_or_default())
            })
    });
    proofs.dedup();
    proofs
}

fn graph_edge_relationship_authority(
    edge: &GraphEdge,
    proofs: &[RelationshipProof],
) -> RelationshipAuthority {
    if proofs
        .iter()
        .filter_map(|proof| proof.target_symbol_id.as_ref())
        .any(|target| symbol_id_node_id(target) != edge.to)
    {
        return RelationshipAuthority::Heuristic;
    }
    relationship_authority(&edge.edge_type, proofs)
}

fn source_range_key(range: &Option<FileRange>) -> (String, Option<u32>, Option<u32>) {
    let Some(range) = range else {
        return (String::new(), None, None);
    };
    (
        range.path.to_string_lossy().replace('\\', "/"),
        range.line_range.as_ref().map(|line| line.start),
        range.line_range.as_ref().map(|line| line.end),
    )
}

impl GraphEdge {
    /// Deserialize and canonicalize typed structural proofs. Malformed metadata returns an error so
    /// trust-sensitive callers can fail closed.
    pub fn try_relationship_proofs(&self) -> Result<Vec<RelationshipProof>, serde_json::Error> {
        let Some(value) = self.properties.get(RELATIONSHIP_PROOFS_PROPERTY) else {
            return Ok(Vec::new());
        };
        let proofs: Vec<RelationshipProof> = serde_json::from_value(value.clone())?;
        Ok(normalize_relationship_proofs(proofs))
    }

    /// Typed proof access for inspection-oriented callers. Malformed payloads become an empty set,
    /// which cannot authorize a structural relationship.
    pub fn relationship_proofs(&self) -> Vec<RelationshipProof> {
        self.try_relationship_proofs().unwrap_or_default()
    }

    /// Persist a canonical typed proof set through the backward-compatible graph-edge extension slot.
    pub fn set_relationship_proofs(
        &mut self,
        proofs: Vec<RelationshipProof>,
    ) -> Result<(), serde_json::Error> {
        let proofs = normalize_relationship_proofs(proofs);
        if proofs.is_empty() {
            self.properties.remove(RELATIONSHIP_PROOFS_PROPERTY);
        } else {
            self.properties.insert(
                RELATIONSHIP_PROOFS_PROPERTY.to_string(),
                serde_json::to_value(proofs)?,
            );
        }
        Ok(())
    }

    /// Effective structural authority. Malformed or legacy/proofless metadata always fails closed.
    pub fn relationship_authority(&self) -> RelationshipAuthority {
        let Ok(proofs) = self.try_relationship_proofs() else {
            return RelationshipAuthority::Heuristic;
        };
        graph_edge_relationship_authority(self, &proofs)
    }

    pub fn is_authoritative_relationship(&self) -> bool {
        self.relationship_authority() == RelationshipAuthority::Authoritative
    }

    pub fn has_relationship_proof_kind(&self, kind: RelationshipProofKind) -> bool {
        self.try_relationship_proofs()
            .map(|proofs| proofs.iter().any(|proof| proof.kind == kind))
            .unwrap_or(false)
    }
}

/// Reusable typed filter for callers that need authority-aware relationship reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipProofFilter {
    #[serde(default)]
    pub minimum_authority: RelationshipAuthority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_proof_kinds: Option<BTreeSet<RelationshipProofKind>>,
}

impl Default for RelationshipProofFilter {
    fn default() -> Self {
        Self {
            minimum_authority: RelationshipAuthority::Heuristic,
            accepted_proof_kinds: None,
        }
    }
}

impl RelationshipProofFilter {
    /// Returns false on malformed proof payloads whenever the filter requires proof semantics.
    pub fn matches(&self, edge: &GraphEdge) -> bool {
        let proofs = match edge.try_relationship_proofs() {
            Ok(proofs) => proofs,
            Err(_) => {
                return self.minimum_authority == RelationshipAuthority::Heuristic
                    && self.accepted_proof_kinds.is_none();
            }
        };
        if graph_edge_relationship_authority(edge, &proofs) < self.minimum_authority {
            return false;
        }
        match self.accepted_proof_kinds.as_ref() {
            Some(accepted) => proofs.iter().any(|proof| accepted.contains(&proof.kind)),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EdgeId, NodeId};
    use serde_json::json;

    fn proof(kind: RelationshipProofKind, candidate_count: usize) -> RelationshipProof {
        RelationshipProof::new(kind, "test", candidate_count)
    }

    fn edge(edge_type: GraphEdgeType, proofs: Vec<RelationshipProof>) -> GraphEdge {
        let mut edge = GraphEdge {
            id: EdgeId::new("edge"),
            from: NodeId::new("from"),
            to: NodeId::new("to"),
            edge_type,
            ..Default::default()
        };
        edge.set_relationship_proofs(proofs).unwrap();
        edge
    }

    #[test]
    fn proof_kind_ceiling_prevents_self_promotion() {
        let mut proof = RelationshipProof::new(RelationshipProofKind::ImportBinding, "import", 1);
        proof.authority = RelationshipAuthority::Authoritative;
        proof.normalize();

        assert_eq!(proof.authority, RelationshipAuthority::Corroborating);
    }

    #[test]
    fn proof_normalization_is_deterministic() {
        let mut proof = RelationshipProof::new(RelationshipProofKind::ExactReference, "scip", 1);
        proof.ambiguity = vec!["b".into(), "a".into(), "a".into()];
        proof.evidence_ids = vec![
            EvidenceId::new("z"),
            EvidenceId::new("a"),
            EvidenceId::new("a"),
        ];
        proof.normalize();

        assert_eq!(proof.ambiguity, vec!["a", "b"]);
        assert_eq!(
            proof.evidence_ids,
            vec![EvidenceId::new("a"), EvidenceId::new("z")]
        );
    }

    #[test]
    fn exact_reference_authorizes_reference_and_type_use() {
        let proof = proof(RelationshipProofKind::ExactReference, 1);
        assert!(
            edge(GraphEdgeType::References, vec![proof.clone()]).is_authoritative_relationship()
        );
        assert!(edge(GraphEdgeType::UsesType, vec![proof]).is_authoritative_relationship());
    }

    #[test]
    fn call_site_requires_unique_target_identity() {
        let call_only = edge(
            GraphEdgeType::Calls,
            vec![proof(RelationshipProofKind::ExactCallSite, 1)],
        );
        assert_eq!(
            call_only.relationship_authority(),
            RelationshipAuthority::Authoritative.min(RelationshipAuthority::Corroborating)
        );
        let proved = edge(
            GraphEdgeType::Calls,
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::ExactReference, 1),
            ],
        );
        assert!(proved.is_authoritative_relationship());
    }

    #[test]
    fn unique_lexical_scope_definition_authorizes_call() {
        let proved = edge(
            GraphEdgeType::Calls,
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::SameScopeDefinition, 1),
            ],
        );
        assert!(proved.is_authoritative_relationship());
    }

    #[test]
    fn ambiguous_lexical_scope_definition_does_not_authorize_call() {
        let ambiguous = edge(
            GraphEdgeType::Calls,
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::SameScopeDefinition, 2),
            ],
        );
        assert_ne!(
            ambiguous.relationship_authority(),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn unique_inheritance_binding_authorizes_call() {
        let proved = edge(
            GraphEdgeType::Calls,
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::InheritanceBinding, 1),
                proof(RelationshipProofKind::ContainingType, 1),
            ],
        );
        assert!(proved.is_authoritative_relationship());
    }

    #[test]
    fn ambiguous_inheritance_binding_does_not_authorize_call() {
        let ambiguous = edge(
            GraphEdgeType::Calls,
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::InheritanceBinding, 2),
                proof(RelationshipProofKind::ContainingType, 2),
            ],
        );
        assert_ne!(
            ambiguous.relationship_authority(),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn same_file_inheritance_proofs_authorize_extends() {
        let proved = edge(
            GraphEdgeType::Extends,
            vec![
                proof(RelationshipProofKind::InheritanceBinding, 1),
                proof(RelationshipProofKind::SameScopeDefinition, 1),
            ],
        );
        assert!(proved.is_authoritative_relationship());
    }

    #[test]
    fn imported_trait_inheritance_proofs_authorize_implements() {
        let proved = edge(
            GraphEdgeType::Implements,
            vec![
                proof(RelationshipProofKind::InheritanceBinding, 1),
                proof(RelationshipProofKind::ImportBinding, 1),
                proof(RelationshipProofKind::TraitOrInterfaceBinding, 1),
            ],
        );
        assert!(proved.is_authoritative_relationship());
    }

    #[test]
    fn ambiguous_inheritance_target_cannot_authorize_extends() {
        let ambiguous = edge(
            GraphEdgeType::Extends,
            vec![
                proof(RelationshipProofKind::InheritanceBinding, 2),
                proof(RelationshipProofKind::SameScopeDefinition, 2),
            ],
        );
        assert_ne!(
            ambiguous.relationship_authority(),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn conflicting_target_ids_fail_closed() {
        let mut first = proof(RelationshipProofKind::ExactReference, 1);
        first.target_symbol_id = Some(SymbolId::new("one"));
        let mut second = proof(RelationshipProofKind::QualifiedName, 1);
        second.target_symbol_id = Some(SymbolId::new("two"));
        let edge = edge(GraphEdgeType::References, vec![first, second]);
        assert_eq!(
            edge.relationship_authority(),
            RelationshipAuthority::Heuristic
        );
    }

    #[test]
    fn conflicting_source_ids_fail_closed() {
        let mut call_site = proof(RelationshipProofKind::ExactCallSite, 1);
        call_site.source_symbol_id = Some(SymbolId::new("caller-a"));
        call_site.target_symbol_id = Some(SymbolId::new("callee"));
        let mut exact_target = proof(RelationshipProofKind::ExactReference, 1);
        exact_target.source_symbol_id = Some(SymbolId::new("caller-b"));
        exact_target.target_symbol_id = Some(SymbolId::new("callee"));
        let edge = edge(GraphEdgeType::Calls, vec![call_site, exact_target]);
        assert_eq!(
            edge.relationship_authority(),
            RelationshipAuthority::Heuristic
        );
    }

    #[test]
    fn persisted_target_identity_must_match_claimed_proof_target() {
        let target = SymbolId::new("symbol:Target.run");
        let mut exact = proof(RelationshipProofKind::ExactReference, 1);
        exact.target_symbol_id = Some(target.clone());

        let mut matching = edge(GraphEdgeType::References, vec![exact.clone()]);
        matching.to = symbol_id_node_id(&target);
        assert!(matching.is_authoritative_relationship());
        assert!(RelationshipProofFilter {
            minimum_authority: RelationshipAuthority::Authoritative,
            accepted_proof_kinds: None,
        }
        .matches(&matching));

        let mut mismatched = edge(GraphEdgeType::References, vec![exact]);
        mismatched.to = symbol_id_node_id(&SymbolId::new("symbol:Other.run"));
        assert_eq!(
            mismatched.relationship_authority(),
            RelationshipAuthority::Heuristic
        );
        assert!(!RelationshipProofFilter {
            minimum_authority: RelationshipAuthority::Authoritative,
            accepted_proof_kinds: None,
        }
        .matches(&mismatched));
    }

    #[test]
    fn legacy_and_malformed_edges_fail_closed() {
        let legacy = GraphEdge {
            edge_type: GraphEdgeType::References,
            ..Default::default()
        };
        assert_eq!(
            legacy.relationship_authority(),
            RelationshipAuthority::Heuristic
        );

        let mut malformed = legacy;
        malformed.properties.insert(
            RELATIONSHIP_PROOFS_PROPERTY.into(),
            json!({"not": "a proof array"}),
        );
        assert!(malformed.try_relationship_proofs().is_err());
        assert_eq!(
            malformed.relationship_authority(),
            RelationshipAuthority::Heuristic
        );
    }
}
