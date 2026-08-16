from pathlib import Path

# Core budget/diagnostic model.
path = Path('crates/open-kioku-core/src/lib.rs')
text = path.read_text()
old = '''#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RetrievalDiagnostics {
    #[serde(default)]
    pub traces: Vec<RetrievalTrace>,
    #[serde(default)]
    pub caveats: Vec<String>,
    #[serde(default)]
    pub sources_attempted: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub sources_succeeded: Vec<RetrievalSourceKind>,
}
'''
new = '''#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextBudget {
    pub max_tokens: usize,
    pub reserve_for_instructions: usize,
    pub reserve_for_validation: usize,
    pub max_per_file: usize,
    pub max_primary_files: usize,
}

impl ContextBudget {
    pub fn available_context_tokens(&self) -> usize {
        self.max_tokens
            .saturating_sub(self.reserve_for_instructions)
            .saturating_sub(self.reserve_for_validation)
    }

    pub fn from_file_limit(limit: usize) -> Self {
        Self {
            max_tokens: usize::MAX / 4,
            reserve_for_instructions: 0,
            reserve_for_validation: 0,
            max_per_file: usize::MAX / 4,
            max_primary_files: limit,
        }
    }
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_tokens: 8_000,
            reserve_for_instructions: 1_000,
            reserve_for_validation: 1_000,
            max_per_file: 2_500,
            max_primary_files: 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RetrievalDiagnostics {
    #[serde(default)]
    pub traces: Vec<RetrievalTrace>,
    #[serde(default)]
    pub caveats: Vec<String>,
    #[serde(default)]
    pub sources_attempted: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub sources_succeeded: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub selection: ContextSelectionDiagnostics,
}
'''
if text.count(old) != 1:
    raise SystemExit(f'core diagnostics anchor count={text.count(old)}')
text = text.replace(old, new, 1)
path.write_text(text)

# Context builder budget selection and diagnostics.
path = Path('crates/open-kioku-context/src/lib.rs')
text = path.read_text()
text = text.replace(
    '    ConfidenceSignalInput, ContextPack, Evidence, EvidenceId, EvidenceSourceType, File, FileRange,\n',
    '    ConfidenceSignalInput, ContextBudget, ContextPack, Evidence, EvidenceId, EvidenceSourceType, File, FileRange,\n',
    1,
)

old = '''    pub fn build(&self, task: &str, limit: usize) -> Result<ContextPack> {
        self.build_with_sources(task, limit, &[])
    }

    pub fn build_with_sources(
        &self,
        task: &str,
        limit: usize,
        external_sources: &[&dyn candidates::ContextCandidateSource],
    ) -> Result<ContextPack> {
        let files = self.store.list_files(usize::MAX, 0)?;
'''
new = '''    pub fn build(&self, task: &str, limit: usize) -> Result<ContextPack> {
        self.build_with_budget_and_sources(task, ContextBudget::from_file_limit(limit), &[])
    }

    pub fn build_with_budget(&self, task: &str, budget: ContextBudget) -> Result<ContextPack> {
        self.build_with_budget_and_sources(task, budget, &[])
    }

    pub fn build_with_sources(
        &self,
        task: &str,
        limit: usize,
        external_sources: &[&dyn candidates::ContextCandidateSource],
    ) -> Result<ContextPack> {
        self.build_with_budget_and_sources(
            task,
            ContextBudget::from_file_limit(limit),
            external_sources,
        )
    }

    pub fn build_with_budget_and_sources(
        &self,
        task: &str,
        budget: ContextBudget,
        external_sources: &[&dyn candidates::ContextCandidateSource],
    ) -> Result<ContextPack> {
        let limit = budget.max_primary_files;
        let files = self.store.list_files(usize::MAX, 0)?;
'''
if text.count(old) != 1:
    raise SystemExit(f'build anchor count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''        let diagnostics = fused.diagnostics;
        let primary = rerank_fused_for_task_with_options(
            fused.results,
            &intent,
            &diagnostics,
            &self.ranking_options,
        );
        self.build_from_primary_with_impact(task, limit, primary, true, false, diagnostics)
'''
new = '''        let mut diagnostics = fused.diagnostics;
        let primary = rerank_fused_for_task_with_options(
            fused.results,
            &intent,
            &diagnostics,
            &self.ranking_options,
        );
        let primary = select_context_units(primary, &budget, &mut diagnostics);
        self.build_from_primary_with_impact(task, limit, primary, true, false, diagnostics)
'''
if text.count(old) != 1:
    raise SystemExit(f'fusion selection anchor count={text.count(old)}')
text = text.replace(old, new, 1)

# Add compact selection diagnostics to human and prompt renderers.
old = '''    if !diagnostics.caveats.is_empty() {
        out.push_str("- Caveats:\\n");
        for caveat in &diagnostics.caveats {
            out.push_str(&format!("  - {caveat}\\n"));
        }
    }
    out.push('\\n');
}
'''
new = '''    if diagnostics.selection.budget.max_tokens > 0 {
        out.push_str(&format!(
            "- Context budget: `{}` tokens (`{}` available after reserves); selected estimate `{}`\\n",
            diagnostics.selection.budget.max_tokens,
            diagnostics.selection.available_context_tokens,
            diagnostics.selection.estimated_tokens_selected
        ));
        if !diagnostics.selection.omitted_high_value.is_empty() {
            out.push_str("- High-value omissions:\\n");
            for omission in &diagnostics.selection.omitted_high_value {
                out.push_str(&format!("  - {omission}\\n"));
            }
        }
    }
    if !diagnostics.caveats.is_empty() {
        out.push_str("- Caveats:\\n");
        for caveat in &diagnostics.caveats {
            out.push_str(&format!("  - {caveat}\\n"));
        }
    }
    out.push('\\n');
}
'''
if text.count(old) != 1:
    raise SystemExit(f'markdown diagnostics anchor count={text.count(old)}')
text = text.replace(old, new, 1)

old = '''    for caveat in &diagnostics.caveats {
        out.push_str(&format!("RETRIEVAL_CAVEAT: {caveat}\\n"));
    }
}
'''
new = '''    if diagnostics.selection.budget.max_tokens > 0 {
        out.push_str(&format!(
            "CONTEXT_BUDGET: max={} available={} selected_estimate={}\\n",
            diagnostics.selection.budget.max_tokens,
            diagnostics.selection.available_context_tokens,
            diagnostics.selection.estimated_tokens_selected
        ));
        for omission in &diagnostics.selection.omitted_high_value {
            out.push_str(&format!("CONTEXT_HIGH_VALUE_OMISSION: {omission}\\n"));
        }
    }
    for caveat in &diagnostics.caveats {
        out.push_str(&format!("RETRIEVAL_CAVEAT: {caveat}\\n"));
    }
}
'''
if text.count(old) != 1:
    raise SystemExit(f'prompt diagnostics anchor count={text.count(old)}')
text = text.replace(old, new, 1)

# Insert deterministic greedy selector before validation seeding.
marker = '''fn validation_seed_results<'a>(
'''
helper = '''fn select_context_units(
    ranked: Vec<SearchResult>,
    budget: &ContextBudget,
    diagnostics: &mut RetrievalDiagnostics,
) -> Vec<SearchResult> {
    let available = budget.available_context_tokens();
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

    let authority_by_path = diagnostics
        .traces
        .iter()
        .map(|trace| (normalize_path(&trace.path), trace.authority))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut selected_indices = std::collections::BTreeSet::new();
    let mut selected_token_sets = Vec::<std::collections::BTreeSet<String>>::new();
    let mut selected_tokens = 0usize;
    let mut per_file_tokens = std::collections::BTreeMap::<std::path::PathBuf, usize>::new();

    let mut order = ranked
        .iter()
        .enumerate()
        .filter(|(_, result)| {
            authority_by_path
                .get(&normalize_path(&result.path))
                .copied()
                .unwrap_or(RetrievalAuthority::Heuristic)
                == RetrievalAuthority::Exact
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    order.extend((0..ranked.len()).filter(|index| !order.contains(index)));

    for index in order {
        if selected_indices.len() >= budget.max_primary_files {
            break;
        }
        let result = &ranked[index];
        let authority = authority_by_path
            .get(&normalize_path(&result.path))
            .copied()
            .unwrap_or(RetrievalAuthority::Heuristic);
        let tokens = estimate_search_result_tokens(result);
        let file_used = per_file_tokens.get(&result.path).copied().unwrap_or_default();
        let is_exact = authority == RetrievalAuthority::Exact;

        if file_used.saturating_add(tokens) > budget.max_per_file {
            let message = format!(
                "{}: estimated {} tokens exceeds per-file cap {}",
                result.path.display(),
                tokens,
                budget.max_per_file
            );
            if is_exact {
                diagnostics.selection.omitted_high_value.push(message);
            } else {
                diagnostics.selection.omitted_due_to_budget.push(message);
            }
            continue;
        }

        let token_set = context_unit_tokens(result);
        if !is_exact
            && selected_token_sets
                .iter()
                .any(|selected| token_set_overlap(&token_set, selected) >= 0.90)
        {
            diagnostics.selection.redundancy_omissions.push(format!(
                "{}: near-duplicate context unit omitted",
                result.path.display()
            ));
            continue;
        }

        if selected_tokens.saturating_add(tokens) > available {
            let message = format!(
                "{}: estimated {} tokens would exceed remaining context budget {}",
                result.path.display(),
                tokens,
                available.saturating_sub(selected_tokens)
            );
            if is_exact {
                diagnostics.selection.omitted_high_value.push(message.clone());
                diagnostics.selection.caveats.push(format!(
                    "exact evidence omitted by hard context budget: {}",
                    result.path.display()
                ));
            } else {
                diagnostics.selection.omitted_due_to_budget.push(message);
            }
            continue;
        }

        selected_tokens = selected_tokens.saturating_add(tokens);
        *per_file_tokens.entry(result.path.clone()).or_default() += tokens;
        selected_token_sets.push(token_set);
        selected_indices.insert(index);
    }

    diagnostics.selection.estimated_tokens_selected = selected_tokens;
    diagnostics.selection.per_file_tokens = per_file_tokens;
    for caveat in &diagnostics.selection.caveats {
        if !diagnostics.caveats.contains(caveat) {
            diagnostics.caveats.push(caveat.clone());
        }
    }

    ranked
        .into_iter()
        .enumerate()
        .filter_map(|(index, result)| selected_indices.contains(&index).then_some(result))
        .collect()
}

fn estimate_search_result_tokens(result: &SearchResult) -> usize {
    // Deliberately model-independent and deterministic. Four UTF-8 chars/token is a conservative
    // local estimate for mixed source/code prose, with fixed metadata overhead.
    let content = result.snippet.chars().count()
        + result.path.to_string_lossy().chars().count()
        + result.match_reason.chars().count();
    content.saturating_add(3) / 4 + 12
}

fn context_unit_tokens(result: &SearchResult) -> std::collections::BTreeSet<String> {
    result
        .snippet
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 4)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn token_set_overlap(
    left: &std::collections::BTreeSet<String>,
    right: &std::collections::BTreeSet<String>,
) -> f32 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count() as f32;
    let smaller = left.len().min(right.len()) as f32;
    intersection / smaller
}

'''
if text.count(marker) != 1:
    raise SystemExit(f'selector insertion marker count={text.count(marker)}')
text = text.replace(marker, helper + marker, 1)

# Regression/adversarial tests near existing context quality tests.
anchor = '''    #[test]
    fn expanded_task_search_terms_include_config_aliases() {
'''
tests = '''    #[test]
    fn token_budget_prevents_one_large_heuristic_unit_from_monopolizing_context() {
        let huge = SearchResult {
            path: "src/huge.rs".into(),
            line_range: Some(LineRange { start: 1, end: 400 }),
            snippet: "large implementation block ".repeat(500),
            symbol: None,
            score: 10.0,
            match_reason: "heuristic".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let compact = SearchResult {
            path: "src/compact.rs".into(),
            line_range: Some(LineRange { start: 10, end: 20 }),
            snippet: "fn compact_target() { validate(); }".into(),
            symbol: None,
            score: 5.0,
            match_reason: "heuristic".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics::default();
        let budget = ContextBudget {
            max_tokens: 800,
            reserve_for_instructions: 100,
            reserve_for_validation: 100,
            max_per_file: 300,
            max_primary_files: 4,
        };

        let selected = select_context_units(vec![huge, compact.clone()], &budget, &mut diagnostics);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].path, compact.path);
        assert!(!diagnostics.selection.omitted_due_to_budget.is_empty());
        assert!(diagnostics.selection.estimated_tokens_selected <= 600);
    }

    #[test]
    fn exact_evidence_is_considered_before_cheaper_heuristics_under_budget() {
        let heuristic = SearchResult {
            path: "src/cheap.rs".into(),
            line_range: None,
            snippet: "cheap candidate".into(),
            symbol: None,
            score: 100.0,
            match_reason: "heuristic".into(),
            evidence: Vec::new(),
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        };
        let exact = SearchResult {
            path: "src/exact.rs".into(),
            line_range: Some(LineRange { start: 20, end: 24 }),
            snippet: "fn exact_target() {}".into(),
            symbol: None,
            score: 0.01,
            match_reason: "exact".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["symbol:exact".into()],
            confidence: 1.0,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics {
            traces: vec![open_kioku_core::RetrievalTrace {
                path: exact.path.clone(),
                fused_score: exact.score,
                authority: RetrievalAuthority::Exact,
                contributions: Vec::new(),
            }],
            ..Default::default()
        };
        let budget = ContextBudget {
            max_tokens: 300,
            reserve_for_instructions: 100,
            reserve_for_validation: 100,
            max_per_file: 100,
            max_primary_files: 1,
        };

        let selected = select_context_units(vec![heuristic, exact.clone()], &budget, &mut diagnostics);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].path, exact.path);
    }

    #[test]
    fn token_selection_preserves_document_section_range_and_dedupes_redundant_units() {
        let first = SearchResult {
            path: "docs/guide.md".into(),
            line_range: Some(LineRange { start: 40, end: 55 }),
            snippet: "configure agent workflow validation boundary evidence".into(),
            symbol: None,
            score: 2.0,
            match_reason: "document section".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["document:guide:section".into()],
            confidence: 0.7,
            score_breakdown: Vec::new(),
        };
        let duplicate = SearchResult {
            path: "docs/copy.md".into(),
            line_range: Some(LineRange { start: 1, end: 8 }),
            snippet: first.snippet.clone(),
            symbol: None,
            score: 1.0,
            match_reason: "document section".into(),
            evidence: Vec::new(),
            evidence_refs: vec!["document:copy:section".into()],
            confidence: 0.6,
            score_breakdown: Vec::new(),
        };
        let mut diagnostics = RetrievalDiagnostics::default();
        let budget = ContextBudget {
            max_tokens: 1_000,
            reserve_for_instructions: 100,
            reserve_for_validation: 100,
            max_per_file: 500,
            max_primary_files: 4,
        };

        let selected = select_context_units(vec![first.clone(), duplicate], &budget, &mut diagnostics);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].path, first.path);
        assert_eq!(selected[0].line_range, first.line_range);
        assert_eq!(diagnostics.selection.redundancy_omissions.len(), 1);
    }

'''
if text.count(anchor) != 1:
    raise SystemExit(f'test insertion anchor count={text.count(anchor)}')
text = text.replace(anchor, tests + anchor, 1)
path.write_text(text)
