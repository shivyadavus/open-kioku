from pathlib import Path

path = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
text = path.read_text()


def replace_once(old: str, new: str, label: str) -> None:
    global text
    count = text.count(old)
    if count != 1:
        raise SystemExit(f'{label}: expected exactly one marker, found {count}')
    text = text.replace(old, new, 1)

replace_once(
    'const RETRIEVAL_REPORT_VERSION: &str = "1.3.0";\n',
    'const RETRIEVAL_REPORT_VERSION: &str = "1.4.0";\nconst RETRIEVAL_QUERY_SHAPE_LABEL_SCHEMA_VERSION: &str = "1.0.0";\n',
    'report version',
)

replace_once(
'''struct RetrievalCase {
    id: String,
    task_family: RetrievalTaskFamily,
    language: String,''',
'''struct RetrievalCase {
    id: String,
    task_family: RetrievalTaskFamily,
    #[serde(skip)]
    expected_query_shape: Option<open_kioku_core::QueryShape>,
    language: String,''',
    'retrieval case expected shape',
)

insert_after = '''impl RetrievalTaskFamily {
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
query_types = insert_after + '''
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

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetrievalQueryShapeLabels {
    schema_version: String,
    corpus_id: String,
    cases: Vec<RetrievalQueryShapeCaseLabel>,
    #[serde(default)]
    adversarial_probes: Vec<RetrievalQueryShapeProbe>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetrievalQueryShapeCaseLabel {
    id: String,
    expected_query_shape: open_kioku_core::QueryShape,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetrievalQueryShapeProbe {
    id: String,
    query: String,
    expected_query_shape: open_kioku_core::QueryShape,
}

#[derive(Debug, Clone, Serialize)]
struct RetrievalQueryShapeMismatch {
    id: String,
    expected: open_kioku_core::QueryShape,
    actual: open_kioku_core::QueryShape,
}

#[derive(Debug, Clone, Default, Serialize)]
struct RetrievalQueryShapeBenchmark {
    labels_file: PathBuf,
    labels_sha256: String,
    labeled_case_count: usize,
    classification_accuracy: f64,
    misclassification_rate: f64,
    confusion_matrix: BTreeMap<String, BTreeMap<String, usize>>,
    mismatches: Vec<RetrievalQueryShapeMismatch>,
    adversarial_probe_count: usize,
    adversarial_probe_accuracy: f64,
    adversarial_probe_mismatches: Vec<RetrievalQueryShapeMismatch>,
}
'''
replace_once(insert_after, query_types, 'query shape helper/types')

replace_once(
'''    advisory_comparisons: Vec<RetrievalStrategyComparison>,
    caveats: Vec<String>,''',
'''    advisory_comparisons: Vec<RetrievalStrategyComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_shape_benchmark: Option<RetrievalQueryShapeBenchmark>,
    caveats: Vec<String>,''',
    'top-level query shape report',
)

replace_once(
'''    by_language: BTreeMap<String, RetrievalMetricSummary>,
    by_task_family: BTreeMap<String, RetrievalMetricSummary>,
    by_split: BTreeMap<String, RetrievalMetricSummary>,''',
'''    by_language: BTreeMap<String, RetrievalMetricSummary>,
    by_task_family: BTreeMap<String, RetrievalMetricSummary>,
    by_query_shape: BTreeMap<String, RetrievalMetricSummary>,
    by_task_family_query_shape: BTreeMap<String, RetrievalMetricSummary>,
    by_split: BTreeMap<String, RetrievalMetricSummary>,''',
    'strategy query shape groups',
)

replace_once(
'''    id: String,
    task_family: RetrievalTaskFamily,
    language: String,''',
'''    id: String,
    task_family: RetrievalTaskFamily,
    expected_query_shape: Option<open_kioku_core::QueryShape>,
    actual_query_shape: open_kioku_core::QueryShape,
    language: String,''',
    'case report query shapes',
)

replace_once(
'''    let corpus = load_retrieval_corpus(&cases_file)?;
    if corpus.cases.len() < args.min_cases {''',
'''    let mut corpus = load_retrieval_corpus(&cases_file)?;
    let query_shape_labels_path = cases_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("retrieval-query-shape-labels.json");
    let query_shape_labels = if query_shape_labels_path.is_file() {
        Some(load_and_apply_query_shape_labels(
            &query_shape_labels_path,
            &mut corpus,
        )?)
    } else {
        None
    };
    if corpus.cases.len() < args.min_cases {''',
    'load query shape labels',
)

replace_once(
'''    let advisory_comparisons = routed_contextpack_comparisons(&strategies, &stream_ablations);
    let (corpus_revision, revision_caveat) = retrieval_corpus_revision(&cases_file);''',
'''    let advisory_comparisons = routed_contextpack_comparisons(&strategies, &stream_ablations);
    let query_shape_benchmark = query_shape_labels
        .as_ref()
        .map(|labels| build_query_shape_benchmark(&corpus, labels, &query_shape_labels_path))
        .transpose()?;
    let (corpus_revision, revision_caveat) = retrieval_corpus_revision(&cases_file);''',
    'build query shape benchmark',
)

replace_once(
'''    caveats.push(
        "cc4:routed_contextpack is advisory and executes task classification, routing policy, routed candidate caps, fusion, budget selection, and ContextPack construction; it does not alter the frozen generic-fusion release gate".into(),
    );''',
'''    caveats.push(
        "cc4:routed_contextpack is advisory and executes task classification, query-shape classification, routing policy, routed candidate caps, fusion, budget selection, and ContextPack construction; it does not alter the frozen generic-fusion release gate".into(),
    );
    if query_shape_benchmark.is_none() {
        caveats.push(
            "query-shape labels are unavailable beside the retrieval corpus; query-shape quality and misclassification reporting are omitted".into(),
        );
    }''',
    'query shape caveat',
)

replace_once(
'''        baseline_deltas,
        advisory_comparisons,
        caveats,''',
'''        baseline_deltas,
        advisory_comparisons,
        query_shape_benchmark,
        caveats,''',
    'report query shape field',
)

marker = '''fn validate_token_budgets(budgets: &[usize], label: &str) -> anyhow::Result<()> {'''
query_loader = '''fn load_and_apply_query_shape_labels(
    path: &Path,
    corpus: &mut RetrievalCorpus,
) -> anyhow::Result<RetrievalQueryShapeLabels> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read query-shape labels {}", path.display()))?;
    let labels: RetrievalQueryShapeLabels = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse query-shape labels {}", path.display()))?;
    if labels.schema_version != RETRIEVAL_QUERY_SHAPE_LABEL_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported query-shape label schema {}; expected {}",
            labels.schema_version,
            RETRIEVAL_QUERY_SHAPE_LABEL_SCHEMA_VERSION
        );
    }
    if labels.corpus_id != corpus.corpus_id {
        anyhow::bail!(
            "query-shape labels target corpus `{}` but retrieval corpus is `{}`",
            labels.corpus_id,
            corpus.corpus_id
        );
    }

    let mut by_id = BTreeMap::new();
    for label in &labels.cases {
        if label.id.trim().is_empty() {
            anyhow::bail!("query-shape case label id must be non-empty");
        }
        if by_id
            .insert(label.id.clone(), label.expected_query_shape)
            .is_some()
        {
            anyhow::bail!("duplicate query-shape case label `{}`", label.id);
        }
    }
    let corpus_ids = corpus
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<BTreeSet<_>>();
    let unknown = by_id
        .keys()
        .filter(|id| !corpus_ids.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        anyhow::bail!(
            "query-shape labels contain unknown retrieval case(s): {}",
            unknown.join(", ")
        );
    }
    let missing = corpus
        .cases
        .iter()
        .filter(|case| !by_id.contains_key(&case.id))
        .map(|case| case.id.clone())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        anyhow::bail!(
            "query-shape labels are incomplete; missing retrieval case(s): {}",
            missing.join(", ")
        );
    }
    for case in &mut corpus.cases {
        case.expected_query_shape = by_id.get(&case.id).copied();
    }

    let mut probe_ids = BTreeSet::new();
    for probe in &labels.adversarial_probes {
        if probe.id.trim().is_empty() || probe.query.trim().is_empty() {
            anyhow::bail!("query-shape adversarial probes require non-empty id and query");
        }
        if !probe_ids.insert(probe.id.clone()) {
            anyhow::bail!("duplicate query-shape adversarial probe `{}`", probe.id);
        }
    }
    Ok(labels)
}

fn build_query_shape_benchmark(
    corpus: &RetrievalCorpus,
    labels: &RetrievalQueryShapeLabels,
    labels_path: &Path,
) -> anyhow::Result<RetrievalQueryShapeBenchmark> {
    let mut confusion_matrix = BTreeMap::<String, BTreeMap<String, usize>>::new();
    let mut mismatches = Vec::new();
    for case in &corpus.cases {
        let Some(expected) = case.expected_query_shape else {
            continue;
        };
        let actual = open_kioku_context::routing::classify_task(&case.query).query_shape;
        *confusion_matrix
            .entry(query_shape_label(expected).into())
            .or_default()
            .entry(query_shape_label(actual).into())
            .or_default() += 1;
        if expected != actual {
            mismatches.push(RetrievalQueryShapeMismatch {
                id: case.id.clone(),
                expected,
                actual,
            });
        }
    }
    let labeled_case_count = corpus
        .cases
        .iter()
        .filter(|case| case.expected_query_shape.is_some())
        .count();
    let correct = labeled_case_count.saturating_sub(mismatches.len());

    let mut probe_mismatches = Vec::new();
    for probe in &labels.adversarial_probes {
        let actual = open_kioku_context::routing::classify_task(&probe.query).query_shape;
        if actual != probe.expected_query_shape {
            probe_mismatches.push(RetrievalQueryShapeMismatch {
                id: probe.id.clone(),
                expected: probe.expected_query_shape,
                actual,
            });
        }
    }
    let probe_count = labels.adversarial_probes.len();
    let probe_correct = probe_count.saturating_sub(probe_mismatches.len());

    Ok(RetrievalQueryShapeBenchmark {
        labels_file: labels_path.to_path_buf(),
        labels_sha256: sha256_file(labels_path)?,
        labeled_case_count,
        classification_accuracy: retrieval_ratio(correct, labeled_case_count),
        misclassification_rate: retrieval_ratio(mismatches.len(), labeled_case_count),
        confusion_matrix,
        mismatches,
        adversarial_probe_count: probe_count,
        adversarial_probe_accuracy: retrieval_ratio(probe_correct, probe_count),
        adversarial_probe_mismatches: probe_mismatches,
    })
}

'''+marker
replace_once(marker, query_loader, 'query shape loader/benchmark')

replace_once(
'''        id: case.id.clone(),
        task_family: case.task_family,
        language: case.language.clone(),''',
'''        id: case.id.clone(),
        task_family: case.task_family,
        expected_query_shape: case.expected_query_shape,
        actual_query_shape: open_kioku_context::routing::classify_task(&case.query).query_shape,
        language: case.language.clone(),''',
    'case report query shape values',
)

replace_once(
'''        by_task_family: summarize_retrieval_groups(&cases, |case| {
            case.task_family.label().into()
        }),
        by_split: summarize_retrieval_groups(&cases, |case| match case.split {''',
'''        by_task_family: summarize_retrieval_groups(&cases, |case| {
            case.task_family.label().into()
        }),
        by_query_shape: summarize_retrieval_groups_optional(&cases, |case| {
            case.expected_query_shape.map(|shape| query_shape_label(shape).into())
        }),
        by_task_family_query_shape: summarize_retrieval_groups_optional(&cases, |case| {
            case.expected_query_shape.map(|shape| {
                format!("{}:{}", case.task_family.label(), query_shape_label(shape))
            })
        }),
        by_split: summarize_retrieval_groups(&cases, |case| match case.split {''',
    'build query shape groups',
)

marker = '''fn summarize_retrieval_cases(cases: &[RetrievalCaseReport]) -> RetrievalMetricSummary {'''
helper = '''fn summarize_retrieval_groups_optional<F>(
    cases: &[RetrievalCaseReport],
    key: F,
) -> BTreeMap<String, RetrievalMetricSummary>
where
    F: Fn(&RetrievalCaseReport) -> Option<String>,
{
    let mut grouped = BTreeMap::<String, Vec<RetrievalCaseReport>>::new();
    for case in cases {
        if let Some(name) = key(case) {
            grouped.entry(name).or_default().push(case.clone());
        }
    }
    grouped
        .into_iter()
        .map(|(name, cases)| (name, summarize_retrieval_cases(&cases)))
        .collect()
}

'''+marker
replace_once(marker, helper, 'optional grouping helper')

replace_once(
'''    for (family, routed_summary) in &routed.by_task_family {
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
}''',
'''    for (family, routed_summary) in &routed.by_task_family {
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
    for (scope, routed_summary) in &routed.by_task_family_query_shape {
        let Some(fusion_summary) = fusion.by_task_family_query_shape.get(scope) else {
            continue;
        };
        comparisons.push(retrieval_strategy_comparison(
            &format!("task_family_query_shape:{scope}"),
            &routed_summary.quality,
            &fusion_summary.quality,
        ));
    }
    comparisons
}''',
    'comparison query shape scopes',
)

replace_once(
'''    if !report.baseline_deltas.is_empty() {
        out.push_str("## Regression deltas vs checked-in baseline''',
'''    if let Some(query_shape) = &report.query_shape_benchmark {
        out.push_str(&format!(
            "## Query-shape classification (frozen labels)\\n\\n- Labeled retrieval cases: `{}`\\n- Classification accuracy: `{:.3}`\\n- Misclassification rate: `{:.3}`\\n- Adversarial probe accuracy: `{:.3}` (`{}` probes)\\n- Label digest: `{}`\\n\\n",
            query_shape.labeled_case_count,
            query_shape.classification_accuracy,
            query_shape.misclassification_rate,
            query_shape.adversarial_probe_accuracy,
            query_shape.adversarial_probe_count,
            query_shape.labels_sha256
        ));
        if !query_shape.mismatches.is_empty() {
            out.push_str("Case-label mismatches:\\n\\n");
            for mismatch in &query_shape.mismatches {
                out.push_str(&format!(
                    "- `{}` expected `{}` but classified `{}`\\n",
                    mismatch.id,
                    query_shape_label(mismatch.expected),
                    query_shape_label(mismatch.actual)
                ));
            }
            out.push('\\n');
        }
        if !query_shape.adversarial_probe_mismatches.is_empty() {
            out.push_str("Adversarial probe mismatches:\\n\\n");
            for mismatch in &query_shape.adversarial_probe_mismatches {
                out.push_str(&format!(
                    "- `{}` expected `{}` but classified `{}`\\n",
                    mismatch.id,
                    query_shape_label(mismatch.expected),
                    query_shape_label(mismatch.actual)
                ));
            }
            out.push('\\n');
        }
    }
    if !report.baseline_deltas.is_empty() {
        out.push_str("## Regression deltas vs checked-in baseline''',
    'markdown query shape classification',
)

replace_once(
'''            out.push('\\n');
        }
    }
    out.push_str("## Reproducibility''',
'''            out.push('\\n');
            out.push_str("### Routed ContextPack by expected query shape\\n\\n| Query shape | R@10 | MRR | F1@10 | No-gold FP | p50 ms | p95 ms | Token-budget gold-file yield |\\n|---|---:|---:|---:|---:|---:|---:|---|\\n");
            for (shape, summary) in &routed.by_query_shape {
                let budgets = summary
                    .quality
                    .token_budget_gold_yield
                    .iter()
                    .map(|(budget, value)| format!("{budget}={value:.3}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.2} | {:.2} | {} |\\n",
                    shape,
                    summary.quality.recall_at_10,
                    summary.quality.mean_reciprocal_rank,
                    summary.quality.file_f1_at_10,
                    summary.quality.no_gold_false_positive_rate,
                    summary.latency.p50_ms,
                    summary.latency.p95_ms,
                    budgets
                ));
            }
            out.push('\\n');
            out.push_str("### Routed ContextPack by task family × expected query shape\\n\\n| Task family × query shape | R@10 | MRR | F1@10 | No-gold FP | p95 ms |\\n|---|---:|---:|---:|---:|---:|\\n");
            for (scope, summary) in &routed.by_task_family_query_shape {
                out.push_str(&format!(
                    "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.2} |\\n",
                    scope,
                    summary.quality.recall_at_10,
                    summary.quality.mean_reciprocal_rank,
                    summary.quality.file_f1_at_10,
                    summary.quality.no_gold_false_positive_rate,
                    summary.latency.p95_ms
                ));
            }
            out.push('\\n');
        }
    }
    out.push_str("## Reproducibility''',
    'markdown routed query shape tables',
)

# Update test helper with expected/actual shape fields.
replace_once(
'''            id: id.into(),
            task_family: RetrievalTaskFamily::IssueToCode,
            language: "rust".into(),''',
'''            id: id.into(),
            task_family: RetrievalTaskFamily::IssueToCode,
            expected_query_shape: Some(open_kioku_core::QueryShape::Conceptual),
            actual_query_shape: open_kioku_core::QueryShape::Conceptual,
            language: "rust".into(),''',
    'test report query shape fields',
)

# Report fixture constructors must include the additive top-level field.
text = text.replace(
'''            advisory_comparisons: Vec::new(),
            caveats: Vec::new(),''',
'''            advisory_comparisons: Vec::new(),
            query_shape_benchmark: None,
            caveats: Vec::new(),''',
)

# Extend comparison test to assert shape and family×shape scopes.
replace_once(
'''        assert_eq!(comparisons.len(), 2);''',
'''        assert_eq!(comparisons.len(), 4);''',
    'comparison count',
)
replace_once(
'''        assert_eq!(comparisons[1].scope, "task_family:issue_to_code");
    }
''',
'''        assert_eq!(comparisons[1].scope, "task_family:issue_to_code");
        assert_eq!(comparisons[2].scope, "query_shape:conceptual");
        assert_eq!(
            comparisons[3].scope,
            "task_family_query_shape:issue_to_code:conceptual"
        );
    }

    #[test]
    fn query_shape_labels_fail_closed_for_missing_or_unknown_case_ids() {
        let mut corpus: RetrievalCorpus = serde_json::from_str(r#"{
            "schema_version":"1.0.0",
            "corpus_id":"fixture",
            "token_budgets":[2000],
            "cases":[{
                "id":"known",
                "task_family":"issue_to_code",
                "language":"rust",
                "repo_fixture":"fixture",
                "base_revision":"sha256:a817b28e702d6f5e830fd02b0aa1c94a2c583c0a5406fa38151729dc41b074b6",
                "split":"holdout",
                "query":"how caching works",
                "gold_files":["src/lib.rs"]
            }]
        }"#).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("labels.json");
        fs::write(&path, r#"{
            "schema_version":"1.0.0",
            "corpus_id":"fixture",
            "cases":[{"id":"unknown","expected_query_shape":"conceptual"}]
        }"#).unwrap();
        let error = load_and_apply_query_shape_labels(&path, &mut corpus).unwrap_err();
        assert!(error.to_string().contains("unknown retrieval case"));
    }

    #[test]
    fn query_shape_benchmark_measures_misclassification_without_changing_gold() {
        let mut corpus: RetrievalCorpus = serde_json::from_str(r#"{
            "schema_version":"1.0.0",
            "corpus_id":"fixture",
            "token_budgets":[2000],
            "cases":[{
                "id":"known",
                "task_family":"issue_to_code",
                "language":"rust",
                "repo_fixture":"fixture",
                "base_revision":"sha256:a817b28e702d6f5e830fd02b0aa1c94a2c583c0a5406fa38151729dc41b074b6",
                "split":"holdout",
                "query":"AuthService",
                "gold_files":["src/lib.rs"]
            }]
        }"#).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("labels.json");
        fs::write(&path, r#"{
            "schema_version":"1.0.0",
            "corpus_id":"fixture",
            "cases":[{"id":"known","expected_query_shape":"conceptual"}],
            "adversarial_probes":[{"id":"probe","query":"AuthService","expected_query_shape":"exact_identifier"}]
        }"#).unwrap();
        let labels = load_and_apply_query_shape_labels(&path, &mut corpus).unwrap();
        let benchmark = build_query_shape_benchmark(&corpus, &labels, &path).unwrap();
        assert_eq!(benchmark.labeled_case_count, 1);
        assert_eq!(benchmark.classification_accuracy, 0.0);
        assert_eq!(benchmark.misclassification_rate, 1.0);
        assert_eq!(benchmark.adversarial_probe_accuracy, 1.0);
        assert_eq!(corpus.cases[0].gold_files, vec![PathBuf::from("src/lib.rs")]);
    }
''',
    'query shape tests',
)

path.write_text(text)
