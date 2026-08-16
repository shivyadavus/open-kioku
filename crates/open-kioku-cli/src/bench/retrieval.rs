const RETRIEVAL_BENCH_SCHEMA_VERSION: &str = "1.0.0";
const RETRIEVAL_TOKEN_ESTIMATOR: &str = "unicode_chars_div_4_plus_metadata_v1";
const RETRIEVAL_K_VALUES: [usize; 4] = [1, 5, 10, 20];

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
}

#[derive(Debug, Clone, Deserialize)]
struct RetrievalCorpus {
    schema_version: String,
    corpus_id: String,
    #[serde(default = "default_retrieval_token_budgets")]
    token_budgets: Vec<usize>,
    cases: Vec<RetrievalCase>,
}

#[derive(Debug, Clone, Deserialize)]
struct RetrievalCase {
    id: String,
    task_family: RetrievalTaskFamily,
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

#[derive(Debug, Serialize)]
struct RetrievalBenchReport {
    schema_version: &'static str,
    corpus_id: String,
    cases_file: PathBuf,
    case_count: usize,
    limit: usize,
    token_estimator: &'static str,
    fixture_digests: BTreeMap<String, String>,
    strategies: Vec<RetrievalStrategyReport>,
}

#[derive(Debug, Serialize)]
struct RetrievalStrategyReport {
    strategy: String,
    summary: RetrievalMetricSummary,
    by_language: BTreeMap<String, RetrievalMetricSummary>,
    by_task_family: BTreeMap<String, RetrievalMetricSummary>,
    by_split: BTreeMap<String, RetrievalMetricSummary>,
    cases: Vec<RetrievalCaseReport>,
}

#[derive(Debug, Clone, Default, Serialize)]
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
struct RetrievalMetricSummary {
    quality: RetrievalQualityMetrics,
    latency: RetrievalLatencyMetrics,
}

#[derive(Debug, Clone, Serialize)]
struct RetrievalCaseReport {
    id: String,
    task_family: RetrievalTaskFamily,
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

#[derive(Debug, Serialize)]
struct RetrievalQualityBaseline {
    schema_version: &'static str,
    corpus_id: String,
    case_count: usize,
    token_estimator: &'static str,
    fixture_digests: BTreeMap<String, String>,
    strategies: Vec<RetrievalStrategyQualityBaseline>,
}

#[derive(Debug, Serialize)]
struct RetrievalStrategyQualityBaseline {
    strategy: String,
    summary: RetrievalQualityMetrics,
    by_language: BTreeMap<String, RetrievalQualityMetrics>,
    by_task_family: BTreeMap<String, RetrievalQualityMetrics>,
    by_split: BTreeMap<String, RetrievalQualityMetrics>,
}

fn default_retrieval_token_budgets() -> Vec<usize> {
    vec![2_000, 4_000, 8_000]
}

fn run_retrieval_bench(args: RetrievalBenchArgs) -> anyhow::Result<RetrievalBenchReport> {
    let root = absolutize(&args.path)?;
    let cases_file = absolutize(&args.cases_file)?;
    let corpus = load_retrieval_corpus(&cases_file)?;
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
    let mut fixture_digests = BTreeMap::new();
    for fixture in fixtures.values() {
        if !args.no_index {
            index_repo(fixture)?;
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
    }

    let report = RetrievalBenchReport {
        schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,
        corpus_id: corpus.corpus_id,
        cases_file,
        case_count: corpus.cases.len(),
        limit,
        token_estimator: RETRIEVAL_TOKEN_ESTIMATOR,
        fixture_digests,
        strategies: vec![
            build_retrieval_strategy_report(RetrievalStrategy::Lexical, lexical_cases),
            build_retrieval_strategy_report(RetrievalStrategy::Fusion, fusion_cases),
        ],
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
    RetrievalStrategyReport {
        strategy: strategy.label().into(),
        summary: summarize_retrieval_cases(&cases),
        by_language: summarize_retrieval_groups(&cases, |case| case.language.clone()),
        by_task_family: summarize_retrieval_groups(&cases, |case| {
            case.task_family.label().into()
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
        latency: RetrievalLatencyMetrics {
            mean_ms: retrieval_mean(&latencies),
            p50_ms: retrieval_percentile(&latencies, 0.50),
            p95_ms: retrieval_percentile(&latencies, 0.95),
        },
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

fn retrieval_quality_baseline(report: &RetrievalBenchReport) -> RetrievalQualityBaseline {
    RetrievalQualityBaseline {
        schema_version: report.schema_version,
        corpus_id: report.corpus_id.clone(),
        case_count: report.case_count,
        token_estimator: report.token_estimator,
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
        "# Repository Context Retrieval Benchmark\n\n- Corpus: `{}`\n- Cases: {}\n- Result limit: {}\n- Token estimator: `{}`\n\n",
        report.corpus_id, report.case_count, report.limit, report.token_estimator
    );
    for strategy in &report.strategies {
        let quality = &strategy.summary.quality;
        out.push_str(&format!(
            "## {}\n\n| Metric | Value |\n|---|---:|\n| Recall@1 | {:.3} |\n| Recall@5 | {:.3} |\n| Recall@10 | {:.3} |\n| Recall@20 | {:.3} |\n| Precision@10 | {:.3} |\n| MRR | {:.3} |\n| File F1@10 | {:.3} |\n| No-gold false-positive rate | {:.3} |\n| p50 latency (observational) | {:.2} ms |\n| p95 latency (observational) | {:.2} ms |\n\n",
            strategy.strategy,
            quality.recall_at_1,
            quality.recall_at_5,
            quality.recall_at_10,
            quality.recall_at_20,
            quality.precision_at_10,
            quality.mean_reciprocal_rank,
            quality.file_f1_at_10,
            quality.no_gold_false_positive_rate,
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
    out.push_str("## Reproducibility\n\nLatency is reported for observability but excluded from the checked-in deterministic quality baseline. Fixture content digests and the corpus schema are part of the baseline so corpus drift is visible.\n");
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
    fn deterministic_baseline_excludes_latency() {
        let strategy = build_retrieval_strategy_report(
            RetrievalStrategy::Lexical,
            vec![report("positive", false, &[Some(1)])],
        );
        let report = RetrievalBenchReport {
            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,
            corpus_id: "fixture".into(),
            cases_file: "cases.json".into(),
            case_count: 1,
            limit: 20,
            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR,
            fixture_digests: BTreeMap::new(),
            strategies: vec![strategy],
        };
        let json = serde_json::to_string(&retrieval_quality_baseline(&report)).unwrap();
        assert!(!json.contains("latency"));
        assert!(!json.contains("p95_ms"));
    }
}
