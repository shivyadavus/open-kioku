from pathlib import Path

# Make incomplete source attribution explicit instead of silently presenting a partial source mix.
path = Path('crates/open-kioku-core/src/lib.rs')
text = path.read_text()
old = '''    #[serde(default)]
    pub ambiguity_unresolved_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_confidence: Option<Confidence>,'''
new = '''    #[serde(default)]
    pub ambiguity_unresolved_count: usize,
    #[serde(default)]
    pub unattributed_selected_file_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_confidence: Option<Confidence>,'''
if text.count(old) != 1:
    raise SystemExit(f'core telemetry field marker count={text.count(old)}')
text = text.replace(old, new, 1)
old = '''            ambiguity_unresolved_count: 0,
            retrieval_confidence: None,'''
new = '''            ambiguity_unresolved_count: 0,
            unattributed_selected_file_count: 0,
            retrieval_confidence: None,'''
if text.count(old) != 1:
    raise SystemExit(f'core telemetry default marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)

path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()
old = '''    let mut exact_paths = std::collections::BTreeSet::new();

    for trace in &diagnostics.traces {'''
new = '''    let mut exact_paths = std::collections::BTreeSet::new();
    let mut traced_selected_paths = std::collections::BTreeSet::new();

    for trace in &diagnostics.traces {'''
if text.count(old) != 1:
    raise SystemExit(f'traced path declaration marker count={text.count(old)}')
text = text.replace(old, new, 1)
old = '''        if !selected_paths.contains(&path) {
            continue;
        }
        if trace.authority == RetrievalAuthority::Exact {'''
new = '''        if !selected_paths.contains(&path) {
            continue;
        }
        traced_selected_paths.insert(path.clone());
        if trace.authority == RetrievalAuthority::Exact {'''
if text.count(old) != 1:
    raise SystemExit(f'traced path insertion marker count={text.count(old)}')
text = text.replace(old, new, 1)
old = '''    diagnostics.selection.exact_evidence_count = exact_paths.len();
    diagnostics.selection.ambiguity_unresolved_count = diagnostics'''
new = '''    diagnostics.selection.exact_evidence_count = exact_paths.len();
    diagnostics.selection.unattributed_selected_file_count = selected_paths
        .difference(&traced_selected_paths)
        .count();
    if diagnostics.selection.unattributed_selected_file_count > 0 {
        let caveat = format!(
            "{} selected file(s) lack retrieval-trace source attribution",
            diagnostics.selection.unattributed_selected_file_count
        );
        if !diagnostics.selection.caveats.contains(&caveat) {
            diagnostics.selection.caveats.push(caveat);
        }
    }
    diagnostics.selection.ambiguity_unresolved_count = diagnostics'''
if text.count(old) != 1:
    raise SystemExit(f'unattributed computation marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''            diagnostics.selection.exact_evidence_count,
            diagnostics.selection.ambiguity_unresolved_count
        ));'''
new = '''            diagnostics.selection.exact_evidence_count,
            diagnostics.selection.ambiguity_unresolved_count
        ));
        if diagnostics.selection.unattributed_selected_file_count > 0 {
            out.push_str(&format!(
                "- Selected files without retrieval-trace attribution: `{}`\\n",
                diagnostics.selection.unattributed_selected_file_count
            ));
        }'''
# one markdown occurrence only (prompt has a different literal)
if text.count(old) != 1:
    raise SystemExit(f'markdown unattributed marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''            diagnostics.selection.exact_evidence_count,
            diagnostics.selection.ambiguity_unresolved_count
        ));
        if let Some(confidence) = diagnostics.selection.retrieval_confidence {'''
new = '''            diagnostics.selection.exact_evidence_count,
            diagnostics.selection.ambiguity_unresolved_count
        ));
        if diagnostics.selection.unattributed_selected_file_count > 0 {
            out.push_str(&format!(
                "RETRIEVAL_UNATTRIBUTED_SELECTED_FILE_COUNT: {}\\n",
                diagnostics.selection.unattributed_selected_file_count
            ));
        }
        if let Some(confidence) = diagnostics.selection.retrieval_confidence {'''
# after markdown replacement, this exact block should correspond to prompt only
if text.count(old) != 1:
    raise SystemExit(f'prompt unattributed marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        assert_eq!(diagnostics.selection.source_stream_mix.len(), 2);
        assert_eq!(
            diagnostics'''
new = '''        assert_eq!(diagnostics.selection.source_stream_mix.len(), 2);
        assert_eq!(diagnostics.selection.unattributed_selected_file_count, 0);
        assert_eq!(
            diagnostics'''
if text.count(old) != 1:
    raise SystemExit(f'existing telemetry assertion marker count={text.count(old)}')
text = text.replace(old, new, 1)

marker = '''    #[test]
    fn context_pack_telemetry_abstains_explicitly_when_no_candidate_survives_selection() {'''
test = '''    #[test]
    fn context_pack_telemetry_fails_closed_when_selected_file_lacks_trace_attribution() {
        let selected = vec![SearchResult {
            path: "src/external.rs".into(),
            line_range: None,
            snippet: "fn external() {}".into(),
            symbol: None,
            score: 1.0,
            match_reason: "externally supplied primary".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        }];
        let mut diagnostics = RetrievalDiagnostics::default();
        let confidence = ConfidenceBreakdown::default();

        refresh_context_pack_retrieval_telemetry(&mut diagnostics, &selected, &confidence);

        assert_eq!(diagnostics.selection.unattributed_selected_file_count, 1);
        assert!(diagnostics
            .selection
            .caveats
            .iter()
            .any(|caveat| caveat.contains("lack retrieval-trace source attribution")));
        assert!(diagnostics.selection.source_stream_mix.is_empty());
    }

    #[test]
    fn context_pack_telemetry_abstains_explicitly_when_no_candidate_survives_selection() {'''
if text.count(marker) != 1:
    raise SystemExit(f'unattributed test insertion marker count={text.count(marker)}')
text = text.replace(marker, test, 1)
path.write_text(text)
