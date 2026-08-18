from pathlib import Path

path = Path('crates/open-kioku-storage-sqlite/src/lib.rs')
text = path.read_text()

marker = '''fn clamp_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_GRAPH_QUERY_LIMIT
    } else {
        limit.min(MAX_GRAPH_QUERY_LIMIT)
    }
}

impl GraphStore for SqliteStore {'''
replacement = '''fn clamp_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_GRAPH_QUERY_LIMIT
    } else {
        limit.min(MAX_GRAPH_QUERY_LIMIT)
    }
}

fn require_authoritative_graph_semantics(store: &SqliteStore) -> Result<()> {
    let manifest = MetadataStore::manifest(store)?;
    let compatibility = open_kioku_core::classify_analysis_semantics(
        manifest
            .as_ref()
            .and_then(|manifest| manifest.analysis_semantics.as_ref()),
        &open_kioku_core::AnalysisSemanticsState::current(),
    );
    if compatibility.status.allows_authoritative_relationships() {
        return Ok(());
    }
    Err(OkError::Index(format!(
        "authoritative relationship evidence unavailable: analysis semantics {:?}: {}; stored={}, current={}; {}",
        compatibility.status,
        compatibility.reasons.join("; "),
        compatibility
            .stored_fingerprint
            .as_deref()
            .unwrap_or("missing"),
        compatibility.current_fingerprint,
        compatibility.recommended_action
    )))
}

impl GraphStore for SqliteStore {'''
assert text.count(marker) == 1, f'helper marker count={text.count(marker)}'
text = text.replace(marker, replacement, 1)

for signature in [
    '''    fn neighbors(&self, node: &str, limit: usize) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)> {
        let conn = self''',
    '''    fn shortest_path(&self, from: &str, to: &str, max_depth: usize) -> Result<Vec<GraphEdge>> {
        use std::collections::{HashSet, VecDeque};

        let conn = self''',
    '''    fn edges_by_type(
        &self,
        edge_type: GraphEdgeType,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GraphEdge>> {
        let conn = self''',
    '''    fn graph_edges_between(&self, from: &str, to: &str, limit: usize) -> Result<Vec<GraphEdge>> {
        let conn = self''',
]:
    assert text.count(signature) == 1, f'method marker count={text.count(signature)} for {signature[:40]!r}'

text = text.replace(
    '''    fn neighbors(&self, node: &str, limit: usize) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)> {
        let conn = self''',
    '''    fn neighbors(&self, node: &str, limit: usize) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)> {
        require_authoritative_graph_semantics(self)?;
        let conn = self''',
    1,
)
text = text.replace(
    '''    fn shortest_path(&self, from: &str, to: &str, max_depth: usize) -> Result<Vec<GraphEdge>> {
        use std::collections::{HashSet, VecDeque};

        let conn = self''',
    '''    fn shortest_path(&self, from: &str, to: &str, max_depth: usize) -> Result<Vec<GraphEdge>> {
        require_authoritative_graph_semantics(self)?;
        use std::collections::{HashSet, VecDeque};

        let conn = self''',
    1,
)
text = text.replace(
    '''    fn edges_by_type(
        &self,
        edge_type: GraphEdgeType,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GraphEdge>> {
        let conn = self''',
    '''    fn edges_by_type(
        &self,
        edge_type: GraphEdgeType,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<GraphEdge>> {
        require_authoritative_graph_semantics(self)?;
        let conn = self''',
    1,
)
text = text.replace(
    '''    fn graph_edges_between(&self, from: &str, to: &str, limit: usize) -> Result<Vec<GraphEdge>> {
        let conn = self''',
    '''    fn graph_edges_between(&self, from: &str, to: &str, limit: usize) -> Result<Vec<GraphEdge>> {
        require_authoritative_graph_semantics(self)?;
        let conn = self''',
    1,
)

old = '''        let (nodes, edges) = store.neighbors("file:src/lib.rs", 10).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].id.0, "e1");
        assert!(nodes.iter().any(|n| n.id == node_a.id));
    }

    #[test]
    fn graph_facts_with_properties_and_confidence_metadata_round_trip() {'''
new = '''        let (nodes, edges) = store.neighbors("file:src/lib.rs", 10).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].id.0, "e1");
        assert!(nodes.iter().any(|n| n.id == node_a.id));

        let mut stale_manifest = manifest.clone();
        let mut semantics = stale_manifest.analysis_semantics.clone().unwrap();
        semantics.descriptor.relationship_resolver_version = "old-resolver".into();
        stale_manifest.analysis_semantics = Some(open_kioku_core::AnalysisSemanticsState::new(
            semantics.descriptor,
        ));
        store.put_manifest(&stale_manifest).unwrap();

        for error in [
            store.neighbors("file:src/lib.rs", 10).unwrap_err(),
            store
                .shortest_path("file:src/lib.rs", "symbol:s1", 4)
                .unwrap_err(),
            store
                .edges_by_type(GraphEdgeType::Defines, 10, 0)
                .unwrap_err(),
            store
                .graph_edges_between("file:src/lib.rs", "symbol:s1", 10)
                .unwrap_err(),
        ] {
            let message = error.to_string();
            assert!(message.contains("authoritative relationship evidence unavailable"));
            assert!(message.contains("RebuildRequired"));
        }

        assert!(store.node_by_id("file:src/lib.rs").unwrap().is_some());
        assert_eq!(store.graph_counts().unwrap().edges, 1);
    }

    #[test]
    fn graph_facts_with_properties_and_confidence_metadata_round_trip() {'''
assert text.count(old) == 1, f'test marker count={text.count(old)}'
text = text.replace(old, new, 1)
path.write_text(text)
