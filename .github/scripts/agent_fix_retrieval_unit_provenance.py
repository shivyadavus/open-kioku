from pathlib import Path
import re

# Cross-layer hardening for #225: preserve retrieval-unit identity through fusion, selection,
# reranking, and telemetry. The product patch is intentionally narrow: no ranking weights or
# benchmark thresholds change.

core = Path('crates/open-kioku-core/src/lib.rs')
text = core.read_text()

# Add a stable, serializable unit identity next to retrieval tracing. Path normalization mirrors
# existing repo-relative normalization and range/symbol identity prevents authority bleed between
# independent sections of one file.
marker = '''#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]\npub struct RetrievalTrace {'''
if text.count(marker) != 1:
    raise SystemExit(f'RetrievalTrace marker count={text.count(marker)}')
unit_type = '''#[derive(\n    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,\n)]\npub struct RetrievalUnitKey {\n    pub path: String,\n    #[serde(default)]\n    pub line_range: Option<LineRange>,\n    #[serde(default)]\n    pub symbol_id: Option<SymbolId>,\n}\n\nimpl RetrievalUnitKey {\n    pub fn from_result(result: &SearchResult) -> Self {\n        Self::from_parts(\n            &result.path,\n            result.line_range.as_ref(),\n            result.symbol.as_ref().map(|symbol| &symbol.id),\n        )\n    }\n\n    pub fn from_parts(\n        path: &Path,\n        line_range: Option<&LineRange>,\n        symbol_id: Option<&SymbolId>,\n    ) -> Self {\n        Self {\n            path: path\n                .to_string_lossy()\n                .replace('\\\\', "/")\n                .trim_start_matches("./")\n                .to_string(),\n            line_range: line_range.cloned(),\n            symbol_id: symbol_id.cloned(),\n        }\n    }\n}\n\n#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]\npub struct RetrievalTrace {'''
text = text.replace(marker, unit_type, 1)

# Additive/serde-compatible trace field. New traces always populate this. Legacy serialized traces
# may omit it and are only allowed a path fallback when that path is unambiguous.
pattern = re.compile(r'(pub struct RetrievalTrace \{\n\s*pub path: PathBuf,\n)')
text, n = pattern.subn(r'\1    #[serde(default, skip_serializing_if = "Option::is_none")]\n    pub unit_key: Option<RetrievalUnitKey>,\n', text, count=1)
if n != 1:
    raise SystemExit(f'RetrievalTrace field insertion count={n}')
core.write_text(text)

cand = Path('crates/open-kioku-context/src/candidates.rs')
text = cand.read_text()
# Import unit identity.
old = '''    RetrievalAuthority, RetrievalContribution, RetrievalDiagnostics, RetrievalSourceKind,\n    RetrievalTrace, ScoreComponent, SearchResult,\n'''
new = '''    RetrievalAuthority, RetrievalContribution, RetrievalDiagnostics, RetrievalSourceKind,\n    RetrievalTrace, RetrievalUnitKey, ScoreComponent, SearchResult,\n'''
if text.count(old) != 1:
    raise SystemExit(f'candidate import marker count={text.count(old)}')
text = text.replace(old, new, 1)

# Every production trace receives the exact representative unit identity.
old = '''        traces.push(RetrievalTrace {\n            path: entry.representative.path.clone(),\n            fused_score: entry.fused_score,\n'''
new = '''        traces.push(RetrievalTrace {\n            path: entry.representative.path.clone(),\n            unit_key: Some(RetrievalUnitKey::from_result(&entry.representative)),\n            fused_score: entry.fused_score,\n'''
if text.count(old) != 1:
    raise SystemExit(f'production trace marker count={text.count(old)}')
text = text.replace(old, new, 1)

# Test-only trace literals in this module must be explicit about legacy/path-only fixtures.
text = re.sub(
    r'(RetrievalTrace \{\n\s*path: [^\n]+,\n)(\s*)(?!unit_key:)',
    r'\1\2unit_key: None,\n\2',
    text,
)
cand.write_text(text)

ctx = Path('crates/open-kioku-context/src/lib.rs')
text = ctx.read_text()
old = '''    HistorySignalQuery, NegativeEvidence, RetrievalAuthority, RetrievalDiagnostics,\n    RetrievalSourceCount, RetrievalSourceKind, RiskReport, RuntimeSignal, ScoreComponent,\n'''
new = '''    HistorySignalQuery, NegativeEvidence, RetrievalAuthority, RetrievalDiagnostics, RetrievalTrace,\n    RetrievalSourceCount, RetrievalSourceKind, RetrievalUnitKey, RiskReport, RuntimeSignal,\n    ScoreComponent,\n'''
if text.count(old) != 1:
    raise SystemExit(f'context import marker count={text.count(old)}')
text = text.replace(old, new, 1)

# Replace file-level telemetry attribution with selected-unit attribution. Source mix remains the
# backwards-compatible selected-file count per source, but it is now derived only from each selected
# unit's own trace. Exact count is a unit count, not a path count.
start = text.index('fn refresh_context_pack_retrieval_telemetry(')
end = text.index('\nfn write_markdown_retrieval_diagnostics', start)
old_block = text[start:end]
new_block = '''fn retrieval_trace_for_result<'a>(\n    diagnostics: &'a RetrievalDiagnostics,\n    result: &SearchResult,\n) -> Option<&'a RetrievalTrace> {\n    let expected = RetrievalUnitKey::from_result(result);\n    if let Some(trace) = diagnostics\n        .traces\n        .iter()\n        .find(|trace| trace.unit_key.as_ref() == Some(&expected))\n    {\n        return Some(trace);\n    }\n\n    // Backward compatibility for serialized diagnostics created before unit identities existed:\n    // path-only fallback is safe only when exactly one legacy trace exists for that path. If two\n    // sections share a path, fail closed rather than borrowing authority from an arbitrary section.\n    let path = normalize_path(&result.path);\n    let mut legacy = diagnostics.traces.iter().filter(|trace| {\n        trace.unit_key.is_none() && normalize_path(&trace.path) == path\n    });\n    let first = legacy.next()?;\n    if legacy.next().is_none() {\n        Some(first)\n    } else {\n        None\n    }\n}\n\nfn refresh_context_pack_retrieval_telemetry(\n    diagnostics: &mut RetrievalDiagnostics,\n    selected: &[SearchResult],\n    confidence: &ConfidenceBreakdown,\n) {\n    let selected_units = selected\n        .iter()\n        .map(RetrievalUnitKey::from_result)\n        .collect::<std::collections::BTreeSet<_>>();\n    let mut source_paths =\n        std::collections::BTreeMap::<RetrievalSourceKind, std::collections::BTreeSet<String>>::new(\n        );\n    let mut exact_units = std::collections::BTreeSet::new();\n    let mut traced_selected_units = std::collections::BTreeSet::new();\n\n    for result in selected {\n        let unit = RetrievalUnitKey::from_result(result);\n        let Some(trace) = retrieval_trace_for_result(diagnostics, result) else {\n            continue;\n        };\n        traced_selected_units.insert(unit.clone());\n        if trace.authority == RetrievalAuthority::Exact {\n            exact_units.insert(unit);\n        }\n        let path = normalize_path(&result.path);\n        for contribution in &trace.contributions {\n            source_paths\n                .entry(contribution.source)\n                .or_default()\n                .insert(path.clone());\n        }\n    }\n\n    diagnostics.selection.source_stream_mix = source_paths\n        .into_iter()\n        .map(|(source, paths)| RetrievalSourceCount {\n            source,\n            selected_file_count: paths.len(),\n        })\n        .collect();\n    diagnostics.selection.exact_evidence_count = exact_units.len();\n    diagnostics.selection.unattributed_selected_file_count =\n        selected_units.difference(&traced_selected_units).count();\n    if diagnostics.selection.unattributed_selected_file_count > 0 {\n        let caveat = format!(\n            "{} selected retrieval unit(s) lack unambiguous retrieval-trace source attribution",\n            diagnostics.selection.unattributed_selected_file_count\n        );\n        if !diagnostics.selection.caveats.contains(&caveat) {\n            diagnostics.selection.caveats.push(caveat);\n        }\n    }\n    diagnostics.selection.ambiguity_unresolved_count = diagnostics\n        .caveats\n        .iter()\n        .filter(|caveat| {\n            let caveat = caveat.to_ascii_lowercase();\n            caveat.contains("ambiguous") || caveat.contains("unresolved")\n        })\n        .count();\n    diagnostics.selection.retrieval_confidence = Some(confidence.overall_enum);\n    diagnostics.selection.abstention_reason = if selected.is_empty() {\n        Some(\n            if diagnostics.selection.budget.max_tokens > 0\n                && diagnostics.selection.available_context_tokens == 0\n            {\n                "context_budget_exhausted".into()\n            } else if diagnostics.traces.is_empty() {\n                "no_task_relevant_candidates".into()\n            } else {\n                "no_candidate_fit_context_selection".into()\n            },\n        )\n    } else {\n        None\n    };\n}\n'''
text = text[:start] + new_block + text[end:]

# Authority and source helpers must resolve the exact selected unit rather than the first trace for
# a path.
text = re.sub(
    r'''fn retrieval_authority_for_result\(\n    diagnostics: &RetrievalDiagnostics,\n    result: &SearchResult,\n\) -> RetrievalAuthority \{.*?\n\}\n\nfn retrieval_sources_for_result''',
    '''fn retrieval_authority_for_result(\n    diagnostics: &RetrievalDiagnostics,\n    result: &SearchResult,\n) -> RetrievalAuthority {\n    retrieval_trace_for_result(diagnostics, result)\n        .map(|trace| trace.authority)\n        .unwrap_or(RetrievalAuthority::Heuristic)\n}\n\nfn retrieval_sources_for_result''',
    text,
    count=1,
    flags=re.S,
)
text = re.sub(
    r'''fn retrieval_sources_for_result\(\n    diagnostics: &RetrievalDiagnostics,\n    result: &SearchResult,\n\) -> std::collections::BTreeSet<RetrievalSourceKind> \{.*?\n\}\n''',
    '''fn retrieval_sources_for_result(\n    diagnostics: &RetrievalDiagnostics,\n    result: &SearchResult,\n) -> std::collections::BTreeSet<RetrievalSourceKind> {\n    retrieval_trace_for_result(diagnostics, result)\n        .map(|trace| {\n            trace\n                .contributions\n                .iter()\n                .map(|contribution| contribution.source)\n                .collect()\n        })\n        .unwrap_or_default()\n}\n''',
    text,
    count=1,
    flags=re.S,
)

# Unit-aware authority tie-break in post-fusion reranking.
old = '''    let authority_by_path = diagnostics\n        .traces\n        .iter()\n        .map(|trace| (normalize_path(&trace.path), trace.authority))\n        .collect::<std::collections::BTreeMap<_, _>>();\n    results.sort_by(|a, b| {'''
new = '''    results.sort_by(|a, b| {'''
if text.count(old) != 1:
    raise SystemExit(f'authority map marker count={text.count(old)}')
text = text.replace(old, new, 1)
old = '''                authority_by_path\n                    .get(&normalize_path(&b.path))\n                    .copied()\n                    .unwrap_or(RetrievalAuthority::Heuristic)\n                    .cmp(\n                        &authority_by_path\n                            .get(&normalize_path(&a.path))\n                            .copied()\n                            .unwrap_or(RetrievalAuthority::Heuristic),\n                    )'''
new = '''                retrieval_authority_for_result(diagnostics, b)\n                    .cmp(&retrieval_authority_for_result(diagnostics, a))'''
if text.count(old) != 1:
    raise SystemExit(f'authority comparator marker count={text.count(old)}')
text = text.replace(old, new, 1)

# Make existing Context-module trace fixtures compile as explicit legacy fixtures.
text = re.sub(
    r'(RetrievalTrace \{\n\s*path: [^\n]+,\n)(\s*)(?!unit_key:)',
    r'\1\2unit_key: None,\n\2',
    text,
)

# Adversarial regression: same path, two ranges, exact authority/source on one must never bleed to
# the selected heuristic document section. Also verify exact/source telemetry reflects only the
# selected unit.
test_marker = '''    #[test]\n    fn compact_retrieval_diagnostics_surface_sources_and_caveats() {'''
if text.count(test_marker) != 1:
    raise SystemExit(f'test insertion marker count={text.count(test_marker)}')
regression = r'''    #[test]
    fn retrieval_unit_provenance_does_not_bleed_between_sections_of_same_file() {
        let heuristic = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(open_kioku_core::LineRange { start: 1, end: 10 }),
            snippet: "heuristic section".into(),
            symbol: None,
            score: 1.0,
            match_reason: "fixture".into(),
            evidence: vec!["document section".into()],
            evidence_refs: vec!["doc:section:one".into()],
            confidence: 0.6,
            score_breakdown: Vec::new(),
        };
        let exact = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(open_kioku_core::LineRange { start: 20, end: 30 }),
            snippet: "other exact section".into(),
            symbol: None,
            score: 2.0,
            match_reason: "fixture".into(),
            evidence: vec!["exact fixture".into()],
            evidence_refs: vec!["symbol:exact-other-section".into()],
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let heuristic_key = RetrievalUnitKey::from_result(&heuristic);
        let exact_key = RetrievalUnitKey::from_result(&exact);
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![
                RetrievalTrace {
                    path: heuristic.path.clone(),
                    unit_key: Some(heuristic_key),
                    fused_score: 1.0,
                    authority: RetrievalAuthority::Heuristic,
                    contributions: vec![open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::Document,
                        rank: 1,
                        raw_score: Some(1.0),
                        rrf_contribution: 1.0,
                        authority: RetrievalAuthority::Heuristic,
                        symbol_id: None,
                        evidence_refs: heuristic.evidence_refs.clone(),
                        rationale: "document section".into(),
                    }],
                },
                RetrievalTrace {
                    path: exact.path.clone(),
                    unit_key: Some(exact_key),
                    fused_score: 2.0,
                    authority: RetrievalAuthority::Exact,
                    contributions: vec![open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::ExactSemantic,
                        rank: 1,
                        raw_score: Some(2.0),
                        rrf_contribution: 2.0,
                        authority: RetrievalAuthority::Exact,
                        symbol_id: None,
                        evidence_refs: exact.evidence_refs.clone(),
                        rationale: "exact other section".into(),
                    }],
                },
            ],
            ..Default::default()
        };

        assert_eq!(
            retrieval_authority_for_result(&diagnostics, &heuristic),
            RetrievalAuthority::Heuristic
        );
        assert_eq!(
            retrieval_sources_for_result(&diagnostics, &heuristic),
            std::collections::BTreeSet::from([RetrievalSourceKind::Document])
        );

        diagnostics.selection.budget.max_tokens = 100;
        diagnostics.selection.available_context_tokens = 100;
        refresh_context_pack_retrieval_telemetry(
            &mut diagnostics,
            std::slice::from_ref(&heuristic),
            &ConfidenceBreakdown::default(),
        );
        assert_eq!(diagnostics.selection.exact_evidence_count, 0);
        assert_eq!(diagnostics.selection.unattributed_selected_file_count, 0);
        assert_eq!(diagnostics.selection.source_stream_mix.len(), 1);
        assert_eq!(
            diagnostics.selection.source_stream_mix[0].source,
            RetrievalSourceKind::Document
        );
    }

    #[test]
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
text = text.replace(test_marker, regression + test_marker, 1)
ctx.write_text(text)
