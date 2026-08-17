from pathlib import Path
import json
import re

path = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
text = path.read_text()

text = text.replace('const RETRIEVAL_REPORT_VERSION: &str = "1.3.0";', 'const RETRIEVAL_REPORT_VERSION: &str = "1.4.0";', 1)

old = '''    task_family: RetrievalTaskFamily,\n    language: String,\n'''
new = '''    task_family: RetrievalTaskFamily,\n    query_shape: open_kioku_core::QueryShape,\n    language: String,\n'''
assert text.count(old) == 1, text.count(old)
text = text.replace(old, new, 1)

old = '''struct RetrievalStrategyReport {\n    strategy: String,\n    summary: RetrievalMetricSummary,\n    by_language: BTreeMap<String, RetrievalMetricSummary>,\n    by_task_family: BTreeMap<String, RetrievalMetricSummary>,\n    by_split: BTreeMap<String, RetrievalMetricSummary>,\n    cases: Vec<RetrievalCaseReport>,\n}\n'''
new = '''struct RetrievalStrategyReport {\n    strategy: String,\n    summary: RetrievalMetricSummary,\n    by_language: BTreeMap<String, RetrievalMetricSummary>,\n    by_task_family: BTreeMap<String, RetrievalMetricSummary>,\n    by_query_shape: BTreeMap<String, RetrievalMetricSummary>,\n    by_task_family_query_shape: BTreeMap<String, RetrievalMetricSummary>,\n    by_split: BTreeMap<String, RetrievalMetricSummary>,\n    cases: Vec<RetrievalCaseReport>,\n}\n'''
assert text.count(old) == 1
text = text.replace(old, new, 1)

old = '''struct RetrievalCaseReport {\n    id: String,\n    task_family: RetrievalTaskFamily,\n    language: String,\n'''
new = '''struct RetrievalCaseReport {\n    id: String,\n    task_family: RetrievalTaskFamily,\n    query_shape: open_kioku_core::QueryShape,\n    classified_query_shape: open_kioku_core::QueryShape,\n    query_shape_match: bool,\n    language: String,\n'''
assert text.count(old) == 1
text = text.replace(old, new, 1)

old = '''    RetrievalCaseReport {\n        id: case.id.clone(),\n        task_family: case.task_family,\n        language: case.language.clone(),\n'''
new = '''    let classified_query_shape = open_kioku_context::routing::classify_task(&case.query).query_shape;\n    RetrievalCaseReport {\n        id: case.id.clone(),\n        task_family: case.task_family,\n        query_shape: case.query_shape,\n        classified_query_shape,\n        query_shape_match: classified_query_shape == case.query_shape,\n        language: case.language.clone(),\n'''
assert text.count(old) == 1
text = text.replace(old, new, 1)

old = '''        by_task_family: summarize_retrieval_groups(&cases, |case| {\n            case.task_family.label().into()\n        }),\n        by_split: summarize_retrieval_groups(&cases, |case| match case.split {\n'''
new = '''        by_task_family: summarize_retrieval_groups(&cases, |case| {\n            case.task_family.label().into()\n        }),\n        by_query_shape: summarize_retrieval_groups(&cases, |case| {\n            query_shape_label(case.query_shape).into()\n        }),\n        by_task_family_query_shape: summarize_retrieval_groups(&cases, |case| {\n            format!(\n                "{}/{}",\n                case.task_family.label(),\n                query_shape_label(case.query_shape)\n            )\n        }),\n        by_split: summarize_retrieval_groups(&cases, |case| match case.split {\n'''
assert text.count(old) == 1
text = text.replace(old, new, 1)

marker = '''fn summarize_retrieval_groups<F>(\n'''
helper = '''fn query_shape_label(shape: open_kioku_core::QueryShape) -> &'static str {\n    use open_kioku_core::QueryShape;\n    match shape {\n        QueryShape::ExactIdentifier => "exact_identifier",\n        QueryShape::QualifiedSymbol => "qualified_symbol",\n        QueryShape::PathReference => "path_reference",\n        QueryShape::ErrorTrace => "error_trace",\n        QueryShape::ApiResource => "api_resource",\n        QueryShape::Conceptual => "conceptual",\n        QueryShape::MixedStructuredNaturalLanguage => "mixed_structured_natural_language",\n        QueryShape::Unknown => "unknown",\n    }\n}\n\nfn summarize_retrieval_groups<F>(\n'''
assert text.count(marker) == 1
text = text.replace(marker, helper, 1)

old = '''    for (family, routed_summary) in &routed.by_task_family {\n        let Some(fusion_summary) = fusion.by_task_family.get(family) else {\n            continue;\n        };\n        comparisons.push(retrieval_strategy_comparison(\n            &format!("task_family:{family}"),\n            &routed_summary.quality,\n            &fusion_summary.quality,\n        ));\n    }\n    comparisons\n}\n'''
new = '''    for (family, routed_summary) in &routed.by_task_family {\n        let Some(fusion_summary) = fusion.by_task_family.get(family) else {\n            continue;\n        };\n        comparisons.push(retrieval_strategy_comparison(\n            &format!("task_family:{family}"),\n            &routed_summary.quality,\n            &fusion_summary.quality,\n        ));\n    }\n    for (shape, routed_summary) in &routed.by_query_shape {\n        let Some(fusion_summary) = fusion.by_query_shape.get(shape) else {\n            continue;\n        };\n        comparisons.push(retrieval_strategy_comparison(\n            &format!("query_shape:{shape}"),\n            &routed_summary.quality,\n            &fusion_summary.quality,\n        ));\n    }\n    for (scope, routed_summary) in &routed.by_task_family_query_shape {\n        let Some(fusion_summary) = fusion.by_task_family_query_shape.get(scope) else {\n            continue;\n        };\n        comparisons.push(retrieval_strategy_comparison(\n            &format!("task_family_query_shape:{scope}"),\n            &routed_summary.quality,\n            &fusion_summary.quality,\n        ));\n    }\n    comparisons\n}\n'''
assert text.count(old) == 1
text = text.replace(old, new, 1)

# Report routing-classification quality as an advisory caveat; do not weaken or mutate the frozen generic release gate.
old = '''    caveats.push(\n        "cc4:routed_contextpack is advisory and executes task classification, routing policy, routed candidate caps, fusion, budget selection, and ContextPack construction; it does not alter the frozen generic-fusion release gate".into(),\n    );\n'''
new = '''    caveats.push(\n        "cc4:routed_contextpack is advisory and executes task classification, query-shape classification, routing policy, routed candidate caps, fusion, budget selection, and ContextPack construction; it does not alter the frozen generic-fusion release gate".into(),\n    );\n    let query_shape_matches = fusion_cases.iter().filter(|case| case.query_shape_match).count();\n    caveats.push(format!(\n        "query-shape labels are frozen benchmark expectations: classifier matched {query_shape_matches}/{} cases ({:.3}); mismatches remain visible per case and are not auto-corrected from classifier output",\n        fusion_cases.len(),\n        retrieval_ratio(query_shape_matches, fusion_cases.len())\n    ));\n'''
# This block occurs before fusion_cases is moved into reports in current code, but replacement location is after move.
# Instead add this metric after report strategies are built by deriving from the fusion strategy cases.
if text.count(old) == 1:
    # Replace with only wording here; metric insertion handled below.
    text = text.replace(old, '''    caveats.push(\n        "cc4:routed_contextpack is advisory and executes task classification, query-shape classification, routing policy, routed candidate caps, fusion, budget selection, and ContextPack construction; it does not alter the frozen generic-fusion release gate".into(),\n    );\n''', 1)

needle = '''    caveats.push(\n        "abstention precision/recall is not reported until calibrated abstention ships; no-gold false-positive rate remains the active negative-case signal".into(),\n    );\n'''
insert = needle + '''    if let Some(fusion) = strategies\n        .iter()\n        .find(|strategy| strategy.strategy == RetrievalStrategy::Fusion.label())\n    {\n        let query_shape_matches = fusion.cases.iter().filter(|case| case.query_shape_match).count();\n        caveats.push(format!(\n            "query-shape labels are frozen benchmark expectations: classifier matched {query_shape_matches}/{} cases ({:.3}); mismatches remain visible per case and are not auto-corrected from classifier output",\n            fusion.cases.len(),\n            retrieval_ratio(query_shape_matches, fusion.cases.len())\n        ));\n    }\n'''
assert text.count(needle) == 1
text = text.replace(needle, insert, 1)

# Markdown: add routed breakdowns by query shape and task-family/query-shape.
needle = '''            out.push_str("### Routed ContextPack by task family\\n\\n| Task family | R@10 | MRR | F1@10 | No-gold FP | Token-budget gold-file yield |\\n|---|---:|---:|---:|---:|---|\\n");\n'''
assert text.count(needle) == 1
# Keep existing table and append new sections just before the routed block closes, using a stable later marker.
marker = '''        }\n    }\n    out\n}\n'''
# There may be multiple occurrences. target last occurrence before tests is risky. Use a unique fragment after by-task loop.
fragment = '''                out.push_str(&format!(\n                    "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {} |\\n",\n                    family,\n                    summary.quality.recall_at_10,\n                    summary.quality.mean_reciprocal_rank,\n                    summary.quality.file_f1_at_10,\n                    summary.quality.no_gold_false_positive_rate,\n                    budgets\n                ));\n            }\n            out.push('\\n');\n'''
if text.count(fragment) != 1:
    raise SystemExit(f'markdown family fragment count={text.count(fragment)}')
addition = fragment + '''            out.push_str("### Routed ContextPack by query shape\\n\\n| Query shape | R@10 | MRR | F1@10 | No-gold FP | p95 ms |\\n|---|---:|---:|---:|---:|---:|\\n");\n            for (shape, summary) in &routed.by_query_shape {\n                out.push_str(&format!(\n                    "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.2} |\\n",\n                    shape,\n                    summary.quality.recall_at_10,\n                    summary.quality.mean_reciprocal_rank,\n                    summary.quality.file_f1_at_10,\n                    summary.quality.no_gold_false_positive_rate,\n                    summary.latency.p95_ms\n                ));\n            }\n            out.push('\\n');\n            out.push_str("### Routed ContextPack by task family × query shape\\n\\n| Scope | R@10 | MRR | F1@10 | No-gold FP | p95 ms |\\n|---|---:|---:|---:|---:|---:|\\n");\n            for (scope, summary) in &routed.by_task_family_query_shape {\n                out.push_str(&format!(\n                    "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.2} |\\n",\n                    scope,\n                    summary.quality.recall_at_10,\n                    summary.quality.mean_reciprocal_rank,\n                    summary.quality.file_f1_at_10,\n                    summary.quality.no_gold_false_positive_rate,\n                    summary.latency.p95_ms\n                ));\n            }\n            out.push('\\n');\n'''
text = text.replace(fragment, addition, 1)

# Add schema/regression tests next to existing retrieval bench tests.
test_marker = '''    fn corpus_schema_rejects_unknown_fields() {\n'''
if text.count(test_marker) != 1:
    raise SystemExit(f'test marker count={text.count(test_marker)}')
test_code = '''    #[test]\n    fn retrieval_report_groups_quality_by_frozen_query_shape() {\n        let case = RetrievalCaseReport {\n            id: "shape".into(),\n            task_family: RetrievalTaskFamily::IssueToCode,\n            query_shape: open_kioku_core::QueryShape::Conceptual,\n            classified_query_shape: open_kioku_core::QueryShape::Conceptual,\n            query_shape_match: true,\n            language: "rust".into(),\n            split: RetrievalSplit::Development,\n            repo_fixture: "fixture".into(),\n            query: "explain cache invalidation behavior".into(),\n            no_gold_expected: false,\n            gold_files: vec!["src/cache.rs".into()],\n            gold_symbols: Vec::new(),\n            ranked_paths: vec!["src/cache.rs".into()],\n            gold_ranks: vec![Some(1)],\n            recall_at: BTreeMap::from([(1, 1.0), (5, 1.0), (10, 1.0), (20, 1.0)]),\n            precision_at: BTreeMap::from([(1, 1.0), (5, 0.2), (10, 0.1), (20, 0.05)]),\n            reciprocal_rank: 1.0,\n            file_f1_at_10: 2.0 / 11.0,\n            token_budget_gold_yield: BTreeMap::from([(2_000, 1.0)]),\n            token_budget_used: BTreeMap::from([(2_000, 100)]),\n            returned_any: true,\n            latency_ms: 1.0,\n        };\n        let report = build_named_retrieval_strategy_report("shape-test", vec![case]);\n        assert!(report.by_query_shape.contains_key("conceptual"));\n        assert!(report\n            .by_task_family_query_shape\n            .contains_key("issue_to_code/conceptual"));\n    }\n\n    #[test]\n    fn frozen_query_shape_label_is_not_overwritten_by_classifier_output() {\n        let case = RetrievalCase {\n            id: "frozen-label".into(),\n            task_family: RetrievalTaskFamily::IssueToCode,\n            query_shape: open_kioku_core::QueryShape::Unknown,\n            language: "rust".into(),\n            repo_fixture: "fixture".into(),\n            base_revision: format!("sha256:{}", "0".repeat(64)),\n            split: RetrievalSplit::Development,\n            query: "PlanEngine".into(),\n            gold_files: vec!["src/lib.rs".into()],\n            gold_symbols: Vec::new(),\n            no_gold_expected: false,\n            token_budgets: Vec::new(),\n        };\n        let report = score_retrieval_case(&case, &[2_000], Vec::new(), 0.0);\n        assert_eq!(report.query_shape, open_kioku_core::QueryShape::Unknown);\n        assert_eq!(\n            report.classified_query_shape,\n            open_kioku_core::QueryShape::ExactIdentifier\n        );\n        assert!(!report.query_shape_match);\n    }\n\n'''
text = text.replace(test_marker, test_code + test_marker, 1)
path.write_text(text)

# Freeze explicit query-shape labels in the existing corpus without changing cases, gold data, or revisions.
path = Path('benchmarks/retrieval-cases.json')
data = json.loads(path.read_text())

def expected_shape(case):
    q = case['query']
    lower = q.lower()
    # Trace/error-shaped cases are explicitly trace text, independent of their retrieval task family label.
    if case['task_family'] == 'trace_to_code' or any(x in lower for x in ('stack trace', 'panic:', 'exception:', 'traceback')):
        return 'error_trace'
    # Existing frozen corpus contains natural-language tasks. Code-style anchors make these mixed;
    # otherwise they are conceptual. Dedicated routing unit tests cover single-token/path/API shapes.
    code_style = re.search(r'\b(?:[A-Z][A-Za-z0-9_]*[A-Z][A-Za-z0-9_]*|[a-z]+_[a-z0-9_]+|[a-z]+[A-Z][A-Za-z0-9_]*)\b', q)
    return 'mixed_structured_natural_language' if code_style else 'conceptual'

for case in data['cases']:
    case['query_shape'] = expected_shape(case)
path.write_text(json.dumps(data, indent=2) + '\n')
