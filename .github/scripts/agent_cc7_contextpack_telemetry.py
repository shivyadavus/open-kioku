from pathlib import Path
import re

# Core telemetry model: extend the existing selection diagnostics rather than creating a parallel report.
path = Path('crates/open-kioku-core/src/lib.rs')
text = path.read_text()
marker = '''#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextSelectionDiagnostics {'''
insert = '''#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RetrievalSourceCount {
    pub source: RetrievalSourceKind,
    pub selected_file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextSelectionDiagnostics {'''
if text.count(marker) != 1:
    raise SystemExit(f'ContextSelectionDiagnostics marker count={text.count(marker)}')
text = text.replace(marker, insert, 1)

old = '''    pub estimated_tokens_selected: usize,
    #[serde(default)]
    pub selected_units: Vec<ContextSelectedUnit>,'''
new = '''    pub estimated_tokens_selected: usize,
    #[serde(default)]
    pub source_stream_mix: Vec<RetrievalSourceCount>,
    #[serde(default)]
    pub exact_evidence_count: usize,
    #[serde(default)]
    pub ambiguity_unresolved_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_confidence: Option<Confidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstention_reason: Option<String>,
    #[serde(default)]
    pub selected_units: Vec<ContextSelectedUnit>,'''
if text.count(old) != 1:
    raise SystemExit(f'selection fields marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''            estimated_tokens_selected: 0,
            selected_units: Vec::new(),'''
new = '''            estimated_tokens_selected: 0,
            source_stream_mix: Vec::new(),
            exact_evidence_count: 0,
            ambiguity_unresolved_count: 0,
            retrieval_confidence: None,
            abstention_reason: None,
            selected_units: Vec::new(),'''
if text.count(old) != 1:
    raise SystemExit(f'selection default marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)

# Context builder: compute telemetry only from selected results + retained provenance.
path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()
old = '''    HistorySignalQuery, NegativeEvidence, RetrievalAuthority, RetrievalDiagnostics,
    RetrievalSourceKind, RiskReport, RuntimeSignal, ScoreComponent, SearchResult, Symbol,
'''
new = '''    HistorySignalQuery, NegativeEvidence, RetrievalAuthority, RetrievalDiagnostics,
    RetrievalSourceCount, RetrievalSourceKind, RiskReport, RuntimeSignal, ScoreComponent,
    SearchResult, Symbol,
'''
if text.count(old) != 1:
    raise SystemExit(f'import marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        retrieval_diagnostics: open_kioku_core::RetrievalDiagnostics,
    ) -> Result<ContextPack> {'''
new = '''        mut retrieval_diagnostics: open_kioku_core::RetrievalDiagnostics,
    ) -> Result<ContextPack> {'''
if text.count(old) != 1:
    raise SystemExit(f'mutable diagnostics marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        let confidence_summary = confidence_summary(&confidence_breakdown);
        Ok(ContextPack {'''
new = '''        refresh_context_pack_retrieval_telemetry(
            &mut retrieval_diagnostics,
            &primary_files,
            &confidence_breakdown,
        );
        let confidence_summary = confidence_summary(&confidence_breakdown);
        Ok(ContextPack {'''
if text.count(old) != 1:
    raise SystemExit(f'telemetry refresh insertion marker count={text.count(old)}')
text = text.replace(old, new, 1)

marker = '''fn write_markdown_retrieval_diagnostics(out: &mut String, diagnostics: &RetrievalDiagnostics) {'''
helper = r'''fn refresh_context_pack_retrieval_telemetry(
    diagnostics: &mut RetrievalDiagnostics,
    selected: &[SearchResult],
    confidence: &ConfidenceBreakdown,
) {
    let selected_paths = selected
        .iter()
        .map(|result| normalize_path(&result.path))
        .collect::<std::collections::BTreeSet<_>>();
    let mut source_paths = std::collections::BTreeMap::<
        RetrievalSourceKind,
        std::collections::BTreeSet<String>,
    >::new();
    let mut exact_paths = std::collections::BTreeSet::new();

    for trace in &diagnostics.traces {
        let path = normalize_path(&trace.path);
        if !selected_paths.contains(&path) {
            continue;
        }
        if trace.authority == RetrievalAuthority::Exact {
            exact_paths.insert(path.clone());
        }
        for contribution in &trace.contributions {
            source_paths
                .entry(contribution.source)
                .or_default()
                .insert(path.clone());
        }
    }

    diagnostics.selection.source_stream_mix = source_paths
        .into_iter()
        .map(|(source, paths)| RetrievalSourceCount {
            source,
            selected_file_count: paths.len(),
        })
        .collect();
    diagnostics.selection.exact_evidence_count = exact_paths.len();
    diagnostics.selection.ambiguity_unresolved_count = diagnostics
        .caveats
        .iter()
        .filter(|caveat| {
            let caveat = caveat.to_ascii_lowercase();
            caveat.contains("ambiguous") || caveat.contains("unresolved")
        })
        .count();
    // Reuse the already evidence-backed ContextPack confidence enum rather than inventing a new
    // probability calibration. CC6 may later replace this qualitative signal with calibrated
    // abstention metrics without breaking this additive telemetry shape.
    diagnostics.selection.retrieval_confidence = Some(confidence.overall_enum);
    diagnostics.selection.abstention_reason = if selected.is_empty() {
        Some(if diagnostics.selection.budget.max_tokens > 0
            && diagnostics.selection.available_context_tokens == 0
        {
            "context_budget_exhausted".into()
        } else if diagnostics.traces.is_empty() {
            "no_task_relevant_candidates".into()
        } else {
            "no_candidate_fit_context_selection".into()
        })
    } else {
        None
    };
}

fn write_markdown_retrieval_diagnostics(out: &mut String, diagnostics: &RetrievalDiagnostics) {'''
if text.count(marker) != 1:
    raise SystemExit(f'helper marker count={text.count(marker)}')
text = text.replace(marker, helper, 1)

old = '''        if !diagnostics.selection.omitted_high_value.is_empty() {
            out.push_str("- High-value omissions:\n");
            for omission in &diagnostics.selection.omitted_high_value {
                out.push_str(&format!("  - {omission}\n"));
            }
        }
    }
'''
new = '''        if !diagnostics.selection.source_stream_mix.is_empty() {
            let source_mix = diagnostics
                .selection
                .source_stream_mix
                .iter()
                .map(|entry| {
                    format!(
                        "{}={}",
                        retrieval_source_label(entry.source),
                        entry.selected_file_count
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("- Selected source mix: `{source_mix}`\n"));
        }
        out.push_str(&format!(
            "- Exact-evidence selections: `{}`; ambiguity/unresolved signals: `{}`\n",
            diagnostics.selection.exact_evidence_count,
            diagnostics.selection.ambiguity_unresolved_count
        ));
        if let Some(confidence) = diagnostics.selection.retrieval_confidence {
            out.push_str(&format!(
                "- Retrieval confidence: `{:?}` (qualitative ContextPack confidence, not a calibrated probability)\n",
                confidence
            ));
        }
        if let Some(reason) = &diagnostics.selection.abstention_reason {
            out.push_str(&format!("- Abstention reason: `{reason}`\n"));
        }
        if !diagnostics.selection.omitted_high_value.is_empty() {
            out.push_str("- High-value omissions:\n");
            for omission in &diagnostics.selection.omitted_high_value {
                out.push_str(&format!("  - {omission}\n"));
            }
        }
    }
'''
if text.count(old) != 1:
    raise SystemExit(f'markdown telemetry marker count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        for omission in &diagnostics.selection.omitted_high_value {
            out.push_str(&format!("CONTEXT_HIGH_VALUE_OMISSION: {omission}\n"));
        }
    }
'''
new = '''        if !diagnostics.selection.source_stream_mix.is_empty() {
            let source_mix = diagnostics
                .selection
                .source_stream_mix
                .iter()
                .map(|entry| {
                    format!(
                        "{}={}",
                        retrieval_source_label(entry.source),
                        entry.selected_file_count
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            out.push_str(&format!("RETRIEVAL_SELECTED_SOURCE_MIX: {source_mix}\n"));
        }
        out.push_str(&format!(
            "RETRIEVAL_EXACT_EVIDENCE_COUNT: {}\nRETRIEVAL_AMBIGUITY_UNRESOLVED_COUNT: {}\n",
            diagnostics.selection.exact_evidence_count,
            diagnostics.selection.ambiguity_unresolved_count
        ));
        if let Some(confidence) = diagnostics.selection.retrieval_confidence {
            out.push_str(&format!("RETRIEVAL_CONFIDENCE: {:?}\n", confidence));
        }
        if let Some(reason) = &diagnostics.selection.abstention_reason {
            out.push_str(&format!("RETRIEVAL_ABSTENTION_REASON: {reason}\n"));
        }
        for omission in &diagnostics.selection.omitted_high_value {
            out.push_str(&format!("CONTEXT_HIGH_VALUE_OMISSION: {omission}\n"));
        }
    }
'''
if text.count(old) != 1:
    raise SystemExit(f'prompt telemetry marker count={text.count(old)}')
text = text.replace(old, new, 1)

# Add focused adversarial tests next to existing retrieval diagnostics tests.
marker = '''    #[test]
    fn compact_retrieval_diagnostics_surface_sources_and_caveats() {'''
tests = r'''    #[test]
    fn context_pack_telemetry_counts_selected_sources_once_per_file_and_preserves_exact_authority() {
        let selected = vec![SearchResult {
            path: "src/a.rs".into(),
            line_range: None,
            snippet: "fn a() {}".into(),
            symbol: None,
            score: 1.0,
            match_reason: "fixture".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 1.0,
            score_breakdown: Vec::new(),
        }];
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![open_kioku_core::RetrievalTrace {
                path: "src/a.rs".into(),
                fused_score: 1.0,
                authority: RetrievalAuthority::Exact,
                contributions: vec![
                    open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::Lexical,
                        rank: 1,
                        raw_score: Some(1.0),
                        rrf_contribution: 0.1,
                        authority: RetrievalAuthority::Heuristic,
                        symbol_id: None,
                        evidence_refs: Vec::new(),
                    },
                    open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::Lexical,
                        rank: 2,
                        raw_score: Some(0.9),
                        rrf_contribution: 0.09,
                        authority: RetrievalAuthority::Heuristic,
                        symbol_id: None,
                        evidence_refs: Vec::new(),
                    },
                    open_kioku_core::RetrievalContribution {
                        source: RetrievalSourceKind::ExactSemantic,
                        rank: 1,
                        raw_score: Some(1.0),
                        rrf_contribution: 0.1,
                        authority: RetrievalAuthority::Exact,
                        symbol_id: None,
                        evidence_refs: vec!["symbol:a".into()],
                    },
                ],
            }],
            caveats: vec![
                "ambiguous exact symbol anchor".into(),
                "unresolved import reduced graph confidence".into(),
            ],
            selection: open_kioku_core::ContextSelectionDiagnostics {
                budget: ContextBudget::from_file_limit(10),
                available_context_tokens: 1_000,
                estimated_tokens_selected: 100,
                ..Default::default()
            },
            ..Default::default()
        };
        let confidence = ConfidenceBreakdown {
            overall_enum: Confidence::High,
            overall_score: 0.85,
            ..Default::default()
        };

        refresh_context_pack_retrieval_telemetry(&mut diagnostics, &selected, &confidence);

        assert_eq!(diagnostics.selection.exact_evidence_count, 1);
        assert_eq!(diagnostics.selection.ambiguity_unresolved_count, 2);
        assert_eq!(diagnostics.selection.retrieval_confidence, Some(Confidence::High));
        assert_eq!(diagnostics.selection.abstention_reason, None);
        assert_eq!(diagnostics.selection.source_stream_mix.len(), 2);
        assert_eq!(
            diagnostics
                .selection
                .source_stream_mix
                .iter()
                .find(|entry| entry.source == RetrievalSourceKind::Lexical)
                .map(|entry| entry.selected_file_count),
            Some(1)
        );
    }

    #[test]
    fn context_pack_telemetry_abstains_explicitly_when_no_candidate_survives_selection() {
        let mut diagnostics = RetrievalDiagnostics {
            selection: open_kioku_core::ContextSelectionDiagnostics {
                budget: ContextBudget {
                    max_tokens: 100,
                    reserve_for_instructions: 100,
                    ..ContextBudget::from_file_limit(10)
                },
                available_context_tokens: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let confidence = ConfidenceBreakdown::default();

        refresh_context_pack_retrieval_telemetry(&mut diagnostics, &[], &confidence);

        assert_eq!(
            diagnostics.selection.abstention_reason.as_deref(),
            Some("context_budget_exhausted")
        );
        assert_eq!(diagnostics.selection.retrieval_confidence, Some(Confidence::Low));
    }

    #[test]
    fn compact_retrieval_diagnostics_surface_sources_and_caveats() {'''
if text.count(marker) != 1:
    raise SystemExit(f'test insertion marker count={text.count(marker)}')
text = text.replace(marker, tests, 1)
path.write_text(text)
