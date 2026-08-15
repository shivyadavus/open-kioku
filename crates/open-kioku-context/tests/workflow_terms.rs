use open_kioku_context::expanded_task_search_terms;

#[test]
fn verification_workflow_language_expands_to_code_vocabulary() {
    let terms = expanded_task_search_terms("verify changed files against saved plans");

    assert!(terms.iter().any(|term| term == "verification"));
    assert!(terms.iter().any(|term| term == "change"));
    assert!(terms.iter().any(|term| term == "plan"));
    assert!(terms.iter().any(|term| term == "verification change"));
    assert!(terms.iter().any(|term| term == "saved plan"));
}
