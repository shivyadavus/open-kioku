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
replace_once('''struct RetrievalCase {
    id: String,
    task_family: RetrievalTaskFamily,
    language: String,''', '''struct RetrievalCase {
    id: String,
    task_family: RetrievalTaskFamily,
    query_shape: open_kioku_core::QueryShape,
    language: String,''', 'corpus case query shape')
replace_once('''    by_task_family: BTreeMap<String, RetrievalMetricSummary>,
    by_split: BTreeMap<String, RetrievalMetricSummary>,''', '''    by_task_family: BTreeMap<String, RetrievalMetricSummary>,
    by_query_shape: BTreeMap<String, RetrievalMetricSummary>,
    by_split: BTreeMap<String, RetrievalMetricSummary>,''', 'strategy query-shape groups')
replace_once('''struct RetrievalCaseReport {
    id: String,
    task_family: RetrievalTaskFamily,
    language: String,''', '''struct RetrievalCaseReport {
    id: String,
    task_family: RetrievalTaskFamily,
    query_shape: open_kioku_core::QueryShape,
    language: String,''', 'case report query shape')
replace_once('''        id: case.id.clone(),
        task_family: case.task_family,
        language: case.language.clone(),''', '''        id: case.id.clone(),
        task_family: case.task_family,
        query_shape: case.query_shape,
        language: case.language.clone(),''', 'score case query shape')
replace_once('''        by_task_family: summarize_retrieval_groups(&cases, |case| {
            case.task_family.label().into()
        }),
        by_split: summarize_retrieval_groups(&cases, |case| match case.split {''', '''        by_task_family: summarize_retrieval_groups(&cases, |case| {
            case.task_family.label().into()
        }),
        by_query_shape: summarize_retrieval_groups(&cases, |case| {
            query_shape_label(case.query_shape).into()
        }),
        by_split: summarize_retrieval_groups(&cases, |case| match case.split {''', 'build query-shape groups')
replace_once('''    for (family, routed_summary) in &routed.by_task_family {
        let Some(fusion_summary) = fusion.by_task_family.get(family) else {
            continue;
        };
        comparisons.push(retrieval_strategy_comparison(
            &format!("task_family:{family}"),
            &routed_summary.quality,
            &fusion_summary.quality,
        ));
    }
    comparisons
}''', '''    for (family, routed_summary) in &routed.by_task_family {
        let Some(fusion_summary) = fusion.by_task_family.get(family) else {
            continue;
        };
        comparisons.push(retrieval_strategy_comparison(
            &format!("task_family:{family}"),
            &routed_summary.quality,
            &fusion_summary.quality,
        ));
    }
    for (shape, routed_summary) in &routed.by_query_shape {
        let Some(fusion_summary) = fusion.by_query_shape.get(shape) else {
            continue;
        };
        comparisons.push(retrieval_strategy_comparison(
            &format!("query_shape:{shape}"),
            &routed_summary.quality,
            &fusion_summary.quality,
        ));
    }
    comparisons
}''', 'query-shape comparisons')
replace_once('''    let builder = open_kioku_context::ContextPackBuilder::new(
        store as &dyn open_kioku_storage::OkStore,
    )''', '''    let routing = open_kioku_context::routing::classify_task(&case.query);
    if routing.query_shape != case.query_shape {
        anyhow::bail!(
            "retrieval case `{}` query-shape label mismatch: frozen={} classifier={} signals={:?} ambiguities={:?} fallback={:?}",
            case.id,
            query_shape_label(case.query_shape),
            query_shape_label(routing.query_shape),
            routing.query_shape_signals,
            routing.query_shape_ambiguities,
            routing.query_shape_fallback_reason,
        );
    }
    let builder = open_kioku_context::ContextPackBuilder::new(
        store as &dyn open_kioku_storage::OkStore,
    )''', 'frozen query-shape validation')
marker = '''impl RetrievalTaskFamily {
    fn label(self) -> &'static str {
        match self {
            Self::IssueToCode => "issue_to_code",
            Self::CodeToTest => "code_to_test",
            Self::TraceToCode => "trace_to_code",
            Self::CommentToContext => "comment_to_context",
            Self::EditToRipple => "edit_to_ripple",
        }
    }
}
'''
helper = marker + '''
fn query_shape_label(shape: open_kioku_core::QueryShape) -> &'static str {
    match shape {
        open_kioku_core::QueryShape::ExactIdentifier => "exact_identifier",
        open_kioku_core::QueryShape::QualifiedSymbol => "qualified_symbol",
        open_kioku_core::QueryShape::PathReference => "path_reference",
        open_kioku_core::QueryShape::ErrorTrace => "error_trace",
        open_kioku_core::QueryShape::ApiResource => "api_resource",
        open_kioku_core::QueryShape::Conceptual => "conceptual",
        open_kioku_core::QueryShape::MixedStructuredNaturalLanguage => {
            "mixed_structured_natural_language"
        }
        open_kioku_core::QueryShape::Unknown => "unknown",
    }
}
'''
replace_once(marker, helper, 'query shape label helper')
marker = '''    out.push_str("## Reproducibility'''
query_table = '''    if let Some(routed) = report
        .stream_ablations
        .iter()
        .find(|strategy| strategy.strategy == "cc4:routed_contextpack")
    {
        out.push_str("### Routed ContextPack by query shape\n\n| Query shape | R@10 | MRR | F1@10 | No-gold FP | p95 ms | Token-budget gold-file yield |\n|---|---:|---:|---:|---:|---:|---|\n");
        for (shape, summary) in &routed.by_query_shape {
            let budgets = summary
                .quality
                .token_budget_gold_yield
                .iter()
                .map(|(budget, value)| format!("{budget}={value:.3}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.2} | {} |\n",
                shape,
                summary.quality.recall_at_10,
                summary.quality.mean_reciprocal_rank,
                summary.quality.file_f1_at_10,
                summary.quality.no_gold_false_positive_rate,
                summary.latency.p95_ms,
                budgets
            ));
        }
        out.push('\n');
    }
''' + marker
replace_once(marker, query_table, 'markdown query-shape table')
replace_once('''            id: id.into(),
            task_family: RetrievalTaskFamily::IssueToCode,
            language: "rust".into(),''', '''            id: id.into(),
            task_family: RetrievalTaskFamily::IssueToCode,
            query_shape: open_kioku_core::QueryShape::Conceptual,
            language: "rust".into(),''', 'test helper query shape')
replace_once('''        assert_eq!(comparisons.len(), 2);''', '''        assert_eq!(comparisons.len(), 3);''', 'comparison count')
replace_once('''        assert_eq!(comparisons[1].scope, "task_family:issue_to_code");
    }''', '''        assert_eq!(comparisons[1].scope, "task_family:issue_to_code");
        assert_eq!(comparisons[2].scope, "query_shape:conceptual");
    }''', 'comparison shape assertion')
path.write_text(text)

# Freeze query-shape annotations as reviewable corpus data. They are deliberately *not* derived
# at benchmark runtime: the routed benchmark separately runs the production classifier and fails
# closed on drift. This keeps the benchmark label independent from the implementation under test.
path = Path('benchmarks/retrieval-cases.json')
data = json.loads(path.read_text())
labels = {
    'java-issue-token-persistence': 'conceptual',
    'java-code-to-test-auth': 'mixed_structured_natural_language',
    'java-trace-login-audit': 'mixed_structured_natural_language',
    'java-comment-login-rejection-audit': 'mixed_structured_natural_language',
    'java-edit-tokenstore-contract': 'mixed_structured_natural_language',
    'java-no-gold-password-reset': 'conceptual',
    'ts-issue-invoice-tax': 'conceptual',
    'ts-code-to-test-invoice': 'mixed_structured_natural_language',
    'ts-comment-webhook-retry': 'conceptual',
    'ts-edit-webhook-contract': 'mixed_structured_natural_language',
    'ts-trace-invoice-created': 'mixed_structured_natural_language',
    'ts-no-gold-subscription-renewal': 'conceptual',
    'python-issue-order-persistence': 'conceptual',
    'python-code-to-test-order': 'mixed_structured_natural_language',
    'python-comment-order-trace': 'mixed_structured_natural_language',
    'python-no-gold-video': 'conceptual',
    'python-trace-order-created': 'mixed_structured_natural_language',
    'python-edit-repository-contract': 'mixed_structured_natural_language',
    'go-issue-shipping-quote': 'conceptual',
    'go-code-to-test-quote': 'mixed_structured_natural_language',
    'go-comment-timeout': 'mixed_structured_natural_language',
    'go-trace-quote-handler': 'mixed_structured_natural_language',
    'go-no-gold-thumbnail': 'conceptual',
    'go-edit-carrier-contract': 'mixed_structured_natural_language',
    'rust-issue-cache-miss': 'conceptual',
    'rust-code-to-test-cache': 'mixed_structured_natural_language',
    'rust-comment-cache-trace': 'mixed_structured_natural_language',
    'rust-edit-store-contract': 'mixed_structured_natural_language',
    'rust-no-gold-http-session': 'conceptual',
    'rust-trace-cache-miss': 'mixed_structured_natural_language',
}
actual = {case['id'] for case in data['cases']}
if actual != set(labels):
    raise SystemExit(f'frozen query-shape label coverage mismatch missing={sorted(actual-set(labels))} extra={sorted(set(labels)-actual)}')
for case in data['cases']:
    case['query_shape'] = labels[case['id']]
path.write_text(json.dumps(data, indent=2) + '\n')
