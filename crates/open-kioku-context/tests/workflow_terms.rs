use open_kioku_context::expanded_task_search_terms;

// This natural-language task previously missed the patch crate after the V2 index expanded
// the candidate pool; normalize workflow vocabulary so retrieval remains stable as the index grows.
#[test]
fn verification_workflow_language_expands_to_code_vocabulary() {
    let terms = expanded_task_search_terms("verify changed files against saved plans");

    assert!(terms.iter().any(|term| term == "verification"));
    assert!(terms.iter().any(|term| term == "change"));
    assert!(terms.iter().any(|term| term == "plan"));
    assert!(terms.iter().any(|term| term == "verification change"));
    assert!(terms.iter().any(|term| term == "saved plan"));
}
