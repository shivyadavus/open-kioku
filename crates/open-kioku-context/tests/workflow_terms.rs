use open_kioku_context::expanded_task_search_terms;

// Natural-language anchors should broaden candidate discovery without introducing a
// benchmark-specific synonym table, promoting grammatical stopwords, or exploding query fan-out.
#[test]
fn natural_language_workflow_terms_expand_without_case_specific_aliases() {
    let terms = expanded_task_search_terms("verify changed files against saved plans");

    assert!(terms.iter().any(|term| term == "verify"));
    assert!(terms.iter().any(|term| term == "changed"));
    assert!(terms.iter().any(|term| term == "files"));
    assert!(terms.iter().any(|term| term == "saved"));
    assert!(terms.iter().any(|term| term == "plans"));
    assert!(terms.iter().any(|term| term == "verify changed"));
    assert!(!terms.iter().any(|term| term == "against"));
    assert!(
        terms.len() <= 16,
        "natural-language expansion must stay bounded"
    );
}
