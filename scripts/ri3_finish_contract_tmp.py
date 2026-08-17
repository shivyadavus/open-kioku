from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str, count: int = 1) -> str:
    actual = text.count(old)
    if actual != count:
        raise SystemExit(f"{label}: expected {count} anchor(s), found {actual}")
    return text.replace(old, new, count)


# Re-export the complete core contract at the crate root for downstream consumers.
core = Path("crates/open-kioku-core/src/lib.rs")
text = core.read_text()
old = "pub use relationship::{RelationshipAuthority, RelationshipProof, RelationshipProofKind};\n"
new = """pub use relationship::{
    normalize_relationship_proofs, relationship_authority, RelationshipAuthority,
    RelationshipProof, RelationshipProofFilter, RelationshipProofKind,
    RELATIONSHIP_PROOFS_PROPERTY,
};
"""
text = replace_exact(text, old, new, "core relationship re-export")
core.write_text(text)


# Make evidence a compatibility/query layer over the single core authority contract.
evidence = Path("crates/open-kioku-evidence/src/lib.rs")
text = evidence.read_text()
text = replace_exact(
    text,
    "use open_kioku_core::{Confidence, Evidence, FileRange, GraphEdge, GraphEdgeType, SymbolId};\n",
    "use open_kioku_core::{Confidence, Evidence, GraphEdge, GraphEdgeType};\n",
    "evidence imports",
)
start_marker = "/// Structured property key used to persist relationship proofs on existing graph edges.\n"
end_marker = "pub const DEFAULT_RELATIONSHIP_EDGE_QUERY_LIMIT: usize = 100;\n"
start = text.find(start_marker)
end = text.find(end_marker)
if start < 0 or end < 0 or end <= start:
    raise SystemExit(f"evidence contract anchors invalid: start={start} end={end}")
compat = """pub use open_kioku_core::{
    normalize_relationship_proofs, relationship_authority, RelationshipAuthority,
    RelationshipProof, RelationshipProofFilter, RelationshipProofKind,
    RELATIONSHIP_PROOFS_PROPERTY,
};

/// Backward-compatible helper retained for callers of the evidence crate.
pub fn proof_kind_authority(kind: RelationshipProofKind) -> RelationshipAuthority {
    kind.maximum_authority()
}

/// Backward-compatible trait surface. The single implementation now delegates to the inherent
/// core `GraphEdge` authority API so downstream consumers cannot drift onto a second policy.
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

"""
text = text[:start] + compat + text[end:]
text = text.replace("use std::collections::BTreeSet;\n", "", 1)
short_page = """            if batch_len < batch_limit {
                source_exhausted = true;
                break;
            }
"""
text = replace_exact(text, short_page, "", "generic short-page exhaustion")
text = replace_exact(
    text,
    "    use std::path::PathBuf;\n",
    "    use std::collections::BTreeSet;\n    use std::path::PathBuf;\n",
    "evidence test BTreeSet import",
)
if "struct CappedPageGraphStore" not in text:
    seam = """    #[test]
    fn exact_occurrence_proves_reference_relationship() {"""
    insertion = """    struct CappedPageGraphStore {
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
    fn exact_occurrence_proves_reference_relationship() {"""
    text = replace_exact(text, seam, insertion, "capped graph store test seam")
if "fn graph_store_query_continues_after_short_pages()" not in text:
    seam = """    #[test]
    fn graph_store_query_reports_scan_truncation() {"""
    insertion = """    #[test]
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
    fn graph_store_query_reports_scan_truncation() {"""
    text = replace_exact(text, seam, insertion, "short-page regression seam")
evidence.write_text(text)


# Architecture enforcement must use structural authority, never confidence as a proxy.
architecture = Path("crates/open-kioku-architecture/src/lib.rs")
text = architecture.read_text()
report_init = """    let mut report = PolicyCheckReport {
        configured: true,
        ..PolicyCheckReport::default()
    };
"""
report_init_new = report_init + "    let mut ignored_non_authoritative_edges = 0usize;\n"
text = replace_exact(text, report_init, report_init_new, "architecture report init", count=2)
text = replace_exact(
    text,
    """            for edge in &batch {
                report.evaluated_edge_count += 1;
""",
    """            for edge in &batch {
                if !edge.is_authoritative_relationship() {
                    ignored_non_authoritative_edges += 1;
                    continue;
                }
                report.evaluated_edge_count += 1;
""",
    "architecture policy authority gate",
)
text = replace_exact(
    text,
    """            for edge in &batch {
                if let Some(evidence) = edge_evidence(
""",
    """            for edge in &batch {
                if !edge.is_authoritative_relationship() {
                    ignored_non_authoritative_edges += 1;
                    continue;
                }
                if let Some(evidence) = edge_evidence(
""",
    "public API authority gate",
)
text = replace_exact(
    text,
    """            offset += batch.len();
            if batch.len() < 1_000 {
                break;
            }
""",
    """            offset += batch.len();
""",
    "architecture generic short-page paging",
    count=2,
)
uncertainty = """    if report.evaluated_edge_count == 0 {
"""
uncertainty_new = """    if ignored_non_authoritative_edges > 0 {
        report.uncertainty.push(format!(
            "ignored {} non-authoritative structural edge(s); architecture enforcement requires typed relationship proof",
            ignored_non_authoritative_edges
        ));
    }
    if report.evaluated_edge_count == 0 {
"""
text = replace_exact(text, uncertainty, uncertainty_new, "architecture authority uncertainty", count=2)
text = replace_exact(
    text,
    """        Confidence, EdgeId, Evidence, File, FileId, GraphEdgeType, GraphNodeType, IndexManifest,
        IndexMode, IndexQuality, Language, NodeId, Repository, RepositoryId,
""",
    """        Confidence, EdgeId, Evidence, File, FileId, GraphEdgeType, GraphNodeType, IndexManifest,
        IndexMode, IndexQuality, Language, NodeId, RelationshipProof, RelationshipProofKind,
        Repository, RepositoryId,
""",
    "architecture RI3 test imports",
)
old_edge_helper = """    fn edge(id: &str, from: &GraphNode, to: &GraphNode, edge_type: GraphEdgeType) -> GraphEdge {
        GraphEdge {
            id: EdgeId::new(id),
            from: from.id.clone(),
            to: to.id.clone(),
            edge_type,
            evidence: Evidence {
                id: open_kioku_core::EvidenceId::new(format!("evidence-{id}")),
                source: "test".into(),
                confidence: Confidence::High,
                message: format!("{id} evidence"),
                ..Evidence::default()
            },
            ..GraphEdge::default()
        }
    }
"""
new_edge_helper = """    fn unproved_edge(
        id: &str,
        from: &GraphNode,
        to: &GraphNode,
        edge_type: GraphEdgeType,
    ) -> GraphEdge {
        GraphEdge {
            id: EdgeId::new(id),
            from: from.id.clone(),
            to: to.id.clone(),
            edge_type,
            evidence: Evidence {
                id: open_kioku_core::EvidenceId::new(format!("evidence-{id}")),
                source: "test".into(),
                confidence: Confidence::High,
                message: format!("{id} evidence"),
                ..Evidence::default()
            },
            ..GraphEdge::default()
        }
    }

    fn edge(id: &str, from: &GraphNode, to: &GraphNode, edge_type: GraphEdgeType) -> GraphEdge {
        let proof_kinds = match &edge_type {
            GraphEdgeType::Imports => vec![RelationshipProofKind::ImportBinding],
            GraphEdgeType::References => vec![RelationshipProofKind::ExactReference],
            GraphEdgeType::Calls => vec![
                RelationshipProofKind::ExactCallSite,
                RelationshipProofKind::ExactReference,
            ],
            _ => Vec::new(),
        };
        let mut edge = unproved_edge(id, from, to, edge_type);
        edge.set_relationship_proofs(
            proof_kinds
                .into_iter()
                .map(|kind| RelationshipProof::new(kind, "architecture-test", 1))
                .collect(),
        )
        .unwrap();
        edge
    }
"""
text = replace_exact(text, old_edge_helper, new_edge_helper, "architecture edge helper")
first_test = """    #[test]
    fn forbidden_dependency_rule_reports_deterministic_violation() {"""
regression = """    #[test]
    fn confidence_without_structural_proof_is_ignored() {
        let domain = file("domain", "src/domain/order.rs");
        let api = file("api", "src/api/http.rs");
        let domain_node = file_node(&domain);
        let api_node = file_node(&api);
        let policy = policy(vec![DependencyRule {
            id: "domain-must-not-call-api".into(),
            from: "domain".into(),
            to: "api".into(),
            action: DependencyAction::Forbid,
            severity: Severity::Error,
            reason: "domain cannot depend on api".into(),
        }]);

        let report = evaluate(
            &[domain.clone(), api.clone()],
            &[domain_node.clone(), api_node.clone()],
            &[unproved_edge(
                "high-confidence-without-proof",
                &domain_node,
                &api_node,
                GraphEdgeType::Calls,
            )],
            &policy,
        );

        assert_eq!(report.evaluated_edge_count, 0);
        assert_eq!(report.violation_count, 0);
        assert!(report
            .uncertainty
            .iter()
            .any(|message| message.contains("non-authoritative structural edge")));
    }

    #[test]
    fn forbidden_dependency_rule_reports_deterministic_violation() {"""
text = replace_exact(text, first_test, regression, "architecture authority regression")
architecture.write_text(text)


# ContextPack must not expose unproved structural edges to planning/retrieval consumers.
context = Path("crates/open-kioku-context/src/lib.rs")
text = context.read_text()
helper_anchor = "fn refresh_context_pack_retrieval_telemetry(\n"
helper = """fn extend_authoritative_relationships(
    target: &mut Vec<GraphEdge>,
    edges: Vec<GraphEdge>,
) {
    target.extend(
        edges
            .into_iter()
            .filter(GraphEdge::is_authoritative_relationship),
    );
}

"""
if text.count(helper_anchor) != 1:
    raise SystemExit(f"context helper anchor count={text.count(helper_anchor)}")
text = text.replace(helper_anchor, helper + helper_anchor, 1)
text = replace_exact(
    text,
    "                dependency_edges.extend(edges);\n",
    "                extend_authoritative_relationships(&mut dependency_edges, edges);\n",
    "context dependency authority gate",
    count=2,
)
text = replace_exact(
    text,
    "    use open_kioku_core::{FileId, Language, LineRange, RepositoryId, SymbolId, SymbolKind};\n",
    """    use open_kioku_core::{
        EdgeId, FileId, Language, LineRange, NodeId, RelationshipProof, RelationshipProofKind,
        RepositoryId, SymbolId, SymbolKind,
    };
""",
    "context RI3 test imports",
)
module_test_anchor = """    #[test]
    fn """
pos = text.find(module_test_anchor, text.find("#[cfg(test)]\nmod tests"))
if pos < 0:
    raise SystemExit("context first test anchor missing")
context_test = """    #[test]
    fn dependency_edges_require_authoritative_relationship_proof() {
        let legacy = GraphEdge {
            id: EdgeId::new("legacy"),
            from: NodeId::new("from"),
            to: NodeId::new("to"),
            edge_type: GraphEdgeType::References,
            ..GraphEdge::default()
        };
        let mut proved = GraphEdge {
            id: EdgeId::new("proved"),
            from: NodeId::new("from"),
            to: NodeId::new("to"),
            edge_type: GraphEdgeType::References,
            ..GraphEdge::default()
        };
        proved
            .set_relationship_proofs(vec![RelationshipProof::new(
                RelationshipProofKind::ExactReference,
                "context-test",
                1,
            )])
            .unwrap();

        let mut selected = Vec::new();
        extend_authoritative_relationships(&mut selected, vec![legacy, proved]);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id.0, "proved");
    }

"""
text = text[:pos] + context_test + text[pos:]
context.write_text(text)


# Refresh the stable MCP schema snapshot additively.
snapshot = Path("crates/open-kioku-mcp/snapshots/mcp/get_evidence_schema.json")
text = snapshot.read_text()
if '"name": "UsesType"' not in text:
    old = """      {
        "count": 0,
        "description": "Edge of type References",
        "evidence_available": false,
        "name": "References",
        "required_evidence": [],
        "source_types": [],
        "stable": true,
        "target_types": []
      },
      {
        "count": 1,
        "description": "Edge of type Calls","""
    new = """      {
        "count": 0,
        "description": "Edge of type References",
        "evidence_available": false,
        "name": "References",
        "required_evidence": [],
        "source_types": [],
        "stable": true,
        "target_types": []
      },
      {
        "count": 0,
        "description": "Edge of type UsesType",
        "evidence_available": false,
        "name": "UsesType",
        "required_evidence": [],
        "source_types": [],
        "stable": true,
        "target_types": []
      },
      {
        "count": 1,
        "description": "Edge of type Calls","""
    text = replace_exact(text, old, new, "MCP UsesType snapshot")
if '"relationship_proofs"' not in text:
    text = replace_exact(
        text,
        """      "config_keys",
      "service_boundaries",
      "read_only_graph_query"""",
        """      "config_keys",
      "service_boundaries",
      "relationship_proofs",
      "read_only_graph_query"""",
        "MCP relationship proof feature",
    )
snapshot.write_text(text)
