from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected exactly one match, found {count}")
    return text.replace(old, new, 1)


# ContextPackBuilder: validation candidates must be selected after runtime evidence is attached,
# and runtime-corroborated source files must not be dropped because of a positional top-3 cutoff.
context_path = Path("crates/open-kioku-context/src/lib.rs")
context = context_path.read_text()

old_early_tests = '''        let mut tests = Vec::new();
        let selector = TestSelector::new(self.store as &dyn open_kioku_storage::MetadataStore);
        for result in primary.iter().take(3) {
            tests.extend(selector.for_changed_path_with_evidence(&result.path, 5)?);
        }
        tests.truncate(10);
'''
context = replace_once(context, old_early_tests, "", "early validation selection")

history_anchor = '''        annotate_results_with_git_history(
            self.store,
            self.history_store,
            task,
            &mut supporting_files,
        )?;
        let runtime_evidence = runtime_signals
'''
validation_block = '''        annotate_results_with_git_history(
            self.store,
            self.history_store,
            task,
            &mut supporting_files,
        )?;

        let selector = TestSelector::new(self.store as &dyn open_kioku_storage::MetadataStore);
        let mut tests_by_id = std::collections::BTreeMap::new();
        for result in validation_seed_results(&primary_files, &supporting_files, 5) {
            for test in selector.for_changed_path_with_evidence(&result.path, 5)? {
                // Validation seeds are ordered by evidence strength. Keep the first observation
                // of a test so runtime-corroborated selection is not overwritten by a weaker path.
                tests_by_id.entry(test.id.clone()).or_insert(test);
            }
        }
        let mut tests = tests_by_id.into_values().collect::<Vec<_>>();
        tests.sort_by(|left, right| {
            right
                .confidence
                .score()
                .partial_cmp(&left.confidence.score())
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.name.cmp(&right.name))
        });
        tests.truncate(10);

        let runtime_evidence = runtime_signals
'''
context = replace_once(context, history_anchor, validation_block, "runtime-aware validation insertion")

helper_marker = '''fn negative_evidence_for_context(
'''
helper = '''fn validation_seed_results<'a>(
    primary_files: &'a [SearchResult],
    supporting_files: &'a [SearchResult],
    limit: usize,
) -> Vec<&'a SearchResult> {
    let mut selected = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let ordered = primary_files
        .iter()
        .filter(|result| has_runtime_corroboration(result))
        .chain(
            supporting_files
                .iter()
                .filter(|result| has_runtime_corroboration(result)),
        )
        .chain(primary_files.iter())
        .chain(supporting_files.iter());

    for result in ordered {
        if is_docs_or_test_path(&result.path.to_string_lossy()) {
            continue;
        }
        let normalized = normalize_path(&result.path);
        if !seen.insert(normalized) {
            continue;
        }
        selected.push(result);
        if selected.len() >= limit {
            break;
        }
    }
    selected
}

fn has_runtime_corroboration(result: &SearchResult) -> bool {
    result.score_breakdown.iter().any(|component| {
        component.signal == "runtime_corroboration" && component.contribution > 0.0
    }) || result
        .evidence
        .iter()
        .any(|evidence| evidence.to_ascii_lowercase().contains("runtime corroboration"))
}

'''
context = replace_once(context, helper_marker, helper + helper_marker, "validation seed helper insertion")
context_path.write_text(context)


# PlanEngine: direct plan construction must use the same evidence priority rather than top-3 rank.
plan_path = Path("crates/open-kioku-plan/src/lib.rs")
plan = plan_path.read_text()
plan = replace_once(
    plan,
    '''        for result in source_results(primary_context).take(3) {\n''',
    '''        for result in validation_source_results(primary_context).into_iter().take(5) {\n''',
    "PlanEngine validation source loop",
)

source_helper = '''fn source_results(primary_context: &[SearchResult]) -> impl Iterator<Item = &SearchResult> {
    primary_context
        .iter()
        .filter(|result| !is_test_path(&result.path))
}
'''
source_helper_replacement = source_helper + '''
fn validation_source_results(primary_context: &[SearchResult]) -> Vec<&SearchResult> {
    let mut runtime_corroborated = Vec::new();
    let mut remaining = Vec::new();
    for result in source_results(primary_context) {
        if result_has_runtime_corroboration(result) {
            runtime_corroborated.push(result);
        } else {
            remaining.push(result);
        }
    }
    runtime_corroborated.extend(remaining);
    runtime_corroborated
}

fn result_has_runtime_corroboration(result: &SearchResult) -> bool {
    result.score_breakdown.iter().any(|component| {
        component.signal == "runtime_corroboration" && component.contribution > 0.0
    }) || result
        .evidence
        .iter()
        .any(|evidence| evidence.to_ascii_lowercase().contains("runtime corroboration"))
}
'''
plan = replace_once(plan, source_helper, source_helper_replacement, "PlanEngine validation helper insertion")
plan_path.write_text(plan)
