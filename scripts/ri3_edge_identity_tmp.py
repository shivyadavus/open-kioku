from pathlib import Path


def replace_exact(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label} seam changed: expected 1, observed {count}")
    return text.replace(old, new, 1)


identity = Path("crates/open-kioku-core/src/identity.rs")
text = identity.read_text()
text = replace_exact(
    text,
    "use crate::{EdgeId, GraphEdgeType, GraphNodeType, Language, NodeId, Symbol, TestTarget};\n",
    "use crate::{\n    EdgeId, GraphEdgeType, GraphNodeType, Language, NodeId, Symbol, SymbolId, TestTarget,\n};\n",
    "identity import",
)
text = replace_exact(
    text,
    '''pub fn symbol_node_id(symbol: &Symbol) -> NodeId {
    NodeId::new(format!(
        "symbol:{}",
        escape_identity_component(&symbol.id.0)
    ))
}
''',
    '''pub fn symbol_node_id(symbol: &Symbol) -> NodeId {
    symbol_id_node_id(&symbol.id)
}

/// Canonical graph-node identity for a symbol ID when the full symbol record is unavailable.
pub fn symbol_id_node_id(symbol_id: &SymbolId) -> NodeId {
    NodeId::new(format!(
        "symbol:{}",
        escape_identity_component(&symbol_id.0)
    ))
}
''',
    "symbol identity helper",
)
test_anchor = '''    #[test]
    fn route_config_test_and_edge_ids_are_deterministic() {
'''
identity_test = '''    #[test]
    fn symbol_id_node_identity_matches_full_symbol_identity() {
        let symbol = symbol("symbol:Repo.save");
        assert_eq!(symbol_id_node_id(&symbol.id), symbol_node_id(&symbol));
        assert_eq!(
            symbol_id_node_id(&SymbolId::new("file:src/lib.rs")).0,
            "symbol:file%3Asrc%2Flib.rs"
        );
    }

'''
text = replace_exact(text, test_anchor, identity_test + test_anchor, "identity regression")
identity.write_text(text)

relationship = Path("crates/open-kioku-core/src/relationship.rs")
text = relationship.read_text()
text = replace_exact(
    text,
    "use crate::{EvidenceId, FileRange, GraphEdge, GraphEdgeType, SymbolId};\n",
    "use crate::identity::symbol_id_node_id;\nuse crate::{EvidenceId, FileRange, GraphEdge, GraphEdgeType, SymbolId};\n",
    "relationship import",
)
source_key_anchor = '''fn source_range_key(range: &Option<FileRange>) -> (String, Option<u32>, Option<u32>) {
'''
edge_policy = '''fn graph_edge_relationship_authority(
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

'''
text = replace_exact(
    text,
    source_key_anchor,
    edge_policy + source_key_anchor,
    "persisted-edge authority policy",
)
text = replace_exact(
    text,
    "        relationship_authority(&self.edge_type, &proofs)\n",
    "        graph_edge_relationship_authority(self, &proofs)\n",
    "GraphEdge authority",
)
text = replace_exact(
    text,
    "        if relationship_authority(&edge.edge_type, &proofs) < self.minimum_authority {\n",
    "        if graph_edge_relationship_authority(edge, &proofs) < self.minimum_authority {\n",
    "relationship filter",
)
test_anchor = '''    #[test]
    fn legacy_and_malformed_edges_fail_closed() {
'''
endpoint_test = '''    #[test]
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

'''
text = replace_exact(
    text,
    test_anchor,
    endpoint_test + test_anchor,
    "persisted-target regression",
)
relationship.write_text(text)
