from pathlib import Path

# Strengthen the public selection model: max_per_file is a unit cap, selected-unit provenance is
# explicit, and an untouched/default diagnostic must not claim that an 8k budget was applied.
path = Path('crates/open-kioku-core/src/lib.rs')
text = path.read_text()
text = text.replace('''            max_per_file: 2_500,
            max_primary_files: 8,
''', '''            max_per_file: 2,
            max_primary_files: 8,
''', 1)
old = '''#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct ContextSelectionDiagnostics {
    pub budget: ContextBudget,
    pub available_context_tokens: usize,
    pub estimated_tokens_selected: usize,
    #[serde(default)]
    pub per_file_tokens: BTreeMap<PathBuf, usize>,
    #[serde(default)]
    pub omitted_due_to_budget: Vec<String>,
    #[serde(default)]
    pub omitted_high_value: Vec<String>,
    #[serde(default)]
    pub redundancy_omissions: Vec<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
}
'''
new = '''#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextSelectedUnit {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<LineRange>,
    pub estimated_tokens: usize,
    pub authority: RetrievalAuthority,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextSelectionDiagnostics {
    pub budget: ContextBudget,
    pub available_context_tokens: usize,
    pub estimated_tokens_selected: usize,
    #[serde(default)]
    pub selected_units: Vec<ContextSelectedUnit>,
    #[serde(default)]
    pub per_file_tokens: BTreeMap<PathBuf, usize>,
    #[serde(default)]
    pub omitted_due_to_budget: Vec<String>,
    #[serde(default)]
    pub omitted_due_to_caps: Vec<String>,
    #[serde(default)]
    pub omitted_high_value: Vec<String>,
    #[serde(default)]
    pub redundancy_omissions: Vec<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
}

impl Default for ContextSelectionDiagnostics {
    fn default() -> Self {
        Self {
            budget: ContextBudget {
                max_tokens: 0,
                reserve_for_instructions: 0,
                reserve_for_validation: 0,
                max_per_file: 0,
                max_primary_files: 0,
            },
            available_context_tokens: 0,
            estimated_tokens_selected: 0,
            selected_units: Vec::new(),
            per_file_tokens: BTreeMap::new(),
            omitted_due_to_budget: Vec::new(),
            omitted_due_to_caps: Vec::new(),
            omitted_high_value: Vec::new(),
            redundancy_omissions: Vec::new(),
            caveats: Vec::new(),
        }
    }
}
'''
if text.count(old) != 1:
    raise SystemExit(f'selection diagnostics model marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)

path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()
text = text.replace(
    '    ConfidenceSignalInput, ContextBudget, ContextPack, Evidence, EvidenceId, EvidenceSourceType, File, FileRange,\n',
    '    ConfidenceSignalInput, ContextBudget, ContextPack, ContextSelectedUnit, Evidence, EvidenceId, EvidenceSourceType, File, FileRange,\n',
    1,
)

start = text.index('fn select_context_units(\n')
end = text.index('fn estimate_search_result_tokens(', start)
selector = r'''fn select_context_units(
    ranked: Vec<SearchResult>,
    budget: &ContextBudget,
    diagnostics: &mut RetrievalDiagnostics,
) -> Vec<SearchResult> {
    let available = budget.available_context_tokens();
    diagnostics.selection = Default::default();
    diagnostics.selection.budget = *budget;
    diagnostics.selection.available_context_tokens = available;

    if budget.max_primary_files == 0 || available == 0 {
        diagnostics.selection.omitted_due_to_budget.extend(
            ranked
                .iter()
                .map(|result| format!("{}: no context budget available", result.path.display())),
        );
        return Vec::new();
    }

    // File-count callers historically select the reranked prefix. Preserve that behavior exactly;
    // the compatibility budget only routes the old API through the new accounting model.
    if is_file_limit_compatibility_budget(budget) {
        let selected = ranked
            .into_iter()
            .take(budget.max_primary_files)
            .collect::<Vec<_>>();
        record_selected_units(&selected, diagnostics);
        return selected;
    }

    let mut selected_indices = std::collections::BTreeSet::new();
    let mut terminally_rejected = std::collections::BTreeSet::new();
    let mut selected_token_sets = Vec::<std::collections::BTreeSet<String>>::new();
    let mut selected_sources = std::collections::BTreeSet::<RetrievalSourceKind>::new();
    let mut selected_tokens = 0usize;
    let mut per_file_units = std::collections::BTreeMap::<std::path::PathBuf, usize>::new();

    while selected_indices.len() < budget.max_primary_files {
        let remaining_tokens = available.saturating_sub(selected_tokens);
        let mut best: Option<(usize, f32, usize, std::collections::BTreeSet<String>)> = None;

        for (index, result) in ranked.iter().enumerate() {
            if selected_indices.contains(&index) || terminally_rejected.contains(&index) {
                continue;
            }
            let authority = retrieval_authority_for_result(diagnostics, result);
            let sources = retrieval_sources_for_result(diagnostics, result);
            let high_value = is_high_value_context(authority, &sources);
            let tokens = estimate_search_result_tokens(result);
            let units_for_file = per_file_units.get(&result.path).copied().unwrap_or_default();

            if units_for_file >= budget.max_per_file {
                let message = format!(
                    "{}: per-file context unit cap {} reached",
                    result.path.display(),
                    budget.max_per_file
                );
                diagnostics.selection.omitted_due_to_caps.push(message.clone());
                if high_value {
                    record_high_value_omission(
                        diagnostics,
                        result,
                        &format!("high-value evidence omitted by per-file cap: {message}"),
                    );
                }
                terminally_rejected.insert(index);
                continue;
            }

            if tokens > remaining_tokens {
                let message = format!(
                    "{}: estimated {} tokens exceeds remaining context budget {}",
                    result.path.display(),
                    tokens,
                    remaining_tokens
                );
                diagnostics.selection.omitted_due_to_budget.push(message.clone());
                if high_value {
                    record_high_value_omission(
                        diagnostics,
                        result,
                        &format!("high-value evidence omitted by hard context budget: {message}"),
                    );
                }
                terminally_rejected.insert(index);
                continue;
            }

            let token_set = context_unit_tokens(result);
            let redundancy = selected_token_sets
                .iter()
                .map(|selected| token_set_overlap(&token_set, selected))
                .fold(0.0_f32, f32::max);
            if redundancy >= 0.90 && !high_value {
                diagnostics.selection.redundancy_omissions.push(format!(
                    "{}: near-duplicate context unit omitted ({redundancy:.2} overlap)",
                    result.path.display()
                ));
                terminally_rejected.insert(index);
                continue;
            }

            let utility = context_value_per_token(
                index,
                tokens,
                authority,
                &sources,
                &selected_sources,
                redundancy,
            );
            match &best {
                Some((best_index, best_utility, _, _))
                    if *best_utility > utility
                        || (*best_utility == utility && *best_index < index) => {}
                _ => best = Some((index, utility, tokens, token_set)),
            }
        }

        let Some((index, _utility, tokens, token_set)) = best else {
            break;
        };
        let result = &ranked[index];
        selected_indices.insert(index);
        selected_tokens = selected_tokens.saturating_add(tokens);
        *per_file_units.entry(result.path.clone()).or_default() += 1;
        selected_token_sets.push(token_set);
        selected_sources.extend(retrieval_sources_for_result(diagnostics, result));
    }

    let selected = ranked
        .into_iter()
        .enumerate()
        .filter_map(|(index, result)| selected_indices.contains(&index).then_some(result))
        .collect::<Vec<_>>();
    record_selected_units(&selected, diagnostics);
    for caveat in &diagnostics.selection.caveats {
        if !diagnostics.caveats.contains(caveat) {
            diagnostics.caveats.push(caveat.clone());
        }
    }
    selected
}

fn is_file_limit_compatibility_budget(budget: &ContextBudget) -> bool {
    budget.max_tokens >= usize::MAX / 8 && budget.max_per_file >= usize::MAX / 8
}

fn retrieval_authority_for_result(
    diagnostics: &RetrievalDiagnostics,
    result: &SearchResult,
) -> RetrievalAuthority {
    diagnostics
        .traces
        .iter()
        .find(|trace| normalize_path(&trace.path) == normalize_path(&result.path))
        .map(|trace| trace.authority)
        .unwrap_or(RetrievalAuthority::Heuristic)
}

fn retrieval_sources_for_result(
    diagnostics: &RetrievalDiagnostics,
    result: &SearchResult,
) -> std::collections::BTreeSet<RetrievalSourceKind> {
    diagnostics
        .traces
        .iter()
        .find(|trace| normalize_path(&trace.path) == normalize_path(&result.path))
        .map(|trace| {
            trace
                .contributions
                .iter()
                .map(|contribution| contribution.source)
                .collect()
        })
        .unwrap_or_default()
}

fn is_high_value_context(
    authority: RetrievalAuthority,
    sources: &std::collections::BTreeSet<RetrievalSourceKind>,
) -> bool {
    authority == RetrievalAuthority::Exact
        || sources.contains(&RetrievalSourceKind::Validation)
        || sources.contains(&RetrievalSourceKind::Graph)
}

fn record_high_value_omission(
    diagnostics: &mut RetrievalDiagnostics,
    result: &SearchResult,
    message: &str,
) {
    diagnostics.selection.omitted_high_value.push(format!(
        "{}{}: {message}",
        result.path.display(),
        result
            .line_range
            .as_ref()
            .map(|range| format!(":{}-{}", range.start, range.end))
            .unwrap_or_default()
    ));
    diagnostics.selection.caveats.push(message.to_string());
}

fn context_value_per_token(
    rank_index: usize,
    tokens: usize,
    authority: RetrievalAuthority,
    sources: &std::collections::BTreeSet<RetrievalSourceKind>,
    selected_sources: &std::collections::BTreeSet<RetrievalSourceKind>,
    redundancy: f32,
) -> f32 {
    let rank_value = 1.0 / (rank_index.saturating_add(1) as f32);
    let authority_weight = match authority {
        RetrievalAuthority::Exact => 3.0,
        RetrievalAuthority::Corroborating => 1.35,
        RetrievalAuthority::Heuristic => 1.0,
    };
    let source_diversity = if sources.iter().any(|source| !selected_sources.contains(source)) {
        1.10
    } else {
        1.0
    };
    let redundancy_discount = 1.0 - redundancy.min(0.85) * 0.50;
    // sqrt(cost) avoids pathological preference for tiny fragments while still rewarding useful
    // compact context. The upstream task-aware rank remains the dominant relevance prior.
    rank_value * authority_weight * source_diversity * redundancy_discount
        / (tokens.max(1) as f32).sqrt()
}

fn record_selected_units(selected: &[SearchResult], diagnostics: &mut RetrievalDiagnostics) {
    diagnostics.selection.selected_units.clear();
    diagnostics.selection.per_file_tokens.clear();
    diagnostics.selection.estimated_tokens_selected = 0;
    for result in selected {
        let estimated_tokens = estimate_search_result_tokens(result);
        let authority = retrieval_authority_for_result(diagnostics, result);
        diagnostics.selection.estimated_tokens_selected = diagnostics
            .selection
            .estimated_tokens_selected
            .saturating_add(estimated_tokens);
        *diagnostics
            .selection
            .per_file_tokens
            .entry(result.path.clone())
            .or_default() += estimated_tokens;
        diagnostics.selection.selected_units.push(ContextSelectedUnit {
            path: result.path.clone(),
            line_range: result.line_range.clone(),
            estimated_tokens,
            authority,
            evidence_refs: result.derived_evidence_ids(),
            rationale: format!(
                "selected under context budget after task-aware retrieval ranking ({authority:?} authority)"
            ),
        });
    }
}

'''
text = text[:start] + selector + text[end:]

# Update the adversarial tests to the corrected max_per_file semantics and verify provenance/defaults.
text = text.replace('''            max_per_file: 300,
            max_primary_files: 4,
''', '''            max_per_file: 2,
            max_primary_files: 4,
''', 1)
text = text.replace('''            max_per_file: 100,
            max_primary_files: 1,
''', '''            max_per_file: 2,
            max_primary_files: 1,
''', 1)
text = text.replace('''            max_per_file: 500,
            max_primary_files: 4,
''', '''            max_per_file: 2,
            max_primary_files: 4,
''', 1)
old = '''        assert_eq!(selected[0].line_range, first.line_range);
        assert_eq!(diagnostics.selection.redundancy_omissions.len(), 1);
    }

    #[test]
    fn expanded_task_search_terms_include_config_aliases() {
'''
new = '''        assert_eq!(selected[0].line_range, first.line_range);
        assert_eq!(diagnostics.selection.redundancy_omissions.len(), 1);
        assert_eq!(diagnostics.selection.selected_units.len(), 1);
        assert_eq!(diagnostics.selection.selected_units[0].line_range, first.line_range);
        assert_eq!(
            diagnostics.selection.selected_units[0].evidence_refs,
            first.evidence_refs
        );
    }

    #[test]
    fn default_retrieval_diagnostics_do_not_claim_a_budget_was_applied() {
        let diagnostics = RetrievalDiagnostics::default();
        assert_eq!(diagnostics.selection.budget.max_tokens, 0);
        assert_eq!(diagnostics.selection.available_context_tokens, 0);
        assert!(diagnostics.selection.selected_units.is_empty());
    }

    #[test]
    fn explicit_budget_enforces_context_unit_cap_per_file() {
        let first = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(LineRange { start: 1, end: 10 }),
            snippet: "first distinct section about setup".into(),
            symbol: None,
            score: 2.0,
            match_reason: "section one".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["doc:first".into()],
            confidence: 0.7,
            score_breakdown: Vec::new(),
        };
        let second = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(LineRange { start: 40, end: 50 }),
            snippet: "second distinct section about deployment".into(),
            symbol: None,
            score: 1.0,
            match_reason: "section two".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["doc:second".into()],
            confidence: 0.6,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics::default();
        let budget = ContextBudget {
            max_tokens: 1_000,
            reserve_for_instructions: 100,
            reserve_for_validation: 100,
            max_per_file: 1,
            max_primary_files: 4,
        };

        let selected = select_context_units(vec![first, second], &budget, &mut diagnostics);
        assert_eq!(selected.len(), 1);
        assert_eq!(diagnostics.selection.omitted_due_to_caps.len(), 1);
    }

    #[test]
    fn expanded_task_search_terms_include_config_aliases() {
'''
if text.count(old) != 1:
    raise SystemExit(f'test hardening marker count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)
