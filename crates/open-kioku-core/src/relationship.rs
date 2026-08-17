//! Typed structural relationship proof contract.
//!
//! Core owns the serialized proof vocabulary shared by graph storage, query APIs, and downstream
//! consumers. Relationship-specific authorization policy intentionally lives above core so parsers
//! and persistence do not make trust decisions independently.

use crate::{EvidenceId, FileRange, SymbolId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    /// Proof-local classification. Effective edge authority must still be recomputed through the
    /// central relationship policy and must never trust this field on its own.
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
