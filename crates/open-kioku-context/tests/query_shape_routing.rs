use open_kioku_context::routing::classify_task;
use open_kioku_core::{QueryShape, RetrievalSourceKind};

#[test]
fn ambiguous_single_word_query_does_not_claim_exact_identity() {
    let route = classify_task("authentication");

    assert_eq!(route.query_shape, QueryShape::Unknown);
    assert!(route.query_shape_fallback_reason.is_some());
    assert_eq!(
        route
            .policy
            .candidate_cap(RetrievalSourceKind::ExactSemantic, 10),
        route
            .policy
            .candidate_cap(RetrievalSourceKind::SemanticVector, 10),
        "an ambiguous plain word must preserve the conservative general policy"
    );
}

#[test]
fn punctuated_identifier_in_prose_keeps_structured_signal() {
    let route = classify_task("explain ContextPackBuilder. selection behavior");

    assert_eq!(
        route.query_shape,
        QueryShape::MixedStructuredNaturalLanguage
    );
    assert!(route
        .query_shape_signals
        .iter()
        .any(|signal| signal.contains("identifier-shaped code reference")));
}

#[test]
fn query_shape_never_removes_parent_required_evidence() {
    let route = classify_task(
        "find regression tests for ContextPackBuilder in crates/open-kioku-context/src/lib.rs",
    );

    assert_eq!(
        route.policy.required_evidence,
        vec![RetrievalSourceKind::Validation]
    );
    assert!(route.policy.missing_required_evidence_is_blocker);
}
