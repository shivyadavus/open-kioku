from pathlib import Path

path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()

old = '''struct TaskSearchIntent {
    primary_anchors: Vec<String>,
    reference_anchors: Vec<String>,
    ticket_anchors: Vec<String>,
    path_anchors: Vec<String>,
    lexical_anchors: Vec<String>,
}

impl TaskSearchIntent {
    fn parse(task: &str) -> Self {
        let mut intent = Self::default();
'''
new = '''struct TaskSearchIntent {
    primary_anchors: Vec<String>,
    reference_anchors: Vec<String>,
    ticket_anchors: Vec<String>,
    path_anchors: Vec<String>,
    lexical_anchors: Vec<String>,
    documentation_target: bool,
}

impl TaskSearchIntent {
    fn parse(task: &str) -> Self {
        let mut intent = Self {
            documentation_target: task_targets_documentation(task),
            ..Self::default()
        };
'''
if text.count(old) != 1:
    raise SystemExit(f'task intent marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        task_relevance_tier(&b_haystack, intent)
            .cmp(&task_relevance_tier(&a_haystack, intent))
'''
new = '''        task_relevance_tier(&b.path, &b_haystack, intent)
            .cmp(&task_relevance_tier(&a.path, &a_haystack, intent))
'''
if text.count(old) != 1:
    raise SystemExit(f'task relevance sort marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''fn task_relevance_tier(haystack: &str, intent: &TaskSearchIntent) -> u8 {
    if intent
        .primary_anchors
        .iter()
        .any(|anchor| contains_anchor(haystack, anchor))
    {
        3
    } else if intent
        .ticket_anchors
        .iter()
        .chain(intent.path_anchors.iter())
        .any(|anchor| contains_anchor(haystack, anchor))
    {
        2
    } else if intent
        .reference_anchors
        .iter()
        .any(|anchor| contains_anchor(haystack, anchor))
    {
        1
    } else {
        0
    }
}
'''
new = '''fn task_relevance_tier(
    path: &std::path::Path,
    haystack: &str,
    intent: &TaskSearchIntent,
) -> u8 {
    if intent
        .primary_anchors
        .iter()
        .any(|anchor| contains_anchor(haystack, anchor))
    {
        3
    } else if intent
        .ticket_anchors
        .iter()
        .chain(intent.path_anchors.iter())
        .any(|anchor| contains_anchor(haystack, anchor))
        || (intent.documentation_target && is_documentation_path(&normalize_path(path)))
    {
        2
    } else if intent
        .reference_anchors
        .iter()
        .any(|anchor| contains_anchor(haystack, anchor))
    {
        1
    } else {
        0
    }
}

fn task_targets_documentation(task: &str) -> bool {
    task.split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .any(|token| {
            matches!(
                token.as_str(),
                "document" | "documentation" | "docs" | "readme" | "guide" | "guides"
            )
        })
}

fn is_documentation_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    path.starts_with("docs/")
        || path.contains("/docs/")
        || path.ends_with("readme.md")
        || path.ends_with(".md")
        || path.ends_with(".mdx")
}
'''
if text.count(old) != 1:
    raise SystemExit(f'task relevance function marker count={text.count(old)}')
text = text.replace(old, new, 1)

marker = '''    #[test]
    fn compact_retrieval_diagnostics_surface_sources_and_caveats() {'''
tests = '''    #[test]
    fn documentation_task_target_precedes_unrelated_exact_code_authority() {
        let docs = SearchResult {
            path: "docs/guides/agent-workflows.md".into(),
            line_range: None,
            snippet: "Agent Workflows for contributors".into(),
            symbol: None,
            score: 0.01,
            match_reason: "documentation target fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let code = SearchResult {
            path: "src/ContributorEngine.rs".into(),
            line_range: None,
            snippet: "struct ContributorEngine;".into(),
            symbol: None,
            score: 10.0,
            match_reason: "exact code fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let diagnostics = RetrievalDiagnostics {
            traces: vec![
                open_kioku_core::RetrievalTrace {
                    path: docs.path.clone(),
                    fused_score: docs.score,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: Vec::new(),
                },
                open_kioku_core::RetrievalTrace {
                    path: code.path.clone(),
                    fused_score: code.score,
                    authority: RetrievalAuthority::Exact,
                    contributions: Vec::new(),
                },
            ],
            ..Default::default()
        };
        let intent = TaskSearchIntent::parse("document agent workflows for contributors");
        assert!(intent.documentation_target);
        let ranked = rerank_fused_for_task(vec![code, docs], &intent, &diagnostics);
        assert_eq!(ranked[0].path, Path::new("docs/guides/agent-workflows.md"));
    }

    #[test]
    fn non_documentation_task_does_not_promote_docs_over_exact_code() {
        let docs = SearchResult {
            path: "docs/guides/engine.md".into(),
            line_range: None,
            snippet: "Engine internals".into(),
            symbol: None,
            score: 10.0,
            match_reason: "docs fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let code = SearchResult {
            path: "src/engine.rs".into(),
            line_range: None,
            snippet: "fn engine() {}".into(),
            symbol: None,
            score: 0.01,
            match_reason: "code fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let diagnostics = RetrievalDiagnostics {
            traces: vec![
                open_kioku_core::RetrievalTrace {
                    path: docs.path.clone(),
                    fused_score: docs.score,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: Vec::new(),
                },
                open_kioku_core::RetrievalTrace {
                    path: code.path.clone(),
                    fused_score: code.score,
                    authority: RetrievalAuthority::Exact,
                    contributions: Vec::new(),
                },
            ],
            ..Default::default()
        };
        let intent = TaskSearchIntent::parse("change engine behavior");
        assert!(!intent.documentation_target);
        let ranked = rerank_fused_for_task(vec![docs, code], &intent, &diagnostics);
        assert_eq!(ranked[0].path, Path::new("src/engine.rs"));
    }

    #[test]
    fn compact_retrieval_diagnostics_surface_sources_and_caveats() {'''
if text.count(marker) != 1:
    raise SystemExit(f'test insertion marker count={text.count(marker)}')
text = text.replace(marker, tests, 1)

path.write_text(text)
