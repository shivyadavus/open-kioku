from pathlib import Path


evidence = Path("crates/open-kioku-evidence/src/lib.rs")
text = evidence.read_text()
short_page = '''            if batch_len < batch_limit {
                source_exhausted = true;
                break;
            }
'''
if short_page in text:
    text = text.replace(short_page, "", 1)

if "struct CappedPageGraphStore" not in text:
    seam = '''    #[test]
    fn exact_occurrence_proves_reference_relationship() {'''
    if text.count(seam) != 1:
        raise SystemExit("evidence test store seam changed")
    insertion = '''    struct CappedPageGraphStore {
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
    text = text.replace(seam, insertion, 1)

if "fn graph_store_query_continues_after_short_pages()" not in text:
    seam = '''    #[test]
    fn graph_store_query_reports_scan_truncation() {'''
    if text.count(seam) != 1:
        raise SystemExit("relationship query regression seam changed")
    insertion = '''    #[test]
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
    text = text.replace(seam, insertion, 1)

evidence.write_text(text)


snapshot = Path("crates/open-kioku-mcp/snapshots/mcp/get_evidence_schema.json")
text = snapshot.read_text()
if '"name": "UsesType"' not in text:
    old = '''      {
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
    new = '''      {
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
    if text.count(old) != 1:
        raise SystemExit("MCP edge snapshot seam changed")
    text = text.replace(old, new, 1)

if '"relationship_proofs"' not in text:
    old = '''      "config_keys",
      "service_boundaries",
      "read_only_graph_query"'''
    new = '''      "config_keys",
      "service_boundaries",
      "relationship_proofs",
      "read_only_graph_query"'''
    if text.count(old) != 1:
        raise SystemExit("MCP feature flag snapshot seam changed")
    text = text.replace(old, new, 1)

snapshot.write_text(text)
