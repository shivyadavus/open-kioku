pub use open_kioku_core::{
    normalize_relationship_proofs, relationship_authority, RelationshipAuthority,
    RelationshipProof, RelationshipProofFilter, RelationshipProofKind,
    RELATIONSHIP_PROOFS_PROPERTY,
};
use open_kioku_core::{Confidence, Evidence, GraphEdge, GraphEdgeType};
#[cfg(test)]
use open_kioku_core::{FileRange, SymbolId};
use open_kioku_errors::OkError;
use open_kioku_storage::GraphStore;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::BTreeSet;

/// Compatibility wrapper for callers that imported this helper from the evidence crate.
pub fn proof_kind_authority(kind: RelationshipProofKind) -> RelationshipAuthority {
    kind.maximum_authority()
}

/// Backward-compatible facade over the core-owned inherent `GraphEdge` proof API.
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
        GraphEdge::try_relationship_proofs(self)
    }

    fn relationship_proofs(&self) -> Vec<RelationshipProof> {
        GraphEdge::relationship_proofs(self)
    }

    fn set_relationship_proofs(
        &mut self,
        proofs: Vec<RelationshipProof>,
    ) -> Result<(), serde_json::Error> {
        GraphEdge::set_relationship_proofs(self, proofs)
    }

    fn relationship_authority(&self) -> RelationshipAuthority {
        GraphEdge::relationship_authority(self)
    }

    fn is_authoritative_relationship(&self) -> bool {
        GraphEdge::is_authoritative_relationship(self)
    }

    fn has_relationship_proof_kind(&self, kind: RelationshipProofKind) -> bool {
        GraphEdge::has_relationship_proof_kind(self, kind)
    }
}

/// How a relationship consumer treats resolutions that carry unresolved ambiguity.
///
/// Ambiguity here means the edge (or its proofs) records competing viable targets. Such an
/// edge can never be consumed as proven structural truth; this behavior only decides whether
/// it may still be *surfaced* as a possible relationship.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguityBehavior {
    /// Keep ambiguous relationships visible as possible (never proven) results.
    #[default]
    Surface,
    /// Drop ambiguous relationships entirely.
    Exclude,
}

/// Consumption class assigned to an edge by [`RelationshipUsePolicy::classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipUseClass {
    /// The edge may be presented as structural truth.
    Proven,
    /// The edge may be presented only as a labeled possibility.
    Possible,
    /// The edge must not be presented at all under this policy.
    Excluded,
}

/// Shared downstream policy for consuming relationship edges.
///
/// The epic invariant this encodes: retrieval may speculate, structural graph truth may not.
/// Consumers (impact, test selection, planning, contracts, verification) use one policy type
/// so a heuristic same-name edge is classified identically everywhere — as possible at best,
/// never as proven.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipUsePolicy {
    /// Minimum effective authority (recomputed from typed proofs) for a proven claim.
    pub minimum_proven_authority: RelationshipAuthority,
    /// Whether below-threshold edges may still be surfaced as possible relationships.
    pub include_possible: bool,
    /// How ambiguous resolutions are handled among possible relationships.
    pub ambiguity: AmbiguityBehavior,
}

impl Default for RelationshipUsePolicy {
    fn default() -> Self {
        Self::proven_and_possible()
    }
}

impl RelationshipUsePolicy {
    /// Only authoritative edges survive; everything weaker is excluded.
    pub fn proven_only() -> Self {
        Self {
            minimum_proven_authority: RelationshipAuthority::Authoritative,
            include_possible: false,
            ambiguity: AmbiguityBehavior::Exclude,
        }
    }

    /// Authoritative edges are proven; weaker unambiguous or ambiguous edges stay visible as
    /// possible relationships.
    pub fn proven_and_possible() -> Self {
        Self {
            minimum_proven_authority: RelationshipAuthority::Authoritative,
            include_possible: true,
            ambiguity: AmbiguityBehavior::Surface,
        }
    }

    /// Classify one edge. Authority is always recomputed from the edge's typed proofs via the
    /// fail-closed core policy; serialized authority claims cannot self-promote an edge.
    pub fn classify(&self, edge: &GraphEdge) -> RelationshipUseClass {
        let authority = edge.relationship_authority();
        if authority >= self.minimum_proven_authority {
            return RelationshipUseClass::Proven;
        }
        if !self.include_possible {
            return RelationshipUseClass::Excluded;
        }
        if edge_is_ambiguous(edge) && self.ambiguity == AmbiguityBehavior::Exclude {
            return RelationshipUseClass::Excluded;
        }
        RelationshipUseClass::Possible
    }
}

/// Whether an edge records unresolved ambiguity on itself or any of its typed proofs.
pub fn edge_is_ambiguous(edge: &GraphEdge) -> bool {
    !edge.ambiguity.is_empty()
        || edge
            .relationship_proofs()
            .iter()
            .any(|proof| !proof.ambiguity.is_empty() || proof.candidate_count > 1)
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
    use open_kioku_core::{EdgeId, EvidenceId, GraphNode, LineRange, NodeId};
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

    struct CappedPageGraphStore {
        edges: Vec<GraphEdge>,
        page_cap: usize,
    }

    impl GraphStore for CappedPageGraphStore {
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
                .take(limit.min(self.page_cap))
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
    fn exact_reference_proves_uses_type_relationship() {
        let proofs = vec![proof(RelationshipProofKind::ExactReference, 1)];
        assert_eq!(
            relationship_authority(&GraphEdgeType::UsesType, &proofs),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn receiver_type_alone_does_not_prove_uses_type_target() {
        let proofs = vec![proof(RelationshipProofKind::ReceiverType, 1)];
        assert_eq!(
            relationship_authority(&GraphEdgeType::UsesType, &proofs),
            RelationshipAuthority::Corroborating
        );
    }

    #[test]
    fn receiver_type_plus_unique_name_proves_uses_type_target() {
        let proofs = vec![
            proof(RelationshipProofKind::ReceiverType, 1),
            proof(RelationshipProofKind::QualifiedName, 1),
        ];
        assert_eq!(
            relationship_authority(&GraphEdgeType::UsesType, &proofs),
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
            path: PathBuf::from("src/lib.rs").into(),
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
                accepted_proof_kinds: Some(BTreeSet::from([RelationshipProofKind::ExactReference])),
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
    fn graph_store_query_continues_after_short_pages() {
        let store = CappedPageGraphStore {
            edges: vec![
                legacy_reference_edge("legacy"),
                reference_edge(
                    "authoritative",
                    vec![proof(RelationshipProofKind::ExactReference, 1)],
                ),
            ],
            page_cap: 1,
        };
        let query = RelationshipEdgeQuery {
            edge_type: GraphEdgeType::References,
            filter: RelationshipProofFilter {
                minimum_authority: RelationshipAuthority::Authoritative,
                accepted_proof_kinds: None,
            },
            limit: 1,
            offset: 0,
            scan_limit: 100,
        };

        let result = store.query_relationship_edges(&query).unwrap();
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].id.0, "authoritative");
        assert_eq!(result.scanned_edges, 2);
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

    #[test]
    fn use_policy_classifies_authoritative_edge_as_proven() {
        let edge = reference_edge(
            "edge:exact",
            vec![proof(RelationshipProofKind::ExactReference, 1)],
        );
        assert_eq!(
            RelationshipUsePolicy::proven_and_possible().classify(&edge),
            RelationshipUseClass::Proven
        );
        assert_eq!(
            RelationshipUsePolicy::proven_only().classify(&edge),
            RelationshipUseClass::Proven
        );
    }

    #[test]
    fn use_policy_never_promotes_a_proofless_edge() {
        let edge = legacy_reference_edge("edge:heuristic");
        assert_eq!(
            RelationshipUsePolicy::proven_and_possible().classify(&edge),
            RelationshipUseClass::Possible
        );
        assert_eq!(
            RelationshipUsePolicy::proven_only().classify(&edge),
            RelationshipUseClass::Excluded
        );
    }

    #[test]
    fn use_policy_ambiguity_behavior_controls_possible_visibility() {
        // Multiple viable candidates: never proven, ambiguous by definition.
        let edge = reference_edge(
            "edge:ambiguous",
            vec![proof(RelationshipProofKind::QualifiedName, 3)],
        );
        assert!(edge_is_ambiguous(&edge));
        assert_eq!(
            RelationshipUsePolicy::proven_and_possible().classify(&edge),
            RelationshipUseClass::Possible
        );
        let excluding = RelationshipUsePolicy {
            ambiguity: AmbiguityBehavior::Exclude,
            ..RelationshipUsePolicy::proven_and_possible()
        };
        assert_eq!(excluding.classify(&edge), RelationshipUseClass::Excluded);
    }

    #[test]
    fn use_policy_serialized_authority_cannot_self_promote() {
        // A weak proof kind claiming authoritative in serialized form is capped by the
        // fail-closed core policy, so it still cannot become proven.
        let mut inflated = proof(RelationshipProofKind::ModuleOrPackageBinding, 1);
        inflated.authority = RelationshipAuthority::Authoritative;
        let edge = reference_edge("edge:inflated", vec![inflated]);
        assert_eq!(
            RelationshipUsePolicy::proven_and_possible().classify(&edge),
            RelationshipUseClass::Possible
        );
    }
}
