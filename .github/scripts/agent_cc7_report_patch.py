from pathlib import Path

p = Path('crates/open-kioku-cli/src/bench/retrieval.rs')
s = p.read_text()

s = s.replace('const RETRIEVAL_REPORT_VERSION: &str = "1.1.0";', 'const RETRIEVAL_REPORT_VERSION: &str = "1.2.0";')

old = '''    /// Write a deterministic quality-only baseline (latency intentionally excluded).\n    #[arg(long, value_name = "PATH")]\n    write_baseline: Option<PathBuf>,\n}'''
new = '''    /// Write a deterministic quality-only baseline (latency intentionally excluded).\n    #[arg(long, value_name = "PATH")]\n    write_baseline: Option<PathBuf>,\n\n    /// Checked-in deterministic quality baseline used to calculate report deltas.\n    #[arg(long, default_value = "benchmarks/retrieval-baseline.json")]\n    baseline_file: PathBuf,\n}'''
assert old in s
s = s.replace(old, new, 1)

old = '''#[derive(Debug, Serialize)]\nstruct RetrievalBenchReport {\n    schema_version: &'static str,\n    report_version: &'static str,\n    corpus_id: String,'''
new = '''#[derive(Debug, Clone, Serialize)]\nstruct RetrievalReportProvenance {\n    open_kioku_version: &'static str,\n    source_revision: String,\n    cases_sha256: String,\n    frozen_fixture_revisions_verified: bool,\n}\n\n#[derive(Debug, Clone, Serialize)]\nstruct RetrievalStrategyIdentity {\n    algorithm: String,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    provider: Option<String>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    model: Option<String>,\n    #[serde(skip_serializing_if = "Option::is_none")]\n    backend: Option<String>,\n}\n\n#[derive(Debug, Clone, Serialize, PartialEq)]\nstruct RetrievalBaselineDelta {\n    strategy: String,\n    split: String,\n    recall_at_10: f64,\n    mean_reciprocal_rank: f64,\n    file_f1_at_10: f64,\n    no_gold_false_positive_rate: f64,\n}\n\n#[derive(Debug, Serialize)]\nstruct RetrievalBenchReport {\n    schema_version: &'static str,\n    report_version: &'static str,\n    provenance: RetrievalReportProvenance,\n    corpus_id: String,'''
assert old in s
s = s.replace(old, new, 1)

old = '''    fixture_digests: BTreeMap<String, String>,\n    strategies: Vec<RetrievalStrategyReport>,\n    /// Advisory Context Compiler V2 source/fusion measurements.'''
new = '''    fixture_digests: BTreeMap<String, String>,\n    strategy_identities: BTreeMap<String, RetrievalStrategyIdentity>,\n    baseline_deltas: Vec<RetrievalBaselineDelta>,\n    caveats: Vec<String>,\n    strategies: Vec<RetrievalStrategyReport>,\n    /// Advisory Context Compiler V2 source/fusion measurements.'''
assert old in s
s = s.replace(old, new, 1)

s = s.replace('#[derive(Debug, Clone, Default, Serialize)]\nstruct RetrievalQualityMetrics', '#[derive(Debug, Clone, Default, Serialize, Deserialize)]\nstruct RetrievalQualityMetrics', 1)
s = s.replace('#[derive(Debug, Serialize)]\nstruct RetrievalQualityBaseline', '#[derive(Debug, Serialize, Deserialize)]\nstruct RetrievalQualityBaseline', 1)
s = s.replace('#[derive(Debug, Serialize)]\nstruct RetrievalStrategyQualityBaseline', '#[derive(Debug, Serialize, Deserialize)]\nstruct RetrievalStrategyQualityBaseline', 1)

old = '''    let report = RetrievalBenchReport {\n        schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,\n        report_version: RETRIEVAL_REPORT_VERSION,\n        corpus_id: corpus.corpus_id,\n        cases_file,\n        case_count: corpus.cases.len(),\n        limit,\n        token_estimator: RETRIEVAL_TOKEN_ESTIMATOR,\n        fixture_digests,\n        strategies: vec![\n            build_retrieval_strategy_report(RetrievalStrategy::Lexical, lexical_cases),\n            build_retrieval_strategy_report(RetrievalStrategy::Fusion, fusion_cases),\n        ],\n        stream_ablations: cc2_cases\n            .into_iter()\n            .map(|(label, cases)| build_named_retrieval_strategy_report(label, cases))\n            .collect(),\n    };\n\n    write_retrieval_outputs(&report, &args)?;'''
new = '''    let strategies = vec![\n        build_retrieval_strategy_report(RetrievalStrategy::Lexical, lexical_cases),\n        build_retrieval_strategy_report(RetrievalStrategy::Fusion, fusion_cases),\n    ];\n    let stream_ablations = cc2_cases\n        .into_iter()\n        .map(|(label, cases)| build_named_retrieval_strategy_report(label, cases))\n        .collect::<Vec<_>>();\n    let (source_revision, revision_caveat) = retrieval_source_revision(&root);\n    let mut caveats = Vec::new();\n    if let Some(caveat) = revision_caveat {\n        caveats.push(caveat);\n    }\n    caveats.push(\n        "abstention precision/recall is not reported until calibrated abstention ships; no-gold false-positive rate remains the active negative-case signal".into(),\n    );\n    let baseline_path = absolutize(&args.baseline_file)?;\n    let baseline_deltas = if baseline_path.is_file() {\n        let baseline = load_retrieval_quality_baseline(&baseline_path)?;\n        compare_retrieval_baseline(&strategies, &baseline, &mut caveats)\n    } else {\n        caveats.push(format!(\n            "checked-in retrieval baseline unavailable at {}; regression deltas omitted",\n            baseline_path.display()\n        ));\n        Vec::new()\n    };\n    let report = RetrievalBenchReport {\n        schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,\n        report_version: RETRIEVAL_REPORT_VERSION,\n        provenance: RetrievalReportProvenance {\n            open_kioku_version: env!("CARGO_PKG_VERSION"),\n            source_revision,\n            cases_sha256: sha256_file(&cases_file)?,\n            frozen_fixture_revisions_verified: true,\n        },\n        corpus_id: corpus.corpus_id,\n        cases_file,\n        case_count: corpus.cases.len(),\n        limit,\n        token_estimator: RETRIEVAL_TOKEN_ESTIMATOR,\n        fixture_digests,\n        strategy_identities: retrieval_strategy_identities(&semantic_config),\n        baseline_deltas,\n        caveats,\n        strategies,\n        stream_ablations,\n    };\n\n    write_retrieval_outputs(&report, &args)?;'''
assert old in s
s = s.replace(old, new, 1)

marker = '''fn retrieval_quality_baseline(report: &RetrievalBenchReport) -> RetrievalQualityBaseline {'''
helper = r'''fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn retrieval_source_revision(root: &Path) -> (String, Option<String>) {
    match ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
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
                    Some("Open Kioku source revision could not be validated as a full git commit; report remains reproducible only by package version and fixture digests".into()),
                )
            }
        }
        Ok(_) | Err(_) => (
            "unavailable".into(),
            Some("Open Kioku source revision is unavailable because git metadata could not be read; report remains reproducible only by package version and fixture digests".into()),
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
    identities
}

fn load_retrieval_quality_baseline(path: &Path) -> anyhow::Result<RetrievalQualityBaseline> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read retrieval baseline {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse retrieval baseline {}", path.display()))
}

fn compare_retrieval_baseline(
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
        deltas.push(RetrievalBaselineDelta {
            strategy: current.strategy.clone(),
            split: split.into(),
            recall_at_10: current_quality.recall_at_10 - previous_quality.recall_at_10,
            mean_reciprocal_rank: current_quality.mean_reciprocal_rank
                - previous_quality.mean_reciprocal_rank,
            file_f1_at_10: current_quality.file_f1_at_10 - previous_quality.file_f1_at_10,
            no_gold_false_positive_rate: current_quality.no_gold_false_positive_rate
                - previous_quality.no_gold_false_positive_rate,
        });
    }
    deltas.sort_by(|left, right| left.strategy.cmp(&right.strategy));
    deltas
}

'''
assert marker in s
s = s.replace(marker, helper + marker, 1)

old = '''    let mut out = format!(\n        "# Repository Context Retrieval Benchmark\\n\\n- Corpus: `{}`\\n- Cases: {}\\n- Result limit: {}\\n- Token estimator: `{}`\\n\\n",\n        report.corpus_id, report.case_count, report.limit, report.token_estimator\n    );'''
new = '''    let mut out = format!(\n        "# Repository Context Retrieval Benchmark\\n\\n- Corpus: `{}`\\n- Cases: {}\\n- Result limit: {}\\n- Token estimator: `{}`\\n- Open Kioku version: `{}`\\n- Open Kioku revision: `{}`\\n- Corpus file digest: `{}`\\n\\n",\n        report.corpus_id,\n        report.case_count,\n        report.limit,\n        report.token_estimator,\n        report.provenance.open_kioku_version,\n        report.provenance.source_revision,\n        report.provenance.cases_sha256\n    );'''
assert old in s
s = s.replace(old, new, 1)

old = '''    if !report.stream_ablations.is_empty() {\n        out.push_str("## Context Compiler V2 stream ablations (advisory)'''
new = '''    if !report.baseline_deltas.is_empty() {\n        out.push_str("## Regression deltas vs checked-in baseline\\n\\nPositive Recall/MRR/F1 is improvement; negative no-gold FP is improvement.\\n\\n| Strategy | Split | Δ R@10 | Δ MRR | Δ F1@10 | Δ no-gold FP |\\n|---|---|---:|---:|---:|---:|\\n");\n        for delta in &report.baseline_deltas {\n            out.push_str(&format!(\n                "| {} | {} | {:+.3} | {:+.3} | {:+.3} | {:+.3} |\\n",\n                delta.strategy,\n                delta.split,\n                delta.recall_at_10,\n                delta.mean_reciprocal_rank,\n                delta.file_f1_at_10,\n                delta.no_gold_false_positive_rate\n            ));\n        }\n        out.push('\\n');\n    }\n    if !report.caveats.is_empty() {\n        out.push_str("## Caveats\\n\\n");\n        for caveat in &report.caveats {\n            out.push_str(&format!("- {caveat}\\n"));\n        }\n        out.push('\\n');\n    }\n    if !report.stream_ablations.is_empty() {\n        out.push_str("## Context Compiler V2 stream ablations (advisory)'''
assert old in s
s = s.replace(old, new, 1)

old = '''        let report = RetrievalBenchReport {\n            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,\n            report_version: RETRIEVAL_REPORT_VERSION,\n            corpus_id: "fixture".into(),'''
new = '''        let report = RetrievalBenchReport {\n            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,\n            report_version: RETRIEVAL_REPORT_VERSION,\n            provenance: RetrievalReportProvenance {\n                open_kioku_version: env!("CARGO_PKG_VERSION"),\n                source_revision: "0123456789012345678901234567890123456789".into(),\n                cases_sha256: "sha256:test".into(),\n                frozen_fixture_revisions_verified: true,\n            },\n            corpus_id: "fixture".into(),'''
assert old in s
s = s.replace(old, new, 1)

old = '''            fixture_digests: BTreeMap::new(),\n            strategies: vec![strategy],\n            stream_ablations: vec![build_named_retrieval_strategy_report('''
new = '''            fixture_digests: BTreeMap::new(),\n            strategy_identities: BTreeMap::new(),\n            baseline_deltas: Vec::new(),\n            caveats: Vec::new(),\n            strategies: vec![strategy],\n            stream_ablations: vec![build_named_retrieval_strategy_report('''
assert old in s
s = s.replace(old, new, 1)

insert = '''\n    #[test]\n    fn baseline_comparison_reports_holdout_quality_deltas_without_latency() {\n        let current = build_retrieval_strategy_report(\n            RetrievalStrategy::Fusion,\n            vec![report("current", false, &[Some(1)])],\n        );\n        let mut previous_quality = RetrievalQualityMetrics::default();\n        previous_quality.recall_at_10 = 0.5;\n        previous_quality.mean_reciprocal_rank = 0.25;\n        previous_quality.file_f1_at_10 = 0.2;\n        previous_quality.no_gold_false_positive_rate = 0.5;\n        let baseline = RetrievalQualityBaseline {\n            schema_version: RETRIEVAL_BENCH_SCHEMA_VERSION,\n            corpus_id: "fixture".into(),\n            case_count: 1,\n            token_estimator: RETRIEVAL_TOKEN_ESTIMATOR,\n            fixture_digests: BTreeMap::new(),\n            strategies: vec![RetrievalStrategyQualityBaseline {\n                strategy: "fusion".into(),\n                summary: previous_quality.clone(),\n                by_language: BTreeMap::new(),\n                by_task_family: BTreeMap::new(),\n                by_split: BTreeMap::from([("holdout".into(), previous_quality)]),\n            }],\n        };\n        let mut caveats = Vec::new();\n        let deltas = compare_retrieval_baseline(&[current], &baseline, &mut caveats);\n        assert!(caveats.is_empty());\n        assert_eq!(deltas.len(), 1);\n        assert_eq!(deltas[0].strategy, "fusion");\n        assert_eq!(deltas[0].split, "holdout");\n        assert!(deltas[0].recall_at_10 > 0.0);\n        let json = serde_json::to_string(&deltas).unwrap();\n        assert!(!json.contains("latency"));\n    }\n\n    #[test]\n    fn strategy_identity_pins_local_semantic_provider_model_and_backend() {\n        let config = cc2_semantic_benchmark_config();\n        let identities = retrieval_strategy_identities(&config);\n        let semantic = identities.get("cc2:semantic_vector_local_hash").unwrap();\n        assert_eq!(semantic.provider.as_deref(), Some("local"));\n        assert_eq!(semantic.model.as_deref(), Some("local-hash"));\n        assert_eq!(semantic.backend.as_deref(), Some("exact-flat"));\n    }\n'''
idx = s.rfind('\n}')
assert idx > 0
s = s[:idx] + insert + s[idx:]

p.write_text(s)
