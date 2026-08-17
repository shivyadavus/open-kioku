use open_kioku_context::routing::classify_task;
use open_kioku_core::{QueryShape, RetrievalSourceKind, TaskFamily};

#[test]
fn query_shape_conformance_covers_structured_conceptual_and_mixed_queries() {
    let cases = [
        ("AuthService", QueryShape::ExactIdentifier),
        ("billing::invoice::finalize", QueryShape::QualifiedSymbol),
        ("crates/open-kioku-context/src/routing.rs", QueryShape::PathReference),
        ("Traceback: panic in AuthService.issueToken", QueryShape::ErrorTrace),
        ("/api/invoices/{id}", QueryShape::ApiResource),
        (
            "how authentication tokens are persisted after login",
            QueryShape::Conceptual,
        ),
        (
            "change AuthService.issueToken to persist expiry metadata",
            QueryShape::MixedStructuredNaturalLanguage,
        ),
    ];

    for (query, expected) in cases {
        let decision = classify_task(query);
        assert_eq!(decision.query_shape, expected, "query: {query}");
        assert!(
            decision.query_shape_confidence > 0.0,
            "classification confidence must be explicit for {query}"
        );
        assert!(
            !decision.query_shape_signals.is_empty(),
            "classification signals must explain {query}"
        );
    }
}

#[test]
fn plain_natural_language_words_do_not_masquerade_as_exact_identifiers() {
    for query in ["authentication", "repository", "persistence", "validation"] {
        let decision = classify_task(query);
        assert_ne!(
            decision.query_shape,
            QueryShape::ExactIdentifier,
            "plain word was promoted to exact identifier: {query}"
        );
    }
}

#[test]
fn punctuation_does_not_hide_identifier_or_path_signals() {
    let symbol = classify_task("fix `AuthService.issueToken`, please");
    assert_eq!(
        symbol.query_shape,
        QueryShape::MixedStructuredNaturalLanguage
    );
    assert!(symbol
        .query_shape_signals
        .iter()
        .any(|signal| signal.contains("qualified symbol")));

    let path = classify_task("inspect (crates/open-kioku-context/src/routing.rs) before editing");
    assert_eq!(path.query_shape, QueryShape::MixedStructuredNaturalLanguage);
    assert!(path
        .query_shape_signals
        .iter()
        .any(|signal| signal.contains("path")));
}

#[test]
fn trace_shape_wins_without_erasing_other_structured_signal_ambiguity() {
    let decision = classify_task(
        "Traceback: exception in crates/open-kioku-context/src/routing.rs at AuthService.issueToken",
    );

    assert_eq!(decision.query_shape, QueryShape::ErrorTrace);
    assert!(
        !decision.query_shape_ambiguities.is_empty(),
        "trace plus path/symbol structure must remain visible as ambiguity metadata"
    );
    assert!(decision
        .query_shape_signals
        .iter()
        .any(|signal| signal.contains("stack-trace")));
    assert!(decision
        .query_shape_signals
        .iter()
        .any(|signal| signal.contains("path")));
}

#[test]
fn query_shape_refinement_never_weakens_task_family_required_evidence() {
    let exact = classify_task("change AuthService callers and identify affected dependents");
    let conceptual = classify_task(
        "what callers and dependents are affected by changing authentication behavior",
    );

    assert_eq!(exact.family, TaskFamily::EditToRipple);
    assert_eq!(conceptual.family, TaskFamily::EditToRipple);

    for decision in [&exact, &conceptual] {
        assert!(decision.policy.missing_required_evidence_is_blocker);
        assert!(decision
            .policy
            .required_evidence
            .contains(&RetrievalSourceKind::ExactSemantic));
        assert!(decision
            .policy
            .required_evidence
            .contains(&RetrievalSourceKind::Graph));
    }
}

#[test]
fn query_shape_refinement_cannot_enable_sources_forbidden_by_task_family() {
    let decision = classify_task("document AuthService method in README.md");

    assert_eq!(decision.family, TaskFamily::MixedCodeDocs);
    assert_eq!(
        decision.query_shape,
        QueryShape::MixedStructuredNaturalLanguage
    );
    assert!(decision
        .policy
        .enabled_sources
        .contains(&RetrievalSourceKind::Document));

    let docs_only = classify_task("document README.md");
    assert_eq!(docs_only.family, TaskFamily::Documentation);
    assert!(!docs_only
        .policy
        .enabled_sources
        .contains(&RetrievalSourceKind::Runtime));
    assert!(!docs_only
        .policy
        .enabled_sources
        .contains(&RetrievalSourceKind::SemanticVector));
}

#[test]
fn unknown_shape_preserves_conservative_fallback_metadata() {
    let decision = classify_task("x");

    assert_eq!(decision.query_shape, QueryShape::Unknown);
    assert!(decision.query_shape_fallback_reason.is_some());
    assert!(decision.query_shape_signals.is_empty());
}
