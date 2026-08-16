from pathlib import Path

p = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
s = p.read_text()

# Baselines are loaded from disk, so their metadata must be owned rather than &'static.
old = '''struct RetrievalQualityBaseline {
    schema_version: &'static str,
    corpus_id: String,
    case_count: usize,
    token_estimator: &'static str,
    fixture_digests: BTreeMap<String, String>,
    strategies: Vec<RetrievalStrategyQualityBaseline>,
}'''
new = '''struct RetrievalQualityBaseline {
    schema_version: String,
    corpus_id: String,
    case_count: usize,
    token_estimator: String,
    fixture_digests: BTreeMap<String, String>,
    strategies: Vec<RetrievalStrategyQualityBaseline>,
}'''
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new, 1)

old = '''    RetrievalQualityBaseline {
        schema_version: report.schema_version,
        corpus_id: report.corpus_id.clone(),
        case_count: report.case_count,
        token_estimator: report.token_estimator,'''
new = '''    RetrievalQualityBaseline {
        schema_version: report.schema_version.to_string(),
        corpus_id: report.corpus_id.clone(),
        case_count: report.case_count,
        token_estimator: report.token_estimator.to_string(),'''
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new, 1)

# The benchmark source revision belongs to the checkout containing the frozen corpus, not the
# target fixture root. This avoids falsely labeling an arbitrary evaluated repository as the
# Open Kioku revision.
s = s.replace(
    'let (source_revision, revision_caveat) = retrieval_source_revision(&root);',
    'let (source_revision, revision_caveat) = retrieval_source_revision(&cases_file);',
    1,
)

old = '''fn retrieval_source_revision(root: &Path) -> (String, Option<String>) {
    match ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
    {'''
new = '''fn retrieval_source_revision(cases_file: &Path) -> (String, Option<String>) {
    let Some(source_root) = cases_file
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
    else {
        return (
            "unavailable".into(),
            Some("Open Kioku source revision is unavailable because the frozen corpus is not inside a git checkout; report remains reproducible by package version, corpus digest, and fixture digests".into()),
        );
    };
    match ProcessCommand::new("git")
        .arg("-C")
        .arg(source_root)
        .args(["rev-parse", "HEAD"])
        .output()
    {'''
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new, 1)

# Public reports should not embed machine-specific absolute corpus paths when the corpus lives
# beneath the requested benchmark root.
old = '''    let report = RetrievalBenchReport {
        schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,'''
new = '''    let report_cases_file = cases_file
        .strip_prefix(&root)
        .unwrap_or(&cases_file)
        .to_path_buf();
    let report = RetrievalBenchReport {
        schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,'''
assert s.count(old) >= 1
s = s.replace(old, new, 1)
s = s.replace('        cases_file,\n        case_count: corpus.cases.len(),', '        cases_file: report_cases_file,\n        case_count: corpus.cases.len(),', 1)

# Test fixtures for the now-owned baseline strings.
needle = '''        let baseline = RetrievalQualityBaseline {
            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,
            corpus_id: "fixture".into(),
            case_count: 1,
            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR,'''
replacement = '''        let baseline = RetrievalQualityBaseline {
            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION.into(),
            corpus_id: "fixture".into(),
            case_count: 1,
            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR.into(),'''
assert s.count(needle) == 1, s.count(needle)
s = s.replace(needle, replacement, 1)

# Keep the regression fixture compliant with workspace-wide -D warnings.
needle = '''        let mut previous_quality = RetrievalQualityMetrics::default();
        previous_quality.recall_at_10 = 0.5;
        previous_quality.mean_reciprocal_rank = 0.25;
        previous_quality.file_f1_at_10 = 0.2;
        previous_quality.no_gold_false_positive_rate = 0.5;'''
replacement = '''        let previous_quality = RetrievalQualityMetrics {
            recall_at_10: 0.5,
            mean_reciprocal_rank: 0.25,
            file_f1_at_10: 0.2,
            no_gold_false_positive_rate: 0.5,
            ..Default::default()
        };'''
assert s.count(needle) == 1, s.count(needle)
s = s.replace(needle, replacement, 1)

p.write_text(s)
