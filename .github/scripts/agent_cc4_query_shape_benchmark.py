from pathlib import Path
import json

path = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
text = path.read_text()

def replace_once(old, new, label):
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f'{label}: expected 1 marker, found {count}')
    text = text.replace(old, new, 1)

replace_once('const RETRIEVAL_REPORT_VERSION: &str = "1.3.0";', 'const RETRIEVAL_REPORT_VERSION: &str = "1.4.0";', 'report version')
replace_once('''    task_family: RetrievalTaskFamily,\n    language: String,''', '''    task_family: RetrievalTaskFamily,\n    query_shape: open_kioku_core::QueryShape,\n    language: String,''', 'case query shape')
replace_once('''    by_task_family: BTreeMap<String, RetrievalMetricSummary>,\n    by_split: BTreeMap<String, RetrievalMetricSummary>,''', '''    by_task_family: BTreeMap<String, RetrievalMetricSummary>,\n    by_query_shape: BTreeMap<String, RetrievalMetricSummary>,\n    by_split: BTreeMap<String, RetrievalMetricSummary>,''', 'strategy query-shape groups')
# Only the case report, not the corpus case (already patched above), still needs a query_shape field.
needle = '''struct RetrievalCaseReport {\n    id: String,\n    task_family: RetrievalTaskFamily,\n    language: String,'''
replacement = '''struct RetrievalCaseReport {\n    id: String,\n    task_family: RetrievalTaskFamily,\n    query_shape: open_kioku_core::QueryShape,\n    language: String,'''
replace_once(needle, replacement, 'case report query shape')
replace_once('''        id: case.id.clone(),\n        task_family: case.task_family,\n        language: case.language.clone(),''', '''        id: case.id.clone(),\n        task_family: case.task_family,\n        query_shape: case.query_shape,\n        language: case.language.clone(),''', 'score case query shape')
replace_once('''        by_task_family: summarize_retrieval_groups(&cases, |case| {\n            case.task_family.label().into()\n        }),\n        by_split: summarize_retrieval_groups(&cases, |case| match case.split {''', '''        by_task_family: summarize_retrieval_groups(&cases, |case| {\n            case.task_family.label().into()\n        }),\n        by_query_shape: summarize_retrieval_groups(&cases, |case| {\n            query_shape_label(case.query_shape).into()\n        }),\n        by_split: summarize_retrieval_groups(&cases, |case| match case.split {''', 'build query-shape groups')
replace_once('''    for (family, routed_summary) in &routed.by_task_family {\n        let Some(fusion_summary) = fusion.by_task_family.get(family) else {\n            continue;\n        };\n        comparisons.push(retrieval_strategy_comparison(\n            &format!("task_family:{family}"),\n            &routed_summary.quality,\n            &fusion_summary.quality,\n        ));\n    }\n    comparisons\n}''', '''    for (family, routed_summary) in &routed.by_task_family {\n        let Some(fusion_summary) = fusion.by_task_family.get(family) else {\n            continue;\n        };\n        comparisons.push(retrieval_strategy_comparison(\n            &format!("task_family:{family}"),\n            &routed_summary.quality,\n            &fusion_summary.quality,\n        ));\n    }\n    for (shape, routed_summary) in &routed.by_query_shape {\n        let Some(fusion_summary) = fusion.by_query_shape.get(shape) else {\n            continue;\n        };\n        comparisons.push(retrieval_strategy_comparison(\n            &format!("query_shape:{shape}"),\n            &routed_summary.quality,\n            &fusion_summary.quality,\n        ));\n    }\n    comparisons\n}''', 'query-shape comparisons')
# Validate frozen labels against the production classifier on the routed path.
replace_once('''    let builder = open_kioku_context::ContextPackBuilder::new(\n        store as &dyn open_kioku_storage::OkStore,\n    )''', '''    let routing = open_kioku_context::routing::classify_task(&case.query);\n    if routing.query_shape != case.query_shape {\n        anyhow::bail!(\n            "retrieval case `{}` query-shape label mismatch: frozen={} classifier={} signals={:?} ambiguities={:?} fallback={:?}",\n            case.id,\n            query_shape_label(case.query_shape),\n            query_shape_label(routing.query_shape),\n            routing.query_shape_signals,\n            routing.query_shape_ambiguities,\n            routing.query_shape_fallback_reason,\n        );\n    }\n    let builder = open_kioku_context::ContextPackBuilder::new(\n        store as &dyn open_kioku_storage::OkStore,\n    )''', 'frozen query-shape validation')
# Add stable label helper near task-family labels.
marker = '''impl RetrievalTaskFamily {\n    fn label(self) -> &'static str {\n        match self {\n            Self::IssueToCode => "issue_to_code",\n            Self::CodeToTest => "code_to_test",\n            Self::TraceToCode => "trace_to_code",\n            Self::CommentToContext => "comment_to_context",\n            Self::EditToRipple => "edit_to_ripple",\n        }\n    }\n}\n'''
helper = marker + '''\nfn query_shape_label(shape: open_kioku_core::QueryShape) -> &'static str {\n    match shape {\n        open_kioku_core::QueryShape::ExactIdentifier => "exact_identifier",\n        open_kioku_core::QueryShape::QualifiedSymbol => "qualified_symbol",\n        open_kioku_core::QueryShape::PathReference => "path_reference",\n        open_kioku_core::QueryShape::ErrorTrace => "error_trace",\n        open_kioku_core::QueryShape::ApiResource => "api_resource",\n        open_kioku_core::QueryShape::Conceptual => "conceptual",\n        open_kioku_core::QueryShape::MixedStructuredNaturalLanguage => {\n            "mixed_structured_natural_language"\n        }\n        open_kioku_core::QueryShape::Unknown => "unknown",\n    }\n}\n'''
replace_once(marker, helper, 'query shape label helper')
# Markdown query-shape table alongside task-family table.
old = '''            out.push('\\n');\n        }\n    }\n    out.push_str("## Reproducibility'''
new = '''            out.push('\\n');\n            out.push_str("### Routed ContextPack by query shape\\n\\n| Query shape | R@10 | MRR | F1@10 | No-gold FP | p95 ms | Token-budget gold-file yield |\\n|---|---:|---:|---:|---:|---:|---|\\n");\n            for (shape, summary) in &routed.by_query_shape {\n                let budgets = summary\n                    .quality\n                    .token_budget_gold_yield\n                    .iter()\n                    .map(|(budget, value)| format!("{budget}={value:.3}"))\n                    .collect::<Vec<_>>()\n                    .join(", ");\n                out.push_str(&format!(\n                    "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.2} | {} |\\n",\n                    shape,\n                    summary.quality.recall_at_10,\n                    summary.quality.mean_reciprocal_rank,\n                    summary.quality.file_f1_at_10,\n                    summary.quality.no_gold_false_positive_rate,\n                    summary.latency.p95_ms,\n                    budgets\n                ));\n            }\n            out.push('\\n');\n        }\n    }\n    out.push_str("## Reproducibility'''
replace_once(old, new, 'markdown query-shape table')
# Test helper + comparison expectations.
replace_once('''            id: id.into(),\n            task_family: RetrievalTaskFamily::IssueToCode,\n            language: "rust".into(),''', '''            id: id.into(),\n            task_family: RetrievalTaskFamily::IssueToCode,\n            query_shape: open_kioku_core::QueryShape::Conceptual,\n            language: "rust".into(),''', 'test helper query shape')
replace_once('''        assert_eq!(comparisons.len(), 2);''', '''        assert_eq!(comparisons.len(), 3);''', 'comparison count')
replace_once('''        assert_eq!(comparisons[1].scope, "task_family:issue_to_code");\n    }''', '''        assert_eq!(comparisons[1].scope, "task_family:issue_to_code");\n        assert_eq!(comparisons[2].scope, "query_shape:conceptual");\n    }''', 'comparison shape assertion')
path.write_text(text)

# Freeze query-shape labels in the existing corpus. These are labels, not routing inputs; the
# benchmark validates them against the production classifier and fails loudly on drift.
path = Path('benchmarks/retrieval-cases.json')
data = json.loads(path.read_text())
labels = {}
for case in data['cases']:
    cid = case['id']
    if '-trace-' in cid or cid.endswith('-trace'):
        label = 'error_trace'
    elif 'no-gold' in cid:
        label = 'conceptual'
    else:
        q = case['query']
        # Current production classifier treats natural-language queries containing code-shaped
        # identifiers/resource syntax as mixed. Plain workflow prose remains conceptual.
        structured_markers = ['_', '.', 'AuthService', 'TokenStore', 'finalizeInvoice', 'publishInvoiceCreated', 'OrderService', 'OrderRepository', 'QuoteHandler', 'CacheStore', 'Service Quote', 'Carrier Rate', 'HTTP 504']
        label = 'mixed_structured_natural_language' if any(m in q for m in structured_markers) else 'conceptual'
    labels[cid] = label
    case['query_shape'] = label
path.write_text(json.dumps(data, indent=2) + '\n')
print(json.dumps(labels, indent=2, sort_keys=True))
