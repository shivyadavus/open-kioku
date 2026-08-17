use open_kioku_core::{
    Confidence, Evidence, EvidenceId, FileRange, GraphEdge, GraphEdgeType, SymbolId,
};
use open_kioku_errors::OkError;
use open_kioku_storage::GraphStore;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Structured property key used to persist relationship proofs on existing graph edges.
///
/// Keeping the payload under the existing `GraphEdge::properties` extension point makes the
/// contract backward-compatible with pre-RI3 indexes while still exposing a typed API. Invalid
/// or malformed payloads always fail closed to heuristic authority.
pub const RELATIONSHIP_PROOFS_PROPERTY: &str = "relationship_proofs";

/// A typed fact that can contribute to proving a structural repository relationship.
///
/// This vocabulary intentionally excludes fuzzy-name, semantic-similarity, and candidate-rank
/// signals. Those signals may remain useful for retrieval, but they are not structural proof.
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

/// Whether a relationship may be used as structural truth.
///
/// Authority is deliberately separate from confidence and retrieval/ranking score.
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

/// Inspectable proof attached to a candidate or emitted graph relationship.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipProof {
    pub kind: RelationshipProofKind,
    /// The proof-local authority classification. Effective edge authority is always recomputed
    /// through [`relationship_authority`] and never trusts this field on its own.
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
    /// Number of viable targets at the point this proof was produced. Authoritative paths require
    /// exactly one candidate and no ambiguity notes.
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
            authority: proof_kind_authority(kind),
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

    /// Canonicalize set-like fields so proof JSON is deterministic across discovery order.
    pub fn normalize(&mut self) {
        self.ambiguity.sort();
        self.ambiguity.dedup();
        self.evidence_ids.sort();
        self.evidence_ids.dedup();
        // Do not permit serialized input to self-promote beyond the central proof-kind policy.
        self.authority = self.authority.min(proof_kind_authority(self.kind));
    }

    pub fn is_unique(&self) -> bool {
        self.candidate_count == 1 && self.ambiguity.is_empty()
    }
}

/// Maximum authority a single proof kind can contribute before relationship-specific combination
/// rules are evaluated.
pub fn proof_kind_authority(kind: RelationshipProofKind) -> RelationshipAuthority {
    match kind {
        RelationshipProofKind::ExactOccurrence
        | RelationshipProofKind::ExactReference
        | RelationshipProofKind::ExactCallSite
        | RelationshipProofKind::ExternalExactIndex => RelationshipAuthority::Authoritative,
        RelationshipProofKind::ImportBinding
        | RelationshipProofKind::QualifiedName
        | RelationshipProofKind::SameScopeDefinition
        | RelationshipProofKind::ContainingType
        | RelationshipProofKind::ReceiverType
        | RelationshipProofKind::TraitOrInterfaceBinding
        | RelationshipProofKind::InheritanceBinding
        | RelationshipProofKind::ModuleOrPackageBinding => RelationshipAuthority::Corroborating,
    }
}

fn normalized_effective_authority(proof: &RelationshipProof) -> RelationshipAuthority {
    proof.authority.min(proof_kind_authority(proof.kind))
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

fn proof_targets_are_coherent(proofs: &[RelationshipProof]) -> bool {
    let mut expected_target: Option<&SymbolId> = None;
    for proof in proofs {
        let Some(target) = proof.target_symbol_id.as_ref() else {
            continue;
        };
        match expected_target {
            Some(expected) if expected != target => return false,
            None => expected_target = Some(target),
            _ => {}
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
    if proofs.is_empty() || !proof_targets_are_coherent(proofs) {
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
        GraphEdgeType::Calls => {
            exact_call_site
                && (exact_target
                    || (receiver_type && (qualified_name || same_scope || containing_type))
                    || (import_binding && (qualified_name || same_scope)))
        }
        GraphEdgeType::Implements => {
            inheritance_binding && (trait_binding || exact_target || qualified_name)
        }
        GraphEdgeType::Extends => inheritance_binding && (exact_target || qualified_name),
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

/// Typed access to RI3 relationship proofs persisted through the existing graph-edge extension
/// point. Malformed proof JSON never becomes structural authority.
pub trait GraphEdgeRelationshipProofExt {
    fn try_relationship_proofs(&self) -> Result<Vec<RelationshipProof>, serde_json::Error>;
    fn relationship_proofs(&self) -> Vec<RelationshipProof>;
    fn set_relationship_proofs(
        &mut self,
        proofs: Vec<RelationshipProof>,
    ) -> Result<(), serde_json::Error>;
    fn relationship_authority(&self) -> RelationshipAuthority;
    fn is_authoritative_relationship(&self) -> bool;
    fn has_relationship_proof_kind(&self, kind: RelationshipProofKind) -> bool;
}

impl GraphEdgeRelationshipProofExt for GraphEdge {
    fn try_relationship_proofs(&self) -> Result<Vec<RelationshipProof>, serde_json::Error> {
        let Some(value) = self.properties.get(RELATIONSHIP_PROOFS_PROPERTY) else {
            return Ok(Vec::new());
        };
        let proofs: Vec<RelationshipProof> = serde_json::from_value(value.clone())?;
        Ok(normalize_relationship_proofs(proofs))
    }

    fn relationship_proofs(&self) -> Vec<RelationshipProof> {
        self.try_relationship_proofs().unwrap_or_default()
    }

    fn set_relationship_proofs(
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

    fn relationship_authority(&self) -> RelationshipAuthority {
        let Ok(proofs) = self.try_relationship_proofs() else {
            return RelationshipAuthority::Heuristic;
        };
        relationship_authority(&self.edge_type, &proofs)
    }

    fn is_authoritative_relationship(&self) -> bool {
        self.relationship_authority() == RelationshipAuthority::Authoritative
    }

    fn has_relationship_proof_kind(&self, kind: RelationshipProofKind) -> bool {
        self.try_relationship_proofs()
            .map(|proofs| proofs.iter().any(|proof| proof.kind == kind))
            .unwrap_or(false)
    }
}

/// Reusable typed filter for graph/query consumers that need authority-aware relationship reads.
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
        if relationship_authority(&edge.edge_type, &proofs) < self.minimum_authority {
            return false;
        }
        match self.accepted_proof_kinds.as_ref() {
            Some(accepted) => proofs.iter().any(|proof| accepted.contains(&proof.kind)),
            None => true,
        }
    }
}

pub const DEFAULT_RELATIONSHIP_EDGE_QUERY_LIMIT: usize = 100;
pub const HARD_RELATIONSHIP_EDGE_QUERY_LIMIT: usize = 500;
pub const DEFAULT_RELATIONSHIP_EDGE_SCAN_LIMIT: usize = 10_000;
pub const HARD_RELATIONSHIP_EDGE_SCAN_LIMIT: usize = 100_000;
const RELATIONSHIP_EDGE_SCAN_BATCH_SIZE: usize = 512;

/// Bounded, typed relationship-edge query over an existing graph store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipEdgeQuery {
    pub edge_type: GraphEdgeType,
    #[serde(default)]
    pub filter: RelationshipProofFilter,
    #[serde(default = "default_relationship_edge_query_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_relationship_edge_scan_limit")]
    pub scan_limit: usize,
}

fn default_relationship_edge_query_limit() -> usize {
    DEFAULT_RELATIONSHIP_EDGE_QUERY_LIMIT
}

fn default_relationship_edge_scan_limit() -> usize {
    DEFAULT_RELATIONSHIP_EDGE_SCAN_LIMIT
}

impl Default for RelationshipEdgeQuery {
    fn default() -> Self {
        Self {
            edge_type: GraphEdgeType::References,
            filter: RelationshipProofFilter::default(),
            limit: DEFAULT_RELATIONSHIP_EDGE_QUERY_LIMIT,
            offset: 0,
            scan_limit: DEFAULT_RELATIONSHIP_EDGE_SCAN_LIMIT,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipEdgeQueryResult {
    pub edges: Vec<GraphEdge>,
    pub scanned_edges: usize,
    pub matched_edges: usize,
    pub effective_limit: usize,
    pub effective_scan_limit: usize,
    pub has_more: bool,
    pub scan_truncated: bool,
}

/// Authority-aware graph-store reads without making the generic storage layer depend on RI3 policy.
pub trait RelationshipGraphStoreExt {
    fn query_relationship_edges(
        &self,
        query: &RelationshipEdgeQuery,
    ) -> Result<RelationshipEdgeQueryResult, OkError>;
}

impl<T: GraphStore + ?Sized> RelationshipGraphStoreExt for T {
    fn query_relationship_edges(
        &self,
        query: &RelationshipEdgeQuery,
    ) -> Result<RelationshipEdgeQueryResult, OkError> {
        let limit = query.limit.min(HARD_RELATIONSHIP_EDGE_QUERY_LIMIT);
        let scan_limit = query.scan_limit.min(HARD_RELATIONSHIP_EDGE_SCAN_LIMIT);
        if limit == 0 || scan_limit == 0 {
            return Ok(RelationshipEdgeQueryResult {
                edges: Vec::new(),
                scanned_edges: 0,
                matched_edges: 0,
                effective_limit: limit,
                effective_scan_limit: scan_limit,
                has_more: false,
                scan_truncated: false,
            });
        }

        let target_matches = query.offset.saturating_add(limit).saturating_add(1);
        let mut edges = Vec::with_capacity(limit);
        let mut scanned_edges = 0usize;
        let mut matched_edges = 0usize;
        let mut source_offset = 0usize;
        let mut source_exhausted = false;
        let mut has_more = false;

        while scanned_edges < scan_limit && matched_edges < target_matches {
            let batch_limit = (scan_limit - scanned_edges).min(RELATIONSHIP_EDGE_SCAN_BATCH_SIZE);
            let batch = self.edges_by_type(query.edge_type.clone(), batch_limit, source_offset)?;
            if batch.is_empty() {
                source_exhausted = true;
                break;
            }

            let batch_len = batch.len();
            source_offset = source_offset.saturating_add(batch_len);
            for edge in batch {
                scanned_edges = scanned_edges.saturating_add(1);
                if query.filter.matches(&edge) {
                    matched_edges = matched_edges.saturating_add(1);
                    if matched_edges > query.offset {
                        if edges.len() < limit {
                            edges.push(edge);
                        } else {
                            has_more = true;
                            break;
                        }
                    }
                }
                if scanned_edges >= scan_limit {
                    break;
                }
            }

            if has_more {
                break;
            }
            if batch_len < batch_limit {
                source_exhausted = true;
                break;
            }
        }

        Ok(RelationshipEdgeQueryResult {
            edges,
            scanned_edges,
            matched_edges,
            effective_limit: limit,
            effective_scan_limit: scan_limit,
            has_more,
            scan_truncated: !source_exhausted && !has_more && scanned_edges >= scan_limit,
        })
    }
}

pub fn minimum_confidence(evidence: &[Evidence]) -> Confidence {
    if evidence
        .iter()
        .any(|item| item.confidence == Confidence::Low)
    {
        Confidence::Low
    } else if evidence
        .iter()
        .any(|item| item.confidence == Confidence::Medium)
    {
        Confidence::Medium
    } else if evidence
        .iter()
        .any(|item| item.confidence == Confidence::High)
    {
        Confidence::High
    } else {
        Confidence::Exact
    }
}

#[derive(Default)]
pub struct EvidenceBuilder {
    evidence: Vec<String>,
    scores: Vec<f32>,
}

impl EvidenceBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(mut self, text: impl Into<String>, score: f32) -> Self {
        self.evidence.push(text.into());
        self.scores.push(score);
        self
    }

    pub fn build(self) -> (Vec<String>, f32) {
        if self.scores.is_empty() {
            return (self.evidence, 0.0);
        }
        let max_score = self.scores.iter().copied().fold(0.0_f32, f32::max);
        let normalized = (max_score * 100.0).round() / 100.0;
        (self.evidence, normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{EdgeId, GraphNode, LineRange, NodeId};
    use serde_json::json;
    use std::path::PathBuf;

    fn proof(kind: RelationshipProofKind, candidate_count: usize) -> RelationshipProof {
        let mut proof = RelationshipProof::new(kind, "test", candidate_count);
        proof.evidence_ids = vec![EvidenceId::new(format!("evidence:{kind:?}"))];
        proof
    }

    fn reference_edge(id: &str, proofs: Vec<RelationshipProof>) -> GraphEdge {
        let mut edge = GraphEdge {
            id: EdgeId::new(id),
            from: NodeId::new(format!("{id}:from")),
            to: NodeId::new(format!("{id}:to")),
            edge_type: GraphEdgeType::References,
            ..Default::default()
        };
        edge.set_relationship_proofs(proofs).unwrap();
        edge
    }

    fn legacy_reference_edge(id: &str) -> GraphEdge {
        GraphEdge {
            id: EdgeId::new(id),
            from: NodeId::new(format!("{id}:from")),
            to: NodeId::new(format!("{id}:to")),
            edge_type: GraphEdgeType::References,
            ..Default::default()
        }
    }

    struct FakeGraphStore {
        edges: Vec<GraphEdge>,
    }

    impl GraphStore for FakeGraphStore {
        fn replace_graph(&self, _nodes: &[GraphNode], _edges: &[GraphEdge]) -> Result<(), OkError> {
            Ok(())
        }

        fn neighbors(
            &self,
            _node: &str,
            _limit: usize,
        ) -> Result<(Vec<GraphNode>, Vec<GraphEdge>), OkError> {
            Ok((Vec::new(), Vec::new()))
        }

        fn shortest_path(
            &self,
            _from: &str,
            _to: &str,
            _max_depth: usize,
        ) -> Result<Vec<GraphEdge>, OkError> {
            Ok(Vec::new())
        }

        fn edges_by_type(
            &self,
            edge_type: GraphEdgeType,
            limit: usize,
            offset: usize,
        ) -> Result<Vec<GraphEdge>, OkError> {
            Ok(self
                .edges
                .iter()
                .filter(|edge| edge.edge_type == edge_type)
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
        }
    }

    #[test]
    fn exact_occurrence_proves_reference_relationship() {
        let proofs = vec![proof(RelationshipProofKind::ExactOccurrence, 1)];
        assert_eq!(
            relationship_authority(&GraphEdgeType::References, &proofs),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn exact_reference_proves_reference_relationship() {
        let proofs = vec![proof(RelationshipProofKind::ExactReference, 1)];
        assert_eq!(
            relationship_authority(&GraphEdgeType::References, &proofs),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn unique_import_binding_proves_import_relationship() {
        let proofs = vec![proof(RelationshipProofKind::ImportBinding, 1)];
        assert_eq!(
            relationship_authority(&GraphEdgeType::Imports, &proofs),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn import_binding_alone_does_not_prove_reference_target() {
        let proofs = vec![proof(RelationshipProofKind::ImportBinding, 1)];
        assert_eq!(
            relationship_authority(&GraphEdgeType::References, &proofs),
            RelationshipAuthority::Corroborating
        );
    }

    #[test]
    fn import_binding_plus_unique_symbol_proves_reference_target() {
        let proofs = vec![
            proof(RelationshipProofKind::ImportBinding, 1),
            proof(RelationshipProofKind::QualifiedName, 1),
        ];
        assert_eq!(
            relationship_authority(&GraphEdgeType::References, &proofs),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn conflicting_target_ids_fail_closed() {
        let mut import = proof(RelationshipProofKind::ImportBinding, 1);
        import.target_symbol_id = Some(SymbolId::new("symbol:a"));
        let mut qualified = proof(RelationshipProofKind::QualifiedName, 1);
        qualified.target_symbol_id = Some(SymbolId::new("symbol:b"));

        assert_eq!(
            relationship_authority(&GraphEdgeType::References, &[import, qualified]),
            RelationshipAuthority::Heuristic
        );
    }

    #[test]
    fn call_site_needs_unique_target_binding() {
        let call_site = proof(RelationshipProofKind::ExactCallSite, 1);
        assert_eq!(
            relationship_authority(&GraphEdgeType::Calls, std::slice::from_ref(&call_site)),
            RelationshipAuthority::Corroborating
        );

        let proofs = vec![
            call_site,
            proof(RelationshipProofKind::ReceiverType, 1),
            proof(RelationshipProofKind::QualifiedName, 1),
        ];
        assert_eq!(
            relationship_authority(&GraphEdgeType::Calls, &proofs),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn proofless_name_only_signal_cannot_authorize_relationship() {
        assert_eq!(
            relationship_authority(&GraphEdgeType::References, &[]),
            RelationshipAuthority::Heuristic
        );
    }

    #[test]
    fn ambiguous_candidates_cannot_become_authoritative() {
        let mut exact = proof(RelationshipProofKind::ExactReference, 2);
        exact.ambiguity.push("two viable targets".into());
        assert_ne!(
            relationship_authority(&GraphEdgeType::References, &[exact]),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn serialized_authority_cannot_self_promote_weak_kind() {
        let mut receiver = proof(RelationshipProofKind::ReceiverType, 1);
        receiver.authority = RelationshipAuthority::Authoritative;
        assert_eq!(
            relationship_authority(&GraphEdgeType::References, &[receiver]),
            RelationshipAuthority::Corroborating
        );
    }

    #[test]
    fn proof_storage_is_deterministic_across_insertion_order() {
        let mut left = proof(RelationshipProofKind::QualifiedName, 1);
        left.evidence_ids = vec![EvidenceId::new("z"), EvidenceId::new("a")];
        left.ambiguity = vec!["z".into(), "a".into()];
        left.source_range = Some(FileRange {
            path: PathBuf::from("src/lib.rs"),
            line_range: Some(LineRange { start: 7, end: 7 }),
        });
        let right = proof(RelationshipProofKind::ImportBinding, 1);

        let mut first = GraphEdge::default();
        first
            .set_relationship_proofs(vec![left.clone(), right.clone()])
            .unwrap();
        let mut second = GraphEdge::default();
        second.set_relationship_proofs(vec![right, left]).unwrap();

        assert_eq!(
            first.properties.get(RELATIONSHIP_PROOFS_PROPERTY),
            second.properties.get(RELATIONSHIP_PROOFS_PROPERTY)
        );
    }

    #[test]
    fn old_edge_without_proof_metadata_remains_readable_and_untrusted() {
        let edge = GraphEdge {
            id: EdgeId::new("edge:legacy"),
            from: NodeId::new("from"),
            to: NodeId::new("to"),
            edge_type: GraphEdgeType::Calls,
            ..Default::default()
        };
        let encoded = serde_json::to_value(edge).unwrap();
        let decoded: GraphEdge = serde_json::from_value(encoded).unwrap();

        assert!(decoded.relationship_proofs().is_empty());
        assert_eq!(
            decoded.relationship_authority(),
            RelationshipAuthority::Heuristic
        );
        assert!(!decoded.is_authoritative_relationship());
    }

    #[test]
    fn malformed_proof_payload_fails_closed() {
        let mut edge = GraphEdge {
            edge_type: GraphEdgeType::References,
            ..Default::default()
        };
        edge.properties
            .insert(RELATIONSHIP_PROOFS_PROPERTY.into(), json!({"bad": true}));

        assert!(edge.try_relationship_proofs().is_err());
        assert_eq!(
            edge.relationship_authority(),
            RelationshipAuthority::Heuristic
        );
        assert!(!edge.is_authoritative_relationship());
    }

    #[test]
    fn typed_filter_respects_authority_and_proof_kind() {
        let mut edge = GraphEdge {
            edge_type: GraphEdgeType::References,
            ..Default::default()
        };
        edge.set_relationship_proofs(vec![proof(RelationshipProofKind::ExactReference, 1)])
            .unwrap();

        let filter = RelationshipProofFilter {
            minimum_authority: RelationshipAuthority::Authoritative,
            accepted_proof_kinds: Some(BTreeSet::from([RelationshipProofKind::ExactReference])),
        };
        assert!(filter.matches(&edge));

        let wrong_kind = RelationshipProofFilter {
            minimum_authority: RelationshipAuthority::Authoritative,
            accepted_proof_kinds: Some(BTreeSet::from([RelationshipProofKind::ImportBinding])),
        };
        assert!(!wrong_kind.matches(&edge));
    }

    #[test]
    fn graph_store_query_filters_before_applying_offset() {
        let store = FakeGraphStore {
            edges: vec![
                legacy_reference_edge("legacy-0"),
                reference_edge(
                    "authoritative-1",
                    vec![proof(RelationshipProofKind::ExactReference, 1)],
                ),
                legacy_reference_edge("legacy-2"),
                reference_edge(
                    "authoritative-3",
                    vec![proof(RelationshipProofKind::ExactReference, 1)],
                ),
            ],
        };
        let query = RelationshipEdgeQuery {
            edge_type: GraphEdgeType::References,
            filter: RelationshipProofFilter {
                minimum_authority: RelationshipAuthority::Authoritative,
                accepted_proof_kinds: None,
            },
            limit: 1,
            offset: 1,
            scan_limit: 100,
        };

        let result = store.query_relationship_edges(&query).unwrap();
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].id.0, "authoritative-3");
        assert_eq!(result.matched_edges, 2);
        assert!(!result.has_more);
        assert!(!result.scan_truncated);
    }

    #[test]
    fn graph_store_query_reports_has_more_after_typed_filtering() {
        let store = FakeGraphStore {
            edges: vec![
                reference_edge("a", vec![proof(RelationshipProofKind::ExactReference, 1)]),
                reference_edge("b", vec![proof(RelationshipProofKind::ExactReference, 1)]),
                reference_edge("c", vec![proof(RelationshipProofKind::ExactReference, 1)]),
            ],
        };
        let query = RelationshipEdgeQuery {
            edge_type: GraphEdgeType::References,
            filter: RelationshipProofFilter {
                minimum_authority: RelationshipAuthority::Authoritative,
                accepted_proof_kinds: Some(BTreeSet::from([
                    RelationshipProofKind::ExactReference,
                ])),
            },
            limit: 1,
            offset: 0,
            scan_limit: 100,
        };

        let result = store.query_relationship_edges(&query).unwrap();
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].id.0, "a");
        assert!(result.has_more);
        assert!(!result.scan_truncated);
    }

    #[test]
    fn graph_store_query_reports_scan_truncation() {
        let store = FakeGraphStore {
            edges: vec![
                legacy_reference_edge("legacy"),
                reference_edge(
                    "authoritative",
                    vec![proof(RelationshipProofKind::ExactReference, 1)],
                ),
            ],
        };
        let query = RelationshipEdgeQuery {
            edge_type: GraphEdgeType::References,
            filter: RelationshipProofFilter {
                minimum_authority: RelationshipAuthority::Authoritative,
                accepted_proof_kinds: None,
            },
            limit: 1,
            offset: 0,
            scan_limit: 1,
        };

        let result = store.query_relationship_edges(&query).unwrap();
        assert!(result.edges.is_empty());
        assert_eq!(result.scanned_edges, 1);
        assert!(result.scan_truncated);
    }
}
