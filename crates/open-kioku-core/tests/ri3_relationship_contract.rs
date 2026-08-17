use open_kioku_core::{
    GraphEdge, GraphEdgeType, RelationshipAuthority, RelationshipProof, RelationshipProofKind,
};

#[test]
fn public_relationship_authority_api_fails_closed_and_accepts_exact_proof() {
    let legacy = GraphEdge {
        edge_type: GraphEdgeType::References,
        ..GraphEdge::default()
    };
    assert_eq!(
        legacy.relationship_authority(),
        RelationshipAuthority::Heuristic
    );
    assert!(!legacy.is_authoritative_relationship());

    let mut proved = GraphEdge {
        edge_type: GraphEdgeType::References,
        ..GraphEdge::default()
    };
    proved
        .set_relationship_proofs(vec![RelationshipProof::new(
            RelationshipProofKind::ExactReference,
            "public-api-contract",
            1,
        )])
        .expect("serialize typed relationship proof");

    assert_eq!(
        proved.relationship_authority(),
        RelationshipAuthority::Authoritative
    );
    assert!(proved.is_authoritative_relationship());
    assert!(proved.has_relationship_proof_kind(RelationshipProofKind::ExactReference));
}

#[test]
fn public_relationship_authority_api_never_promotes_confidence_without_proof() {
    let mut edge = GraphEdge {
        edge_type: GraphEdgeType::Calls,
        ..GraphEdge::default()
    };
    edge.evidence.confidence = open_kioku_core::Confidence::Exact;

    assert_eq!(
        edge.relationship_authority(),
        RelationshipAuthority::Heuristic
    );
    assert!(!edge.is_authoritative_relationship());
}
