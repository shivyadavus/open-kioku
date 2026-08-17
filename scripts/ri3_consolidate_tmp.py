from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}: {old[:80]!r}")
    file.write_text(text.replace(old, new, 1))


core = Path("crates/open-kioku-core/src/lib.rs")
core_text = core.read_text()
old_core_seam = "pub mod identity;\n"
new_core_seam = (
    "pub mod identity;\n"
    "pub mod relationship;\n\n"
    "pub use relationship::{RelationshipAuthority, RelationshipProof, RelationshipProofKind};\n"
)
if core_text.count(old_core_seam) != 1:
    raise SystemExit("core module seam changed")
core.write_text(core_text.replace(old_core_seam, new_core_seam, 1))


evidence = Path("crates/open-kioku-evidence/src/lib.rs")
evidence_text = evidence.read_text()
start_marker = "/// A typed fact that can contribute to proving a structural repository relationship."
end_marker = "fn normalized_effective_authority(proof: &RelationshipProof) -> RelationshipAuthority {"
if evidence_text.count(start_marker) != 1 or evidence_text.count(end_marker) != 1:
    raise SystemExit("evidence proof vocabulary seam changed")
start = evidence_text.index(start_marker)
end = evidence_text.index(end_marker)
replacement = '''pub use open_kioku_core::{RelationshipAuthority, RelationshipProof, RelationshipProofKind};

/// Maximum authority a single proof kind can contribute before relationship-specific combination
/// rules are evaluated. Kept as an evidence-layer compatibility wrapper around the core ceiling.
pub fn proof_kind_authority(kind: RelationshipProofKind) -> RelationshipAuthority {
    kind.maximum_authority()
}

'''
evidence_text = evidence_text[:start] + replacement + evidence_text[end:]

short_page = '''            if batch_len < batch_limit {
                source_exhausted = true;
                break;
            }
'''
if evidence_text.count(short_page) != 1:
    raise SystemExit("relationship query short-page seam changed")
evidence_text = evidence_text.replace(short_page, "", 1)

store_seam = '''    #[test]
    fn exact_occurrence_proves_reference_relationship() {'''
if evidence_text.count(store_seam) != 1:
    raise SystemExit("evidence test store seam changed")
capped_store = '''    struct CappedPageGraphStore {
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
    fn exact_occurrence_proves_reference_relationship() {'''
evidence_text = evidence_text.replace(store_seam, capped_store, 1)

test_seam = '''    #[test]
    fn graph_store_query_reports_scan_truncation() {'''
if evidence_text.count(test_seam) != 1:
    raise SystemExit("relationship query regression seam changed")
new_test = '''    #[test]
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
    fn graph_store_query_reports_scan_truncation() {'''
evidence_text = evidence_text.replace(test_seam, new_test, 1)
evidence.write_text(evidence_text)


snapshot = Path("crates/open-kioku-mcp/snapshots/mcp/get_evidence_schema.json")
snapshot_text = snapshot.read_text()
old_edge = '''      {
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
        "description": "Edge of type Calls",'''
new_edge = '''      {
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
        "description": "Edge of type Calls",'''
if snapshot_text.count(old_edge) != 1:
    raise SystemExit("MCP edge snapshot seam changed")
snapshot_text = snapshot_text.replace(old_edge, new_edge, 1)
old_flags = '''      "config_keys",
      "service_boundaries",
      "read_only_graph_query"'''
new_flags = '''      "config_keys",
      "service_boundaries",
      "relationship_proofs",
      "read_only_graph_query"'''
if snapshot_text.count(old_flags) != 1:
    raise SystemExit("MCP feature flag snapshot seam changed")
snapshot.write_text(snapshot_text.replace(old_flags, new_flags, 1))
