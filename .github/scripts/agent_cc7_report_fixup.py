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

# This is the revision of the checkout containing the benchmark corpus. Name it accordingly: a
# custom corpus can live in a different repository, and we must never label that commit as the
# Open Kioku source revision.
s = s.replace('    source_revision: String,', '    corpus_revision: String,', 1)
s = s.replace(
    'let (source_revision, revision_caveat) = retrieval_source_revision(&root);',
    'let (corpus_revision, revision_caveat) = retrieval_corpus_revision(&cases_file);',
    1,
)
s = s.replace('            source_revision,', '            corpus_revision,', 1)
s = s.replace(
    'fn retrieval_source_revision(root: &Path) -> (String, Option<String>) {',
    'fn retrieval_corpus_revision(cases_file: &Path) -> (String, Option<String>) {',
    1,
)
s = s.replace('        .arg(root)\n', '        .arg(source_root)\n', 1)
old = '''fn retrieval_corpus_revision(cases_file: &Path) -> (String, Option<String>) {
    match ProcessCommand::new("git")'''
new = '''fn retrieval_corpus_revision(cases_file: &Path) -> (String, Option<String>) {
    let Some(source_root) = cases_file
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
    else {
        return (
            "unavailable".into(),
            Some("frozen corpus revision is unavailable because the corpus is not inside a git checkout; reproducibility remains anchored by corpus digest and fixture digests".into()),
        );
    };
    match ProcessCommand::new("git")'''
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new, 1)
s = s.replace(
    '"Open Kioku source revision could not be validated as a full git commit; report remains reproducible only by package version and fixture digests"',
    '"frozen corpus revision could not be validated as a full git commit; reproducibility remains anchored by corpus digest and fixture digests"',
)
s = s.replace(
    '"Open Kioku source revision is unavailable because git metadata could not be read; report remains reproducible only by package version and fixture digests"',
    '"frozen corpus revision is unavailable because git metadata could not be read; reproducibility remains anchored by corpus digest and fixture digests"',
)
s = s.replace('report.provenance.source_revision,', 'report.provenance.corpus_revision,', 1)
s = s.replace('- Open Kioku revision: `{}`\\n', '- Frozen corpus revision: `{}`\\n', 1)
s = s.replace('                source_revision: "0123456789012345678901234567890123456789".into(),', '                corpus_revision: "0123456789012345678901234567890123456789".into(),', 1)

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

# The tiny unit fixture has no current holdout split, so comparison intentionally falls back to
# overall. The workflow below exercises and asserts the real frozen-corpus holdout path.
s = s.replace(
    'fn baseline_comparison_reports_holdout_quality_deltas_without_latency()',
    'fn baseline_comparison_reports_quality_deltas_without_latency()',
    1,
)
s = s.replace('assert_eq!(deltas[0].split, "holdout");', 'assert_eq!(deltas[0].split, "overall");', 1)

p.write_text(s)
