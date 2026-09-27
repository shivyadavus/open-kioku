//! Typed structural relationship proof and authority contract.
//!
//! Core owns both the serialized proof vocabulary and the single effective-authority policy shared
//! by graph storage, query APIs, and downstream consumers. Parsers may produce proof facts, but they
//! do not independently decide whether a structural graph relationship is trusted.

use crate::identity::symbol_id_node_id;
use crate::{Confidence, EvidenceId, FileRange, GraphEdge, GraphEdgeType, SymbolId};
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
    /// The derived file's own header names its origin (`generated from <path>`) and that path
    /// resolved to exactly one indexed file. A claim the file makes about itself in prose: better
    /// evidence than a naming guess, but never structural truth, so it caps at corroborating.
    DeclaredOrigin,
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
            | Self::ModuleOrPackageBinding
            | Self::DeclaredOrigin => RelationshipAuthority::Corroborating,
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
                || (receiver_type && (qualified_name || same_scope))
                || (import_binding && qualified_name)
                || (inheritance_binding && (qualified_name || same_scope || import_binding))
        }
        GraphEdgeType::Calls => {
            exact_call_site
                && (exact_target
                    || same_scope
                    || (receiver_type && (qualified_name || same_scope || containing_type))
                    || (import_binding && (qualified_name || same_scope))
                    || (module_binding && qualified_name))
        }
        GraphEdgeType::Implements => {
            inheritance_binding && (trait_binding || exact_target || qualified_name)
        }
        GraphEdgeType::Extends => {
            inheritance_binding && (exact_target || qualified_name || same_scope || import_binding)
        }
        GraphEdgeType::Imports => import_binding || module_binding || external_exact,
        GraphEdgeType::DependsOn => module_binding || import_binding || external_exact,
        // Never authoritative. A generation banner is a claim a comment makes about itself, and
        // every other proof kind here is established by parsed structure. A declared origin is
        // strong enough to corroborate and no more; a naming-convention pairing has no proof at
        // all and stays heuristic.
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

/// Rank of an evidence confidence for window ordering: higher is stronger. `Confidence` has no
/// derived order, and its declaration order is the reverse of what a window keeps first.
fn confidence_rank(confidence: Confidence) -> u8 {
    match confidence {
        Confidence::Low => 0,
        Confidence::Medium => 1,
        Confidence::High => 2,
        Confidence::Exact => 3,
    }
}

/// The highest tier [`graph_edge_window_tier`] returns.
pub const GRAPH_EDGE_WINDOW_TIER_MAX: u8 = 3;

/// Evidence tier a bounded window ranks an edge by, strongest first:
///
/// - 3: a relationship proven by its typed proofs ([`RelationshipAuthority::Authoritative`]).
/// - 2: parsed containment. `CONTAINS` and `DEFINES` resolve no name — they record where an
///   extracted symbol lives — so no proof policy applies to them. One extracted by a parser or an
///   index (tree-sitter, SCIP, LSP) is a fact, but not a dependency: it ranks below every proven
///   relationship, so a file with many symbols cannot push a proven import or dependent out of its
///   own window, and above everything unproven.
/// - 1: a corroborated relationship.
/// - 0: heuristic — proofless or statistical edges (`SIMILAR_TO`, `SEMANTICALLY_RELATED`, a
///   symbol-registry name match), and containment from the regex fallback, which is a guess
///   about where a symbol is.
pub fn graph_edge_window_tier(edge: &GraphEdge) -> u8 {
    match edge.edge_type {
        GraphEdgeType::Contains | GraphEdgeType::Defines => {
            if edge.evidence.source_type.is_exact_reference_source() {
                2
            } else {
                0
            }
        }
        _ => match edge.relationship_authority() {
            RelationshipAuthority::Authoritative => GRAPH_EDGE_WINDOW_TIER_MAX,
            RelationshipAuthority::Corroborating => 1,
            RelationshipAuthority::Heuristic => 0,
        },
    }
}

/// Version of [`graph_edge_window_rank`]. A store that persists ranks records the version it
/// wrote them with and recomputes them when it differs, so bump it with any change to the rank an
/// edge gets: to the tiers, to their order, or to how confidence orders edges within one.
pub const GRAPH_EDGE_WINDOW_RANK_VERSION: u32 = 1;

/// Window position class of one edge: 0 is kept first, 15 last.
/// Evidence tier ([`graph_edge_window_tier`]) decides it, and evidence confidence orders edges
/// within one tier, so a confident heuristic edge never ranks ahead of a proven one.
///
/// One integer, rather than a tuple, so a store can persist it beside the edge and index it:
/// ordering by `(rank, edge id)` is then exactly [`sort_graph_edges_for_window`], and a bounded
/// read is an index range scan instead of a decode and sort of every edge of the node.
pub fn graph_edge_window_rank(edge: &GraphEdge) -> u8 {
    const CONFIDENCE_LEVELS: u8 = 4;
    (GRAPH_EDGE_WINDOW_TIER_MAX - graph_edge_window_tier(edge)) * CONFIDENCE_LEVELS
        + (CONFIDENCE_LEVELS - 1 - confidence_rank(edge.evidence.confidence))
}

/// Sort key of one edge in window order: see [`sort_graph_edges_for_window`]. Smaller sorts first.
pub fn graph_edge_window_key(edge: &GraphEdge) -> (u8, String) {
    (graph_edge_window_rank(edge), edge.id.0.clone())
}

/// Order a set of graph edges the way every bounded edge window keeps them: by evidence tier
/// ([`graph_edge_window_tier`]: proven relationships, then parsed containment, then
/// corroborated, then heuristic); within a tier stronger evidence confidence first; then edge id.
///
/// A window that truncates by edge id alone lets whichever edges happen to hash low fill it, so
/// a heuristic edge could displace a proven one from a node's neighbourhood. Sorting rather than
/// reserving slots keeps every prefix tier-ordered, so any `limit` a caller picks cuts the
/// lowest-ranked edges. Confidence only orders edges within one tier — a confident heuristic edge
/// never outranks a proven one — and the edge id makes the order total and independent of
/// insertion order.
pub fn sort_graph_edges_for_window(edges: &mut [GraphEdge]) {
    // The rank parses the typed proofs, so it is computed once per edge rather than per
    // comparison.
    edges.sort_by_cached_key(graph_edge_window_key);
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
    use crate::{EdgeId, EvidenceSourceType, NodeId};
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

    /// Stores persist this rank and recompute it only when the version changes, so the table and
    /// the version are pinned together: a change to either without the other fails here.
    #[test]
    fn window_rank_table_is_pinned_to_its_version() {
        let ranked = |tier: &str, confidence: Confidence| {
            let mut edge = edge(GraphEdgeType::Imports, Vec::new());
            match tier {
                "proven" => edge
                    .set_relationship_proofs(vec![proof(RelationshipProofKind::ImportBinding, 1)])
                    .unwrap(),
                "containment" => {
                    edge.edge_type = GraphEdgeType::Contains;
                    edge.evidence.source_type = EvidenceSourceType::TreeSitter;
                }
                "corroborated" => edge
                    .set_relationship_proofs(vec![proof(RelationshipProofKind::QualifiedName, 1)])
                    .unwrap(),
                _ => {}
            }
            edge.evidence.confidence = confidence;
            graph_edge_window_rank(&edge)
        };
        let mut table = Vec::new();
        for tier in ["proven", "containment", "corroborated", "heuristic"] {
            for confidence in [
                Confidence::Exact,
                Confidence::High,
                Confidence::Medium,
                Confidence::Low,
            ] {
                table.push(ranked(tier, confidence));
            }
        }
        assert_eq!(
            (GRAPH_EDGE_WINDOW_RANK_VERSION, table),
            (1, (0..16).collect::<Vec<u8>>()),
            "a change to graph_edge_window_rank must bump GRAPH_EDGE_WINDOW_RANK_VERSION"
        );
    }

    #[test]
    fn window_order_ranks_evidence_tier_then_confidence_then_edge_id() {
        let windowed = |id: &str, proofs: Vec<RelationshipProof>, confidence: Confidence| {
            let mut edge = edge(GraphEdgeType::Imports, proofs);
            edge.id = EdgeId::new(id);
            edge.evidence.confidence = confidence;
            edge
        };
        // Declared weakest-first by id, so an id-ordered cut would keep exactly the wrong ones.
        let mut edges = vec![
            windowed("a-heuristic-low", Vec::new(), Confidence::Low),
            windowed("b-heuristic-high", Vec::new(), Confidence::High),
            windowed("c-heuristic-high", Vec::new(), Confidence::High),
            // An ambiguous binding proves nothing, so it stays in the heuristic tier however
            // confident the edge claims to be; confidence only orders it within that tier.
            windowed(
                "d-ambiguous-binding",
                vec![proof(RelationshipProofKind::ImportBinding, 2)],
                Confidence::Exact,
            ),
            windowed(
                "z-proven",
                vec![proof(RelationshipProofKind::ImportBinding, 1)],
                Confidence::Medium,
            ),
            windowed(
                "y-corroborated",
                vec![proof(RelationshipProofKind::DeclaredOrigin, 1)],
                Confidence::Exact,
            ),
        ];
        edges.last_mut().unwrap().edge_type = GraphEdgeType::DerivedFrom;
        // A parsed containment edge is a fact but not a dependency: below every proven
        // relationship, above everything unproven. The same edge from the regex fallback is a
        // guess and ranks with heuristic ones.
        let mut defines = windowed("x-defines", Vec::new(), Confidence::High);
        defines.edge_type = GraphEdgeType::Defines;
        defines.evidence.source_type = EvidenceSourceType::TreeSitter;
        let mut guessed = defines.clone();
        guessed.id = EdgeId::new("w-regex-defines");
        guessed.evidence.source_type = EvidenceSourceType::Regex;
        guessed.evidence.confidence = Confidence::Medium;
        edges.push(defines);
        edges.push(guessed);
        sort_graph_edges_for_window(&mut edges);
        let order = edges
            .iter()
            .map(|edge| edge.id.0.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            [
                "z-proven",
                "x-defines",
                "y-corroborated",
                "d-ambiguous-binding",
                "b-heuristic-high",
                "c-heuristic-high",
                "w-regex-defines",
                "a-heuristic-low"
            ]
        );
    }

    #[test]
    fn exact_module_qualified_call_is_authoritative() {
        let proofs = vec![
            proof(RelationshipProofKind::ExactCallSite, 1),
            proof(RelationshipProofKind::ModuleOrPackageBinding, 1),
            proof(RelationshipProofKind::QualifiedName, 1),
        ];
        assert_eq!(
            relationship_authority(&GraphEdgeType::Calls, &proofs),
            RelationshipAuthority::Authoritative
        );
    }

    #[test]
    fn a_declared_origin_can_corroborate_but_never_prove() {
        // A banner is prose. It cannot reach the tier reserved for parsed structure, however
        // unambiguous the path it names, so `proven_impact` can never contain one.
        let declared = vec![proof(RelationshipProofKind::DeclaredOrigin, 1)];
        assert_eq!(
            relationship_authority(&GraphEdgeType::DerivedFrom, &declared),
            RelationshipAuthority::Corroborating
        );
        assert!(!edge(GraphEdgeType::DerivedFrom, declared).is_authoritative_relationship());
        // Not even by claiming it in serialized data.
        let mut forged = RelationshipProof::new(RelationshipProofKind::DeclaredOrigin, "x", 1);
        forged.authority = RelationshipAuthority::Authoritative;
        assert_eq!(
            relationship_authority(&GraphEdgeType::DerivedFrom, &[forged]),
            RelationshipAuthority::Corroborating
        );

        // Two files could have been meant: the header is then not even corroboration.
        let ambiguous = vec![proof(RelationshipProofKind::DeclaredOrigin, 2)];
        assert_eq!(
            relationship_authority(&GraphEdgeType::DerivedFrom, &ambiguous),
            RelationshipAuthority::Heuristic
        );
        // A naming-convention pairing carries no proof at all.
        assert_eq!(
            relationship_authority(&GraphEdgeType::DerivedFrom, &[]),
            RelationshipAuthority::Heuristic
        );
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
