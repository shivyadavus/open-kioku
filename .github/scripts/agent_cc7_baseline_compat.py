from pathlib import Path

p = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
s = p.read_text()

old = '''        compare_retrieval_baseline(&strategies, &baseline, &mut caveats)
'''
new = '''        compare_retrieval_baseline(
            &strategies,
            &baseline,
            &corpus.corpus_id,
            corpus.cases.len(),
            &fixture_digests,
            &mut caveats,
        )
'''
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new, 1)

old = '''fn compare_retrieval_baseline(
    strategies: &[RetrievalStrategyReport],
    baseline: &RetrievalQualityBaseline,
    caveats: &mut Vec<String>,
) -> Vec<RetrievalBaselineDelta> {
    if baseline.schema_version != RETRIEVAL_BENCH_SCHEMA_VERSION {
        caveats.push(format!(
            "retrieval baseline schema {} does not match {}; regression deltas omitted",
            baseline.schema_version, RETRIEVAL_BENCH_SCHEMA_VERSION
        ));
        return Vec::new();
    }
'''
new = '''fn compare_retrieval_baseline(
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
'''
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new, 1)

old = '''        let deltas = compare_retrieval_baseline(&[current], &baseline, &mut caveats);
'''
new = '''        let deltas = compare_retrieval_baseline(
            &[current],
            &baseline,
            "fixture",
            1,
            &BTreeMap::new(),
            &mut caveats,
        );
'''
assert s.count(old) == 1, s.count(old)
s = s.replace(old, new, 1)

insert = '''
    #[test]
    fn baseline_comparison_fails_closed_for_a_different_corpus() {
        let current = build_retrieval_strategy_report(
            RetrievalStrategy::Fusion,
            vec![report("current", false, &[Some(1)])],
        );
        let baseline = RetrievalQualityBaseline {
            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION.into(),
            corpus_id: "different-corpus".into(),
            case_count: 1,
            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR.into(),
            fixture_digests: BTreeMap::new(),
            strategies: vec![RetrievalStrategyQualityBaseline {
                strategy: "fusion".into(),
                summary: RetrievalQualityMetrics::default(),
                by_language: BTreeMap::new(),
                by_task_family: BTreeMap::new(),
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
'''
needle = '''    #[test]
    fn strategy_identity_pins_local_semantic_provider_model_and_backend() {
'''
assert s.count(needle) == 1, s.count(needle)
s = s.replace(needle, insert + '\n' + needle, 1)

p.write_text(s)
