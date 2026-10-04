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

/// Authority of one graph edge as a surface reports it beside the edge: the graph query's hops,
/// and `edge_authority` on MCP and CLI reads that return edges.
///
/// A relationship edge has its effective [`GraphEdge::relationship_authority`]. Containment
/// (`CONTAINS`, `DEFINES`) resolves no name, so no proof policy applies to it, and it splits the
/// way [`graph_edge_window_tier`] splits it: extracted by a parser or an index it records where a
/// symbol is (`Authoritative`); from the regex fallback it is a guess (`Heuristic`). Read through
/// `relationship_authority` alone, every parsed `DEFINES` edge would report `heuristic`, the
/// class a symbol-registry name match gets.
///
/// This is the class an edge is reported under, not a gate: callers that decide whether a
/// relationship may be consumed as structural truth keep reading
/// [`GraphEdge::relationship_authority`] or a [`RelationshipProofFilter`].
pub fn graph_edge_authority(edge: &GraphEdge) -> RelationshipAuthority {
    match edge.edge_type {
        GraphEdgeType::Contains | GraphEdgeType::Defines => {
            if edge.evidence.source_type.is_exact_reference_source() {
                RelationshipAuthority::Authoritative
            } else {
                RelationshipAuthority::Heuristic
            }
        }
        _ => edge.relationship_authority(),
    }
}

/// Whether an edge type records where something is (`CONTAINS`, `DEFINES`) rather than a
/// relationship between two things.
pub fn is_containment_edge_type(edge_type: &GraphEdgeType) -> bool {
    matches!(edge_type, GraphEdgeType::Contains | GraphEdgeType::Defines)
}

/// Authority one hop of a forward route contributes to the route, given whether a relationship
/// hop came before it.
///
/// Containment is not transitive across a relationship: "A imports the file that defines X" is
/// not "A depends on X". A containment hop taken after a relationship hop descends from what the
/// route reached into everything it contains, so its contribution is capped at `Corroborating`
/// however established the containment itself is. Containment before any relationship hop (a
/// file's symbol that calls something) and relationship hops keep [`graph_edge_authority`].
pub fn graph_route_hop_authority(
    edge: &GraphEdge,
    after_relationship: bool,
) -> RelationshipAuthority {
    let authority = graph_edge_authority(edge);
    if after_relationship && is_containment_edge_type(&edge.edge_type) {
        authority.min(RelationshipAuthority::Corroborating)
    } else {
        authority
    }
}

/// Each hop's [`graph_route_hop_authority`] along a forward route, in order. Every graph route a
/// surface returns (`shortest_path`, a multi-hop query walk) follows edges from `from` to `to`,
/// so a containment hop after a relationship hop is always a descent.
///
/// The converse overclaim, a symbol up to its containing file and then across that file's
/// relationship ("this symbol's file imports X" read as "this symbol depends on X"), needs a
/// containment edge traversed backwards. No route surface does that: `shortest_path` reads only a
/// node's outgoing edges, the query walk follows only edges leaving the current node and rejects
/// reverse hop ranges, and `explain_flow` follows outgoing `CALLS` only. A surface that starts
/// walking edges backwards must cap that ascent here first;
/// `a_symbol_never_reaches_its_file_s_imports` (open-kioku-graph) and
/// `a_route_into_an_imported_file_s_other_symbols_is_not_authoritative` (open-kioku-cli) pin it.
pub fn graph_route_authorities(edges: &[GraphEdge]) -> Vec<RelationshipAuthority> {
    let mut after_relationship = false;
    edges
        .iter()
        .map(|edge| {
            let authority = graph_route_hop_authority(edge, after_relationship);
            after_relationship |= !is_containment_edge_type(&edge.edge_type);
            authority
        })
        .collect()
}

/// A route [`strongest_shortest_route`] keeps to one node: the lowest contribution any of its
/// hops makes ([`graph_route_hop_authority`]), the order it was first kept in, and its edges.
struct KeptRoute {
    weakest: RelationshipAuthority,
    found: usize,
    edges: Vec<GraphEdge>,
}

/// The shortest forward route from `from` to `to` of at most `max_hops` hops whose weakest hop
/// contributes the most, or no edges when there is none (or `from == to`).
///
/// A route is only as established as its weakest hop, so among routes of equal length a caller
/// that reports [`graph_route_authorities`] must not be handed one through a heuristic or capped
/// hop while an equally short established one exists. Length still comes first: a longer route
/// is never returned for being stronger, because "the nearest connection" is what a path answers.
///
/// The walk is breadth first, one depth at a time. `outgoing` returns a node's outgoing edges in
/// the order ties are broken in (stores pass [`sort_graph_edges_for_window`] order). Each node is
/// settled at the first depth it is reached, since any prefix of a shortest route is a shortest
/// route to where it ends. At that depth the walk keeps, per node, the route with the strongest
/// weakest hop, and of equals the first one found. What a route's next hop contributes depends
/// on whether it has crossed a relationship yet (containment after one is capped), so a node
/// keeps one route for each of those two states, and a route that needed no relationship is
/// never displaced by a stronger-so-far one that a later containment hop would cap. Keeping the
/// best per node and state is exact: the weakest hop of a whole route is the lower of its
/// prefix's and its suffix's, and the suffix's depends only on the node and the state.
///
/// With no stronger route anywhere, this returns the route the plain breadth-first walk did:
/// nodes are expanded in the order they were first reached and edges in `outgoing` order.
pub fn strongest_shortest_route<E>(
    from: &str,
    to: &str,
    max_hops: usize,
    mut outgoing: impl FnMut(&str) -> Result<Vec<GraphEdge>, E>,
) -> Result<Vec<GraphEdge>, E> {
    use std::collections::{HashMap, HashSet};

    if from == to {
        return Ok(Vec::new());
    }
    // Index 0 is a route that has crossed no relationship hop, index 1 one that has.
    type States = [Option<KeptRoute>; 2];
    let mut settled = HashSet::from([from.to_string()]);
    let mut layer: Vec<(String, States)> = vec![(
        from.to_string(),
        [
            Some(KeptRoute {
                weakest: RelationshipAuthority::Authoritative,
                found: 0,
                edges: Vec::new(),
            }),
            None,
        ],
    )];
    let mut found = 0;
    for _ in 0..max_hops {
        let mut next: Vec<(String, States)> = Vec::new();
        let mut slots: HashMap<String, usize> = HashMap::new();
        for (node, states) in &layer {
            for edge in outgoing(node)? {
                let target = edge.to.0.as_str();
                if settled.contains(target) {
                    continue;
                }
                for (crossed, kept) in states.iter().enumerate() {
                    let Some(kept) = kept else {
                        continue;
                    };
                    let after_relationship = crossed == 1;
                    let weakest = kept
                        .weakest
                        .min(graph_route_hop_authority(&edge, after_relationship));
                    let crosses = after_relationship || !is_containment_edge_type(&edge.edge_type);
                    let slot = *slots.entry(target.to_string()).or_insert_with(|| {
                        next.push((target.to_string(), [None, None]));
                        next.len() - 1
                    });
                    let entry = &mut next[slot].1[usize::from(crosses)];
                    if entry.as_ref().is_none_or(|best| weakest > best.weakest) {
                        let mut edges = Vec::with_capacity(kept.edges.len() + 1);
                        edges.extend_from_slice(&kept.edges);
                        edges.push(edge.clone());
                        found += 1;
                        let first = entry.as_ref().map_or(found, |best| best.found);
                        *entry = Some(KeptRoute {
                            weakest,
                            found: first,
                            edges,
                        });
                    }
                }
            }
        }
        if let Some(&slot) = slots.get(to) {
            let [plain, crossed] = std::mem::take(&mut next[slot].1);
            let best = [plain, crossed].into_iter().flatten().max_by(|a, b| {
                a.weakest
                    .cmp(&b.weakest)
                    .then_with(|| b.found.cmp(&a.found))
            });
            return Ok(best.map(|route| route.edges).unwrap_or_default());
        }
        if next.is_empty() {
            break;
        }
        settled.extend(slots.into_keys());
        layer = next;
    }
    Ok(Vec::new())
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
///
/// Before the first bump: a writer checks the recorded version only when it opens the store, so
/// a long-running writer of the previous version (an `ok watch` started before the upgrade) would
/// keep inserting non-negative ranks of the old function under the new version, which the
/// unranked-insert trigger cannot see. The bump must make writers re-check the stored version
/// before each write and withdraw it on a mismatch, or record the version per row.
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

    /// Stores persist this rank and recompute it only when the version changes, so the rank of
    /// every shape of edge is pinned with the version: a change to the tiers, to which sources
    /// count as parsed containment, to how an authority class maps to a tier, or to how
    /// confidence orders edges fails here unless the version moves with it.
    ///
    /// The expected tier is spelled out rather than computed through the functions under test.
    /// The authority policy itself is versioned with the analysis semantics, whose change forces
    /// a full rebuild and so a fresh rank for every edge; this pins how each class it can return
    /// ranks.
    #[test]
    fn window_rank_table_is_pinned_to_its_version() {
        const SOURCES: [EvidenceSourceType; 11] = [
            EvidenceSourceType::TreeSitter,
            EvidenceSourceType::Scip,
            EvidenceSourceType::Lsp,
            EvidenceSourceType::Regex,
            EvidenceSourceType::Lexical,
            EvidenceSourceType::Semantic,
            EvidenceSourceType::Runtime,
            EvidenceSourceType::GitHistory,
            EvidenceSourceType::StaticAnalysis,
            EvidenceSourceType::ExternalIntegration,
            EvidenceSourceType::Heuristic,
        ];
        const CONFIDENCES: [Confidence; 4] = [
            Confidence::Exact,
            Confidence::High,
            Confidence::Medium,
            Confidence::Low,
        ];
        let relationship_types = [
            GraphEdgeType::References,
            GraphEdgeType::UsesType,
            GraphEdgeType::Calls,
            GraphEdgeType::Implements,
            GraphEdgeType::Extends,
            GraphEdgeType::Imports,
            GraphEdgeType::DependsOn,
            GraphEdgeType::ExposesEndpoint,
            GraphEdgeType::CallsEndpoint,
            GraphEdgeType::ReadsConfig,
            GraphEdgeType::WritesConfig,
            GraphEdgeType::ReadsTable,
            GraphEdgeType::WritesTable,
            GraphEdgeType::PublishesEvent,
            GraphEdgeType::ConsumesEvent,
            GraphEdgeType::Tests,
            GraphEdgeType::TestCovers,
            GraphEdgeType::Validates,
            GraphEdgeType::OwnedBy,
            GraphEdgeType::ChangedBy,
            GraphEdgeType::FailedIn,
            GraphEdgeType::BelongsTo,
            GraphEdgeType::MentionedIn,
            GraphEdgeType::RelatedToTicket,
            GraphEdgeType::SimilarTo,
            GraphEdgeType::SemanticallyRelated,
            GraphEdgeType::DerivedFrom,
        ];
        let proof_sets: Vec<Vec<RelationshipProof>> = vec![
            Vec::new(),
            vec![proof(RelationshipProofKind::ImportBinding, 1)],
            vec![proof(RelationshipProofKind::ImportBinding, 2)],
            vec![proof(RelationshipProofKind::QualifiedName, 1)],
            vec![proof(RelationshipProofKind::DeclaredOrigin, 1)],
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::SameScopeDefinition, 1),
            ],
            vec![
                proof(RelationshipProofKind::ImportBinding, 1),
                proof(RelationshipProofKind::QualifiedName, 1),
            ],
            vec![
                proof(RelationshipProofKind::InheritanceBinding, 1),
                proof(RelationshipProofKind::TraitOrInterfaceBinding, 1),
            ],
            vec![proof(RelationshipProofKind::ModuleOrPackageBinding, 1)],
        ];
        let expected_rank =
            |tier: u8, confidence_index: usize| (3 - tier) * 4 + confidence_index as u8;
        let mut checked = 0;
        for (confidence_index, confidence) in CONFIDENCES.into_iter().enumerate() {
            // Containment: parsed from a parser or index is tier 2, anything else is heuristic,
            // whatever proofs the edge carries.
            for edge_type in [GraphEdgeType::Contains, GraphEdgeType::Defines] {
                for source in SOURCES {
                    for proofs in &proof_sets {
                        let mut containment = edge(edge_type.clone(), proofs.clone());
                        containment.evidence.source_type = source.clone();
                        containment.evidence.confidence = confidence;
                        let parsed = matches!(
                            source,
                            EvidenceSourceType::TreeSitter
                                | EvidenceSourceType::Scip
                                | EvidenceSourceType::Lsp
                        );
                        assert_eq!(
                            (
                                GRAPH_EDGE_WINDOW_RANK_VERSION,
                                graph_edge_window_rank(&containment)
                            ),
                            (
                                1,
                                expected_rank(if parsed { 2 } else { 0 }, confidence_index)
                            ),
                            "{edge_type:?} from {source:?} at {confidence:?}: a change to the \
                             window rank must bump GRAPH_EDGE_WINDOW_RANK_VERSION"
                        );
                        checked += 1;
                    }
                }
            }
            // Relationships: the authority class decides the tier, and the source does not.
            let mut classes = BTreeSet::new();
            for edge_type in &relationship_types {
                for source in SOURCES {
                    for proofs in &proof_sets {
                        let mut relationship = edge(edge_type.clone(), proofs.clone());
                        relationship.evidence.source_type = source.clone();
                        relationship.evidence.confidence = confidence;
                        let authority = relationship.relationship_authority();
                        classes.insert(format!("{authority:?}"));
                        let tier = match authority {
                            RelationshipAuthority::Authoritative => 3,
                            RelationshipAuthority::Corroborating => 1,
                            RelationshipAuthority::Heuristic => 0,
                        };
                        assert_eq!(
                            (
                                GRAPH_EDGE_WINDOW_RANK_VERSION,
                                graph_edge_window_rank(&relationship)
                            ),
                            (1, expected_rank(tier, confidence_index)),
                            "{edge_type:?} {authority:?} from {source:?} at {confidence:?}: a \
                             change to the window rank must bump GRAPH_EDGE_WINDOW_RANK_VERSION"
                        );
                        checked += 1;
                    }
                }
            }
            assert_eq!(
                classes.len(),
                3,
                "every authority class must be exercised: {classes:?}"
            );
        }
        assert_eq!(checked, 4 * (2 + 27) * 11 * 9);
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

    /// `ledger.rs` imports `audit.rs`, which defines `archive`: both edges are established, but
    /// the route does not establish that ledger.rs relates to archive.
    #[test]
    fn containment_after_a_relationship_hop_caps_the_route_at_corroborating() {
        let mut imports = edge(
            GraphEdgeType::Imports,
            vec![proof(RelationshipProofKind::ImportBinding, 1)],
        );
        imports.evidence.source_type = EvidenceSourceType::TreeSitter;
        assert_eq!(
            graph_edge_authority(&imports),
            RelationshipAuthority::Authoritative
        );
        let mut defines = edge(GraphEdgeType::Defines, Vec::new());
        defines.evidence.source_type = EvidenceSourceType::TreeSitter;

        assert_eq!(
            graph_route_authorities(&[imports.clone(), defines.clone()]),
            [
                RelationshipAuthority::Authoritative,
                RelationshipAuthority::Corroborating
            ]
        );
        // Containment first: the file's symbol is what the relationship leaves from.
        assert_eq!(
            graph_route_authorities(&[defines.clone(), imports.clone()]),
            [
                RelationshipAuthority::Authoritative,
                RelationshipAuthority::Authoritative
            ]
        );
        assert_eq!(
            graph_route_authorities(&[defines.clone(), defines.clone()]),
            [
                RelationshipAuthority::Authoritative,
                RelationshipAuthority::Authoritative
            ]
        );
        // A cap never raises: regex containment stays heuristic after a relationship hop.
        let mut regex = defines;
        regex.evidence.source_type = EvidenceSourceType::Regex;
        assert_eq!(
            graph_route_hop_authority(&regex, true),
            RelationshipAuthority::Heuristic
        );
    }

    /// A route hop for [`strongest_shortest_route`]: a proven or proofless `IMPORTS`, or parsed
    /// containment.
    fn route_hop(id: &str, from: &str, to: &str, kind: &str) -> GraphEdge {
        let (edge_type, proofs) = match kind {
            "proven" => (
                GraphEdgeType::Imports,
                vec![proof(RelationshipProofKind::ImportBinding, 1)],
            ),
            "heuristic" => (GraphEdgeType::Imports, Vec::new()),
            "defines" => (GraphEdgeType::Defines, Vec::new()),
            "contains" => (GraphEdgeType::Contains, Vec::new()),
            other => panic!("unknown hop kind {other}"),
        };
        let mut hop = edge(edge_type, proofs);
        hop.id = EdgeId::new(id);
        hop.from = NodeId::new(from);
        hop.to = NodeId::new(to);
        hop.evidence.source_type = EvidenceSourceType::TreeSitter;
        hop
    }

    /// The route's edge ids, with each node's outgoing edges given in window order as a store
    /// gives them.
    fn route_ids(edges: &[GraphEdge], from: &str, to: &str, max_hops: usize) -> Vec<String> {
        let route = strongest_shortest_route(from, to, max_hops, |node| {
            let mut out = edges
                .iter()
                .filter(|edge| edge.from.0 == node)
                .cloned()
                .collect::<Vec<_>>();
            sort_graph_edges_for_window(&mut out);
            Ok::<_, ()>(out)
        })
        .unwrap();
        route.into_iter().map(|edge| edge.id.0).collect()
    }

    /// Two routes of two hops: the one tried first (`a-b` sorts before `a-c`) has a heuristic
    /// second hop. A breadth-first walk that returns the first route found takes it.
    #[test]
    fn of_equally_short_routes_the_one_with_the_strongest_weakest_hop_is_returned() {
        let edges = [
            route_hop("a-b", "a", "b", "proven"),
            route_hop("a-c", "a", "c", "proven"),
            route_hop("b-d", "b", "d", "heuristic"),
            route_hop("c-d", "c", "d", "proven"),
        ];
        assert_eq!(route_ids(&edges, "a", "d", 12), ["a-c", "c-d"]);
        // Equally strong routes keep the first one found, as the plain walk did.
        let tied = [
            route_hop("a-b", "a", "b", "proven"),
            route_hop("a-c", "a", "c", "proven"),
            route_hop("b-d", "b", "d", "proven"),
            route_hop("c-d", "c", "d", "proven"),
        ];
        assert_eq!(route_ids(&tied, "a", "d", 12), ["a-b", "b-d"]);
    }

    /// `ledger.rs` imports `audit.rs`, which defines `record`, and ledger.rs's own `post` calls
    /// `record`. The import is tried first (a proven relationship ranks above containment in the
    /// window) but its DEFINES hop is capped; the route through `post` establishes the relation.
    #[test]
    fn an_equally_short_route_without_a_capped_hop_is_preferred() {
        let edges = [
            route_hop("imports", "ledger", "audit", "proven"),
            route_hop("audit-defines-record", "audit", "record", "defines"),
            route_hop("ledger-defines-post", "ledger", "post", "defines"),
            route_hop("post-uses-record", "post", "record", "proven"),
        ];
        let ids = route_ids(&edges, "ledger", "record", 12);
        assert_eq!(ids, ["ledger-defines-post", "post-uses-record"]);
        let route = edges
            .iter()
            .filter(|edge| ids.contains(&edge.id.0))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            graph_route_authorities(&route).into_iter().min(),
            Some(RelationshipAuthority::Authoritative)
        );
    }

    /// `m` is reached at one depth through a proven import and through containment, both
    /// authoritative so far. Only the containment route keeps the next DEFINES hop
    /// authoritative, so a walk that keeps one route per node (the import, found first) reports
    /// corroborating where an authoritative route exists.
    #[test]
    fn a_route_that_crossed_no_relationship_is_kept_beside_one_that_did() {
        let edges = [
            route_hop("f-imports-m", "f", "m", "proven"),
            route_hop("f-contains-m", "f", "m", "contains"),
            route_hop("m-defines-x", "m", "x", "defines"),
        ];
        assert_eq!(
            route_ids(&edges, "f", "x", 12),
            ["f-contains-m", "m-defines-x"]
        );
    }

    /// Length comes first: a longer proven route never replaces a shorter heuristic one, and a
    /// route longer than `max_hops` is not found.
    #[test]
    fn a_stronger_route_is_never_returned_over_a_shorter_one() {
        let edges = [
            route_hop("a-d", "a", "d", "heuristic"),
            route_hop("a-b", "a", "b", "proven"),
            route_hop("b-d", "b", "d", "proven"),
        ];
        assert_eq!(route_ids(&edges, "a", "d", 12), ["a-d"]);
        let chain = [
            route_hop("a-b", "a", "b", "proven"),
            route_hop("b-c", "b", "c", "proven"),
        ];
        assert_eq!(route_ids(&chain, "a", "c", 2), ["a-b", "b-c"]);
        assert!(route_ids(&chain, "a", "c", 1).is_empty());
        assert!(route_ids(&chain, "a", "a", 12).is_empty());
        assert!(route_ids(&chain, "c", "a", 12).is_empty());
    }

    /// Parsed containment reports as the fact it is and regex containment as the guess it is;
    /// every other edge type reports its proof-derived authority, so no evidence source or
    /// confidence lifts a proofless relationship.
    #[test]
    fn reported_edge_authority_splits_containment_by_source_and_reads_proofs_otherwise() {
        for edge_type in [GraphEdgeType::Contains, GraphEdgeType::Defines] {
            for (source_type, expected) in [
                (
                    EvidenceSourceType::TreeSitter,
                    RelationshipAuthority::Authoritative,
                ),
                (
                    EvidenceSourceType::Scip,
                    RelationshipAuthority::Authoritative,
                ),
                (EvidenceSourceType::Regex, RelationshipAuthority::Heuristic),
                (
                    EvidenceSourceType::Heuristic,
                    RelationshipAuthority::Heuristic,
                ),
            ] {
                let mut containment = edge(edge_type.clone(), Vec::new());
                containment.evidence.source_type = source_type.clone();
                assert_eq!(
                    graph_edge_authority(&containment),
                    expected,
                    "{edge_type:?} from {source_type:?}"
                );
            }
        }

        let mut registry = edge(GraphEdgeType::Calls, Vec::new());
        registry.evidence.source_type = EvidenceSourceType::Scip;
        registry.evidence.confidence = Confidence::Exact;
        assert_eq!(
            graph_edge_authority(&registry),
            RelationshipAuthority::Heuristic
        );
        let corroborated = edge(
            GraphEdgeType::Calls,
            vec![proof(RelationshipProofKind::ImportBinding, 1)],
        );
        assert_eq!(
            graph_edge_authority(&corroborated),
            RelationshipAuthority::Corroborating
        );
        let proven = edge(
            GraphEdgeType::Calls,
            vec![
                proof(RelationshipProofKind::ExactCallSite, 1),
                proof(RelationshipProofKind::SameScopeDefinition, 1),
            ],
        );
        assert_eq!(
            graph_edge_authority(&proven),
            RelationshipAuthority::Authoritative
        );
    }
}
