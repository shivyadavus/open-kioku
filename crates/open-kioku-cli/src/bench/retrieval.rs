const RETRIEVAL_BENCH_SCHEMA_VERSION: &str = "1.0.0";
const RETRIEVAL_REPORT_VERSION: &str = "1.7.0";
const RETRIEVAL_QUERY_SHAPE_LABEL_SCHEMA_VERSION: &str = "1.0.0";
const RETRIEVAL_BASELINE_DIMENSIONS_VERSION: &str = "2.0.0";
const RETRIEVAL_TOKEN_ESTIMATOR: &str = "unicode_chars_div_4_plus_metadata_v1";
const RETRIEVAL_K_VALUES: [usize; 4] = [1, 5, 10, 20];
const RETRIEVAL_CC6_MAX_DEV_POSITIVE_ABSTENTION_RATE: f64 = 0.0;
const RETRIEVAL_CC6_MIN_DEV_NO_GOLD_ABSTENTION_RECALL: f64 = 0.0;

#[derive(Args, Debug, Clone)]
struct RetrievalBenchArgs {
    /// Base directory used to resolve repository fixtures in the corpus.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Versioned JSON corpus containing frozen retrieval cases.
    #[arg(long, default_value = "benchmarks/retrieval-cases.json")]
    cases_file: PathBuf,

    /// Maximum number of ranked file results retained per case.
    #[arg(long, default_value_t = 20)]
    limit: usize,

    /// Reuse fixture indexes instead of rebuilding them before the run.
    #[arg(long, default_value_t = false)]
    no_index: bool,

    /// Fail unless at least this many validated cases are loaded.
    #[arg(long, default_value_t = 30)]
    min_cases: usize,

    /// Minimum Fusion recall@10 required on the holdout split (or overall when no holdout exists).
    #[arg(long, default_value_t = 0.0)]
    min_fusion_recall_at_10: f64,

    /// Minimum Fusion MRR required on the holdout split (or overall when no holdout exists).
    #[arg(long, default_value_t = 0.0)]
    min_fusion_mrr: f64,

    /// Maximum no-gold false-positive rate allowed on the holdout split.
    #[arg(long, default_value_t = 1.0)]
    max_no_gold_false_positive_rate: f64,

    /// Write the complete machine-readable report, including observational latency.
    #[arg(long, value_name = "PATH")]
    write_json: Option<PathBuf>,

    /// Write a human-readable Markdown summary.
    #[arg(long, value_name = "PATH")]
    write_markdown: Option<PathBuf>,

    /// Write a deterministic quality-only baseline (latency intentionally excluded).
    #[arg(long, value_name = "PATH")]
    write_baseline: Option<PathBuf>,

    /// Checked-in deterministic quality baseline used to calculate report deltas.
    #[arg(long, default_value = "benchmarks/retrieval-baseline.json")]
    baseline_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetrievalCorpus {
    schema_version: String,
    corpus_id: String,
    #[serde(default = "default_retrieval_token_budgets")]
    token_budgets: Vec<usize>,
    cases: Vec<RetrievalCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetrievalCase {
    id: String,
    task_family: RetrievalTaskFamily,
    #[serde(skip)]
    expected_query_shape: Option<open_kioku_core::QueryShape>,
    language: String,
    repo_fixture: PathBuf,
    base_revision: String,
    split: RetrievalSplit,
    query: String,
    #[serde(default)]
    gold_files: Vec<PathBuf>,
    #[serde(default)]
    gold_symbols: Vec<String>,
    #[serde(default)]
    no_gold_expected: bool,
    #[serde(default)]
    token_budgets: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RetrievalTaskFamily {
    IssueToCode,
    CodeToTest,
    TraceToCode,
    CommentToContext,
    EditToRipple,
}

impl RetrievalTaskFamily {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RetrievalSplit {
    Development,
    Holdout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetrievalStrategy {
    Lexical,
    Fusion,
}

impl RetrievalStrategy {
    fn label(self) -> &'static str {
        match self {
            Self::Lexical => "lexical",
            Self::Fusion => "fusion",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct RetrievalReportProvenance {
    open_kioku_version: &'static str,
    corpus_revision: String,
    cases_sha256: String,
    frozen_fixture_revisions_verified: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RetrievalStrategyIdentity {
    algorithm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct RetrievalBaselineDelta {
    strategy: String,
    split: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    recall_at_10: f64,
    mean_reciprocal_rank: f64,
    file_f1_at_10: f64,
    no_gold_false_positive_rate: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct RetrievalStrategyComparison {
    candidate_strategy: String,
    baseline_strategy: String,
    scope: String,
    delta_recall_at_10: f64,
    delta_mean_reciprocal_rank: f64,
    delta_file_f1_at_10: f64,
    delta_no_gold_false_positive_rate: f64,
    delta_token_budget_gold_yield: BTreeMap<usize, f64>,
}

#[derive(Debug, Serialize)]
struct RetrievalBenchReport {
    schema_version: &'static str,
    report_version: &'static str,
    provenance: RetrievalReportProvenance,
    corpus_id: String,
    cases_file: PathBuf,
    case_count: usize,
    limit: usize,
    token_estimator: &'static str,
    fixture_digests: BTreeMap<String, String>,
    strategy_identities: BTreeMap<String, RetrievalStrategyIdentity>,
    baseline_deltas: Vec<RetrievalBaselineDelta>,
    advisory_comparisons: Vec<RetrievalStrategyComparison>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_shape_benchmark: Option<RetrievalQueryShapeBenchmark>,
    /// Development-selected, untouched-holdout CC6 calibration. Advisory only.
    abstention_calibration: Option<cc6_calibration::AbstentionCalibrationResult>,
    caveats: Vec<String>,
    strategies: Vec<RetrievalStrategyReport>,
    /// Advisory source/fusion/routing measurements. Excluded from the frozen generic retrieval
    /// quality baseline and release thresholds until explicitly promoted.
    stream_ablations: Vec<RetrievalStrategyReport>,
}

#[derive(Debug, Serialize)]
struct RetrievalStrategyReport {
    strategy: String,
    summary: RetrievalMetricSummary,
    by_language: BTreeMap<String, RetrievalMetricSummary>,
    by_task_family: BTreeMap<String, RetrievalMetricSummary>,
    by_query_shape: BTreeMap<String, RetrievalMetricSummary>,
    by_task_family_query_shape: BTreeMap<String, RetrievalMetricSummary>,
    by_split: BTreeMap<String, RetrievalMetricSummary>,
    cases: Vec<RetrievalCaseReport>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct RetrievalQualityMetrics {
    positive_cases: usize,
    no_gold_cases: usize,
    recall_at_1: f64,
    recall_at_5: f64,
    recall_at_10: f64,
    recall_at_20: f64,
    precision_at_1: f64,
    precision_at_5: f64,
    precision_at_10: f64,
    precision_at_20: f64,
    mean_reciprocal_rank: f64,
    file_f1_at_10: f64,
    no_gold_false_positive_rate: f64,
    token_budget_gold_yield: BTreeMap<usize, f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct RetrievalLatencyMetrics {
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct RetrievalAbstentionMetrics {
    abstained_cases: usize,
    correct_no_gold_abstentions: usize,
    incorrect_positive_abstentions: usize,
    precision: f64,
    recall: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct RetrievalMetricSummary {
    quality: RetrievalQualityMetrics,
    abstention: RetrievalAbstentionMetrics,
    latency: RetrievalLatencyMetrics,
}

#[derive(Debug, Clone, Serialize)]
struct RetrievalCaseReport {
    id: String,
    task_family: RetrievalTaskFamily,
    expected_query_shape: Option<open_kioku_core::QueryShape>,
    actual_query_shape: open_kioku_core::QueryShape,
    language: String,
    split: RetrievalSplit,
    repo_fixture: PathBuf,
    query: String,
    no_gold_expected: bool,
    gold_files: Vec<PathBuf>,
    gold_symbols: Vec<String>,
    ranked_paths: Vec<PathBuf>,
    gold_ranks: Vec<Option<usize>>,
    recall_at: BTreeMap<usize, f64>,
    precision_at: BTreeMap<usize, f64>,
    reciprocal_rank: f64,
    file_f1_at_10: f64,
    token_budget_gold_yield: BTreeMap<usize, f64>,
    token_budget_used: BTreeMap<usize, usize>,
    returned_any: bool,
    latency_ms: f64,
}

#[derive(Debug, Serialize, Deserialize)]
struct RetrievalQualityBaseline {
    schema_version: String,
    #[serde(default)]
    quality_dimensions_version: Option<String>,
    corpus_id: String,
    case_count: usize,
    token_estimator: String,
    fixture_digests: BTreeMap<String, String>,
    strategies: Vec<RetrievalStrategyQualityBaseline>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RetrievalStrategyQualityBaseline {
    strategy: String,
    summary: RetrievalQualityMetrics,
    by_language: BTreeMap<String, RetrievalQualityMetrics>,
    by_task_family: BTreeMap<String, RetrievalQualityMetrics>,
    #[serde(default)]
    by_query_shape: BTreeMap<String, RetrievalQualityMetrics>,
    #[serde(default)]
    by_task_family_query_shape: BTreeMap<String, RetrievalQualityMetrics>,
    by_split: BTreeMap<String, RetrievalQualityMetrics>,
}

fn default_retrieval_token_budgets() -> Vec<usize> {
    vec![2_000, 4_000, 8_000]
}

fn run_retrieval_bench(args: RetrievalBenchArgs) -> anyhow::Result<RetrievalBenchReport> {
    let root = absolutize(&args.path)?;
    let cases_file = absolutize(&args.cases_file)?;
    let mut corpus = load_retrieval_corpus(&cases_file)?;
    let query_shape_labels_path = query_shape_labels_path(&cases_file);
    let query_shape_labels = match query_shape_labels_path.as_ref() {
        Some(path) if path.is_file() => Some(load_and_apply_query_shape_labels(path, &mut corpus)?),
        _ => None,
    };
    if corpus.cases.len() < args.min_cases {
        anyhow::bail!(
            "retrieval benchmark loaded {} cases, below required {}",
            corpus.cases.len(),
            args.min_cases
        );
    }
    let limit = args.limit.clamp(20, 100);
    let fixtures = retrieval_fixture_paths(&root, &corpus.cases)?;
    validate_retrieval_gold_files(&root, &corpus.cases)?;
    let semantic_config = cc2_semantic_benchmark_config();
    let mut fixture_digests = BTreeMap::new();
    for fixture in fixtures.values() {
        if !args.no_index {
            index_repo(fixture)?;
            let store = open_store(fixture)?;
            SemanticIndexManager::new(fixture, &store, &semantic_config).rebuild()?;
        }
        let digest = retrieval_fixture_digest(fixture)?;
        fixture_digests.insert(
            fixture
                .strip_prefix(&root)
                .unwrap_or(fixture)
                .to_string_lossy()
                .replace('\\', "/"),
            digest,
        );
    }
    validate_retrieval_fixture_revisions(&root, &corpus.cases, &fixture_digests)?;

    let mut lexical_cases = Vec::with_capacity(corpus.cases.len());
    let mut fusion_cases = Vec::with_capacity(corpus.cases.len());
    let mut cc2_cases = BTreeMap::<String, Vec<RetrievalCaseReport>>::new();
    let mut cc6_abstention_cases = Vec::with_capacity(corpus.cases.len());
    for case in &corpus.cases {
        let fixture = fixtures
            .get(&case.repo_fixture)
            .ok_or_else(|| anyhow::anyhow!("fixture not prepared for case `{}`", case.id))?;
        let store = open_store(fixture)?;
        let budgets = if case.token_budgets.is_empty() {
            &corpus.token_budgets
        } else {
            &case.token_budgets
        };

        lexical_cases.push(run_retrieval_case(
            fixture,
            &store,
            case,
            budgets,
            limit,
            RetrievalStrategy::Lexical,
        )?);
        fusion_cases.push(run_retrieval_case(
            fixture,
            &store,
            case,
            budgets,
            limit,
            RetrievalStrategy::Fusion,
        )?);
        for source in cc2_benchmark_sources() {
            let label = format!("cc2:{}", retrieval_source_label(source));
            cc2_cases.entry(label).or_default().push(run_cc2_retrieval_case(
                &store,
                case,
                budgets,
                limit,
                Some(source),
                &open_kioku_context::candidates::FusionConfig::unweighted(),
            )?);
        }
        cc2_cases
            .entry("cc2:semantic_vector_local_hash".into())
            .or_default()
            .push(run_cc2_semantic_retrieval_case(
                fixture,
                &store,
                &semantic_config,
                case,
                budgets,
                limit,
            )?);
        cc2_cases
            .entry("cc2:rrf_unweighted".into())
            .or_default()
            .push(run_cc2_retrieval_case(
                &store,
                case,
                budgets,
                limit,
                None,
                &open_kioku_context::candidates::FusionConfig::unweighted(),
            )?);
        cc2_cases
            .entry("cc2:rrf_evidence_prior".into())
            .or_default()
            .push(run_cc2_retrieval_case(
                &store,
                case,
                budgets,
                limit,
                None,
                &open_kioku_context::candidates::FusionConfig::evidence_prior_weighted(),
            )?);
        let (routed_report, abstention_case) =
            run_routed_contextpack_retrieval_case(&store, case, budgets, limit)?;
        cc2_cases
            .entry("cc4:routed_contextpack".into())
            .or_default()
            .push(routed_report);
        cc6_abstention_cases.push(abstention_case);
    }

    let strategies = vec![
        build_retrieval_strategy_report(RetrievalStrategy::Lexical, lexical_cases),
        build_retrieval_strategy_report(RetrievalStrategy::Fusion, fusion_cases),
    ];
    let stream_ablations = cc2_cases
        .into_iter()
        .map(|(label, cases)| build_named_retrieval_strategy_report(label, cases))
        .collect::<Vec<_>>();
    let advisory_comparisons = routed_contextpack_comparisons(&strategies, &stream_ablations);
    let query_shape_benchmark = match (&query_shape_labels, &query_shape_labels_path) {
        (Some(labels), Some(path)) => Some(build_query_shape_benchmark(&corpus, labels, path)?),
        _ => None,
    };
    let (corpus_revision, revision_caveat) = retrieval_corpus_revision(&cases_file);
    let mut caveats = Vec::new();
    let abstention_calibration = Some(
        cc6_calibration::calibrate_abstention_policy(
            &cc6_abstention_cases,
            cc6_calibration::AbstentionCalibrationConstraints {
                max_positive_abstention_rate: RETRIEVAL_CC6_MAX_DEV_POSITIVE_ABSTENTION_RATE,
                min_no_gold_abstention_recall: RETRIEVAL_CC6_MIN_DEV_NO_GOLD_ABSTENTION_RECALL,
            },
        )
        .map_err(|error| anyhow::anyhow!("CC6 routed abstention calibration failed: {error}"))?,
    );
    if let Some(caveat) = revision_caveat {
        caveats.push(caveat);
    }
    caveats.push(
        "abstention precision/recall measures empty-result abstention behavior and remains advisory until CC6 calibration selects thresholds from the development/calibration split; no-gold false-positive rate remains the active release-gated negative-case signal".into(),
    );
    caveats.push(
        "cc4:routed_contextpack is advisory and executes task classification, query-shape classification, routing policy, routed candidate caps, fusion, budget selection, and ContextPack construction; it does not alter the frozen generic-fusion release gate".into(),
    );
    if query_shape_benchmark.is_none() {
        caveats.push(
            "query-shape labels are unavailable beside the retrieval corpus; query-shape quality and misclassification reporting are omitted".into(),
        );
    }
    let baseline_path = absolutize(&args.baseline_file)?;
    let baseline_deltas = if baseline_path.is_file() {
        let baseline = load_retrieval_quality_baseline(&baseline_path)?;
        compare_retrieval_baseline(
            &strategies,
            &baseline,
            &corpus.corpus_id,
            corpus.cases.len(),
            &fixture_digests,
            &mut caveats,
        )
    } else {
        caveats.push(format!(
            "checked-in retrieval baseline unavailable at {}; regression deltas omitted",
            baseline_path.display()
        ));
        Vec::new()
    };
    let report_cases_file = cases_file
        .strip_prefix(&root)
        .unwrap_or(&cases_file)
        .to_path_buf();
    let report = RetrievalBenchReport {
        schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,
        report_version: RETRIEVAL_REPORT_VERSION,
        provenance: RetrievalReportProvenance {
            open_kioku_version: env!("CARGO_PKG_VERSION"),
            corpus_revision,
            cases_sha256: sha256_file(&cases_file)?,
            frozen_fixture_revisions_verified: true,
        },
        corpus_id: corpus.corpus_id,
        cases_file: report_cases_file,
        case_count: corpus.cases.len(),
        limit,
        token_estimator: RETRIEVAL_TOKEN_ESTIMATOR,
        fixture_digests,
        strategy_identities: retrieval_strategy_identities(&semantic_config),
        baseline_deltas,
        advisory_comparisons,
        query_shape_benchmark,
        abstention_calibration,
        caveats,
        strategies,
        stream_ablations,
    };

    write_retrieval_outputs(&report, &args)?;
    Ok(report)
}

fn load_retrieval_corpus(path: &Path) -> anyhow::Result<RetrievalCorpus> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read retrieval corpus {}", path.display()))?;
    let mut corpus: RetrievalCorpus = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse retrieval corpus {}", path.display()))?;
    if corpus.schema_version != RETRIEVAL_BENCH_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported retrieval corpus schema {}; expected {}",
            corpus.schema_version,
            RETRIEVAL_BENCH_SCHEMA_VERSION
        );
    }
    if corpus.corpus_id.trim().is_empty() {
        anyhow::bail!("retrieval corpus id must be non-empty");
    }
    validate_token_budgets(&corpus.token_budgets, "corpus defaults")?;

    let mut ids = std::collections::HashSet::new();
    let mut holdout_count = 0usize;
    for case in &mut corpus.cases {
        if !ids.insert(case.id.clone()) {
            anyhow::bail!("duplicate retrieval case id `{}`", case.id);
        }
        if case.id.trim().is_empty()
            || case.query.trim().is_empty()
            || case.language.trim().is_empty()
            || case.base_revision.trim().is_empty()
        {
            anyhow::bail!(
                "retrieval case requires non-empty id, query, language, and base_revision"
            );
        }
        validate_fixture_revision(&case.base_revision, &case.id)?;
        validate_safe_relative_path(&case.repo_fixture, "repo_fixture", &case.id)?;
        let mut gold_paths = std::collections::HashSet::new();
        for gold in &case.gold_files {
            validate_safe_relative_path(gold, "gold_files", &case.id)?;
            let normalized = normalize_path_fragment(&gold.to_string_lossy());
            if !gold_paths.insert(normalized) {
                anyhow::bail!("retrieval case `{}` contains a duplicate gold file", case.id);
            }
        }
        if case.no_gold_expected && !case.gold_files.is_empty() {
            anyhow::bail!(
                "retrieval case `{}` is no-gold but also declares gold_files",
                case.id
            );
        }
        if !case.no_gold_expected && case.gold_files.is_empty() {
            anyhow::bail!(
                "retrieval case `{}` requires gold_files unless no_gold_expected=true",
                case.id
            );
        }
        if !case.token_budgets.is_empty() {
            validate_token_budgets(&case.token_budgets, &case.id)?;
        }
        if case.split == RetrievalSplit::Holdout {
            holdout_count += 1;
        }
    }
    if corpus.cases.is_empty() {
        anyhow::bail!("retrieval corpus contains no cases");
    }
    if holdout_count == 0 {
        anyhow::bail!("retrieval corpus must reserve at least one holdout case");
    }
    Ok(corpus)
}

fn query_shape_labels_path(cases_file: &Path) -> Option<PathBuf> {
    let stem = cases_file.file_stem()?.to_str()?;
    let prefix = stem.strip_suffix("-cases")?;
    Some(
        cases_file
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{prefix}-query-shape-labels.json")),
    )
}

fn load_and_apply_query_shape_labels(
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
            labels.corpus_id, corpus.corpus_id
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

    let report_labels_file = labels_path
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("query-shape-labels.json"));
    Ok(RetrievalQueryShapeBenchmark {
        labels_file: report_labels_file,
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

fn validate_token_budgets(budgets: &[usize], label: &str) -> anyhow::Result<()> {
    if budgets.is_empty() || budgets.contains(&0) {
        anyhow::bail!("retrieval token budgets for `{label}` must be non-empty and positive");
    }
    if budgets.windows(2).any(|pair| pair[0] >= pair[1]) {
        anyhow::bail!("retrieval token budgets for `{label}` must be strictly increasing");
    }
    Ok(())
}

fn validate_fixture_revision(revision: &str, case_id: &str) -> anyhow::Result<()> {
    let Some(hex) = revision.strip_prefix("sha256:") else {
        anyhow::bail!(
            "retrieval case `{case_id}` base_revision must be a frozen sha256 digest"
        );
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!(
            "retrieval case `{case_id}` base_revision is not a valid sha256 digest"
        );
    }
    Ok(())
}

fn validate_safe_relative_path(path: &Path, field: &str, case_id: &str) -> anyhow::Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        anyhow::bail!("retrieval case `{case_id}` {field} must be a relative path");
    }
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        anyhow::bail!("retrieval case `{case_id}` {field} must not escape its root");
    }
    Ok(())
}

fn retrieval_fixture_paths(
    root: &Path,
    cases: &[RetrievalCase],
) -> anyhow::Result<BTreeMap<PathBuf, PathBuf>> {
    let mut fixtures = BTreeMap::new();
    for case in cases {
        let path = root.join(&case.repo_fixture);
        if !path.is_dir() {
            anyhow::bail!(
                "retrieval fixture for case `{}` does not exist: {}",
                case.id,
                path.display()
            );
        }
        fixtures.entry(case.repo_fixture.clone()).or_insert(path);
    }
    Ok(fixtures)
}

fn validate_retrieval_gold_files(root: &Path, cases: &[RetrievalCase]) -> anyhow::Result<()> {
    for case in cases {
        let fixture = root.join(&case.repo_fixture);
        for gold in &case.gold_files {
            let gold_path = fixture.join(gold);
            if !gold_path.is_file() {
                anyhow::bail!(
                    "retrieval case `{}` gold file does not exist: {}",
                    case.id,
                    gold_path.display()
                );
            }
        }
    }
    Ok(())
}

fn retrieval_fixture_digest(fixture: &Path) -> anyhow::Result<String> {
    fn collect(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            if name == ".ok" || name == ".git" || name == "target" {
                continue;
            }
            if path.is_dir() {
                collect(root, &path, files)?;
            } else if path.is_file() {
                files.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    collect(fixture, fixture, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    hasher.update(b"open-kioku-retrieval-fixture-v1\0");
    for relative in files {
        hasher.update(relative.to_string_lossy().replace('\\', "/").as_bytes());
        hasher.update(b"\0");
        hasher.update(fs::read(fixture.join(&relative))?);
        hasher.update(b"\0");
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn validate_retrieval_fixture_revisions(
    root: &Path,
    cases: &[RetrievalCase],
    digests: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    for case in cases {
        let key = root
            .join(&case.repo_fixture)
            .strip_prefix(root)
            .unwrap_or(&case.repo_fixture)
            .to_string_lossy()
            .replace('\\', "/");
        let actual = digests.get(&key).ok_or_else(|| {
            anyhow::anyhow!("missing fixture digest for retrieval case `{}`", case.id)
        })?;
        if actual != &case.base_revision {
            anyhow::bail!(
                "retrieval fixture revision mismatch for `{}`: expected {}, got {}",
                case.id,
                case.base_revision,
                actual
            );
        }
    }
    Ok(())
}

fn run_retrieval_case(
    fixture: &Path,
    store: &dyn MetadataStore,
    case: &RetrievalCase,
    token_budgets: &[usize],
    limit: usize,
    strategy: RetrievalStrategy,
) -> anyhow::Result<RetrievalCaseReport> {
    let started = Instant::now();
    let candidates = retrieval_candidate_pool(fixture, store, &case.query, limit)?;
    let ranked = match strategy {
        RetrievalStrategy::Lexical => top_unique_paths(rerank_baseline(candidates), limit),
        RetrievalStrategy::Fusion => {
            let mut options = ranking_options_for_repo(fixture)?;
            options.mode = RankingMode::Fusion;
            options.query = Some(case.query.clone());
            top_unique_paths(rerank_with_options(candidates, &options), limit)
        }
    };
    let latency_ms = duration_ms(started.elapsed());
    Ok(score_retrieval_case(case, token_budgets, ranked, latency_ms))
}

fn cc2_semantic_benchmark_config() -> open_kioku_config::SemanticConfig {
    let mut config = OkConfig::default().semantic;
    config.enabled = true;
    config.provider = "local".into();
    config.model = "local-hash".into();
    config.backend = "exact-flat".into();
    config
}

fn cc2_benchmark_sources() -> [open_kioku_core::RetrievalSourceKind; 7] {
    [
        open_kioku_core::RetrievalSourceKind::Lexical,
        open_kioku_core::RetrievalSourceKind::Document,
        open_kioku_core::RetrievalSourceKind::ExactSemantic,
        open_kioku_core::RetrievalSourceKind::Graph,
        open_kioku_core::RetrievalSourceKind::Validation,
        open_kioku_core::RetrievalSourceKind::GitHistory,
        open_kioku_core::RetrievalSourceKind::Runtime,
    ]
}

fn retrieval_source_label(source: open_kioku_core::RetrievalSourceKind) -> &'static str {
    match source {
        open_kioku_core::RetrievalSourceKind::Lexical => "lexical",
        open_kioku_core::RetrievalSourceKind::Document => "document",
        open_kioku_core::RetrievalSourceKind::ExactSemantic => "exact_semantic",
        open_kioku_core::RetrievalSourceKind::Graph => "graph",
        open_kioku_core::RetrievalSourceKind::SemanticVector => "semantic_vector",
        open_kioku_core::RetrievalSourceKind::Validation => "validation",
        open_kioku_core::RetrievalSourceKind::GitHistory => "git_history",
        open_kioku_core::RetrievalSourceKind::Runtime => "runtime",
    }
}

fn run_cc2_semantic_retrieval_case(
    fixture: &Path,
    store: &SqliteStore,
    config: &open_kioku_config::SemanticConfig,
    case: &RetrievalCase,
    token_budgets: &[usize],
    limit: usize,
) -> anyhow::Result<RetrievalCaseReport> {
    let started = Instant::now();
    let manager = SemanticIndexManager::new(fixture, store as &dyn MetadataStore, config);
    let ranked = if manager.status().ready {
        let stream = open_kioku_context::candidates::CandidateStream::success(
            open_kioku_core::RetrievalSourceKind::SemanticVector,
            manager
                .search(&case.query, ranking_candidate_limit(limit))?
                .into_iter()
                .map(|result| {
                    open_kioku_context::candidates::StreamCandidate::from_result(
                        result,
                        open_kioku_core::RetrievalAuthority::Heuristic,
                        "current local-hash semantic-vector similarity",
                    )
                })
                .collect(),
        );
        top_unique_paths(
            open_kioku_context::candidates::fuse_candidate_streams(
                &[stream],
                limit,
                &open_kioku_context::candidates::FusionConfig::unweighted(),
            )
            .results,
            limit,
        )
    } else {
        Vec::new()
    };
    Ok(score_retrieval_case(
        case,
        token_budgets,
        ranked,
        duration_ms(started.elapsed()),
    ))
}

fn run_cc2_retrieval_case(
    store: &SqliteStore,
    case: &RetrievalCase,
    token_budgets: &[usize],
    limit: usize,
    only_source: Option<open_kioku_core::RetrievalSourceKind>,
    config: &open_kioku_context::candidates::FusionConfig,
) -> anyhow::Result<RetrievalCaseReport> {
    let started = Instant::now();
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = store.all_chunks()?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    let request = open_kioku_context::candidates::CandidateRequest::new(
        &case.query,
        expanded_task_search_terms(&case.query),
        ranking_candidate_limit(limit),
    );
    let context = open_kioku_context::candidates::builtins::BuiltinCandidateContext {
        store: store as &dyn OkStore,
        history_store: Some(store as &dyn HistoryStore),
        files: &files,
        chunks: &chunks,
        symbols: &symbols,
    };
    let streams = if let Some(source) = only_source {
        let excluded = cc2_benchmark_sources()
            .into_iter()
            .filter(|candidate| *candidate != source)
            .collect::<BTreeSet<_>>();
        context.collect_excluding(&request, &excluded)
    } else {
        context.collect(&request)
    };
    let ranked = top_unique_paths(
        open_kioku_context::candidates::fuse_candidate_streams(&streams, limit, config).results,
        limit,
    );
    let latency_ms = duration_ms(started.elapsed());
    Ok(score_retrieval_case(case, token_budgets, ranked, latency_ms))
}

fn run_routed_contextpack_retrieval_case(
    store: &SqliteStore,
    case: &RetrievalCase,
    token_budgets: &[usize],
    limit: usize,
) -> anyhow::Result<(RetrievalCaseReport, cc6_calibration::AbstentionCalibrationCase)> {
    let builder = open_kioku_context::ContextPackBuilder::new(
        store as &dyn open_kioku_storage::OkStore,
    )
    .with_history_store(Some(store as &dyn open_kioku_storage::HistoryStore));
    let started = Instant::now();
    let compatibility_pack = builder.build(&case.query, limit)?;
    let latency_ms = duration_ms(started.elapsed());
    let abstention_case = routed_abstention_calibration_case(case, &compatibility_pack)?;
    let mut report = score_retrieval_case(
        case,
        token_budgets,
        compatibility_pack.primary_files,
        latency_ms,
    );
    let gold = case
        .gold_files
        .iter()
        .map(|path| normalize_path_fragment(&path.to_string_lossy()))
        .collect::<Vec<_>>();

    for budget in token_budgets {
        let pack = builder.build_with_budget(
            &case.query,
            open_kioku_core::ContextBudget {
                max_tokens: *budget,
                reserve_for_instructions: 0,
                reserve_for_validation: 0,
                max_per_file: 2,
                max_primary_files: limit,
            },
        )?;
        let selected = pack
            .primary_files
            .iter()
            .map(|result| normalize_path_fragment(&result.path.to_string_lossy()))
            .collect::<std::collections::HashSet<_>>();
        let hits = gold.iter().filter(|path| selected.contains(*path)).count();
        report
            .token_budget_gold_yield
            .insert(*budget, retrieval_ratio(hits, gold.len()));
        report.token_budget_used.insert(
            *budget,
            pack.retrieval_diagnostics.selection.estimated_tokens_selected,
        );
    }

    Ok((report, abstention_case))
}

fn routed_abstention_calibration_case(
    case: &RetrievalCase,
    pack: &open_kioku_core::ContextPack,
) -> anyhow::Result<cc6_calibration::AbstentionCalibrationCase> {
    let traced = pack
        .primary_files
        .iter()
        .map(|result| {
            let unit = open_kioku_core::RetrievalUnitKey::from_result(result);
            let trace = pack
                .retrieval_diagnostics
                .traces
                .iter()
                .find(|trace| trace.unit_key.as_ref() == Some(&unit))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "CC6 calibration requires exact trace provenance for selected unit {:?}",
                        unit
                    )
                })?;
            Ok((
                trace.authority,
                result.score as f64,
                trace
                    .contributions
                    .iter()
                    .map(|contribution| contribution.source)
                    .collect::<BTreeSet<_>>()
                    .len(),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(cc6_calibration::AbstentionCalibrationCase {
        id: case.id.clone(),
        split: match case.split {
            RetrievalSplit::Development => cc6_calibration::CalibrationSplit::Development,
            RetrievalSplit::Holdout => cc6_calibration::CalibrationSplit::Holdout,
        },
        no_gold_expected: case.no_gold_expected,
        exact_evidence_present: pack.retrieval_diagnostics.selection.exact_evidence_count > 0,
        top_score_margin: same_authority_top_score_margin(&traced),
        independent_stream_count: traced.first().map(|(_, _, streams)| *streams).unwrap_or(0),
        ambiguity_unresolved_count: pack
            .retrieval_diagnostics
            .selection
            .ambiguity_unresolved_count,
    })
}

fn same_authority_top_score_margin(
    traced: &[(open_kioku_core::RetrievalAuthority, f64, usize)],
) -> Option<f64> {
    let (top_authority, top_score, _) = *traced.first()?;
    if !top_score.is_finite() {
        return None;
    }
    let second_score = traced
        .iter()
        .skip(1)
        .find_map(|(authority, score, _)| {
            (*authority == top_authority && score.is_finite()).then_some(*score)
        })?;
    let margin = top_score - second_score;
    (margin >= 0.0).then_some(margin)
}

fn retrieval_candidate_pool(
    fixture: &Path,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let candidate_limit = ranking_candidate_limit(limit);
    let mut candidates = BTreeMap::<String, SearchResult>::new();
    for term in expanded_task_search_terms(query) {
        for result in search_raw(fixture, store, &term, candidate_limit)? {
            let range = result
                .line_range
                .as_ref()
                .map(|range| format!("{}:{}", range.start, range.end))
                .unwrap_or_else(|| "-".into());
            let key = format!(
                "{}|{}|{}",
                normalize_path_fragment(&result.path.to_string_lossy()),
                range,
                result
                    .symbol
                    .as_ref()
                    .map(|symbol| symbol.qualified_name.as_str())
                    .unwrap_or("")
            );
            match candidates.get(&key) {
                Some(existing) if existing.score >= result.score => {}
                _ => {
                    candidates.insert(key, result);
                }
            }
        }
    }
    Ok(candidates.into_values().collect())
}

fn score_retrieval_case(
    case: &RetrievalCase,
    token_budgets: &[usize],
    ranked: Vec<SearchResult>,
    latency_ms: f64,
) -> RetrievalCaseReport {
    let gold = case
        .gold_files
        .iter()
        .map(|path| normalize_path_fragment(&path.to_string_lossy()))
        .collect::<Vec<_>>();
    let ranked_paths = ranked
        .iter()
        .map(|result| result.path.clone())
        .collect::<Vec<_>>();
    let ranked_normalized = ranked_paths
        .iter()
        .map(|path| normalize_path_fragment(&path.to_string_lossy()))
        .collect::<Vec<_>>();
    let gold_ranks = gold
        .iter()
        .map(|gold_path| {
            ranked_normalized
                .iter()
                .position(|path| path == gold_path)
                .map(|rank| rank + 1)
        })
        .collect::<Vec<_>>();

    let mut recall_at = BTreeMap::new();
    let mut precision_at = BTreeMap::new();
    for k in RETRIEVAL_K_VALUES {
        let hits = gold_ranks
            .iter()
            .filter(|rank| rank.is_some_and(|rank| rank <= k))
            .count();
        recall_at.insert(k, retrieval_ratio(hits, gold.len()));
        precision_at.insert(k, if case.no_gold_expected { 0.0 } else { hits as f64 / k as f64 });
    }
    let reciprocal_rank = gold_ranks
        .iter()
        .filter_map(|rank| *rank)
        .min()
        .map(|rank| 1.0 / rank as f64)
        .unwrap_or(0.0);
    let p10 = *precision_at.get(&10).unwrap_or(&0.0);
    let r10 = *recall_at.get(&10).unwrap_or(&0.0);
    let file_f1_at_10 = if p10 + r10 > 0.0 {
        2.0 * p10 * r10 / (p10 + r10)
    } else {
        0.0
    };

    let mut token_budget_gold_yield = BTreeMap::new();
    let mut token_budget_used = BTreeMap::new();
    for budget in token_budgets {
        let (selected, used) = pack_ranked_results(&ranked, *budget);
        let selected = selected
            .iter()
            .map(|result| normalize_path_fragment(&result.path.to_string_lossy()))
            .collect::<std::collections::HashSet<_>>();
        let hits = gold.iter().filter(|path| selected.contains(*path)).count();
        token_budget_gold_yield.insert(*budget, retrieval_ratio(hits, gold.len()));
        token_budget_used.insert(*budget, used);
    }

    RetrievalCaseReport {
        id: case.id.clone(),
        task_family: case.task_family,
        expected_query_shape: case.expected_query_shape,
        actual_query_shape: open_kioku_context::routing::classify_task(&case.query).query_shape,
        language: case.language.clone(),
        split: case.split,
        repo_fixture: case.repo_fixture.clone(),
        query: case.query.clone(),
        no_gold_expected: case.no_gold_expected,
        gold_files: case.gold_files.clone(),
        gold_symbols: case.gold_symbols.clone(),
        ranked_paths,
        gold_ranks,
        recall_at,
        precision_at,
        reciprocal_rank,
        file_f1_at_10,
        token_budget_gold_yield,
        token_budget_used,
        returned_any: !ranked.is_empty(),
        latency_ms,
    }
}

fn pack_ranked_results(ranked: &[SearchResult], budget: usize) -> (Vec<&SearchResult>, usize) {
    let mut selected = Vec::new();
    let mut used = 0usize;
    for result in ranked {
        let tokens = estimate_retrieval_tokens(result);
        if used.saturating_add(tokens) > budget {
            continue;
        }
        selected.push(result);
        used += tokens;
    }
    (selected, used)
}

fn estimate_retrieval_tokens(result: &SearchResult) -> usize {
    let chars = result.snippet.chars().count()
        + result.path.to_string_lossy().chars().count()
        + result
            .symbol
            .as_ref()
            .map(|symbol| symbol.qualified_name.chars().count())
            .unwrap_or(0);
    chars.div_ceil(4).saturating_add(12).max(1)
}

fn build_retrieval_strategy_report(
    strategy: RetrievalStrategy,
    cases: Vec<RetrievalCaseReport>,
) -> RetrievalStrategyReport {
    build_named_retrieval_strategy_report(strategy.label(), cases)
}

fn build_named_retrieval_strategy_report(
    strategy: impl Into<String>,
    cases: Vec<RetrievalCaseReport>,
) -> RetrievalStrategyReport {
    RetrievalStrategyReport {
        strategy: strategy.into(),
        summary: summarize_retrieval_cases(&cases),
        by_language: summarize_retrieval_groups(&cases, |case| case.language.clone()),
        by_task_family: summarize_retrieval_groups(&cases, |case| {
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
        by_split: summarize_retrieval_groups(&cases, |case| match case.split {
            RetrievalSplit::Development => "development".into(),
            RetrievalSplit::Holdout => "holdout".into(),
        }),
        cases,
    }
}

fn summarize_retrieval_groups<F>(
    cases: &[RetrievalCaseReport],
    key: F,
) -> BTreeMap<String, RetrievalMetricSummary>
where
    F: Fn(&RetrievalCaseReport) -> String,
{
    let mut grouped = BTreeMap::<String, Vec<RetrievalCaseReport>>::new();
    for case in cases {
        grouped.entry(key(case)).or_default().push(case.clone());
    }
    grouped
        .into_iter()
        .map(|(name, cases)| (name, summarize_retrieval_cases(&cases)))
        .collect()
}

fn summarize_retrieval_groups_optional<F>(
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

fn summarize_retrieval_cases(cases: &[RetrievalCaseReport]) -> RetrievalMetricSummary {
    let positives = cases
        .iter()
        .filter(|case| !case.no_gold_expected)
        .collect::<Vec<_>>();
    let no_gold = cases
        .iter()
        .filter(|case| case.no_gold_expected)
        .collect::<Vec<_>>();

    let metric_at = |source: &BTreeMap<usize, f64>, k| *source.get(&k).unwrap_or(&0.0);
    let mean_positive = |f: &dyn Fn(&RetrievalCaseReport) -> f64| {
        if positives.is_empty() {
            0.0
        } else {
            positives.iter().map(|case| f(case)).sum::<f64>() / positives.len() as f64
        }
    };
    let budgets = cases
        .iter()
        .flat_map(|case| case.token_budget_gold_yield.keys().copied())
        .collect::<BTreeSet<_>>();
    let token_budget_gold_yield = budgets
        .into_iter()
        .map(|budget| {
            let value = mean_positive(&|case| {
                *case.token_budget_gold_yield.get(&budget).unwrap_or(&0.0)
            });
            (budget, value)
        })
        .collect();
    let no_gold_false_positive_rate = if no_gold.is_empty() {
        0.0
    } else {
        no_gold.iter().filter(|case| case.returned_any).count() as f64 / no_gold.len() as f64
    };
    let correct_no_gold_abstentions = no_gold
        .iter()
        .filter(|case| !case.returned_any)
        .count();
    let incorrect_positive_abstentions = positives
        .iter()
        .filter(|case| !case.returned_any)
        .count();
    let abstained_cases = correct_no_gold_abstentions + incorrect_positive_abstentions;
    let abstention = RetrievalAbstentionMetrics {
        abstained_cases,
        correct_no_gold_abstentions,
        incorrect_positive_abstentions,
        precision: retrieval_ratio(correct_no_gold_abstentions, abstained_cases),
        recall: retrieval_ratio(correct_no_gold_abstentions, no_gold.len()),
    };
    let mut latencies = cases.iter().map(|case| case.latency_ms).collect::<Vec<_>>();
    latencies.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));

    RetrievalMetricSummary {
        quality: RetrievalQualityMetrics {
            positive_cases: positives.len(),
            no_gold_cases: no_gold.len(),
            recall_at_1: mean_positive(&|case| metric_at(&case.recall_at, 1)),
            recall_at_5: mean_positive(&|case| metric_at(&case.recall_at, 5)),
            recall_at_10: mean_positive(&|case| metric_at(&case.recall_at, 10)),
            recall_at_20: mean_positive(&|case| metric_at(&case.recall_at, 20)),
            precision_at_1: mean_positive(&|case| metric_at(&case.precision_at, 1)),
            precision_at_5: mean_positive(&|case| metric_at(&case.precision_at, 5)),
            precision_at_10: mean_positive(&|case| metric_at(&case.precision_at, 10)),
            precision_at_20: mean_positive(&|case| metric_at(&case.precision_at, 20)),
            mean_reciprocal_rank: mean_positive(&|case| case.reciprocal_rank),
            file_f1_at_10: mean_positive(&|case| case.file_f1_at_10),
            no_gold_false_positive_rate,
            token_budget_gold_yield,
        },
        abstention,
        latency: RetrievalLatencyMetrics {
            mean_ms: retrieval_mean(&latencies),
            p50_ms: retrieval_percentile(&latencies, 0.50),
            p95_ms: retrieval_percentile(&latencies, 0.95),
        },
    }
}

fn routed_contextpack_comparisons(
    strategies: &[RetrievalStrategyReport],
    advisory: &[RetrievalStrategyReport],
) -> Vec<RetrievalStrategyComparison> {
    let Some(fusion) = strategies
        .iter()
        .find(|strategy| strategy.strategy == RetrievalStrategy::Fusion.label())
    else {
        return Vec::new();
    };
    let Some(routed) = advisory
        .iter()
        .find(|strategy| strategy.strategy == "cc4:routed_contextpack")
    else {
        return Vec::new();
    };

    let mut comparisons = vec![retrieval_strategy_comparison(
        "overall",
        &routed.summary.quality,
        &fusion.summary.quality,
    )];
    for (family, routed_summary) in &routed.by_task_family {
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
}

fn retrieval_strategy_comparison(
    scope: &str,
    candidate: &RetrievalQualityMetrics,
    baseline: &RetrievalQualityMetrics,
) -> RetrievalStrategyComparison {
    let budgets = candidate
        .token_budget_gold_yield
        .keys()
        .chain(baseline.token_budget_gold_yield.keys())
        .copied()
        .collect::<BTreeSet<_>>();
    RetrievalStrategyComparison {
        candidate_strategy: "cc4:routed_contextpack".into(),
        baseline_strategy: RetrievalStrategy::Fusion.label().into(),
        scope: scope.into(),
        delta_recall_at_10: candidate.recall_at_10 - baseline.recall_at_10,
        delta_mean_reciprocal_rank: candidate.mean_reciprocal_rank - baseline.mean_reciprocal_rank,
        delta_file_f1_at_10: candidate.file_f1_at_10 - baseline.file_f1_at_10,
        delta_no_gold_false_positive_rate: candidate.no_gold_false_positive_rate
            - baseline.no_gold_false_positive_rate,
        delta_token_budget_gold_yield: budgets
            .into_iter()
            .map(|budget| {
                (
                    budget,
                    candidate
                        .token_budget_gold_yield
                        .get(&budget)
                        .copied()
                        .unwrap_or_default()
                        - baseline
                            .token_budget_gold_yield
                            .get(&budget)
                            .copied()
                            .unwrap_or_default(),
                )
            })
            .collect(),
    }
}

fn retrieval_ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn retrieval_mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn retrieval_percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[rank]
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn retrieval_corpus_revision(cases_file: &Path) -> (String, Option<String>) {
    let Some(source_root) = cases_file
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
    else {
        return (
            "unavailable".into(),
            Some("frozen corpus revision is unavailable because the corpus is not inside a git checkout; reproducibility remains anchored by corpus digest and fixture digests".into()),
        );
    };
    match ProcessCommand::new("git")
        .arg("-C")
        .arg(source_root)
        .args(["rev-parse", "HEAD"])
        .output()
    {
        Ok(output) if output.status.success() => {
            let revision = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                (revision, None)
            } else {
                (
                    "unavailable".into(),
                    Some("frozen corpus revision could not be validated as a full git commit; reproducibility remains anchored by corpus digest and fixture digests".into()),
                )
            }
        }
        Ok(_) | Err(_) => (
            "unavailable".into(),
            Some("frozen corpus revision is unavailable because git metadata could not be read; reproducibility remains anchored by corpus digest and fixture digests".into()),
        ),
    }
}

fn retrieval_strategy_identities(
    semantic: &open_kioku_config::SemanticConfig,
) -> BTreeMap<String, RetrievalStrategyIdentity> {
    let mut identities = BTreeMap::new();
    identities.insert(
        "lexical".into(),
        RetrievalStrategyIdentity {
            algorithm: "lexical_baseline".into(),
            provider: None,
            model: None,
            backend: None,
        },
    );
    identities.insert(
        "fusion".into(),
        RetrievalStrategyIdentity {
            algorithm: "ranking_fusion".into(),
            provider: None,
            model: None,
            backend: None,
        },
    );
    for source in cc2_benchmark_sources() {
        identities.insert(
            format!("cc2:{}", retrieval_source_label(source)),
            RetrievalStrategyIdentity {
                algorithm: format!("single_stream_{}", retrieval_source_label(source)),
                provider: None,
                model: None,
                backend: None,
            },
        );
    }
    identities.insert(
        "cc2:semantic_vector_local_hash".into(),
        RetrievalStrategyIdentity {
            algorithm: "semantic_vector".into(),
            provider: Some(semantic.provider.clone()),
            model: Some(semantic.model.clone()),
            backend: Some(semantic.backend.clone()),
        },
    );
    identities.insert(
        "cc2:rrf_unweighted".into(),
        RetrievalStrategyIdentity {
            algorithm: format!("rrf_unweighted_k{}", open_kioku_context::candidates::DEFAULT_RRF_K),
            provider: None,
            model: None,
            backend: None,
        },
    );
    identities.insert(
        "cc2:rrf_evidence_prior".into(),
        RetrievalStrategyIdentity {
            algorithm: format!("rrf_evidence_prior_k{}", open_kioku_context::candidates::DEFAULT_RRF_K),
            provider: None,
            model: None,
            backend: None,
        },
    );
    identities.insert(
        "cc4:routed_contextpack".into(),
        RetrievalStrategyIdentity {
            algorithm: "deterministic_task_family_routed_contextpack".into(),
            provider: None,
            model: None,
            backend: None,
        },
    );
    identities
}

fn load_retrieval_quality_baseline(path: &Path) -> anyhow::Result<RetrievalQualityBaseline> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read retrieval baseline {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse retrieval baseline {}", path.display()))
}

fn retrieval_baseline_delta(
    strategy: &str,
    split: &str,
    scope: Option<String>,
    current: &RetrievalQualityMetrics,
    previous: &RetrievalQualityMetrics,
) -> RetrievalBaselineDelta {
    RetrievalBaselineDelta {
        strategy: strategy.into(),
        split: split.into(),
        scope,
        recall_at_10: current.recall_at_10 - previous.recall_at_10,
        mean_reciprocal_rank: current.mean_reciprocal_rank - previous.mean_reciprocal_rank,
        file_f1_at_10: current.file_f1_at_10 - previous.file_f1_at_10,
        no_gold_false_positive_rate: current.no_gold_false_positive_rate
            - previous.no_gold_false_positive_rate,
    }
}

fn append_retrieval_group_deltas(
    deltas: &mut Vec<RetrievalBaselineDelta>,
    caveats: &mut Vec<String>,
    strategy: &str,
    split: &str,
    dimension_label: &str,
    current: &BTreeMap<String, RetrievalMetricSummary>,
    previous: &BTreeMap<String, RetrievalQualityMetrics>,
) {
    if current.keys().ne(previous.keys()) {
        caveats.push(format!(
            "strategy {strategy} {dimension_label} baseline keys differ from the live report; {dimension_label} regression deltas omitted for this strategy"
        ));
        return;
    }

    for (scope, current_summary) in current {
        let previous_quality = previous
            .get(scope)
            .expect("group key sets were checked above");
        deltas.push(retrieval_baseline_delta(
            strategy,
            split,
            Some(scope.clone()),
            &current_summary.quality,
            previous_quality,
        ));
    }
}

fn compare_retrieval_baseline(
    strategies: &[RetrievalStrategyReport],
    baseline: &RetrievalQualityBaseline,
    corpus_id: &str,
    case_count: usize,
    fixture_digests: &BTreeMap<String, String>,
    caveats: &mut Vec<String>,
) -> Vec<RetrievalBaselineDelta> {
    let mut incompatibilities = Vec::new();
    if baseline.schema_version != RETRIEVAL_BENCH_SCHEMA_VERSION {
        incompatibilities.push(format!(
            "schema {} != {}",
            baseline.schema_version, RETRIEVAL_BENCH_SCHEMA_VERSION
        ));
    }
    if baseline.quality_dimensions_version.as_deref()
        != Some(RETRIEVAL_BASELINE_DIMENSIONS_VERSION)
    {
        incompatibilities.push(format!(
            "quality_dimensions_version {} != {}; regenerate the retrieval baseline so query-shape quality is explicitly covered",
            baseline
                .quality_dimensions_version
                .as_deref()
                .unwrap_or("legacy/missing"),
            RETRIEVAL_BASELINE_DIMENSIONS_VERSION
        ));
    }
    if baseline.corpus_id != corpus_id {
        incompatibilities.push(format!(
            "corpus_id {} != {}",
            baseline.corpus_id, corpus_id
        ));
    }
    if baseline.case_count != case_count {
        incompatibilities.push(format!(
            "case_count {} != {}",
            baseline.case_count, case_count
        ));
    }
    if baseline.token_estimator != RETRIEVAL_TOKEN_ESTIMATOR {
        incompatibilities.push(format!(
            "token_estimator {} != {}",
            baseline.token_estimator, RETRIEVAL_TOKEN_ESTIMATOR
        ));
    }
    if baseline.fixture_digests != *fixture_digests {
        incompatibilities.push("fixture digests differ".into());
    }
    if !incompatibilities.is_empty() {
        caveats.push(format!(
            "retrieval baseline is incompatible ({}); regression deltas omitted",
            incompatibilities.join(", ")
        ));
        return Vec::new();
    }
    let mut deltas = Vec::new();
    for current in strategies {
        let Some(previous) = baseline
            .strategies
            .iter()
            .find(|candidate| candidate.strategy == current.strategy)
        else {
            caveats.push(format!(
                "strategy {} is absent from the checked-in baseline; its regression delta is unavailable",
                current.strategy
            ));
            continue;
        };
        let (split, current_quality, previous_quality) = match (
            current.by_split.get("holdout"),
            previous.by_split.get("holdout"),
        ) {
            (Some(current), Some(previous)) => ("holdout", &current.quality, previous),
            _ => ("overall", &current.summary.quality, &previous.summary),
        };
        deltas.push(retrieval_baseline_delta(
            &current.strategy,
            split,
            None,
            current_quality,
            previous_quality,
        ));

        append_retrieval_group_deltas(
            &mut deltas,
            caveats,
            &current.strategy,
            "language",
            "language",
            &current.by_language,
            &previous.by_language,
        );
        append_retrieval_group_deltas(
            &mut deltas,
            caveats,
            &current.strategy,
            "task_family",
            "task-family",
            &current.by_task_family,
            &previous.by_task_family,
        );

        if current.by_query_shape.keys().collect::<Vec<_>>()
            != previous.by_query_shape.keys().collect::<Vec<_>>()
        {
            caveats.push(format!(
                "strategy {} query-shape baseline keys differ from the live report; query-shape regression deltas omitted for this strategy",
                current.strategy
            ));
        } else {
            for (shape, current_summary) in &current.by_query_shape {
                let previous_quality = previous
                    .by_query_shape
                    .get(shape)
                    .expect("query-shape key sets were checked above");
                deltas.push(retrieval_baseline_delta(
                    &current.strategy,
                    "query_shape",
                    Some(shape.clone()),
                    &current_summary.quality,
                    previous_quality,
                ));
            }
        }

        if current
            .by_task_family_query_shape
            .keys()
            .collect::<Vec<_>>()
            != previous
                .by_task_family_query_shape
                .keys()
                .collect::<Vec<_>>()
        {
            caveats.push(format!(
                "strategy {} task-family/query-shape baseline keys differ from the live report; combined regression deltas omitted for this strategy",
                current.strategy
            ));
        } else {
            for (scope, current_summary) in &current.by_task_family_query_shape {
                let previous_quality = previous
                    .by_task_family_query_shape
                    .get(scope)
                    .expect("task-family/query-shape key sets were checked above");
                deltas.push(retrieval_baseline_delta(
                    &current.strategy,
                    "task_family_query_shape",
                    Some(scope.clone()),
                    &current_summary.quality,
                    previous_quality,
                ));
            }
        }
    }
    deltas.sort_by(|left, right| {
        left.strategy
            .cmp(&right.strategy)
            .then_with(|| left.split.cmp(&right.split))
            .then_with(|| left.scope.cmp(&right.scope))
    });
    deltas
}

fn retrieval_quality_baseline(report: &RetrievalBenchReport) -> RetrievalQualityBaseline {
    RetrievalQualityBaseline {
        schema_version: report.schema_version.to_string(),
        quality_dimensions_version: Some(RETRIEVAL_BASELINE_DIMENSIONS_VERSION.into()),
        corpus_id: report.corpus_id.clone(),
        case_count: report.case_count,
        token_estimator: report.token_estimator.to_string(),
        fixture_digests: report.fixture_digests.clone(),
        strategies: report
            .strategies
            .iter()
            .map(|strategy| RetrievalStrategyQualityBaseline {
                strategy: strategy.strategy.clone(),
                summary: strategy.summary.quality.clone(),
                by_language: strategy
                    .by_language
                    .iter()
                    .map(|(key, summary)| (key.clone(), summary.quality.clone()))
                    .collect(),
                by_task_family: strategy
                    .by_task_family
                    .iter()
                    .map(|(key, summary)| (key.clone(), summary.quality.clone()))
                    .collect(),
                by_query_shape: strategy
                    .by_query_shape
                    .iter()
                    .map(|(key, summary)| (key.clone(), summary.quality.clone()))
                    .collect(),
                by_task_family_query_shape: strategy
                    .by_task_family_query_shape
                    .iter()
                    .map(|(key, summary)| (key.clone(), summary.quality.clone()))
                    .collect(),
                by_split: strategy
                    .by_split
                    .iter()
                    .map(|(key, summary)| (key.clone(), summary.quality.clone()))
                    .collect(),
            })
            .collect(),
    }
}

fn retrieval_gate_quality(report: &RetrievalBenchReport) -> anyhow::Result<&RetrievalQualityMetrics> {
    let fusion = report
        .strategies
        .iter()
        .find(|strategy| strategy.strategy == RetrievalStrategy::Fusion.label())
        .ok_or_else(|| anyhow::anyhow!("retrieval benchmark did not produce Fusion results"))?;
    Ok(fusion
        .by_split
        .get("holdout")
        .map(|summary| &summary.quality)
        .unwrap_or(&fusion.summary.quality))
}

fn write_retrieval_outputs(
    report: &RetrievalBenchReport,
    args: &RetrievalBenchArgs,
) -> anyhow::Result<()> {
    if let Some(path) = &args.write_json {
        write_retrieval_file(path, &serde_json::to_string_pretty(report)?)?;
    }
    if let Some(path) = &args.write_markdown {
        write_retrieval_file(path, &render_retrieval_markdown(report))?;
    }
    if let Some(path) = &args.write_baseline {
        write_retrieval_file(
            path,
            &serde_json::to_string_pretty(&retrieval_quality_baseline(report))?,
        )?;
    }
    Ok(())
}

fn write_retrieval_file(path: &Path, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn render_retrieval_markdown(report: &RetrievalBenchReport) -> String {
    let mut out = format!(
        "# Repository Context Retrieval Benchmark\n\n- Corpus: `{}`\n- Cases: {}\n- Result limit: {}\n- Token estimator: `{}`\n- Open Kioku version: `{}`\n- Frozen corpus revision: `{}`\n- Corpus file digest: `{}`\n\n",
        report.corpus_id,
        report.case_count,
        report.limit,
        report.token_estimator,
        report.provenance.open_kioku_version,
        report.provenance.corpus_revision,
        report.provenance.cases_sha256
    );
    for strategy in &report.strategies {
        let quality = &strategy.summary.quality;
        out.push_str(&format!(
            "## {}\n\n| Metric | Value |\n|---|---:|\n| Recall@1 | {:.3} |\n| Recall@5 | {:.3} |\n| Recall@10 | {:.3} |\n| Recall@20 | {:.3} |\n| Precision@10 | {:.3} |\n| MRR | {:.3} |\n| File F1@10 | {:.3} |\n| No-gold false-positive rate | {:.3} |\n| Abstention precision (advisory) | {:.3} |\n| Abstention recall (advisory) | {:.3} |\n| p50 latency (observational) | {:.2} ms |\n| p95 latency (observational) | {:.2} ms |\n\n",
            strategy.strategy,
            quality.recall_at_1,
            quality.recall_at_5,
            quality.recall_at_10,
            quality.recall_at_20,
            quality.precision_at_10,
            quality.mean_reciprocal_rank,
            quality.file_f1_at_10,
            quality.no_gold_false_positive_rate,
            strategy.summary.abstention.precision,
            strategy.summary.abstention.recall,
            strategy.summary.latency.p50_ms,
            strategy.summary.latency.p95_ms
        ));
        out.push_str("### Token-budget gold-file yield\n\n");
        for (budget, value) in &quality.token_budget_gold_yield {
            out.push_str(&format!("- {} tokens: {:.3}\n", budget, value));
        }
        out.push_str("\n### Holdout\n\n");
        if let Some(holdout) = strategy.by_split.get("holdout") {
            out.push_str(&format!(
                "Recall@10 {:.3}, MRR {:.3}, no-gold FP {:.3}.\n\n",
                holdout.quality.recall_at_10,
                holdout.quality.mean_reciprocal_rank,
                holdout.quality.no_gold_false_positive_rate
            ));
        } else {
            out.push_str("No holdout cases.\n\n");
        }
    }
    if let Some(calibration) = &report.abstention_calibration {
        out.push_str("## CC6 abstention calibration (advisory)\n\n");
        out.push_str(&format!(
            "- Development constraints: positive abstention <= `{:.3}`, no-gold recall >= `{:.3}`\n- Selected policy: margin `{:?}`, independent streams `{:?}`, unresolved ambiguity max `{:?}`\n- Development: no-gold recall `{:.3}`, positive abstention `{:.3}`\n- Untouched holdout: no-gold recall `{:.3}`, positive abstention `{:.3}`\n\n",
            calibration.constraints.max_positive_abstention_rate,
            calibration.constraints.min_no_gold_abstention_recall,
            calibration.policy.min_top_score_margin,
            calibration.policy.min_independent_streams,
            calibration.policy.max_ambiguity_unresolved,
            calibration.development.no_gold_recall,
            calibration.development.positive_abstention_rate,
            calibration.holdout.no_gold_recall,
            calibration.holdout.positive_abstention_rate,
        ));
    }

    if let Some(query_shape) = &report.query_shape_benchmark {
        out.push_str(&format!(
            "## Query-shape classification (frozen labels)\n\n- Labeled retrieval cases: `{}`\n- Classification accuracy: `{:.3}`\n- Misclassification rate: `{:.3}`\n- Adversarial probe accuracy: `{:.3}` (`{}` probes)\n- Label digest: `{}`\n\n",
            query_shape.labeled_case_count,
            query_shape.classification_accuracy,
            query_shape.misclassification_rate,
            query_shape.adversarial_probe_accuracy,
            query_shape.adversarial_probe_count,
            query_shape.labels_sha256
        ));
        if !query_shape.mismatches.is_empty() {
            out.push_str("Case-label mismatches:\n\n");
            for mismatch in &query_shape.mismatches {
                out.push_str(&format!(
                    "- `{}` expected `{}` but classified `{}`\n",
                    mismatch.id,
                    query_shape_label(mismatch.expected),
                    query_shape_label(mismatch.actual)
                ));
            }
            out.push('\n');
        }
        if !query_shape.adversarial_probe_mismatches.is_empty() {
            out.push_str("Adversarial probe mismatches:\n\n");
            for mismatch in &query_shape.adversarial_probe_mismatches {
                out.push_str(&format!(
                    "- `{}` expected `{}` but classified `{}`\n",
                    mismatch.id,
                    query_shape_label(mismatch.expected),
                    query_shape_label(mismatch.actual)
                ));
            }
            out.push('\n');
        }
    }
    if !report.baseline_deltas.is_empty() {
        out.push_str("## Regression deltas vs checked-in baseline\n\nPositive Recall/MRR/F1 is improvement; negative no-gold FP is improvement.\n\n| Strategy | Dimension | Scope | Δ R@10 | Δ MRR | Δ F1@10 | Δ no-gold FP |\n|---|---|---|---:|---:|---:|---:|\n");
        for delta in &report.baseline_deltas {
            out.push_str(&format!(
                "| {} | {} | {} | {:+.3} | {:+.3} | {:+.3} | {:+.3} |\n",
                delta.strategy,
                delta.split,
                delta.scope.as_deref().unwrap_or("-"),
                delta.recall_at_10,
                delta.mean_reciprocal_rank,
                delta.file_f1_at_10,
                delta.no_gold_false_positive_rate
            ));
        }
        out.push('\n');
    }
    if !report.advisory_comparisons.is_empty() {
        out.push_str("## Routed ContextPack vs generic fusion (advisory)\n\nPositive Recall/MRR/F1/token-yield delta is improvement; negative no-gold FP delta is improvement.\n\n| Scope | Δ R@10 | Δ MRR | Δ F1@10 | Δ no-gold FP | Token-budget gold-yield deltas |\n|---|---:|---:|---:|---:|---|\n");
        for comparison in &report.advisory_comparisons {
            let budget_deltas = comparison
                .delta_token_budget_gold_yield
                .iter()
                .map(|(budget, delta)| format!("{budget}={delta:+.3}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "| {} | {:+.3} | {:+.3} | {:+.3} | {:+.3} | {} |\n",
                comparison.scope,
                comparison.delta_recall_at_10,
                comparison.delta_mean_reciprocal_rank,
                comparison.delta_file_f1_at_10,
                comparison.delta_no_gold_false_positive_rate,
                budget_deltas
            ));
        }
        out.push('\n');
    }
    if !report.caveats.is_empty() {
        out.push_str("## Caveats\n\n");
        for caveat in &report.caveats {
            out.push_str(&format!("- {caveat}\n"));
        }
        out.push('\n');
    }
    if !report.stream_ablations.is_empty() {
        out.push_str("## Advisory retrieval strategies\n\nThese measurements are excluded from the frozen generic retrieval release baseline. `cc2:semantic_vector_local_hash` measures the current deterministic local-hash/exact-flat backend; `cc4:routed_contextpack` measures the complete deterministic task-family routing and ContextPack path.\n\n");
        out.push_str("| Strategy | R@5 | R@10 | MRR | F1@10 | No-gold FP | Holdout R@10 | Holdout MRR | p95 ms |\n|---|---:|---:|---:|---:|---:|---:|---:|---:|\n");
        for strategy in &report.stream_ablations {
            let quality = &strategy.summary.quality;
            let holdout = strategy.by_split.get("holdout").unwrap_or(&strategy.summary);
            out.push_str(&format!(
                "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.3} | {:.2} |\n",
                strategy.strategy,
                quality.recall_at_5,
                quality.recall_at_10,
                quality.mean_reciprocal_rank,
                quality.file_f1_at_10,
                quality.no_gold_false_positive_rate,
                holdout.quality.recall_at_10,
                holdout.quality.mean_reciprocal_rank,
                strategy.summary.latency.p95_ms,
            ));
        }
        out.push('\n');
        if let Some(routed) = report
            .stream_ablations
            .iter()
            .find(|strategy| strategy.strategy == "cc4:routed_contextpack")
        {
            out.push_str("### Routed ContextPack by task family\n\n| Task family | R@10 | MRR | F1@10 | No-gold FP | Token-budget gold-file yield |\n|---|---:|---:|---:|---:|---|\n");
            for (family, summary) in &routed.by_task_family {
                let budgets = summary
                    .quality
                    .token_budget_gold_yield
                    .iter()
                    .map(|(budget, value)| format!("{budget}={value:.3}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {} |\n",
                    family,
                    summary.quality.recall_at_10,
                    summary.quality.mean_reciprocal_rank,
                    summary.quality.file_f1_at_10,
                    summary.quality.no_gold_false_positive_rate,
                    budgets
                ));
            }
            out.push('\n');
            out.push_str("### Routed ContextPack by expected query shape\n\n| Query shape | R@10 | MRR | F1@10 | No-gold FP | p50 ms | p95 ms | Token-budget gold-file yield |\n|---|---:|---:|---:|---:|---:|---:|---|\n");
            for (shape, summary) in &routed.by_query_shape {
                let budgets = summary
                    .quality
                    .token_budget_gold_yield
                    .iter()
                    .map(|(budget, value)| format!("{budget}={value:.3}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                out.push_str(&format!(
                    "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.2} | {:.2} | {} |\n",
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
            out.push('\n');
            out.push_str("### Routed ContextPack by task family × expected query shape\n\n| Task family × query shape | R@10 | MRR | F1@10 | No-gold FP | p95 ms |\n|---|---:|---:|---:|---:|---:|\n");
            for (scope, summary) in &routed.by_task_family_query_shape {
                out.push_str(&format!(
                    "| {} | {:.3} | {:.3} | {:.3} | {:.3} | {:.2} |\n",
                    scope,
                    summary.quality.recall_at_10,
                    summary.quality.mean_reciprocal_rank,
                    summary.quality.file_f1_at_10,
                    summary.quality.no_gold_false_positive_rate,
                    summary.latency.p95_ms
                ));
            }
            out.push('\n');
        }
    }
    out.push_str("## Reproducibility\n\nLatency is reported for observability but excluded from the checked-in deterministic quality baseline. Fixture content digests, corpus schema, and baseline quality-dimension version are part of compatibility checks so corpus or query-shape coverage drift is visible. Advisory retrieval measurements are also excluded until explicitly promoted.\n");
    out
}

fn print_retrieval_bench_report(report: &RetrievalBenchReport) {
    println!(
        "Retrieval benchmark: {} case(s), corpus {}, limit {}",
        report.case_count, report.corpus_id, report.limit
    );
    for strategy in &report.strategies {
        let quality = &strategy.summary.quality;
        println!(
            "{}: R@1 {:.3}, R@5 {:.3}, R@10 {:.3}, R@20 {:.3}, MRR {:.3}, F1@10 {:.3}, no-gold FP {:.3}, p95 {:.2}ms",
            strategy.strategy,
            quality.recall_at_1,
            quality.recall_at_5,
            quality.recall_at_10,
            quality.recall_at_20,
            quality.mean_reciprocal_rank,
            quality.file_f1_at_10,
            quality.no_gold_false_positive_rate,
            strategy.summary.latency.p95_ms
        );
    }
    if !report.stream_ablations.is_empty() {
        println!("Advisory retrieval strategies (excluded from frozen baseline):");
        for strategy in &report.stream_ablations {
            let quality = &strategy.summary.quality;
            println!(
                "  {}: R@5 {:.3}, R@10 {:.3}, MRR {:.3}, no-gold FP {:.3}, p95 {:.2}ms",
                strategy.strategy,
                quality.recall_at_5,
                quality.recall_at_10,
                quality.mean_reciprocal_rank,
                quality.no_gold_false_positive_rate,
                strategy.summary.latency.p95_ms
            );
        }
    }
}

#[cfg(test)]
mod retrieval_bench_tests {
    use super::*;

    fn report(id: &str, no_gold: bool, ranks: &[Option<usize>]) -> RetrievalCaseReport {
        let mut recall_at = BTreeMap::new();
        let mut precision_at = BTreeMap::new();
        for k in RETRIEVAL_K_VALUES {
            let hits = ranks
                .iter()
                .filter(|rank| rank.is_some_and(|rank| rank <= k))
                .count();
            recall_at.insert(k, retrieval_ratio(hits, ranks.len()));
            precision_at.insert(k, if no_gold { 0.0 } else { hits as f64 / k as f64 });
        }
        RetrievalCaseReport {
            id: id.into(),
            task_family: RetrievalTaskFamily::IssueToCode,
            expected_query_shape: Some(open_kioku_core::QueryShape::Conceptual),
            actual_query_shape: open_kioku_core::QueryShape::Conceptual,
            language: "rust".into(),
            split: RetrievalSplit::Development,
            repo_fixture: "fixture".into(),
            query: "query".into(),
            no_gold_expected: no_gold,
            gold_files: if no_gold { Vec::new() } else { vec!["src/lib.rs".into()] },
            gold_symbols: Vec::new(),
            ranked_paths: Vec::new(),
            gold_ranks: ranks.to_vec(),
            recall_at,
            precision_at,
            reciprocal_rank: ranks
                .iter()
                .filter_map(|rank| *rank)
                .min()
                .map(|rank| 1.0 / rank as f64)
                .unwrap_or(0.0),
            file_f1_at_10: 0.0,
            token_budget_gold_yield: BTreeMap::from([(2_000, if no_gold { 0.0 } else { 1.0 })]),
            token_budget_used: BTreeMap::from([(2_000, 100)]),
            returned_any: no_gold,
            latency_ms: 10.0,
        }
    }

    fn report_with_shape(
        id: &str,
        task_family: RetrievalTaskFamily,
        shape: open_kioku_core::QueryShape,
        rank: usize,
    ) -> RetrievalCaseReport {
        let mut case = report(id, false, &[Some(rank)]);
        case.task_family = task_family;
        case.expected_query_shape = Some(shape);
        case.actual_query_shape = shape;
        case
    }

    fn quality_baseline_for(
        strategy: &RetrievalStrategyReport,
    ) -> RetrievalStrategyQualityBaseline {
        RetrievalStrategyQualityBaseline {
            strategy: strategy.strategy.clone(),
            summary: strategy.summary.quality.clone(),
            by_language: strategy
                .by_language
                .iter()
                .map(|(key, value)| (key.clone(), value.quality.clone()))
                .collect(),
            by_task_family: strategy
                .by_task_family
                .iter()
                .map(|(key, value)| (key.clone(), value.quality.clone()))
                .collect(),
            by_query_shape: strategy
                .by_query_shape
                .iter()
                .map(|(key, value)| (key.clone(), value.quality.clone()))
                .collect(),
            by_task_family_query_shape: strategy
                .by_task_family_query_shape
                .iter()
                .map(|(key, value)| (key.clone(), value.quality.clone()))
                .collect(),
            by_split: strategy
                .by_split
                .iter()
                .map(|(key, value)| (key.clone(), value.quality.clone()))
                .collect(),
        }
    }

    fn compatible_baseline(
        case_count: usize,
        strategy: RetrievalStrategyQualityBaseline,
    ) -> RetrievalQualityBaseline {
        RetrievalQualityBaseline {
            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION.into(),
            quality_dimensions_version: Some(RETRIEVAL_BASELINE_DIMENSIONS_VERSION.into()),
            corpus_id: "fixture".into(),
            case_count,
            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR.into(),
            fixture_digests: BTreeMap::new(),
            strategies: vec![strategy],
        }
    }

    fn report_with_deltas(deltas: Vec<RetrievalBaselineDelta>) -> RetrievalBenchReport {
        RetrievalBenchReport {
            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,
            report_version: RETRIEVAL_REPORT_VERSION,
            provenance: RetrievalReportProvenance {
                open_kioku_version: env!("CARGO_PKG_VERSION"),
                corpus_revision: "0123456789012345678901234567890123456789".into(),
                cases_sha256: "sha256:test".into(),
                frozen_fixture_revisions_verified: true,
            },
            corpus_id: "fixture".into(),
            cases_file: "cases.json".into(),
            case_count: 0,
            limit: 20,
            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR,
            fixture_digests: BTreeMap::new(),
            strategy_identities: BTreeMap::new(),
            baseline_deltas: deltas,
            advisory_comparisons: Vec::new(),
            query_shape_benchmark: None,
            abstention_calibration: None,
            caveats: Vec::new(),
            strategies: Vec::new(),
            stream_ablations: Vec::new(),
        }
    }

    #[test]
    fn cc6_margin_uses_only_the_top_authority_tier() {
        use open_kioku_core::RetrievalAuthority;
        let margin = same_authority_top_score_margin(&[
            (RetrievalAuthority::Exact, 0.90, 2),
            (RetrievalAuthority::Heuristic, 0.89, 4),
            (RetrievalAuthority::Exact, 0.60, 1),
        ])
        .unwrap();
        assert!((margin - 0.30).abs() < 1e-12);
    }

    #[test]
    fn cc6_margin_fails_closed_without_comparable_monotonic_evidence() {
        use open_kioku_core::RetrievalAuthority;
        assert_eq!(
            same_authority_top_score_margin(&[
                (RetrievalAuthority::Exact, 0.9, 1),
                (RetrievalAuthority::Heuristic, 0.1, 1),
            ]),
            None
        );
        assert_eq!(
            same_authority_top_score_margin(&[
                (RetrievalAuthority::Heuristic, 0.2, 1),
                (RetrievalAuthority::Heuristic, 0.3, 1),
            ]),
            None
        );
    }

    #[test]
    fn abstention_quality_reports_precision_and_recall_without_tuning_holdout() {
        let mut positive_returned = report("positive-returned", false, &[Some(1)]);
        positive_returned.returned_any = true;
        let mut positive_abstained = report("positive-abstained", false, &[None]);
        positive_abstained.returned_any = false;
        let mut no_gold_abstained = report("no-gold-abstained", true, &[]);
        no_gold_abstained.returned_any = false;
        let mut no_gold_returned = report("no-gold-returned", true, &[]);
        no_gold_returned.returned_any = true;

        let summary = summarize_retrieval_cases(&[
            positive_returned,
            positive_abstained,
            no_gold_abstained,
            no_gold_returned,
        ]);

        assert_eq!(summary.abstention.abstained_cases, 2);
        assert_eq!(summary.abstention.correct_no_gold_abstentions, 1);
        assert_eq!(summary.abstention.incorrect_positive_abstentions, 1);
        assert_eq!(summary.abstention.precision, 0.5);
        assert_eq!(summary.abstention.recall, 0.5);
        assert_eq!(summary.quality.no_gold_false_positive_rate, 0.5);
    }

    #[test]
    fn abstention_quality_is_serialized_for_breakdown_slices() {
        let mut positive = report("positive", false, &[Some(1)]);
        positive.returned_any = true;
        let mut no_gold = report("no-gold", true, &[]);
        no_gold.returned_any = false;
        let strategy = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![positive, no_gold],
        );

        let value = serde_json::to_value(&strategy).unwrap();
        assert_eq!(value.pointer("/summary/abstention/precision").and_then(serde_json::Value::as_f64), Some(1.0));
        assert_eq!(value.pointer("/summary/abstention/recall").and_then(serde_json::Value::as_f64), Some(1.0));
        assert_eq!(
            value.pointer("/by_task_family/issue_to_code/abstention/correct_no_gold_abstentions")
                .and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert_eq!(
            value.pointer("/by_language/rust/abstention/incorrect_positive_abstentions")
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );
    }

    #[test]
    fn routed_contextpack_comparison_reports_overall_and_task_family_deltas() {
        let fusion_cases = vec![report("fusion", false, &[Some(1)])];
        let routed_cases = vec![report("routed", false, &[Some(2)])];
        let strategies = vec![build_named_retrieval_strategy_report(
            "fusion",
            fusion_cases,
        )];
        let advisory = vec![build_named_retrieval_strategy_report(
            "cc4:routed_contextpack",
            routed_cases,
        )];

        let comparisons = routed_contextpack_comparisons(&strategies, &advisory);

        assert_eq!(comparisons.len(), 4);
        assert_eq!(comparisons[0].scope, "overall");
        assert!(comparisons[0].delta_mean_reciprocal_rank < 0.0);
        assert_eq!(
            comparisons[0]
                .delta_token_budget_gold_yield
                .get(&2_000),
            Some(&0.0)
        );
        assert_eq!(comparisons[1].scope, "task_family:issue_to_code");
        assert_eq!(comparisons[2].scope, "query_shape:conceptual");
        assert_eq!(
            comparisons[3].scope,
            "task_family_query_shape:issue_to_code:conceptual"
        );
    }

    #[test]
    fn query_shape_sidecar_discovery_is_corpus_derived_and_report_path_is_portable() {
        assert_eq!(
            query_shape_labels_path(Path::new("benchmarks/retrieval-cases.json")),
            Some(PathBuf::from("benchmarks/retrieval-query-shape-labels.json"))
        );
        assert_eq!(
            query_shape_labels_path(Path::new("benchmarks/custom-cases.json")),
            Some(PathBuf::from("benchmarks/custom-query-shape-labels.json"))
        );
        assert_eq!(query_shape_labels_path(Path::new("benchmarks/custom.json")), None);
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

    #[test]
    fn corpus_schema_rejects_unknown_fields() {
        let unknown_corpus = r#"{
            "schema_version": "1.0.0",
            "corpus_id": "strict",
            "token_budgets": [2000],
            "cases": [],
            "unexpected": true
        }"#;
        assert!(serde_json::from_str::<RetrievalCorpus>(unknown_corpus).is_err());

        let unknown_case = r#"{
            "schema_version": "1.0.0",
            "corpus_id": "strict",
            "token_budgets": [2000],
            "cases": [{
                "id": "case",
                "task_family": "issue_to_code",
                "language": "rust",
                "repo_fixture": "fixture",
                "base_revision": "sha256:a817b28e702d6f5e830fd02b0aa1c94a2c583c0a5406fa38151729dc41b074b6",
                "split": "holdout",
                "query": "query",
                "gold_files": ["src/lib.rs"],
                "unexpected": true
            }]
        }"#;
        assert!(serde_json::from_str::<RetrievalCorpus>(unknown_case).is_err());
    }

    #[test]
    fn frozen_revision_validation_rejects_unpinned_and_malformed_values() {
        assert!(validate_fixture_revision(
            "sha256:a817b28e702d6f5e830fd02b0aa1c94a2c583c0a5406fa38151729dc41b074b6",
            "valid"
        )
        .is_ok());
        assert!(validate_fixture_revision("main", "unpinned").is_err());
        assert!(validate_fixture_revision("sha256:not-a-digest", "malformed").is_err());
    }

    #[test]
    fn no_gold_cases_are_separate_from_positive_retrieval_metrics() {
        let cases = vec![report("positive", false, &[Some(1)]), report("no-gold", true, &[])];
        let summary = summarize_retrieval_cases(&cases);
        assert_eq!(summary.quality.positive_cases, 1);
        assert_eq!(summary.quality.no_gold_cases, 1);
        assert_eq!(summary.quality.recall_at_1, 1.0);
        assert_eq!(summary.quality.mean_reciprocal_rank, 1.0);
        assert_eq!(summary.quality.no_gold_false_positive_rate, 1.0);
    }

    #[test]
    fn deterministic_baseline_excludes_latency_and_includes_query_shape_quality() {
        let strategy = build_retrieval_strategy_report(
            RetrievalStrategy::Lexical,
            vec![report("positive", false, &[Some(1)])],
        );
        let report = RetrievalBenchReport {
            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,
            report_version: RETRIEVAL_REPORT_VERSION,
            provenance: RetrievalReportProvenance {
                open_kioku_version: env!("CARGO_PKG_VERSION"),
                corpus_revision: "0123456789012345678901234567890123456789".into(),
                cases_sha256: "sha256:test".into(),
                frozen_fixture_revisions_verified: true,
            },
            corpus_id: "fixture".into(),
            cases_file: "cases.json".into(),
            case_count: 1,
            limit: 20,
            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR,
            fixture_digests: BTreeMap::new(),
            strategy_identities: BTreeMap::new(),
            baseline_deltas: Vec::new(),
            advisory_comparisons: Vec::new(),
            query_shape_benchmark: None,
            abstention_calibration: None,
            caveats: Vec::new(),
            strategies: vec![strategy],
            stream_ablations: vec![build_named_retrieval_strategy_report(
                "cc2:rrf_unweighted",
                vec![report("advisory", false, &[Some(1)])],
            )],
        };
        let baseline = retrieval_quality_baseline(&report);
        let json = serde_json::to_string(&baseline).unwrap();
        assert_eq!(
            baseline.quality_dimensions_version.as_deref(),
            Some(RETRIEVAL_BASELINE_DIMENSIONS_VERSION)
        );
        assert!(baseline.strategies[0].by_query_shape.contains_key("conceptual"));
        assert!(baseline.strategies[0]
            .by_task_family_query_shape
            .contains_key("issue_to_code:conceptual"));
        assert!(!json.contains("latency"));
        assert!(!json.contains("p95_ms"));
        assert!(!json.contains("cc2:rrf_unweighted"));
    }

    #[test]
    fn query_shape_baseline_is_deterministic_across_case_insertion_order() {
        let first = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![
                report_with_shape(
                    "error",
                    RetrievalTaskFamily::TraceToCode,
                    open_kioku_core::QueryShape::ErrorTrace,
                    1,
                ),
                report_with_shape(
                    "exact",
                    RetrievalTaskFamily::IssueToCode,
                    open_kioku_core::QueryShape::ExactIdentifier,
                    2,
                ),
            ],
        );
        let second = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![
                report_with_shape(
                    "exact",
                    RetrievalTaskFamily::IssueToCode,
                    open_kioku_core::QueryShape::ExactIdentifier,
                    2,
                ),
                report_with_shape(
                    "error",
                    RetrievalTaskFamily::TraceToCode,
                    open_kioku_core::QueryShape::ErrorTrace,
                    1,
                ),
            ],
        );
        let quality_map = |strategy: RetrievalStrategyReport| RetrievalStrategyQualityBaseline {
            strategy: strategy.strategy,
            summary: strategy.summary.quality,
            by_language: strategy
                .by_language
                .into_iter()
                .map(|(key, value)| (key, value.quality))
                .collect(),
            by_task_family: strategy
                .by_task_family
                .into_iter()
                .map(|(key, value)| (key, value.quality))
                .collect(),
            by_query_shape: strategy
                .by_query_shape
                .into_iter()
                .map(|(key, value)| (key, value.quality))
                .collect(),
            by_task_family_query_shape: strategy
                .by_task_family_query_shape
                .into_iter()
                .map(|(key, value)| (key, value.quality))
                .collect(),
            by_split: strategy
                .by_split
                .into_iter()
                .map(|(key, value)| (key, value.quality))
                .collect(),
        };
        assert_eq!(
            serde_json::to_string(&quality_map(first)).unwrap(),
            serde_json::to_string(&quality_map(second)).unwrap()
        );
    }

    #[test]
    fn baseline_comparison_reports_quality_and_query_shape_deltas_without_latency() {
        let current = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![report_with_shape(
                "current",
                RetrievalTaskFamily::TraceToCode,
                open_kioku_core::QueryShape::ErrorTrace,
                1,
            )],
        );
        let previous_quality = RetrievalQualityMetrics {
            recall_at_10: 0.5,
            mean_reciprocal_rank: 0.25,
            file_f1_at_10: 0.2,
            no_gold_false_positive_rate: 0.5,
            ..Default::default()
        };
        let baseline = compatible_baseline(
            1,
            RetrievalStrategyQualityBaseline {
                strategy: "fusion".into(),
                summary: previous_quality.clone(),
                by_language: BTreeMap::from([("rust".into(), previous_quality.clone())]),
                by_task_family: BTreeMap::from([(
                    "trace_to_code".into(),
                    previous_quality.clone(),
                )]),
                by_query_shape: BTreeMap::from([(
                    "error_trace".into(),
                    previous_quality.clone(),
                )]),
                by_task_family_query_shape: BTreeMap::from([(
                    "trace_to_code:error_trace".into(),
                    previous_quality.clone(),
                )]),
                by_split: BTreeMap::from([("holdout".into(), previous_quality)]),
            },
        );
        let mut caveats = Vec::new();
        let deltas = compare_retrieval_baseline(
            &[current],
            &baseline,
            "fixture",
            1,
            &BTreeMap::new(),
            &mut caveats,
        );
        assert!(caveats.is_empty());
        assert_eq!(deltas.len(), 5);
        assert!(deltas.iter().any(|delta| {
            delta.split == "language" && delta.scope.as_deref() == Some("rust")
        }));
        assert!(deltas.iter().any(|delta| {
            delta.split == "task_family"
                && delta.scope.as_deref() == Some("trace_to_code")
        }));
        assert!(deltas.iter().any(|delta| {
            delta.split == "query_shape" && delta.scope.as_deref() == Some("error_trace")
        }));
        assert!(deltas.iter().any(|delta| {
            delta.split == "task_family_query_shape"
                && delta.scope.as_deref() == Some("trace_to_code:error_trace")
        }));
        let json = serde_json::to_string(&deltas).unwrap();
        assert!(!json.contains("latency"));
    }

    #[test]
    fn degraded_query_shape_is_visible_even_when_aggregate_metrics_match() {
        let mut current_error = report_with_shape(
            "error",
            RetrievalTaskFamily::TraceToCode,
            open_kioku_core::QueryShape::ErrorTrace,
            2,
        );
        current_error.reciprocal_rank = 0.5;
        let current = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![current_error],
        );
        let mut previous = retrieval_quality_baseline(&RetrievalBenchReport {
            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,
            report_version: RETRIEVAL_REPORT_VERSION,
            provenance: RetrievalReportProvenance {
                open_kioku_version: env!("CARGO_PKG_VERSION"),
                corpus_revision: "0123456789012345678901234567890123456789".into(),
                cases_sha256: "sha256:test".into(),
                frozen_fixture_revisions_verified: true,
            },
            corpus_id: "fixture".into(),
            cases_file: "cases.json".into(),
            case_count: 1,
            limit: 20,
            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR,
            fixture_digests: BTreeMap::new(),
            strategy_identities: BTreeMap::new(),
            baseline_deltas: Vec::new(),
            advisory_comparisons: Vec::new(),
            query_shape_benchmark: None,
            abstention_calibration: None,
            caveats: Vec::new(),
            strategies: vec![build_retrieval_strategy_report(
                RetrievalStrategy::Fusion,
                vec![report_with_shape(
                    "previous",
                    RetrievalTaskFamily::TraceToCode,
                    open_kioku_core::QueryShape::ErrorTrace,
                    1,
                )],
            )],
            stream_ablations: Vec::new(),
        });
        previous.strategies[0].summary = current.summary.quality.clone();
        previous.strategies[0].by_split = current
            .by_split
            .iter()
            .map(|(key, value)| (key.clone(), value.quality.clone()))
            .collect();
        let mut caveats = Vec::new();
        let deltas = compare_retrieval_baseline(
            &[current],
            &previous,
            "fixture",
            1,
            &BTreeMap::new(),
            &mut caveats,
        );
        assert!(caveats.is_empty());
        let aggregate = deltas.iter().find(|delta| delta.scope.is_none()).unwrap();
        assert_eq!(aggregate.mean_reciprocal_rank, 0.0);
        let error_trace = deltas
            .iter()
            .find(|delta| {
                delta.split == "query_shape"
                    && delta.scope.as_deref() == Some("error_trace")
            })
            .unwrap();
        assert!(error_trace.mean_reciprocal_rank < 0.0);
    }

    #[test]
    fn degraded_language_is_visible_even_when_aggregate_metrics_match() {
        let mut rust = report("rust", false, &[Some(2)]);
        rust.language = "rust".into();
        let mut python = report("python", false, &[Some(1)]);
        python.language = "python".into();
        let current = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![rust, python],
        );
        let mut previous = quality_baseline_for(&current);
        previous.by_language.get_mut("rust").unwrap().mean_reciprocal_rank = 1.0;
        let baseline = compatible_baseline(2, previous);

        let mut caveats = Vec::new();
        let deltas = compare_retrieval_baseline(
            &[current],
            &baseline,
            "fixture",
            2,
            &BTreeMap::new(),
            &mut caveats,
        );

        assert!(caveats.is_empty());
        assert_eq!(
            deltas
                .iter()
                .find(|delta| delta.scope.is_none())
                .unwrap()
                .mean_reciprocal_rank,
            0.0
        );
        assert_eq!(
            deltas
                .iter()
                .find(|delta| {
                    delta.split == "language" && delta.scope.as_deref() == Some("rust")
                })
                .unwrap()
                .mean_reciprocal_rank,
            -0.5
        );
    }

    #[test]
    fn degraded_task_family_is_visible_even_when_aggregate_metrics_match() {
        let current = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![
                report_with_shape(
                    "issue",
                    RetrievalTaskFamily::IssueToCode,
                    open_kioku_core::QueryShape::Conceptual,
                    1,
                ),
                report_with_shape(
                    "trace",
                    RetrievalTaskFamily::TraceToCode,
                    open_kioku_core::QueryShape::ErrorTrace,
                    2,
                ),
            ],
        );
        let mut previous = quality_baseline_for(&current);
        previous
            .by_task_family
            .get_mut("trace_to_code")
            .unwrap()
            .mean_reciprocal_rank = 1.0;
        let baseline = compatible_baseline(2, previous);

        let mut caveats = Vec::new();
        let deltas = compare_retrieval_baseline(
            &[current],
            &baseline,
            "fixture",
            2,
            &BTreeMap::new(),
            &mut caveats,
        );

        assert!(caveats.is_empty());
        assert_eq!(
            deltas
                .iter()
                .find(|delta| delta.scope.is_none())
                .unwrap()
                .mean_reciprocal_rank,
            0.0
        );
        assert_eq!(
            deltas
                .iter()
                .find(|delta| {
                    delta.split == "task_family"
                        && delta.scope.as_deref() == Some("trace_to_code")
                })
                .unwrap()
                .mean_reciprocal_rank,
            -0.5
        );
    }

    #[test]
    fn language_key_mismatches_are_fail_visible_and_never_partial() {
        let mut rust = report("rust", false, &[Some(1)]);
        rust.language = "rust".into();
        let mut python = report("python", false, &[Some(1)]);
        python.language = "python".into();
        let current = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![rust, python],
        );

        for key_to_remove in ["python", "rust"] {
            let mut previous = quality_baseline_for(&current);
            previous.by_language.remove(key_to_remove);
            let baseline = compatible_baseline(2, previous);
            let mut caveats = Vec::new();
            let deltas = compare_retrieval_baseline(
                std::slice::from_ref(&current),
                &baseline,
                "fixture",
                2,
                &BTreeMap::new(),
                &mut caveats,
            );
            assert!(caveats.iter().any(|caveat| caveat.contains("language baseline keys differ")));
            assert!(!deltas.iter().any(|delta| delta.split == "language"));
            assert!(deltas.iter().any(|delta| delta.split == "query_shape"));
        }

        let mut previous = quality_baseline_for(&current);
        previous
            .by_language
            .insert("typescript".into(), RetrievalQualityMetrics::default());
        let baseline = compatible_baseline(2, previous);
        let mut caveats = Vec::new();
        let deltas = compare_retrieval_baseline(
            &[current],
            &baseline,
            "fixture",
            2,
            &BTreeMap::new(),
            &mut caveats,
        );
        assert!(caveats.iter().any(|caveat| caveat.contains("language baseline keys differ")));
        assert!(!deltas.iter().any(|delta| delta.split == "language"));
    }

    #[test]
    fn task_family_key_mismatches_are_fail_visible_and_never_partial() {
        let current = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![
                report_with_shape(
                    "issue",
                    RetrievalTaskFamily::IssueToCode,
                    open_kioku_core::QueryShape::Conceptual,
                    1,
                ),
                report_with_shape(
                    "trace",
                    RetrievalTaskFamily::TraceToCode,
                    open_kioku_core::QueryShape::ErrorTrace,
                    1,
                ),
            ],
        );

        for key_to_remove in ["issue_to_code", "trace_to_code"] {
            let mut previous = quality_baseline_for(&current);
            previous.by_task_family.remove(key_to_remove);
            let baseline = compatible_baseline(2, previous);
            let mut caveats = Vec::new();
            let deltas = compare_retrieval_baseline(
                std::slice::from_ref(&current),
                &baseline,
                "fixture",
                2,
                &BTreeMap::new(),
                &mut caveats,
            );
            assert!(caveats.iter().any(|caveat| caveat.contains("task-family baseline keys differ")));
            assert!(!deltas.iter().any(|delta| delta.split == "task_family"));
            assert!(deltas
                .iter()
                .any(|delta| delta.split == "task_family_query_shape"));
        }

        let mut previous = quality_baseline_for(&current);
        previous.by_task_family.insert(
            "documentation_lookup".into(),
            RetrievalQualityMetrics::default(),
        );
        let baseline = compatible_baseline(2, previous);
        let mut caveats = Vec::new();
        let deltas = compare_retrieval_baseline(
            &[current],
            &baseline,
            "fixture",
            2,
            &BTreeMap::new(),
            &mut caveats,
        );
        assert!(caveats.iter().any(|caveat| caveat.contains("task-family baseline keys differ")));
        assert!(!deltas.iter().any(|delta| delta.split == "task_family"));
    }

    #[test]
    fn markdown_renders_language_and_task_family_deltas_in_deterministic_order() {
        let mut rust = report("rust", false, &[Some(2)]);
        rust.language = "rust".into();
        let mut python = report("python", false, &[Some(1)]);
        python.language = "python".into();
        python.task_family = RetrievalTaskFamily::TraceToCode;
        let current = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![rust, python],
        );
        let baseline = compatible_baseline(2, quality_baseline_for(&current));
        let mut caveats = Vec::new();
        let deltas = compare_retrieval_baseline(
            &[current],
            &baseline,
            "fixture",
            2,
            &BTreeMap::new(),
            &mut caveats,
        );
        let markdown = render_retrieval_markdown(&report_with_deltas(deltas));

        let python_row = markdown.find("| fusion | language | python |").unwrap();
        let rust_row = markdown.find("| fusion | language | rust |").unwrap();
        let issue_row = markdown
            .find("| fusion | task_family | issue_to_code |")
            .unwrap();
        let trace_row = markdown
            .find("| fusion | task_family | trace_to_code |")
            .unwrap();
        assert!(python_row < rust_row);
        assert!(rust_row < issue_row);
        assert!(issue_row < trace_row);
        assert_eq!(markdown.matches("| fusion | language |").count(), 2);
        assert_eq!(markdown.matches("| fusion | task_family |").count(), 2);
    }

    #[test]
    fn legacy_baseline_dimensions_fail_closed_with_actionable_caveat() {
        let current = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![report("current", false, &[Some(1)])],
        );
        let baseline = RetrievalQualityBaseline {
            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION.into(),
            quality_dimensions_version: None,
            corpus_id: "fixture".into(),
            case_count: 1,
            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR.into(),
            fixture_digests: BTreeMap::new(),
            strategies: Vec::new(),
        };
        let mut caveats = Vec::new();
        let deltas = compare_retrieval_baseline(
            &[current],
            &baseline,
            "fixture",
            1,
            &BTreeMap::new(),
            &mut caveats,
        );
        assert!(deltas.is_empty());
        assert_eq!(caveats.len(), 1);
        assert!(caveats[0].contains("quality_dimensions_version"));
        assert!(caveats[0].contains("regenerate the retrieval baseline"));
    }

    #[test]
    fn baseline_comparison_fails_closed_for_a_different_corpus() {
        let current = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![report("current", false, &[Some(1)])],
        );
        let baseline = RetrievalQualityBaseline {
            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION.into(),
            quality_dimensions_version: Some(RETRIEVAL_BASELINE_DIMENSIONS_VERSION.into()),
            corpus_id: "different-corpus".into(),
            case_count: 1,
            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR.into(),
            fixture_digests: BTreeMap::new(),
            strategies: vec![RetrievalStrategyQualityBaseline {
                strategy: "fusion".into(),
                summary: RetrievalQualityMetrics::default(),
                by_language: BTreeMap::new(),
                by_task_family: BTreeMap::new(),
                by_query_shape: BTreeMap::new(),
                by_task_family_query_shape: BTreeMap::new(),
                by_split: BTreeMap::new(),
            }],
        };
        let mut caveats = Vec::new();
        let deltas = compare_retrieval_baseline(
            &[current],
            &baseline,
            "expected-corpus",
            1,
            &BTreeMap::new(),
            &mut caveats,
        );
        assert!(deltas.is_empty());
        assert_eq!(caveats.len(), 1);
        assert!(caveats[0].contains("baseline is incompatible"));
        assert!(caveats[0].contains("corpus_id"));
    }

    #[test]
    fn strategy_identity_pins_local_semantic_provider_model_and_backend() {
        let config = cc2_semantic_benchmark_config();
        let identities = retrieval_strategy_identities(&config);
        let semantic = identities.get("cc2:semantic_vector_local_hash").unwrap();
        assert_eq!(semantic.provider.as_deref(), Some("local"));
        assert_eq!(semantic.model.as_deref(), Some("local-hash"));
        assert_eq!(semantic.backend.as_deref(), Some("exact-flat"));
    }
}
