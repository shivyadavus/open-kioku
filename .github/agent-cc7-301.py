from pathlib import Path

path = Path("crates/open-kioku-context/src/lib.rs")
text = path.read_text()

old = '''    diagnostics.selection.ambiguity_unresolved_count = diagnostics
        .caveats
        .iter()
        .filter(|caveat| {
            let caveat = caveat.to_ascii_lowercase();
            caveat.contains("ambiguous") || caveat.contains("unresolved")
        })
        .count();
'''
new = '''    diagnostics.selection.ambiguity_unresolved_count = diagnostics
        .caveats
        .iter()
        .chain(diagnostics.selection.caveats.iter())
        .filter_map(|caveat| {
            let caveat = caveat.to_ascii_lowercase();
            (caveat.contains("ambiguous") || caveat.contains("unresolved")).then_some(caveat)
        })
        .collect::<std::collections::BTreeSet<_>>()
        .len();
'''
assert text.count(old) == 1, "ambiguity counter marker changed upstream"
text = text.replace(old, new, 1)

old = '''fn write_markdown_retrieval_diagnostics(out: &mut String, diagnostics: &RetrievalDiagnostics) {
    if diagnostics.sources_attempted.is_empty() && diagnostics.caveats.is_empty() {
        return;
    }
'''
new = '''fn write_markdown_retrieval_diagnostics(out: &mut String, diagnostics: &RetrievalDiagnostics) {
    if diagnostics.sources_attempted.is_empty()
        && diagnostics.caveats.is_empty()
        && diagnostics.selection.caveats.is_empty()
    {
        return;
    }
'''
assert text.count(old) == 1, "markdown guard marker changed upstream"
text = text.replace(old, new, 1)

old = '''    if !diagnostics.caveats.is_empty() {
        out.push_str("- Caveats:\\n");
        for caveat in &diagnostics.caveats {
            out.push_str(&format!("  - {caveat}\\n"));
        }
    }
    out.push('\\n');
}

fn write_prompt_retrieval_diagnostics'''
new = '''    if !diagnostics.caveats.is_empty() {
        out.push_str("- Caveats:\\n");
        for caveat in &diagnostics.caveats {
            out.push_str(&format!("  - {caveat}\\n"));
        }
    }
    if !diagnostics.selection.caveats.is_empty() {
        out.push_str("- Selection caveats:\\n");
        for caveat in &diagnostics.selection.caveats {
            out.push_str(&format!("  - {caveat}\\n"));
        }
    }
    out.push('\\n');
}

fn write_prompt_retrieval_diagnostics'''
assert text.count(old) == 1, "markdown caveat marker changed upstream"
text = text.replace(old, new, 1)

old = '''    for caveat in &diagnostics.caveats {
        out.push_str(&format!("RETRIEVAL_CAVEAT: {caveat}\\n"));
    }
}

fn write_markdown_confidence_breakdown'''
new = '''    for caveat in &diagnostics.caveats {
        out.push_str(&format!("RETRIEVAL_CAVEAT: {caveat}\\n"));
    }
    for caveat in &diagnostics.selection.caveats {
        out.push_str(&format!("RETRIEVAL_SELECTION_CAVEAT: {caveat}\\n"));
    }
}

fn write_markdown_confidence_breakdown'''
assert text.count(old) == 1, "prompt caveat marker changed upstream"
text = text.replace(old, new, 1)

old = '''            selection: open_kioku_core::ContextSelectionDiagnostics {
                budget: ContextBudget::from_file_limit(10),
                available_context_tokens: 1_000,
                estimated_tokens_selected: 100,
                ..Default::default()
            },
'''
new = '''            selection: open_kioku_core::ContextSelectionDiagnostics {
                budget: ContextBudget::from_file_limit(10),
                available_context_tokens: 1_000,
                estimated_tokens_selected: 100,
                caveats: vec!["ambiguous exact symbol anchor".into()],
                ..Default::default()
            },
'''
assert text.count(old) == 1, "dedupe fixture marker changed upstream"
text = text.replace(old, new, 1)

old = '''        assert_eq!(diagnostics.selection.exact_evidence_count, 0);
        assert_eq!(diagnostics.selection.unattributed_selected_file_count, 0);
        assert_eq!(diagnostics.selection.source_stream_mix.len(), 1);
'''
new = '''        assert_eq!(diagnostics.selection.exact_evidence_count, 0);
        assert_eq!(diagnostics.selection.unattributed_selected_file_count, 0);
        assert_eq!(diagnostics.selection.ambiguity_unresolved_count, 0);
        assert_eq!(diagnostics.selection.source_stream_mix.len(), 1);
'''
assert text.count(old) == 1, "unique-attribution control marker changed upstream"
text = text.replace(old, new, 1)

old = '''    #[test]
    fn ambiguous_legacy_same_path_traces_fail_closed_for_unit_attribution() {
        let result = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(open_kioku_core::LineRange { start: 1, end: 10 }),
            snippet: "section".into(),
            symbol: None,
            score: 1.0,
            match_reason: "fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let diagnostics = RetrievalDiagnostics {
            traces: vec![
                RetrievalTrace {
                    path: result.path.clone(),
                    unit_key: None,
                    fused_score: 1.0,
                    authority: RetrievalAuthority::Exact,
                    contributions: Vec::new(),
                },
                RetrievalTrace {
                    path: result.path.clone(),
                    unit_key: None,
                    fused_score: 0.5,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: Vec::new(),
                },
            ],
            ..Default::default()
        };
        assert!(retrieval_trace_for_result(&diagnostics, &result).is_none());
        assert_eq!(
            retrieval_authority_for_result(&diagnostics, &result),
            RetrievalAuthority::Heuristic
        );
    }
'''
new = '''    #[test]
    fn ambiguous_legacy_same_path_traces_fail_closed_for_unit_attribution() {
        let result = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(open_kioku_core::LineRange { start: 1, end: 10 }),
            snippet: "section".into(),
            symbol: None,
            score: 1.0,
            match_reason: "fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![
                RetrievalTrace {
                    path: result.path.clone(),
                    unit_key: None,
                    fused_score: 1.0,
                    authority: RetrievalAuthority::Exact,
                    contributions: Vec::new(),
                },
                RetrievalTrace {
                    path: result.path.clone(),
                    unit_key: None,
                    fused_score: 0.5,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: Vec::new(),
                },
            ],
            ..Default::default()
        };
        diagnostics.selection.budget.max_tokens = 1_000;
        diagnostics.selection.available_context_tokens = 900;

        assert!(retrieval_trace_for_result(&diagnostics, &result).is_none());
        assert_eq!(
            retrieval_authority_for_result(&diagnostics, &result),
            RetrievalAuthority::Heuristic
        );

        refresh_context_pack_retrieval_telemetry(
            &mut diagnostics,
            std::slice::from_ref(&result),
            &ConfidenceBreakdown::default(),
        );

        assert_eq!(diagnostics.selection.unattributed_selected_file_count, 1);
        assert_eq!(diagnostics.selection.ambiguity_unresolved_count, 1);
        assert_eq!(diagnostics.selection.exact_evidence_count, 0);
        assert!(diagnostics.selection.source_stream_mix.is_empty());
        assert!(diagnostics
            .selection
            .caveats
            .iter()
            .any(|caveat| caveat.contains("ambiguous or unavailable")));

        let json = serde_json::to_string(&diagnostics).unwrap();
        assert!(json.contains("\\\"ambiguity_unresolved_count\\\":1"));

        let mut markdown = String::new();
        write_markdown_retrieval_diagnostics(&mut markdown, &diagnostics);
        assert!(markdown.contains("ambiguity/unresolved signals: `1`"));
        assert!(markdown.contains("Selection caveats:"));
        assert!(markdown.contains("ambiguous or unavailable"));

        let mut prompt = String::new();
        write_prompt_retrieval_diagnostics(&mut prompt, &diagnostics);
        assert!(prompt.contains("RETRIEVAL_AMBIGUITY_UNRESOLVED_COUNT: 1"));
        assert!(prompt.contains("RETRIEVAL_SELECTION_CAVEAT:"));
        assert!(prompt.contains("ambiguous or unavailable"));
    }
'''
assert text.count(old) == 1, "legacy ambiguity regression marker changed upstream"
text = text.replace(old, new, 1)

path.write_text(text)
