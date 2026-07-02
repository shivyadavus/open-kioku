fn run_bench(args: BenchArgs) -> anyhow::Result<BenchReport> {
    let path = args.path;
    let quality_cases = parse_quality_cases(&args.quality_cases)?;

    let start = Instant::now();
    let snapshot = index_repo(&path)?;
    let index_duration = start.elapsed();

    let index = TantivySearchIndex::open_or_create(default_index_dir(&path))?;
    let bm25_median = median_duration(time_searches(10, || index.search("fn", 10).map(|_| ()))?);

    let store = open_store(&path)?;
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = store.all_chunks()?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    let regex_median = median_duration(time_searches(10, || {
        search_chunks(&chunks, &files, &symbols, "fn", 10).map(|_| ())
    })?);

    let quality = if quality_cases.is_empty() {
        None
    } else {
        Some(evaluate_quality_cases(
            &index,
            &quality_cases,
            args.quality_limit,
        )?)
    };
    let manifest = snapshot.manifest;
    let elapsed_seconds = index_duration.as_secs_f64();

    Ok(BenchReport {
        repo: path,
        index: IndexBenchReport {
            file_count: manifest.file_count,
            symbol_count: manifest.symbol_count,
            chunk_count: manifest.chunk_count,
            elapsed_ms: duration_ms(index_duration),
            files_per_second: if elapsed_seconds > 0.0 {
                manifest.file_count as f64 / elapsed_seconds
            } else {
                0.0
            },
        },
        search: SearchBenchReport {
            bm25_median_ms: duration_ms(bm25_median),
            regex_median_ms: duration_ms(regex_median),
        },
        quality,
    })
}

fn parse_quality_cases(values: &[String]) -> anyhow::Result<Vec<QualityCase>> {
    values
        .iter()
        .map(|value| {
            let (query, expected_path) = value.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("quality case must use QUERY=EXPECTED_PATH_SUBSTRING: {value}")
            })?;
            let query = query.trim();
            let expected_path = expected_path.trim();
            if query.is_empty() || expected_path.is_empty() {
                anyhow::bail!("quality case query and expected path must be non-empty: {value}");
            }
            Ok(QualityCase {
                query: query.to_string(),
                expected_path: expected_path.to_string(),
            })
        })
        .collect()
}

fn evaluate_quality_cases(
    index: &TantivySearchIndex,
    cases: &[QualityCase],
    limit: usize,
) -> anyhow::Result<QualityBenchReport> {
    let limit = limit.max(1);
    let mut reports = Vec::with_capacity(cases.len());
    let mut top_hits = 0usize;
    let mut any_hits = 0usize;
    let mut reciprocal_rank = 0.0;

    for case in cases {
        let results = index.search(&case.query, limit)?;
        let expected = normalize_path_fragment(&case.expected_path);
        let rank = results.iter().position(|result| {
            normalize_path_fragment(&result.path.to_string_lossy()).contains(&expected)
        });
        let rank = rank.map(|value| value + 1);
        if rank == Some(1) {
            top_hits += 1;
        }
        if let Some(rank) = rank {
            any_hits += 1;
            reciprocal_rank += 1.0 / rank as f64;
        }
        reports.push(QualityCaseReport {
            query: case.query.clone(),
            expected_path: case.expected_path.clone(),
            rank,
            top_path: results.first().map(|result| result.path.clone()),
            matched_path: rank
                .and_then(|rank| results.get(rank - 1).map(|result| result.path.clone())),
            result_count: results.len(),
        });
    }

    let total = cases.len() as f64;
    Ok(QualityBenchReport {
        case_count: cases.len(),
        precision_at_1: top_hits as f64 / total,
        hit_rate_at_k: any_hits as f64 / total,
        mean_reciprocal_rank: reciprocal_rank / total,
        limit,
        cases: reports,
    })
}

fn print_bench_report(report: &BenchReport) {
    println!(
        "Indexed {} files, {} symbols, and {} chunks in {:.2}ms",
        report.index.file_count,
        report.index.symbol_count,
        report.index.chunk_count,
        report.index.elapsed_ms
    );
    println!("{:.2} files/sec", report.index.files_per_second);
    println!("BM25 search: {:.2}ms median", report.search.bm25_median_ms);
    println!(
        "Regex search: {:.2}ms median",
        report.search.regex_median_ms
    );

    if let Some(quality) = &report.quality {
        println!(
            "Quality: precision@1 {:.3}, hit-rate@{} {:.3}, MRR {:.3}",
            quality.precision_at_1,
            quality.limit,
            quality.hit_rate_at_k,
            quality.mean_reciprocal_rank
        );
        for case in &quality.cases {
            let status = match case.rank {
                Some(1) => "pass",
                Some(_) => "hit",
                None => "miss",
            };
            let rank = case
                .rank
                .map(|rank| rank.to_string())
                .unwrap_or_else(|| "-".to_string());
            let top_path = case
                .top_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string());
            println!(
                "  {status}: query {:?}, expected {:?}, rank {}, top {}",
                case.query, case.expected_path, rank, top_path
            );
        }
    }
}

fn run_architecture_policy_bench(
    args: ArchitectureBenchArgs,
) -> anyhow::Result<ArchitecturePolicyBenchReport> {
    let repo = absolutize(&args.path)?;
    let cases_file = absolutize(&args.cases_file)?;
    let cases = load_architecture_policy_bench_cases(&cases_file)?;
    if cases.is_empty() {
        anyhow::bail!(
            "architecture policy benchmark cases file is empty: {}",
            cases_file.display()
        );
    }
    if !args.no_index {
        index_repo(&repo)?;
    }
    let Some(policy) = load_architecture_policy(&repo)? else {
        anyhow::bail!(
            "architecture policy benchmark requires a configured policy in {}",
            repo.display()
        );
    };
    let store = open_store(&repo)?;
    let resolver = PolicyResolver::new(&policy)?;
    let iterations = args.iterations.max(1);
    let mut durations = Vec::with_capacity(iterations);
    let mut report = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let check = evaluate_policy(&store, &resolver, &policy)?;
        durations.push(started.elapsed());
        report = Some(check);
    }
    let report = report.expect("at least one architecture policy benchmark iteration");
    let actual_findings = architecture_policy_actual_findings(&policy, &report);
    let (summary, families, case_reports) =
        score_architecture_policy_cases(&policy, &cases, &actual_findings);

    Ok(ArchitecturePolicyBenchReport {
        repo,
        cases_file,
        case_count: cases.len(),
        iterations,
        p95_policy_check_ms: percentile_duration_ms(&durations, 0.95),
        summary,
        rule_families: families,
        cases: case_reports,
    })
}

fn load_architecture_policy_bench_cases(
    path: &Path,
) -> anyhow::Result<Vec<ArchitecturePolicyBenchCase>> {
    let raw = fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read architecture policy cases {}",
            path.display()
        )
    })?;
    let cases: Vec<ArchitecturePolicyBenchCase> =
        serde_json::from_str(&raw).with_context(|| {
            format!(
                "failed to parse architecture policy cases {}",
                path.display()
            )
        })?;
    let mut seen = BTreeMap::new();
    for case in &cases {
        if case.id.trim().is_empty() {
            anyhow::bail!("architecture policy benchmark case id must be non-empty");
        }
        if let Some(previous) = seen.insert(case.id.clone(), true) {
            if previous {
                anyhow::bail!(
                    "duplicate architecture policy benchmark case id `{}`",
                    case.id
                );
            }
        }
        if matches!(
            case.expected,
            ArchitecturePolicyBenchOutcome::Violation | ArchitecturePolicyBenchOutcome::Exempted
        ) && case.rule_id.as_deref().unwrap_or_default().is_empty()
        {
            anyhow::bail!(
                "architecture policy benchmark case `{}` requires rule_id for {:?}",
                case.id,
                case.expected
            );
        }
    }
    Ok(cases)
}

fn architecture_policy_actual_findings(
    policy: &ArchitecturePolicy,
    report: &open_kioku_core::PolicyCheckReport,
) -> Vec<ArchitecturePolicyActualFinding> {
    let mut findings = Vec::new();
    for violation in &report.violations {
        findings.push(ArchitecturePolicyActualFinding {
            rule_family: architecture_policy_rule_family(policy, &violation.rule_id),
            outcome: ArchitecturePolicyBenchOutcome::Violation,
            rule_id: Some(violation.rule_id.clone()),
            source_path: violation.source_path.clone(),
            target_path: violation.target_path.clone(),
            edge_type: violation.edge_type,
        });
    }
    for exemption in &report.exemptions {
        findings.push(ArchitecturePolicyActualFinding {
            rule_family: architecture_policy_rule_family(policy, &exemption.rule_id),
            outcome: ArchitecturePolicyBenchOutcome::Exempted,
            rule_id: Some(exemption.rule_id.clone()),
            source_path: exemption.source_path.clone(),
            target_path: exemption.target_path.clone(),
            edge_type: exemption.evidence.edge_type,
        });
    }
    for unknown in &report.unknown_edges {
        findings.push(ArchitecturePolicyActualFinding {
            rule_family: ArchitecturePolicyRuleFamily::Unknown,
            outcome: ArchitecturePolicyBenchOutcome::Unknown,
            rule_id: None,
            source_path: unknown.evidence.source_path.clone(),
            target_path: unknown.evidence.target_path.clone(),
            edge_type: unknown.evidence.edge_type,
        });
    }
    findings.sort_by(|left, right| {
        left.rule_family
            .cmp(&right.rule_family)
            .then_with(|| left.rule_id.cmp(&right.rule_id))
            .then_with(|| left.source_path.cmp(&right.source_path))
            .then_with(|| left.target_path.cmp(&right.target_path))
            .then_with(|| left.edge_type.cmp(&right.edge_type))
            .then_with(|| format!("{:?}", left.outcome).cmp(&format!("{:?}", right.outcome)))
    });
    findings.dedup_by(|left, right| {
        left.rule_family == right.rule_family
            && left.outcome == right.outcome
            && left.rule_id == right.rule_id
            && same_architecture_bench_path(&left.source_path, &right.source_path)
            && same_architecture_bench_path(&left.target_path, &right.target_path)
            && left.edge_type == right.edge_type
    });
    findings
}

fn score_architecture_policy_cases(
    _policy: &ArchitecturePolicy,
    cases: &[ArchitecturePolicyBenchCase],
    actual_findings: &[ArchitecturePolicyActualFinding],
) -> (
    ArchitecturePolicyBenchSummary,
    Vec<ArchitecturePolicyBenchFamilyReport>,
    Vec<ArchitecturePolicyBenchCaseReport>,
) {
    let mut overall = ArchitecturePolicyBenchCounts::default();
    let mut families: BTreeMap<ArchitecturePolicyRuleFamily, ArchitecturePolicyBenchCounts> =
        BTreeMap::new();
    let mut matched_positive_cases = vec![false; cases.len()];
    let mut case_reports = Vec::with_capacity(cases.len());

    for (case_index, case) in cases.iter().enumerate() {
        let matching = actual_findings
            .iter()
            .filter(|finding| architecture_policy_case_selector_matches(case, finding))
            .collect::<Vec<_>>();
        let actual = matching
            .iter()
            .map(|finding| finding.outcome)
            .collect::<Vec<_>>();
        let matched = matching
            .iter()
            .any(|finding| architecture_policy_case_exact_match(case, finding));
        let passed = if case.expected == ArchitecturePolicyBenchOutcome::Allowed {
            matching.is_empty()
        } else {
            matched
        };
        if matched && case.expected != ArchitecturePolicyBenchOutcome::Allowed {
            matched_positive_cases[case_index] = true;
        }
        let mut notes = Vec::new();
        if !passed {
            if case.expected == ArchitecturePolicyBenchOutcome::Allowed {
                notes.push("expected no policy finding, but at least one finding matched".into());
            } else if matching.is_empty() {
                notes.push("expected policy finding was not reported".into());
            } else {
                notes.push(
                    "reported policy finding did not match expected outcome, family, or rule"
                        .into(),
                );
            }
        }
        case_reports.push(ArchitecturePolicyBenchCaseReport {
            id: case.id.clone(),
            rule_family: case.rule_family,
            expected: case.expected,
            actual,
            rule_id: case.rule_id.clone(),
            source_path: case.source_path.clone(),
            target_path: case.target_path.clone(),
            edge_type: case.edge_type,
            passed,
            notes,
        });
    }

    for case in cases {
        if case.expected != ArchitecturePolicyBenchOutcome::Allowed {
            overall.expected_positive_count += 1;
            families
                .entry(case.rule_family)
                .or_default()
                .expected_positive_count += 1;
        }
    }

    for finding in actual_findings {
        let Some((case_index, case)) = cases
            .iter()
            .enumerate()
            .find(|(_, case)| architecture_policy_case_selector_matches(case, finding))
        else {
            continue;
        };
        overall.actual_positive_count += 1;
        families
            .entry(finding.rule_family)
            .or_default()
            .actual_positive_count += 1;
        if architecture_policy_case_exact_match(case, finding) {
            overall.true_positives += 1;
            families
                .entry(finding.rule_family)
                .or_default()
                .true_positives += 1;
            matched_positive_cases[case_index] = true;
        } else {
            overall.false_positives += 1;
            families
                .entry(finding.rule_family)
                .or_default()
                .false_positives += 1;
        }
    }

    for (case_index, case) in cases.iter().enumerate() {
        if case.expected != ArchitecturePolicyBenchOutcome::Allowed
            && !matched_positive_cases[case_index]
        {
            overall.false_negatives += 1;
            families
                .entry(case.rule_family)
                .or_default()
                .false_negatives += 1;
        }
    }

    let summary = architecture_policy_counts_summary(overall);
    let family_reports = families
        .into_iter()
        .map(|(rule_family, counts)| architecture_policy_family_report(rule_family, counts))
        .collect::<Vec<_>>();

    (summary, family_reports, case_reports)
}

fn architecture_policy_case_selector_matches(
    case: &ArchitecturePolicyBenchCase,
    finding: &ArchitecturePolicyActualFinding,
) -> bool {
    same_architecture_bench_path(&case.source_path, &finding.source_path)
        && same_architecture_bench_path(&case.target_path, &finding.target_path)
        && case.edge_type == finding.edge_type
        && case
            .rule_id
            .as_ref()
            .map(|rule_id| finding.rule_id.as_ref() == Some(rule_id))
            .unwrap_or(true)
}

fn architecture_policy_case_exact_match(
    case: &ArchitecturePolicyBenchCase,
    finding: &ArchitecturePolicyActualFinding,
) -> bool {
    architecture_policy_case_selector_matches(case, finding)
        && case.expected == finding.outcome
        && case.rule_family == finding.rule_family
}

fn same_architecture_bench_path(left: &Path, right: &Path) -> bool {
    normalize_path_fragment(&left.display().to_string())
        == normalize_path_fragment(&right.display().to_string())
}

fn architecture_policy_rule_family(
    policy: &ArchitecturePolicy,
    rule_id: &str,
) -> ArchitecturePolicyRuleFamily {
    if policy
        .dependency_rules
        .iter()
        .any(|rule| rule.id == rule_id)
    {
        ArchitecturePolicyRuleFamily::DependencyRule
    } else if policy
        .public_api_rules
        .iter()
        .any(|rule| rule.id == rule_id)
    {
        ArchitecturePolicyRuleFamily::PublicApiRule
    } else if policy
        .internal_only_rules
        .iter()
        .any(|rule| rule.id == rule_id)
    {
        ArchitecturePolicyRuleFamily::InternalOnlyRule
    } else {
        ArchitecturePolicyRuleFamily::Unknown
    }
}

fn architecture_policy_counts_summary(
    counts: ArchitecturePolicyBenchCounts,
) -> ArchitecturePolicyBenchSummary {
    ArchitecturePolicyBenchSummary {
        precision: ratio(counts.true_positives, counts.actual_positive_count),
        recall: ratio(counts.true_positives, counts.expected_positive_count),
        true_positives: counts.true_positives,
        false_positives: counts.false_positives,
        false_negatives: counts.false_negatives,
        expected_positive_count: counts.expected_positive_count,
        actual_positive_count: counts.actual_positive_count,
    }
}

fn architecture_policy_family_report(
    rule_family: ArchitecturePolicyRuleFamily,
    counts: ArchitecturePolicyBenchCounts,
) -> ArchitecturePolicyBenchFamilyReport {
    ArchitecturePolicyBenchFamilyReport {
        rule_family,
        precision: ratio(counts.true_positives, counts.actual_positive_count),
        recall: ratio(counts.true_positives, counts.expected_positive_count),
        true_positives: counts.true_positives,
        false_positives: counts.false_positives,
        false_negatives: counts.false_negatives,
        expected_positive_count: counts.expected_positive_count,
        actual_positive_count: counts.actual_positive_count,
    }
}

fn percentile_duration_ms(durations: &[Duration], percentile: f64) -> f64 {
    if durations.is_empty() {
        return 0.0;
    }
    let mut values = durations
        .iter()
        .map(|duration| duration_ms(*duration))
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((values.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    values[rank]
}

fn print_architecture_policy_bench_report(report: &ArchitecturePolicyBenchReport) {
    println!(
        "Architecture policy benchmark: {} case(s), p95 {:.2}ms",
        report.case_count, report.p95_policy_check_ms
    );
    println!(
        "Overall: precision {:.3}, recall {:.3}, TP {}, FP {}, FN {}",
        report.summary.precision,
        report.summary.recall,
        report.summary.true_positives,
        report.summary.false_positives,
        report.summary.false_negatives
    );
    for family in &report.rule_families {
        println!(
            "  {:?}: precision {:.3}, recall {:.3}, TP {}, FP {}, FN {}",
            family.rule_family,
            family.precision,
            family.recall,
            family.true_positives,
            family.false_positives,
            family.false_negatives
        );
    }
    for case in &report.cases {
        let status = if case.passed { "pass" } else { "fail" };
        println!(
            "  {status}: {} {:?} {:?} {} -> {} via {:?}",
            case.id,
            case.rule_family,
            case.expected,
            case.source_path.display(),
            case.target_path.display(),
            case.edge_type
        );
        for note in &case.notes {
            println!("    note: {note}");
        }
    }
}

fn run_history_bench(repo: &Path, args: HistoryBenchArgs) -> anyhow::Result<HistoryBenchReport> {
    let cases_file = if args.cases_file.is_absolute() {
        args.cases_file.clone()
    } else {
        repo.join(&args.cases_file)
    };
    let corpus = load_history_bench_corpus(&cases_file)?;
    if corpus.cases.is_empty() {
        anyhow::bail!(
            "history benchmark cases file is empty: {}",
            cases_file.display()
        );
    }

    let mut scores = HistoryBenchScoring::default();
    let mut cases = Vec::with_capacity(corpus.cases.len());
    let mut failures = Vec::new();
    for case in &corpus.cases {
        let report = score_history_bench_case(case, &mut scores)?;
        failures.extend(history_bench_failures(&report));
        cases.push(report);
    }

    let family_p95_ms = scores
        .family_latencies_ms
        .iter()
        .map(|(family, values)| (family.clone(), p95_ms(values)))
        .collect::<BTreeMap<_, _>>();

    Ok(HistoryBenchReport {
        cases_file,
        schema_version: corpus.schema_version,
        case_count: cases.len(),
        family_counts: scores.family_counts,
        min_reviewer_accuracy: args.min_reviewer_accuracy,
        reviewer_accuracy: ratio(scores.reviewer_passed, scores.reviewer_total),
        min_similar_recall_at_5: args.min_similar_recall_at_5,
        similar_recall_at_5: ratio(scores.similar_matched_total, scores.similar_expected_total),
        max_similar_p95_ms: args.max_similar_p95_ms,
        similar_p95_ms: p95_ms(&scores.similar_latencies_ms),
        max_lookup_p95_ms: args.max_lookup_p95_ms,
        ownership_churn_p95_ms: p95_ms(&scores.ownership_churn_latencies_ms),
        family_p95_ms,
        failures,
        cases,
    })
}

#[derive(Default)]
struct HistoryBenchScoring {
    family_counts: HistoryBenchFamilyCounts,
    similar_expected_total: usize,
    similar_matched_total: usize,
    reviewer_total: usize,
    reviewer_passed: usize,
    similar_latencies_ms: Vec<f64>,
    ownership_churn_latencies_ms: Vec<f64>,
    family_latencies_ms: BTreeMap<String, Vec<f64>>,
}

fn load_history_bench_corpus(path: &Path) -> anyhow::Result<HistoryBenchCorpus> {
    let raw = fs::read_to_string(path)?;
    let corpus: HistoryBenchCorpus = serde_json::from_str(&raw)?;
    if corpus.schema_version != 1 {
        anyhow::bail!(
            "unsupported history benchmark schema_version {}; expected 1",
            corpus.schema_version
        );
    }

    let mut top_ids = BTreeSet::new();
    let mut child_ids = BTreeSet::new();
    for case in &corpus.cases {
        if case.id.trim().is_empty() {
            anyhow::bail!("history benchmark cases require non-empty id");
        }
        if !top_ids.insert(case.id.clone()) {
            anyhow::bail!("duplicate history benchmark case id `{}`", case.id);
        }
        if case.similar.is_empty()
            && case.ownership.is_empty()
            && case.reviewers.is_empty()
            && case.churn.is_empty()
            && case.provenance.is_empty()
        {
            anyhow::bail!(
                "history benchmark case `{}` must include at least one public API family",
                case.id
            );
        }
        for child in &case.similar {
            validate_history_bench_child_id(&case.id, "similar", &child.id, &mut child_ids)?;
            if child.expected_top_5.is_empty() {
                anyhow::bail!(
                    "history benchmark similar case `{}::{}` requires expected_top_5",
                    case.id,
                    child.id
                );
            }
        }
        for child in &case.ownership {
            validate_history_bench_child_id(&case.id, "ownership", &child.id, &mut child_ids)?;
            if child.path.as_os_str().is_empty() || child.expected_owner.trim().is_empty() {
                anyhow::bail!(
                    "history benchmark ownership case `{}::{}` requires path and expected_owner",
                    case.id,
                    child.id
                );
            }
        }
        for child in &case.reviewers {
            validate_history_bench_child_id(&case.id, "reviewers", &child.id, &mut child_ids)?;
            if child.path.as_os_str().is_empty() || child.expected_top_reviewer.trim().is_empty() {
                anyhow::bail!(
                    "history benchmark reviewer case `{}::{}` requires path and expected_top_reviewer",
                    case.id,
                    child.id
                );
            }
        }
        for child in &case.churn {
            validate_history_bench_child_id(&case.id, "churn", &child.id, &mut child_ids)?;
            let provided = usize::from(child.path.is_some())
                + usize::from(child.module.is_some())
                + usize::from(child.symbol_id.is_some());
            if provided != 1 {
                anyhow::bail!(
                    "history benchmark churn case `{}::{}` must provide exactly one of path, module, or symbol_id",
                    case.id,
                    child.id
                );
            }
        }
        for child in &case.provenance {
            validate_history_bench_child_id(&case.id, "provenance", &child.id, &mut child_ids)?;
            if child.path.as_os_str().is_empty()
                || child.expected_first_seen.trim().is_empty()
                || child.expected_last_touched.trim().is_empty()
            {
                anyhow::bail!(
                    "history benchmark provenance case `{}::{}` requires path, expected_first_seen, and expected_last_touched",
                    case.id,
                    child.id
                );
            }
        }
    }
    Ok(corpus)
}

fn validate_history_bench_child_id(
    case_id: &str,
    family: &str,
    child_id: &str,
    seen: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    if child_id.trim().is_empty() {
        anyhow::bail!("history benchmark {family} cases require non-empty id");
    }
    let key = format!("{case_id}::{family}::{child_id}");
    if !seen.insert(key.clone()) {
        anyhow::bail!("duplicate history benchmark child case id `{key}`");
    }
    Ok(())
}

fn score_history_bench_case(
    case: &HistoryBenchCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchCaseReport> {
    let store = SqliteStore::open(":memory:")?;
    store.put_history_snapshot(&case.snapshot)?;
    let fixture_repo = prepare_history_bench_repo(&case.id, &case.codeowners)?;

    let similar = case
        .similar
        .iter()
        .map(|child| score_history_bench_similar(&store, child, scores))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let ownership = case
        .ownership
        .iter()
        .map(|child| score_history_bench_ownership(&fixture_repo.path, &store, child, scores))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let reviewers = case
        .reviewers
        .iter()
        .map(|child| score_history_bench_reviewer(&fixture_repo.path, &store, child, scores))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let churn = case
        .churn
        .iter()
        .map(|child| score_history_bench_churn(&store, child, scores))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let provenance = case
        .provenance
        .iter()
        .map(|child| score_history_bench_provenance(&store, child, scores))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let passed = similar.iter().all(|report| report.passed)
        && ownership.iter().all(|report| report.passed)
        && reviewers.iter().all(|report| report.passed)
        && churn.iter().all(|report| report.passed)
        && provenance.iter().all(|report| report.passed);

    Ok(HistoryBenchCaseReport {
        id: case.id.clone(),
        similar,
        ownership,
        reviewers,
        churn,
        provenance,
        passed,
    })
}

struct HistoryBenchTempRepo {
    path: PathBuf,
}

impl Drop for HistoryBenchTempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn prepare_history_bench_repo(
    case_id: &str,
    codeowners: &[String],
) -> anyhow::Result<HistoryBenchTempRepo> {
    let stamp = chrono::Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_else(|| chrono::Utc::now().timestamp_micros());
    let path = std::env::temp_dir().join(format!(
        "open-kioku-history-bench-{}-{}-{}",
        std::process::id(),
        stamp,
        sanitize_temp_path_fragment(case_id)
    ));
    fs::create_dir_all(&path)?;
    if !codeowners.is_empty() {
        let codeowners_dir = path.join(".github");
        fs::create_dir_all(&codeowners_dir)?;
        fs::write(codeowners_dir.join("CODEOWNERS"), codeowners.join("\n"))?;
    }
    Ok(HistoryBenchTempRepo { path })
}

fn score_history_bench_similar(
    store: &SqliteStore,
    case: &HistoryBenchSimilarCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchSimilarCaseReport> {
    let started_at = Instant::now();
    let report = store.similar_changes(&case.query, 5)?;
    let latency_ms = elapsed_ms(started_at);
    record_history_bench_latency(scores, "similar", latency_ms);
    scores.family_counts.similar += 1;

    let actual_top_5 = report
        .hits
        .iter()
        .map(|hit| hit.change.commit.id.0.clone())
        .collect::<Vec<_>>();
    let expected = case.expected_top_5.iter().cloned().collect::<BTreeSet<_>>();
    let actual = actual_top_5.iter().cloned().collect::<BTreeSet<_>>();
    let matched = expected.intersection(&actual).cloned().collect::<Vec<_>>();
    scores.similar_expected_total += expected.len();
    scores.similar_matched_total += matched.len();
    let recall_at_5 = ratio(matched.len(), expected.len());

    Ok(HistoryBenchSimilarCaseReport {
        id: case.id.clone(),
        expected_top_5: case.expected_top_5.clone(),
        actual_top_5,
        matched,
        recall_at_5,
        latency_ms,
        passed: recall_at_5 >= 1.0,
    })
}

fn score_history_bench_ownership(
    repo: &Path,
    store: &SqliteStore,
    case: &HistoryBenchOwnershipCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchOwnershipCaseReport> {
    let started_at = Instant::now();
    let report = open_kioku_git::ownership_for_path(open_kioku_git::OwnershipInput {
        repo,
        path: &case.path,
        history: store,
        memory_facts: &[],
        components: Vec::new(),
    })?;
    let latency_ms = elapsed_ms(started_at);
    record_history_bench_latency(scores, "ownership", latency_ms);
    scores.family_counts.ownership += 1;

    let rank = report
        .owners
        .iter()
        .position(|suggestion| owner_matches_expected(&suggestion.owner, &case.expected_owner))
        .map(|index| index + 1);
    let top = report.owners.first();
    let actual_owner = top.map(|suggestion| owner_display(&suggestion.owner));
    let actual_source_types = top
        .map(|suggestion| suggestion.source_types.clone())
        .unwrap_or_default();
    let source_types_match = case
        .expected_source_types
        .iter()
        .all(|expected| actual_source_types.contains(expected));

    Ok(HistoryBenchOwnershipCaseReport {
        id: case.id.clone(),
        path: case.path.clone(),
        expected_owner: case.expected_owner.clone(),
        actual_owner,
        rank,
        expected_source_types: case.expected_source_types.clone(),
        actual_source_types,
        latency_ms,
        passed: rank == Some(1) && source_types_match,
    })
}

fn score_history_bench_reviewer(
    repo: &Path,
    store: &SqliteStore,
    case: &HistoryBenchReviewerCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchReviewerCaseReport> {
    let started_at = Instant::now();
    let ownership = open_kioku_git::ownership_for_path(open_kioku_git::OwnershipInput {
        repo,
        path: &case.path,
        history: store,
        memory_facts: &[],
        components: Vec::new(),
    })?;
    let report = open_kioku_git::suggest_reviewers(open_kioku_git::ReviewerSuggestionInput {
        path: &case.path,
        history: store,
        ownership: Some(&ownership),
    })?;
    let latency_ms = elapsed_ms(started_at);
    record_history_bench_latency(scores, "reviewers", latency_ms);
    scores.family_counts.reviewers += 1;
    scores.reviewer_total += 1;

    let rank = report
        .suggestions
        .iter()
        .position(|suggestion| {
            owner_matches_expected(&suggestion.reviewer, &case.expected_top_reviewer)
        })
        .map(|index| index + 1);
    let top = report.suggestions.first();
    let actual_top_reviewer = top.map(|suggestion| owner_display(&suggestion.reviewer));
    let actual_review_evidence = top.map(|suggestion| suggestion.actual_review_evidence);
    let inferred_from_authors = top.map(|suggestion| suggestion.inferred_from_authors);
    let availability_correct = report.availability == case.expected_availability;
    let actual_review_evidence_correct = case
        .expected_actual_review_evidence
        .zip(actual_review_evidence)
        .map(|(expected, actual)| expected == actual)
        .unwrap_or(true);
    let inferred_from_authors_correct = case
        .expected_inferred_from_authors
        .zip(inferred_from_authors)
        .map(|(expected, actual)| expected == actual)
        .unwrap_or(true);
    let passed = rank == Some(1)
        && availability_correct
        && actual_review_evidence_correct
        && inferred_from_authors_correct;
    if passed {
        scores.reviewer_passed += 1;
    }

    Ok(HistoryBenchReviewerCaseReport {
        id: case.id.clone(),
        path: case.path.clone(),
        expected_top_reviewer: case.expected_top_reviewer.clone(),
        actual_top_reviewer,
        rank,
        expected_availability: case.expected_availability,
        availability: report.availability,
        availability_correct,
        expected_actual_review_evidence: case.expected_actual_review_evidence,
        actual_review_evidence,
        actual_review_evidence_correct,
        expected_inferred_from_authors: case.expected_inferred_from_authors,
        inferred_from_authors,
        inferred_from_authors_correct,
        latency_ms,
        passed,
    })
}

fn score_history_bench_churn(
    store: &SqliteStore,
    case: &HistoryBenchChurnCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchChurnCaseReport> {
    let target = history_bench_churn_target(case);
    let started_at = Instant::now();
    let summary = if let Some(path) = &case.path {
        store.churn_for_file(path)?
    } else if let Some(module) = &case.module {
        store.churn_for_module(module)?
    } else if let Some(symbol_id) = &case.symbol_id {
        store.churn_for_symbol(symbol_id)?
    } else {
        unreachable!("history benchmark churn targets are validated before scoring");
    };
    let latency_ms = elapsed_ms(started_at);
    record_history_bench_latency(scores, "churn", latency_ms);
    scores.family_counts.churn += 1;

    let passed = summary.stats.touch_count >= case.min_touch_count
        && summary.stats.hotspot_score >= case.min_hotspot_score;
    Ok(HistoryBenchChurnCaseReport {
        id: case.id.clone(),
        target,
        touch_count: summary.stats.touch_count,
        hotspot_score: summary.stats.hotspot_score,
        min_touch_count: case.min_touch_count,
        min_hotspot_score: case.min_hotspot_score,
        confidence: summary.confidence,
        latency_ms,
        passed,
    })
}

fn score_history_bench_provenance(
    store: &SqliteStore,
    case: &HistoryBenchProvenanceCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchProvenanceCaseReport> {
    let started_at = Instant::now();
    let report = store.provenance_for_path(&case.path, case.limit.unwrap_or(20))?;
    let latency_ms = elapsed_ms(started_at);
    record_history_bench_latency(scores, "provenance", latency_ms);
    scores.family_counts.provenance += 1;

    let actual_first_seen = report
        .first_seen
        .as_ref()
        .map(|touch| touch.commit.id.0.clone());
    let actual_last_touched = report
        .last_touched
        .as_ref()
        .map(|touch| touch.commit.id.0.clone());
    let passed = actual_first_seen.as_deref() == Some(case.expected_first_seen.as_str())
        && actual_last_touched.as_deref() == Some(case.expected_last_touched.as_str())
        && report.recent_touches.len() >= case.min_recent_touches;

    Ok(HistoryBenchProvenanceCaseReport {
        id: case.id.clone(),
        path: case.path.clone(),
        expected_first_seen: case.expected_first_seen.clone(),
        actual_first_seen,
        expected_last_touched: case.expected_last_touched.clone(),
        actual_last_touched,
        min_recent_touches: case.min_recent_touches,
        recent_touch_count: report.recent_touches.len(),
        confidence: report.confidence,
        latency_ms,
        passed,
    })
}

fn history_bench_churn_target(case: &HistoryBenchChurnCase) -> String {
    if let Some(path) = &case.path {
        format!("file:{}", path.display())
    } else if let Some(module) = &case.module {
        format!("module:{}", module.display())
    } else if let Some(symbol_id) = &case.symbol_id {
        format!("symbol:{}", symbol_id.0)
    } else {
        "unknown".into()
    }
}

fn history_bench_failures(report: &HistoryBenchCaseReport) -> Vec<String> {
    let mut failures = Vec::new();
    for case in &report.similar {
        if !case.passed {
            failures.push(format!(
                "{}::similar::{} expected Top-5 {:?}, got {:?}",
                report.id, case.id, case.expected_top_5, case.actual_top_5
            ));
        }
    }
    for case in &report.ownership {
        if !case.passed {
            failures.push(format!(
                "{}::ownership::{} expected owner `{}` at rank 1, got {:?} at rank {:?}",
                report.id, case.id, case.expected_owner, case.actual_owner, case.rank
            ));
        }
    }
    for case in &report.reviewers {
        if !case.passed {
            failures.push(format!(
                "{}::reviewers::{} expected reviewer `{}` at rank 1 with {:?}, got {:?} at rank {:?} with {:?}",
                report.id,
                case.id,
                case.expected_top_reviewer,
                case.expected_availability,
                case.actual_top_reviewer,
                case.rank,
                case.availability
            ));
        }
    }
    for case in &report.churn {
        if !case.passed {
            failures.push(format!(
                "{}::churn::{} expected touch_count >= {} and hotspot_score >= {:.3}, got {} and {:.3}",
                report.id,
                case.id,
                case.min_touch_count,
                case.min_hotspot_score,
                case.touch_count,
                case.hotspot_score
            ));
        }
    }
    for case in &report.provenance {
        if !case.passed {
            failures.push(format!(
                "{}::provenance::{} expected first/last {}/{}, got {:?}/{:?}",
                report.id,
                case.id,
                case.expected_first_seen,
                case.expected_last_touched,
                case.actual_first_seen,
                case.actual_last_touched
            ));
        }
    }
    failures
}

fn record_history_bench_latency(scores: &mut HistoryBenchScoring, family: &str, latency_ms: f64) {
    if family == "similar" {
        scores.similar_latencies_ms.push(latency_ms);
    }
    if matches!(family, "ownership" | "churn") {
        scores.ownership_churn_latencies_ms.push(latency_ms);
    }
    scores
        .family_latencies_ms
        .entry(family.to_string())
        .or_default()
        .push(latency_ms);
}

fn elapsed_ms(started_at: Instant) -> f64 {
    started_at.elapsed().as_secs_f64() * 1000.0
}

fn p95_ms(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    let index = ((sorted.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

fn default_history_bench_min_recent_touches() -> usize {
    1
}

fn print_history_bench_report(report: &HistoryBenchReport) {
    println!(
        "History API benchmark: {} case set(s); reviewer accuracy {:.3} (min {:.3}); similar Top-5 recall {:.3} (min {:.3})",
        report.case_count,
        report.reviewer_accuracy,
        report.min_reviewer_accuracy,
        report.similar_recall_at_5,
        report.min_similar_recall_at_5
    );
    println!(
        "Latency p95: similar {:.2}ms (max {:.2}ms); ownership/churn {:.2}ms (max {:.2}ms)",
        report.similar_p95_ms,
        report.max_similar_p95_ms,
        report.ownership_churn_p95_ms,
        report.max_lookup_p95_ms
    );
    println!(
        "Families: similar {}, ownership {}, reviewers {}, churn {}, provenance {}",
        report.family_counts.similar,
        report.family_counts.ownership,
        report.family_counts.reviewers,
        report.family_counts.churn,
        report.family_counts.provenance
    );
    for (family, latency_ms) in &report.family_p95_ms {
        println!("  {family}: p95 {latency_ms:.2}ms");
    }
    for case in &report.cases {
        let status = if case.passed { "pass" } else { "fail" };
        println!("  {status}: {}", case.id);
    }
    if !report.failures.is_empty() {
        println!("Failures:");
        for failure in &report.failures {
            println!("- {failure}");
        }
    }
}

fn run_reviewer_bench(repo: &Path, args: ReviewerBenchArgs) -> anyhow::Result<ReviewerBenchReport> {
    let cases_file = if args.cases_file.is_absolute() {
        args.cases_file.clone()
    } else {
        repo.join(&args.cases_file)
    };
    let cases = load_reviewer_bench_cases(&cases_file)?;
    if cases.is_empty() {
        anyhow::bail!(
            "reviewer benchmark cases file is empty: {}",
            cases_file.display()
        );
    }

    let mut reports = Vec::with_capacity(cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let report = score_reviewer_bench_case(case)?;
        if !report.passed {
            failures.push(format!(
                "{} expected top reviewer `{}` at rank 1, got {:?} at rank {:?}",
                case.id, case.expected_top_reviewer, report.actual_top_reviewer, report.rank
            ));
        }
        reports.push(report);
    }

    let passed = reports.iter().filter(|case| case.passed).count();
    Ok(ReviewerBenchReport {
        cases_file,
        case_count: reports.len(),
        min_accuracy: args.min_accuracy,
        accuracy: ratio(passed, reports.len()),
        failures,
        cases: reports,
    })
}

fn run_similar_history_bench(
    repo: &Path,
    args: SimilarHistoryBenchArgs,
) -> anyhow::Result<SimilarHistoryBenchReport> {
    let cases_file = if args.cases_file.is_absolute() {
        args.cases_file.clone()
    } else {
        repo.join(&args.cases_file)
    };
    let cases = load_similar_history_bench_cases(&cases_file)?;
    if cases.is_empty() {
        anyhow::bail!(
            "similar history benchmark cases file is empty: {}",
            cases_file.display()
        );
    }

    let mut reports = Vec::with_capacity(cases.len());
    let mut failures = Vec::new();
    let mut expected_total = 0_usize;
    let mut matched_total = 0_usize;
    for case in &cases {
        let report = score_similar_history_bench_case(case)?;
        expected_total += report.expected_top_5.len();
        matched_total += report.matched.len();
        if !report.passed {
            failures.push(format!(
                "{} expected Top-5 commit(s) {:?}, got {:?}",
                case.id, report.expected_top_5, report.actual_top_5
            ));
        }
        reports.push(report);
    }

    Ok(SimilarHistoryBenchReport {
        cases_file,
        case_count: reports.len(),
        min_recall_at_5: args.min_recall_at_5,
        recall_at_5: ratio(matched_total, expected_total),
        failures,
        cases: reports,
    })
}

fn load_reviewer_bench_cases(path: &Path) -> anyhow::Result<Vec<ReviewerBenchCase>> {
    let raw = fs::read_to_string(path)?;
    let cases: Vec<ReviewerBenchCase> = serde_json::from_str(&raw)?;
    let mut ids = BTreeSet::new();
    for case in &cases {
        if case.id.trim().is_empty() || case.expected_top_reviewer.trim().is_empty() {
            anyhow::bail!(
                "reviewer benchmark cases require non-empty id and expected_top_reviewer"
            );
        }
        if case.path.as_os_str().is_empty() {
            anyhow::bail!("reviewer benchmark case `{}` requires a path", case.id);
        }
        if !ids.insert(case.id.clone()) {
            anyhow::bail!("duplicate reviewer benchmark case id `{}`", case.id);
        }
    }
    Ok(cases)
}

fn load_similar_history_bench_cases(path: &Path) -> anyhow::Result<Vec<SimilarHistoryBenchCase>> {
    let raw = fs::read_to_string(path)?;
    let cases: Vec<SimilarHistoryBenchCase> = serde_json::from_str(&raw)?;
    let mut ids = BTreeSet::new();
    for case in &cases {
        if case.id.trim().is_empty() || case.expected_top_5.is_empty() {
            anyhow::bail!(
                "similar history benchmark cases require non-empty id and expected_top_5"
            );
        }
        if !ids.insert(case.id.clone()) {
            anyhow::bail!("duplicate similar history benchmark case id `{}`", case.id);
        }
    }
    Ok(cases)
}

fn score_similar_history_bench_case(
    case: &SimilarHistoryBenchCase,
) -> anyhow::Result<SimilarHistoryBenchCaseReport> {
    let store = SqliteStore::open(":memory:")?;
    store.put_history_snapshot(&case.snapshot)?;
    let report = store.similar_changes(&case.query, 5)?;
    let actual_top_5 = report
        .hits
        .iter()
        .map(|hit| hit.change.commit.id.0.clone())
        .collect::<Vec<_>>();
    let expected = case.expected_top_5.iter().cloned().collect::<BTreeSet<_>>();
    let actual = actual_top_5.iter().cloned().collect::<BTreeSet<_>>();
    let matched = expected.intersection(&actual).cloned().collect::<Vec<_>>();
    let recall_at_5 = ratio(matched.len(), expected.len());

    Ok(SimilarHistoryBenchCaseReport {
        id: case.id.clone(),
        expected_top_5: case.expected_top_5.clone(),
        actual_top_5,
        matched,
        recall_at_5,
        passed: recall_at_5 >= 1.0,
    })
}

fn score_reviewer_bench_case(case: &ReviewerBenchCase) -> anyhow::Result<ReviewerBenchCaseReport> {
    let history = ReviewerBenchHistoryStore::from_case(case);
    let ownership = reviewer_bench_ownership_report(case);
    let report = open_kioku_git::suggest_reviewers(open_kioku_git::ReviewerSuggestionInput {
        path: &case.path,
        history: &history,
        ownership: Some(&ownership),
    })?;

    let rank = report
        .suggestions
        .iter()
        .position(|suggestion| {
            owner_matches_expected(&suggestion.reviewer, &case.expected_top_reviewer)
        })
        .map(|index| index + 1);
    let top = report.suggestions.first();
    let actual_top_reviewer = top.map(|suggestion| owner_display(&suggestion.reviewer));
    let actual_review_evidence = top.map(|suggestion| suggestion.actual_review_evidence);
    let inferred_from_authors = top.map(|suggestion| suggestion.inferred_from_authors);
    let top_score = top.map(|suggestion| suggestion.score);

    let availability_correct = report.availability == case.expected_availability;
    let actual_review_evidence_correct = case
        .expected_actual_review_evidence
        .zip(actual_review_evidence)
        .map(|(expected, actual)| expected == actual)
        .unwrap_or(true);
    let inferred_from_authors_correct = case
        .expected_inferred_from_authors
        .zip(inferred_from_authors)
        .map(|(expected, actual)| expected == actual)
        .unwrap_or(true);
    let passed = rank == Some(1)
        && availability_correct
        && actual_review_evidence_correct
        && inferred_from_authors_correct;

    Ok(ReviewerBenchCaseReport {
        id: case.id.clone(),
        path: case.path.clone(),
        expected_top_reviewer: case.expected_top_reviewer.clone(),
        actual_top_reviewer,
        rank,
        expected_availability: case.expected_availability,
        availability: report.availability,
        availability_correct,
        expected_actual_review_evidence: case.expected_actual_review_evidence,
        actual_review_evidence,
        actual_review_evidence_correct,
        expected_inferred_from_authors: case.expected_inferred_from_authors,
        inferred_from_authors,
        inferred_from_authors_correct,
        top_score,
        passed,
    })
}

#[derive(Clone)]
struct ReviewerBenchHistoryStore {
    history: HistorySummary,
    provenance: FileProvenance,
}

impl ReviewerBenchHistoryStore {
    fn from_case(case: &ReviewerBenchCase) -> Self {
        let mut reviewer_evidence = Vec::with_capacity(case.review_evidence.len());
        for (index, evidence) in case.review_evidence.iter().enumerate() {
            reviewer_evidence.push(ReviewerEvidence {
                id: HistoryRecordId::new(format!("reviewer-bench:{}:{index}", case.id)),
                commit_id: Some(GitCommitId::new(format!(
                    "reviewer-bench-{}-{index}",
                    case.id
                ))),
                path: Some(case.path.clone()),
                reviewer: owner_from_token(&evidence.reviewer),
                role: evidence.role,
                observed_at: reviewer_bench_time(evidence.days_ago),
                source: evidence
                    .source
                    .clone()
                    .unwrap_or_else(|| format!("reviewer-bench:{}", case.id)),
                confidence: evidence.confidence,
            });
        }

        let mut touches = Vec::new();
        let mut touch_index = 0usize;
        for touch in &case.author_touches {
            for offset in 0..touch.count.max(1) {
                touches.push(reviewer_bench_touch(case, touch, touch_index, offset));
                touch_index += 1;
            }
        }
        touches.sort_by(|left, right| {
            right
                .commit
                .committed_at
                .cmp(&left.commit.committed_at)
                .then_with(|| left.commit.id.0.cmp(&right.commit.id.0))
        });

        Self {
            history: HistorySummary {
                path: case.path.clone(),
                recent_commits: Vec::new(),
                file_touches: Vec::new(),
                symbol_touches: Vec::new(),
                cochange_neighbors: Vec::new(),
                reviewer_evidence,
                truncated: false,
                uncertainty: Vec::new(),
            },
            provenance: FileProvenance {
                path: case.path.clone(),
                first_seen: touches.last().cloned(),
                last_touched: touches.first().cloned(),
                recent_touches: touches,
                confidence: Confidence::High,
                truncated: false,
                uncertainty: Vec::new(),
            },
        }
    }
}

impl HistoryStore for ReviewerBenchHistoryStore {
    fn put_history_snapshot(&self, _snapshot: &HistorySnapshot) -> open_kioku_errors::Result<()> {
        Ok(())
    }

    fn history_for_file(
        &self,
        path: &Path,
        _limit: usize,
    ) -> open_kioku_errors::Result<HistorySummary> {
        if path == self.history.path {
            Ok(self.history.clone())
        } else {
            Ok(HistorySummary::empty(path))
        }
    }

    fn provenance_for_path(
        &self,
        path: &Path,
        _limit: usize,
    ) -> open_kioku_errors::Result<FileProvenance> {
        if path == self.provenance.path {
            Ok(self.provenance.clone())
        } else {
            Ok(FileProvenance {
                path: path.to_path_buf(),
                first_seen: None,
                last_touched: None,
                recent_touches: Vec::new(),
                confidence: Confidence::Low,
                truncated: false,
                uncertainty: vec!["reviewer benchmark provenance unavailable for this path".into()],
            })
        }
    }

    fn provenance_for_symbol(
        &self,
        symbol_id: &SymbolId,
        _limit: usize,
    ) -> open_kioku_errors::Result<SymbolProvenance> {
        Ok(SymbolProvenance {
            symbol_id: symbol_id.clone(),
            qualified_name: "reviewer_bench::unknown".into(),
            file_path: self.provenance.path.clone(),
            range: None,
            first_seen: None,
            last_touched: None,
            recent_touches: Vec::new(),
            confidence: Confidence::Low,
            truncated: false,
            uncertainty: vec!["reviewer benchmark symbol provenance unavailable".into()],
        })
    }

    fn cochange_neighbors(
        &self,
        _path: &Path,
        _limit: usize,
    ) -> open_kioku_errors::Result<Vec<GitCochangeEdge>> {
        Ok(Vec::new())
    }

    fn recent_commits(&self, _limit: usize) -> open_kioku_errors::Result<Vec<GitCommitRecord>> {
        Ok(Vec::new())
    }
}

fn reviewer_bench_ownership_report(case: &ReviewerBenchCase) -> OwnershipReport {
    let generated_at = chrono::Utc::now();
    let owners = case
        .ownership
        .iter()
        .enumerate()
        .map(|(index, evidence)| {
            let owner = owner_from_token(&evidence.owner);
            let source_types = if evidence.source_types.is_empty() {
                default_reviewer_bench_source_types()
            } else {
                evidence.source_types.clone()
            };
            let observed_at = reviewer_bench_time(evidence.days_ago);
            let stale = reviewer_bench_is_stale(evidence.days_ago);
            let confidence = Confidence::from_score(evidence.score);
            let source = evidence
                .source
                .clone()
                .unwrap_or_else(|| format!("reviewer-bench:{}:{index}", case.id));
            let ownership_evidence = source_types
                .iter()
                .map(|source_type| OwnershipEvidence {
                    source_type: *source_type,
                    owner: owner.clone(),
                    source: source.clone(),
                    message: "reviewer benchmark ownership signal".into(),
                    confidence,
                    observed_at: Some(observed_at),
                    stale,
                })
                .collect::<Vec<_>>();
            OwnerSuggestion {
                owner,
                rationale: "reviewer benchmark ownership signal".into(),
                confidence,
                score: evidence.score,
                source_types: source_types.clone(),
                stale,
                evidence: ownership_evidence,
                confidence_breakdown: open_kioku_core::OwnershipConfidenceBreakdown {
                    codeowners: if source_types.contains(&OwnershipSourceType::Codeowners) {
                        evidence.score
                    } else {
                        0.0
                    },
                    git_history: if source_types.contains(&OwnershipSourceType::GitHistory) {
                        evidence.score
                    } else {
                        0.0
                    },
                    memory: if source_types.contains(&OwnershipSourceType::RepoMemory) {
                        evidence.score
                    } else {
                        0.0
                    },
                    freshness: if stale { 0.0 } else { 0.05 },
                    ambiguity_penalty: 0.0,
                    final_score: evidence.score,
                },
            }
        })
        .collect();

    OwnershipReport {
        path: case.path.clone(),
        components: Vec::new(),
        generated_at,
        owners,
        uncertainty: Vec::new(),
    }
}

fn reviewer_bench_touch(
    case: &ReviewerBenchCase,
    touch: &ReviewerBenchAuthorTouch,
    index: usize,
    offset: usize,
) -> ProvenanceTouch {
    let author = owner_from_token(&touch.author);
    let observed_at = reviewer_bench_time(touch.days_ago + offset as i64);
    let commit_id = GitCommitId::new(format!("reviewer-bench-{}-touch-{index}", case.id));
    ProvenanceTouch {
        commit: GitCommitRecord {
            id: commit_id,
            parent_ids: Vec::new(),
            author: author.clone(),
            committer: None,
            authored_at: observed_at,
            committed_at: observed_at,
            summary: format!("reviewer benchmark touch by {}", owner_display(&author)),
            message: format!("reviewer benchmark touch by {}", owner_display(&author)),
            file_count: 1,
        },
        path: case.path.clone(),
        previous_path: None,
        symbol_id: None,
        qualified_name: None,
        change_kind: GitChangeKind::Modified,
        line_ranges: Vec::new(),
        confidence: Confidence::High,
        uncertainty: Vec::new(),
    }
}

fn default_reviewer_bench_confidence() -> Confidence {
    Confidence::High
}

fn default_reviewer_bench_source_types() -> Vec<OwnershipSourceType> {
    vec![OwnershipSourceType::Codeowners]
}

fn default_reviewer_bench_owner_score() -> f32 {
    0.90
}

fn default_reviewer_bench_touch_count() -> usize {
    1
}

fn reviewer_bench_time(days_ago: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() - chrono::Duration::days(days_ago.max(0))
}

fn reviewer_bench_is_stale(days_ago: i64) -> bool {
    days_ago > 365
}

fn owner_from_token(value: &str) -> Owner {
    let trimmed = value.trim();
    if let (Some(start), Some(end)) = (trimmed.rfind('<'), trimmed.rfind('>')) {
        if start < end {
            let name = trimmed[..start].trim();
            let email = trimmed[start + 1..end].trim();
            return Owner {
                name: if name.is_empty() {
                    owner_name_from_email(email)
                } else {
                    name.to_string()
                },
                email: (!email.is_empty()).then(|| email.to_string()),
            };
        }
    }
    if trimmed.contains('@') {
        Owner {
            name: owner_name_from_email(trimmed),
            email: Some(trimmed.to_string()),
        }
    } else {
        Owner {
            name: trimmed.to_string(),
            email: None,
        }
    }
}

fn owner_name_from_email(email: &str) -> String {
    email.split('@').next().unwrap_or(email).to_string()
}

fn owner_matches_expected(owner: &Owner, expected: &str) -> bool {
    let expected = expected.trim().to_ascii_lowercase();
    owner
        .email
        .as_deref()
        .is_some_and(|email| email.eq_ignore_ascii_case(&expected))
        || owner.name.eq_ignore_ascii_case(&expected)
}

fn owner_display(owner: &Owner) -> String {
    owner.email.clone().unwrap_or_else(|| owner.name.clone())
}

fn print_reviewer_bench_report(report: &ReviewerBenchReport) {
    println!(
        "Reviewer benchmark: {} case(s), accuracy {:.3}, min {:.3}",
        report.case_count, report.accuracy, report.min_accuracy
    );
    for case in &report.cases {
        println!(
            "  {}: rank={:?} top={:?} availability={:?} score={:?} passed={}",
            case.id,
            case.rank,
            case.actual_top_reviewer,
            case.availability,
            case.top_score,
            case.passed
        );
    }
    if !report.failures.is_empty() {
        println!("Failures:");
        for failure in &report.failures {
            println!("- {failure}");
        }
    }
}

fn print_similar_history_bench_report(report: &SimilarHistoryBenchReport) {
    println!(
        "Similar history benchmark: {} case(s), Top-5 recall {:.3}, min {:.3}",
        report.case_count, report.recall_at_5, report.min_recall_at_5
    );
    for case in &report.cases {
        println!(
            "  {}: recall_at_5={:.3} expected={:?} actual={:?} passed={}",
            case.id, case.recall_at_5, case.expected_top_5, case.actual_top_5, case.passed
        );
    }
    if !report.failures.is_empty() {
        println!("Failures:");
        for failure in &report.failures {
            println!("- {failure}");
        }
    }
}

fn run_workflow_bench(args: WorkflowBenchArgs) -> anyhow::Result<WorkflowBenchReport> {
    let repo = absolutize(&args.path)?;
    let cases_file = if args.cases_file.is_absolute() {
        args.cases_file.clone()
    } else {
        repo.join(&args.cases_file)
    };
    let cases = load_workflow_bench_cases(&cases_file)?;
    if cases.is_empty() {
        anyhow::bail!(
            "workflow benchmark cases file is empty: {}",
            cases_file.display()
        );
    }
    if !args.no_index {
        index_repo(&repo)?;
    }
    let store = open_store(&repo)?;
    let index_dir = default_index_dir(&repo);
    let search_index = if TantivySearchIndex::exists(&index_dir) {
        Some(TantivySearchIndex::open_or_create(&index_dir)?)
    } else {
        None
    };
    let planner = PlanEngine::new(&store as &dyn OkStore)
        .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
        .with_history_store(Some(&store));
    let verifier = ChangeVerifier::new(&store as &dyn OkStore)
        .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex));
    let limit = args.limit.clamp(1, 100);
    let mut reports = Vec::with_capacity(cases.len());
    for case in cases {
        let baseline_paths = baseline_context_paths(&repo, &store, &case.task, limit, &cases_file)?;
        let plan = workflow_plan(&repo, &store, &planner, &case.task, limit, &cases_file)?;
        reports.push(score_workflow_case(
            &repo,
            &verifier,
            &case,
            &plan,
            &baseline_paths,
            limit,
        )?);
    }
    let workflow = summarize_workflow_cases(&reports, false);
    let baseline = summarize_workflow_cases(&reports, true);
    let deltas = WorkflowBenchDeltas {
        context_recall_at_k: workflow.context_recall_at_k - baseline.context_recall_at_k,
        impact_recall_at_k: workflow.impact_recall_at_k - baseline.impact_recall_at_k,
        test_recall_at_k: workflow.test_recall_at_k - baseline.test_recall_at_k,
        boundary_precision: workflow.boundary_precision - baseline.boundary_precision,
        boundary_recall: workflow.boundary_recall - baseline.boundary_recall,
        confidence_calibration_error: baseline.confidence_calibration_error
            - workflow.confidence_calibration_error,
        verification_verdict_accuracy: workflow.verification_verdict_accuracy
            - baseline.verification_verdict_accuracy,
    };
    Ok(WorkflowBenchReport {
        repo,
        cases_file,
        limit,
        case_count: reports.len(),
        baseline,
        workflow,
        deltas,
        cases: reports,
    })
}

fn load_workflow_bench_cases(path: &Path) -> anyhow::Result<Vec<WorkflowBenchCase>> {
    let raw = fs::read_to_string(path)?;
    let cases: Vec<WorkflowBenchCase> = serde_json::from_str(&raw)?;
    for case in &cases {
        if case.id.trim().is_empty() || case.task.trim().is_empty() {
            anyhow::bail!("workflow benchmark cases require non-empty id and task");
        }
    }
    Ok(cases)
}

fn baseline_context_paths(
    repo: &Path,
    store: &dyn MetadataStore,
    task: &str,
    limit: usize,
    cases_file: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut raw = search_raw(repo, store, task, ranking_candidate_limit(limit))?;
    filter_workflow_benchmark_artifacts(&mut raw, repo, cases_file);
    Ok(top_unique_paths(rerank_baseline(raw), limit)
        .into_iter()
        .map(|result| result.path)
        .collect())
}

fn workflow_plan(
    repo: &Path,
    store: &SqliteStore,
    planner: &PlanEngine,
    task: &str,
    limit: usize,
    cases_file: &Path,
) -> anyhow::Result<PlanReport> {
    let mut context = build_context_pack(repo, store, task, limit)?;
    context
        .primary_files
        .retain(|result| !is_workflow_benchmark_artifact(&result.path, repo, cases_file));
    context
        .supporting_files
        .retain(|result| !is_workflow_benchmark_artifact(&result.path, repo, cases_file));
    planner
        .plan_from_context(task, limit, context)
        .map_err(Into::into)
}

fn filter_workflow_benchmark_artifacts(
    results: &mut Vec<open_kioku_core::SearchResult>,
    repo: &Path,
    cases_file: &Path,
) {
    results.retain(|result| !is_workflow_benchmark_artifact(&result.path, repo, cases_file));
}

fn is_workflow_benchmark_artifact(path: &Path, repo: &Path, cases_file: &Path) -> bool {
    let normalized = normalize_path_fragment(&path.to_string_lossy());
    let cases = cases_file
        .strip_prefix(repo)
        .unwrap_or(cases_file)
        .to_string_lossy();
    normalized == normalize_path_fragment(&cases) || normalized.starts_with("benchmarks/")
}

fn score_workflow_case(
    repo: &Path,
    verifier: &ChangeVerifier,
    case: &WorkflowBenchCase,
    plan: &PlanReport,
    baseline_paths: &[PathBuf],
    limit: usize,
) -> anyhow::Result<WorkflowBenchCaseReport> {
    let context_paths = plan
        .primary_context
        .iter()
        .take(limit)
        .map(|result| result.path.clone())
        .collect::<Vec<_>>();
    let impact_paths = plan
        .impact
        .direct_impacts
        .iter()
        .chain(plan.impact.indirect_impacts.iter())
        .take(limit)
        .map(|result| result.path.clone())
        .collect::<Vec<_>>();
    let test_names = plan
        .validation
        .iter()
        .take(limit)
        .map(|test| test.name.clone())
        .collect::<Vec<_>>();
    let boundary_paths = plan
        .recommended_change_boundary
        .allowed_files
        .iter()
        .chain(plan.recommended_change_boundary.caution_files.iter())
        .cloned()
        .collect::<Vec<_>>();

    let context_hits = matching_expected_values(&case.expected_primary_context, &context_paths);
    let impact_hits = matching_expected_values(&case.expected_impact, &impact_paths);
    let test_hits = matching_expected_strings(&case.expected_tests, &test_names);
    let boundary_hits = matching_expected_values(&case.expected_boundary, &boundary_paths);
    let forbidden_boundary_hits = matching_expected_values(&case.forbidden_paths, &boundary_paths);
    let baseline_context_hits =
        matching_expected_values(&case.expected_primary_context, baseline_paths);

    let expected_success = case.expected_confidence.unwrap_or_else(|| {
        !case
            .expected_verdict
            .is_some_and(|verdict| verdict == VerificationVerdict::Fail)
    });
    let confidence_probability = plan_success_probability(plan);
    let confidence_calibration_error =
        Some((confidence_probability - if expected_success { 1.0 } else { 0.0 }).abs());

    let verification = if case.expected_verdict.is_some()
        && (!case.changed_files.is_empty() || case.unified_diff.is_some())
    {
        Some(verifier.verify(
            repo,
            plan,
            VerifyChangeInput {
                changed_files: case.changed_files.clone(),
                unified_diff: case.unified_diff.clone(),
                evidence_refs: Vec::new(),
                run_commands: false,
                write_attestation: false,
                validation_attestations: Vec::new(),
                traceability_strict: false,
                check_api_surface: false,
                check_dependency_delta: false,
                architecture_policy: None,
                suppress_plan_validation_pending: false,
            },
        )?)
    } else {
        None
    };
    let actual_verdict = verification.as_ref().map(|report| report.verdict);
    let verification_correct = case
        .expected_verdict
        .zip(actual_verdict)
        .map(|(expected, actual)| expected == actual);

    Ok(WorkflowBenchCaseReport {
        id: case.id.clone(),
        task: case.task.clone(),
        context_recall: ratio(context_hits.len(), case.expected_primary_context.len()),
        impact_recall: ratio(impact_hits.len(), case.expected_impact.len()),
        test_recall: ratio(test_hits.len(), case.expected_tests.len()),
        boundary_precision: boundary_precision(&boundary_paths, &case.forbidden_paths),
        boundary_recall: ratio(boundary_hits.len(), case.expected_boundary.len()),
        confidence_expected_success: Some(expected_success),
        confidence_probability,
        confidence_calibration_error,
        expected_verdict: case.expected_verdict,
        actual_verdict,
        verification_correct,
        baseline_context_recall: ratio(
            baseline_context_hits.len(),
            case.expected_primary_context.len(),
        ),
        baseline_impact_recall: 0.0,
        baseline_test_recall: 0.0,
        context_hits,
        impact_hits,
        test_hits,
        boundary_hits,
        forbidden_boundary_hits,
        top_context_paths: context_paths,
        top_impact_paths: impact_paths,
        top_tests: test_names,
    })
}

fn summarize_workflow_cases(
    reports: &[WorkflowBenchCaseReport],
    baseline: bool,
) -> WorkflowBenchSummary {
    let count = reports.len() as f64;
    let verification = reports
        .iter()
        .filter_map(|case| case.verification_correct)
        .collect::<Vec<_>>();
    let calibration = reports
        .iter()
        .filter_map(|case| case.confidence_calibration_error)
        .collect::<Vec<_>>();
    WorkflowBenchSummary {
        context_recall_at_k: mean(
            reports
                .iter()
                .map(|case| {
                    if baseline {
                        case.baseline_context_recall
                    } else {
                        case.context_recall
                    }
                })
                .sum::<f64>(),
            count,
        ),
        impact_recall_at_k: mean(
            reports
                .iter()
                .map(|case| {
                    if baseline {
                        case.baseline_impact_recall
                    } else {
                        case.impact_recall
                    }
                })
                .sum::<f64>(),
            count,
        ),
        test_recall_at_k: mean(
            reports
                .iter()
                .map(|case| {
                    if baseline {
                        case.baseline_test_recall
                    } else {
                        case.test_recall
                    }
                })
                .sum::<f64>(),
            count,
        ),
        boundary_precision: if baseline {
            0.0
        } else {
            mean(
                reports
                    .iter()
                    .map(|case| case.boundary_precision)
                    .sum::<f64>(),
                count,
            )
        },
        boundary_recall: if baseline {
            0.0
        } else {
            mean(
                reports.iter().map(|case| case.boundary_recall).sum::<f64>(),
                count,
            )
        },
        confidence_calibration_error: if baseline {
            1.0
        } else {
            mean(calibration.iter().sum::<f64>(), calibration.len() as f64)
        },
        verification_verdict_accuracy: if baseline {
            0.0
        } else {
            mean(
                verification.iter().filter(|correct| **correct).count() as f64,
                verification.len() as f64,
            )
        },
    }
}

fn boundary_precision(selected: &[PathBuf], forbidden: &[String]) -> f64 {
    if selected.is_empty() {
        return 1.0;
    }
    let forbidden_hits = matching_expected_values(forbidden, selected).len();
    (selected.len().saturating_sub(forbidden_hits)) as f64 / selected.len() as f64
}

fn plan_success_probability(plan: &PlanReport) -> f64 {
    match plan.risk.level.as_str() {
        "low" => 0.85,
        "medium" => 0.6,
        "high" => 0.3,
        "critical" => 0.1,
        _ => 0.5,
    }
}

fn mean(sum: f64, count: f64) -> f64 {
    if count == 0.0 {
        1.0
    } else {
        sum / count
    }
}

fn print_workflow_bench_report(report: &WorkflowBenchReport) {
    println!(
        "Workflow benchmark: {} case(s), limit {}",
        report.case_count, report.limit
    );
    println!(
        "Workflow: context recall {:.3}, impact recall {:.3}, test recall {:.3}, boundary precision {:.3}, boundary recall {:.3}, calibration error {:.3}, verification accuracy {:.3}",
        report.workflow.context_recall_at_k,
        report.workflow.impact_recall_at_k,
        report.workflow.test_recall_at_k,
        report.workflow.boundary_precision,
        report.workflow.boundary_recall,
        report.workflow.confidence_calibration_error,
        report.workflow.verification_verdict_accuracy
    );
    println!(
        "Deltas vs baseline: context {:+.3}, impact {:+.3}, test {:+.3}, boundary precision {:+.3}, boundary recall {:+.3}, calibration {:+.3}, verification {:+.3}",
        report.deltas.context_recall_at_k,
        report.deltas.impact_recall_at_k,
        report.deltas.test_recall_at_k,
        report.deltas.boundary_precision,
        report.deltas.boundary_recall,
        report.deltas.confidence_calibration_error,
        report.deltas.verification_verdict_accuracy
    );
    for case in &report.cases {
        let verdict = case
            .actual_verdict
            .map(|verdict| format!("{verdict:?}"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {}: context {:.3}, impact {:.3}, tests {:.3}, boundary {:.3}/{:.3}, verdict {}",
            case.id,
            case.context_recall,
            case.impact_recall,
            case.test_recall,
            case.boundary_precision,
            case.boundary_recall,
            verdict
        );
    }
}

const REQUIRED_CONTRACT_BENCH_RULE_FAMILIES: [ContractBenchRuleFamily; 7] = [
    ContractBenchRuleFamily::AllowedEdit,
    ContractBenchRuleFamily::ForbiddenEdit,
    ContractBenchRuleFamily::MissingTests,
    ContractBenchRuleFamily::ArchitectureViolation,
    ContractBenchRuleFamily::DependencyDelta,
    ContractBenchRuleFamily::ApiSurfaceDelta,
    ContractBenchRuleFamily::ExplanationQuality,
];

fn run_contract_bench(args: ContractBenchArgs) -> anyhow::Result<ContractBenchReport> {
    let repo = absolutize(&args.path)?;
    let cases_file = resolve_contract_bench_cases_file(&repo, &args.cases_file)?;
    let cases = load_contract_bench_cases(&cases_file)?;
    validate_contract_bench_coverage(&cases)?;
    let limit = args.limit.clamp(1, 100);
    let mut reports = Vec::with_capacity(cases.len());

    for case in cases {
        let temp_repo = prepare_contract_bench_repo(&repo, &case.id)?;
        if !args.no_index {
            index_repo(&temp_repo.path)?;
        }
        let store = open_store(&temp_repo.path)?;
        let index_dir = default_index_dir(&temp_repo.path);
        let search_index = if TantivySearchIndex::exists(&index_dir) {
            Some(TantivySearchIndex::open_or_create(&index_dir)?)
        } else {
            None
        };
        let planner = PlanEngine::new(&store as &dyn OkStore)
            .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
            .with_history_store(Some(&store));
        let generation_started = Instant::now();
        let plan = workflow_plan(
            &temp_repo.path,
            &store,
            &planner,
            &case.task,
            limit,
            &cases_file,
        )?;
        let mut contract = ContractBuilder::from_plan(&plan)?;
        apply_contract_bench_overlay(&mut contract, &case.contract_overlay)?;
        let generation_ms = duration_ms(generation_started.elapsed());
        apply_contract_bench_edits(&temp_repo.path, &case)?;
        let verifier = ContractVerifier::new(&store as &dyn OkStore)
            .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex));
        let verification_started = Instant::now();
        let verification = verifier.verify(
            &temp_repo.path,
            &contract,
            VerifyChangeInput {
                changed_files: contract_bench_changed_files(&case),
                unified_diff: case.unified_diff.clone(),
                evidence_refs: Vec::new(),
                run_commands: false,
                write_attestation: false,
                validation_attestations: Vec::new(),
                traceability_strict: case.traceability_strict,
                check_api_surface: case.check_api_surface,
                check_dependency_delta: case.check_dependency_delta,
                architecture_policy: load_architecture_policy(&temp_repo.path)?,
                suppress_plan_validation_pending: false,
            },
        )?;
        let verification_ms = duration_ms(verification_started.elapsed());
        reports.push(score_contract_bench_case(
            &case,
            &contract,
            &verification,
            generation_ms,
            verification_ms,
        )?);
    }

    let summary = summarize_contract_bench_cases(&reports);
    let rule_families = summarize_contract_bench_families(&reports);
    let failures = reports
        .iter()
        .filter(|case| !case.passed)
        .map(|case| case.id.clone())
        .collect::<Vec<_>>();
    Ok(ContractBenchReport {
        repo,
        cases_file,
        limit,
        case_count: reports.len(),
        summary,
        rule_families,
        failures,
        cases: reports,
    })
}

fn resolve_contract_bench_cases_file(repo: &Path, cases_file: &Path) -> anyhow::Result<PathBuf> {
    if cases_file.is_absolute() {
        return Ok(cases_file.to_path_buf());
    }
    let repo_relative = repo.join(cases_file);
    if repo_relative.exists() {
        return Ok(repo_relative);
    }
    absolutize(cases_file)
}

fn load_contract_bench_cases(path: &Path) -> anyhow::Result<Vec<ContractBenchCase>> {
    let raw = fs::read_to_string(path)?;
    let cases: Vec<ContractBenchCase> = serde_json::from_str(&raw)?;
    let mut seen = BTreeSet::new();
    for case in &cases {
        if case.id.trim().is_empty() || case.task.trim().is_empty() {
            anyhow::bail!("contract benchmark cases require non-empty id and task");
        }
        if !seen.insert(case.id.clone()) {
            anyhow::bail!("duplicate contract benchmark case id `{}`", case.id);
        }
        if case.changed_files.is_empty() && case.unified_diff.is_none() && case.edits.is_empty() {
            anyhow::bail!(
                "contract benchmark case `{}` requires changed_files, unified_diff, or edits",
                case.id
            );
        }
    }
    Ok(cases)
}

fn validate_contract_bench_coverage(cases: &[ContractBenchCase]) -> anyhow::Result<()> {
    let covered = cases
        .iter()
        .map(|case| case.rule_family)
        .collect::<BTreeSet<_>>();
    let missing = REQUIRED_CONTRACT_BENCH_RULE_FAMILIES
        .iter()
        .copied()
        .filter(|family| !covered.contains(family))
        .map(|family| format!("{family:?}"))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        anyhow::bail!(
            "contract benchmark cases missing required rule family coverage: {}",
            missing.join(", ")
        );
    }
    Ok(())
}

struct ContractBenchTempRepo {
    path: PathBuf,
}

impl Drop for ContractBenchTempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn prepare_contract_bench_repo(
    repo: &Path,
    case_id: &str,
) -> anyhow::Result<ContractBenchTempRepo> {
    let stamp = chrono::Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_else(|| chrono::Utc::now().timestamp_micros());
    let path = std::env::temp_dir().join(format!(
        "open-kioku-contract-bench-{}-{}-{}",
        std::process::id(),
        stamp,
        sanitize_temp_path_fragment(case_id)
    ));
    copy_contract_bench_repo(repo, &path)?;
    Ok(ContractBenchTempRepo { path })
}

fn sanitize_temp_path_fragment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn copy_contract_bench_repo(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    fs::create_dir_all(destination)?;
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty() || should_skip_contract_bench_copy(relative) {
            continue;
        }
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn should_skip_contract_bench_copy(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        matches!(name.as_ref(), ".git" | ".ok" | "target")
    })
}

fn apply_contract_bench_overlay(
    contract: &mut ChangeContractV1,
    overlay: &ContractBenchContractOverlay,
) -> anyhow::Result<()> {
    merge_contract_files(&mut contract.primary_files, &overlay.primary_files);
    merge_contract_files(&mut contract.secondary_files, &overlay.secondary_files);
    merge_contract_files(&mut contract.forbidden_files, &overlay.forbidden_files);
    remove_contract_files(&mut contract.primary_files, &overlay.forbidden_files);
    remove_contract_files(&mut contract.secondary_files, &overlay.forbidden_files);
    contract
        .api_surface_constraints
        .extend(overlay.api_surface_constraints.clone());
    contract
        .dependency_delta_constraints
        .extend(overlay.dependency_delta_constraints.clone());
    contract.validate().map_err(|err| {
        anyhow::anyhow!("contract benchmark overlay produced invalid contract: {err}")
    })
}

fn merge_contract_files(target: &mut Vec<ContractFile>, additions: &[ContractFile]) {
    for addition in additions {
        if !target
            .iter()
            .any(|current| same_normalized_contract_file(current, addition))
        {
            target.push(addition.clone());
        }
    }
}

fn remove_contract_files(target: &mut Vec<ContractFile>, removals: &[ContractFile]) {
    target.retain(|candidate| {
        !removals
            .iter()
            .any(|removal| same_normalized_contract_file(candidate, removal))
    });
}

fn same_normalized_contract_file(left: &ContractFile, right: &ContractFile) -> bool {
    normalize_path_fragment(left.as_str()) == normalize_path_fragment(right.as_str())
}

fn apply_contract_bench_edits(repo: &Path, case: &ContractBenchCase) -> anyhow::Result<()> {
    for edit in &case.edits {
        let path = repo.join(&edit.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &edit.content)?;
    }
    Ok(())
}

fn contract_bench_changed_files(case: &ContractBenchCase) -> Vec<PathBuf> {
    if !case.changed_files.is_empty() {
        return case.changed_files.clone();
    }
    case.edits.iter().map(|edit| edit.path.clone()).collect()
}

fn score_contract_bench_case(
    case: &ContractBenchCase,
    contract: &ChangeContractV1,
    verification: &ContractVerificationReport,
    generation_ms: f64,
    verification_ms: f64,
) -> anyhow::Result<ContractBenchCaseReport> {
    let contract_primary = contract_pathbufs(&contract.primary_files);
    let contract_allowed_boundary = contract_allowed_boundary_paths(contract);
    let contract_forbidden = contract_pathbufs(&contract.forbidden_files);
    let primary_file_hits =
        matching_expected_values(&case.expected_contract.primary_files, &contract_primary);
    let boundary_hits = matching_expected_values(
        &case.expected_contract.allowed_boundary,
        &contract_allowed_boundary,
    );
    let forbidden_boundary_hits = matching_expected_values(
        &case.expected_contract.forbidden_paths,
        &contract_allowed_boundary,
    );
    let forbidden_contract_hits =
        matching_expected_values(&case.expected_contract.forbidden_paths, &contract_forbidden);
    let mut missing_contract_fields = Vec::new();
    push_missing_expected_values(
        &mut missing_contract_fields,
        "primary_files",
        &case.expected_contract.primary_files,
        &primary_file_hits,
    );
    push_missing_expected_values(
        &mut missing_contract_fields,
        "allowed_boundary",
        &case.expected_contract.allowed_boundary,
        &boundary_hits,
    );
    push_missing_expected_values(
        &mut missing_contract_fields,
        "forbidden_files",
        &case.expected_contract.forbidden_paths,
        &forbidden_contract_hits,
    );
    if contract.required_tests.len() < case.expected_contract.min_required_tests {
        missing_contract_fields.push(format!(
            "required_tests: expected at least {}, got {}",
            case.expected_contract.min_required_tests,
            contract.required_tests.len()
        ));
    }
    if contract.traceability.len() < case.expected_contract.min_traceability {
        missing_contract_fields.push(format!(
            "traceability: expected at least {}, got {}",
            case.expected_contract.min_traceability,
            contract.traceability.len()
        ));
    }
    if contract.architecture_constraints.len() < case.expected_contract.min_architecture_constraints
    {
        missing_contract_fields.push(format!(
            "architecture_constraints: expected at least {}, got {}",
            case.expected_contract.min_architecture_constraints,
            contract.architecture_constraints.len()
        ));
    }
    if contract.evidence_refs.len() < case.expected_contract.min_evidence_refs {
        missing_contract_fields.push(format!(
            "evidence_refs: expected at least {}, got {}",
            case.expected_contract.min_evidence_refs,
            contract.evidence_refs.len()
        ));
    }

    let actual_finding_keys = contract_bench_finding_keys(verification);
    let finding_hits = matching_expected_strings(&case.expected_findings, &actual_finding_keys);
    let mut missing_findings = Vec::new();
    push_missing_expected_values(
        &mut missing_findings,
        "findings",
        &case.expected_findings,
        &finding_hits,
    );
    let explanation = render_contract_explain_markdown(&explain_contract(contract));
    let explanation_lower = explanation.to_ascii_lowercase();
    let explanation_hits = case
        .explanation_terms
        .iter()
        .filter(|term| explanation_lower.contains(&term.to_ascii_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    let mut missing_explanation_terms = Vec::new();
    push_missing_expected_values(
        &mut missing_explanation_terms,
        "explanation_terms",
        &case.explanation_terms,
        &explanation_hits,
    );
    let pretty_json = serde_json::to_string_pretty(contract)?;
    let toon = render_contract_toon(contract);
    let pretty_json_bytes = pretty_json.len();
    let toon_bytes = toon.len();
    let toon_reduction = if pretty_json_bytes == 0 {
        0.0
    } else {
        1.0 - (toon_bytes as f64 / pretty_json_bytes as f64)
    };
    let actual_verdict = verification.change_report.verdict;
    let verdict_correct = actual_verdict == case.expected_verdict;
    let boundary_precision = boundary_precision(
        &contract_allowed_boundary,
        &case.expected_contract.forbidden_paths,
    );
    let boundary_recall = ratio(
        boundary_hits.len(),
        case.expected_contract.allowed_boundary.len(),
    );
    let passed = verdict_correct
        && missing_contract_fields.is_empty()
        && missing_findings.is_empty()
        && missing_explanation_terms.is_empty()
        && forbidden_boundary_hits.is_empty();
    Ok(ContractBenchCaseReport {
        id: case.id.clone(),
        rule_family: case.rule_family,
        task: case.task.clone(),
        contract_id: contract.id.0.clone(),
        expected_verdict: case.expected_verdict,
        actual_verdict,
        verdict_correct,
        boundary_precision,
        boundary_recall,
        primary_file_hits,
        boundary_hits,
        forbidden_boundary_hits,
        missing_contract_fields,
        finding_hits,
        missing_findings,
        explanation_hits,
        missing_explanation_terms,
        pretty_json_bytes,
        toon_bytes,
        toon_reduction,
        generation_ms,
        verification_ms,
        passed,
    })
}

fn push_missing_expected_values(
    target: &mut Vec<String>,
    field: &str,
    expected: &[String],
    hits: &[String],
) {
    for value in expected {
        if !hits.iter().any(|hit| hit == value) {
            target.push(format!("{field}: missing `{value}`"));
        }
    }
}

fn contract_pathbufs(files: &[ContractFile]) -> Vec<PathBuf> {
    files
        .iter()
        .map(|file| PathBuf::from(file.as_str()))
        .collect()
}

fn contract_allowed_boundary_paths(contract: &ChangeContractV1) -> Vec<PathBuf> {
    contract
        .primary_files
        .iter()
        .chain(contract.secondary_files.iter())
        .map(|file| PathBuf::from(file.as_str()))
        .collect()
}

fn contract_bench_finding_keys(report: &ContractVerificationReport) -> Vec<String> {
    let mut keys = Vec::new();
    keys.extend(
        report
            .change_report
            .boundary_violations
            .iter()
            .map(|finding| finding.kind.clone()),
    );
    keys.extend(
        report
            .change_report
            .warnings
            .iter()
            .map(|finding| finding.kind.clone()),
    );
    keys.extend(
        report
            .change_report
            .missing_tests
            .iter()
            .map(|finding| finding.kind.clone()),
    );
    keys.extend(
        report
            .change_report
            .changed_impact
            .iter()
            .map(|finding| finding.kind.clone()),
    );
    keys.extend(
        report
            .change_report
            .api_surface_deltas
            .iter()
            .map(|finding| finding.kind.clone()),
    );
    keys.extend(
        report
            .change_report
            .dependency_deltas
            .iter()
            .map(|finding| {
                format!("dependency_delta:{:?}", finding.classification).to_ascii_lowercase()
            }),
    );
    keys.sort();
    keys.dedup();
    keys
}

fn summarize_contract_bench_cases(reports: &[ContractBenchCaseReport]) -> ContractBenchSummary {
    let count = reports.len() as f64;
    let verdict_correct = reports.iter().filter(|case| case.verdict_correct).count() as f64;
    let mut true_positives = 0;
    let mut false_positives = 0;
    let mut false_negatives = 0;
    for case in reports {
        let expected_positive = case.expected_verdict != VerificationVerdict::Pass;
        let actual_positive = case.actual_verdict != VerificationVerdict::Pass;
        match (expected_positive, actual_positive) {
            (true, true) => true_positives += 1,
            (false, true) => false_positives += 1,
            (true, false) => false_negatives += 1,
            (false, false) => {}
        }
    }
    let min_toon_reduction = reports
        .iter()
        .map(|case| case.toon_reduction)
        .fold(1.0, f64::min);
    ContractBenchSummary {
        verdict_accuracy: mean(verdict_correct, count),
        verification_precision: mean(
            true_positives as f64,
            (true_positives + false_positives) as f64,
        ),
        boundary_precision: mean(
            reports
                .iter()
                .map(|case| case.boundary_precision)
                .sum::<f64>(),
            count,
        ),
        boundary_recall: mean(
            reports.iter().map(|case| case.boundary_recall).sum::<f64>(),
            count,
        ),
        min_toon_reduction,
        mean_toon_reduction: mean(
            reports.iter().map(|case| case.toon_reduction).sum::<f64>(),
            count,
        ),
        mean_generation_ms: mean(
            reports.iter().map(|case| case.generation_ms).sum::<f64>(),
            count,
        ),
        mean_verification_ms: mean(
            reports.iter().map(|case| case.verification_ms).sum::<f64>(),
            count,
        ),
        true_positives,
        false_positives,
        false_negatives,
    }
}

fn summarize_contract_bench_families(
    reports: &[ContractBenchCaseReport],
) -> Vec<ContractBenchFamilyReport> {
    let mut grouped = BTreeMap::<ContractBenchRuleFamily, Vec<&ContractBenchCaseReport>>::new();
    for report in reports {
        grouped.entry(report.rule_family).or_default().push(report);
    }
    grouped
        .into_iter()
        .map(|(rule_family, cases)| {
            let count = cases.len() as f64;
            ContractBenchFamilyReport {
                rule_family,
                case_count: cases.len(),
                verdict_accuracy: mean(
                    cases.iter().filter(|case| case.verdict_correct).count() as f64,
                    count,
                ),
                boundary_precision: mean(
                    cases
                        .iter()
                        .map(|case| case.boundary_precision)
                        .sum::<f64>(),
                    count,
                ),
                boundary_recall: mean(
                    cases.iter().map(|case| case.boundary_recall).sum::<f64>(),
                    count,
                ),
            }
        })
        .collect()
}

fn print_contract_bench_report(report: &ContractBenchReport) {
    println!(
        "Contract benchmark: {} case(s), limit {}",
        report.case_count, report.limit
    );
    println!(
        "Summary: verdict accuracy {:.3}, verification precision {:.3}, boundary precision {:.3}, boundary recall {:.3}, min TOON reduction {:.3}, mean TOON reduction {:.3}",
        report.summary.verdict_accuracy,
        report.summary.verification_precision,
        report.summary.boundary_precision,
        report.summary.boundary_recall,
        report.summary.min_toon_reduction,
        report.summary.mean_toon_reduction
    );
    for family in &report.rule_families {
        println!(
            "  {:?}: {} case(s), verdict {:.3}, boundary {:.3}/{:.3}",
            family.rule_family,
            family.case_count,
            family.verdict_accuracy,
            family.boundary_precision,
            family.boundary_recall
        );
    }
    for case in &report.cases {
        println!(
            "  {}: {:?}, verdict {:?}/{:?}, boundary {:.3}/{:.3}, TOON {:+.1}%, {}",
            case.id,
            case.rule_family,
            case.actual_verdict,
            case.expected_verdict,
            case.boundary_precision,
            case.boundary_recall,
            case.toon_reduction * 100.0,
            if case.passed { "pass" } else { "fail" }
        );
    }
}

fn run_eval(args: EvalArgs) -> anyhow::Result<EvalReport> {
    let repo = absolutize(&args.path)?;
    let limit = args.limit.clamp(1, 100);
    let cases = load_eval_cases(&args.cases, args.cases_file.as_ref())?;
    if cases.is_empty() {
        anyhow::bail!("no eval cases provided; pass --case TASK=EXPECTED_PATH or --cases-file");
    }

    if !args.no_index {
        index_repo(&repo)?;
    }
    let store = open_store(&repo)?;
    let ranking_options = ranking_options_for_repo(&repo)?;
    let mut semantic_config = OkConfig::load_from_repo(&repo)?.semantic;
    semantic_config.enabled = true;
    let semantic_manager = SemanticIndexManager::new(&repo, &store, &semantic_config);
    let semantic_ready = semantic_manager.status().ready;
    let mut case_reports = Vec::with_capacity(cases.len());
    let mut recall_sum = 0.0;
    let mut mrr_sum = 0.0;
    let mut ndcg_sum = 0.0;
    let mut semantic_recall_sum = 0.0;
    let mut semantic_mrr_sum = 0.0;
    let mut semantic_ndcg_sum = 0.0;
    let mut baseline_recall_sum = 0.0;
    let mut baseline_mrr_sum = 0.0;
    let mut baseline_ndcg_sum = 0.0;
    let signals = ranking_ablation_signals();
    let mut ablation_sums = signals
        .iter()
        .map(|signal| (*signal, 0.0, 0.0, 0.0))
        .collect::<Vec<_>>();
    let mut context_recall_sum = 0.0;
    let mut test_recall_sum = 0.0;
    let mut abstention_required = 0usize;

    for case in cases {
        let mut raw_candidates =
            search_raw(&repo, &store, &case.task, ranking_candidate_limit(limit))?;
        let baseline_results = top_unique_paths(rerank_baseline(raw_candidates.clone()), limit);
        annotate_candidates_with_git_history(&store, &mut raw_candidates)?;
        let semantic_results = if semantic_ready {
            semantic_manager.search(&case.task, ranking_candidate_limit(limit))?
        } else {
            Vec::new()
        };
        if semantic_ready {
            raw_candidates.extend(semantic_results.clone());
        }
        let mut case_ranking_options = ranking_options.clone();
        case_ranking_options.query = Some(case.task.clone());
        let search_results = top_unique_paths_merging(
            rerank_with_options(raw_candidates.clone(), &case_ranking_options),
            limit,
        );
        let context = build_context_pack(&repo, &store, &case.task, limit)?;
        let search_paths = search_results
            .iter()
            .map(|result| result.path.clone())
            .collect::<Vec<_>>();
        let baseline_paths = baseline_results
            .iter()
            .map(|result| result.path.clone())
            .collect::<Vec<_>>();
        let context_paths = context
            .primary_files
            .iter()
            .chain(context.supporting_files.iter())
            .map(|result| result.path.clone())
            .collect::<Vec<_>>();
        let selected_tests = context
            .test_candidates
            .iter()
            .map(|test| test.name.clone())
            .collect::<Vec<_>>();

        let baseline_ranks = expected_path_ranks(&case.expected_paths, &baseline_paths);
        baseline_recall_sum += ratio(
            baseline_ranks.iter().filter(|rank| rank.is_some()).count(),
            case.expected_paths.len(),
        );
        baseline_mrr_sum += reciprocal_rank(&baseline_ranks);
        baseline_ndcg_sum += ndcg(&baseline_ranks, limit);

        if semantic_ready {
            let semantic_paths = semantic_results
                .iter()
                .map(|result| result.path.clone())
                .collect::<Vec<_>>();
            let semantic_ranks = expected_path_ranks(&case.expected_paths, &semantic_paths);
            semantic_recall_sum += ratio(
                semantic_ranks.iter().filter(|rank| rank.is_some()).count(),
                case.expected_paths.len(),
            );
            semantic_mrr_sum += reciprocal_rank(&semantic_ranks);
            semantic_ndcg_sum += ndcg(&semantic_ranks, limit);
        }

        let search_ranks = expected_path_ranks(&case.expected_paths, &search_paths);
        let search_hits = search_ranks.iter().filter(|rank| rank.is_some()).count();
        let search_recall = ratio(search_hits, case.expected_paths.len());
        recall_sum += search_recall;
        mrr_sum += reciprocal_rank(&search_ranks);
        ndcg_sum += ndcg(&search_ranks, limit);
        for (signal, recall, mrr, ndcg_value) in &mut ablation_sums {
            let mut ablation_options = case_ranking_options.clone();
            ablation_options.mode = RankingMode::WithoutSignal(*signal);
            let candidates = if *signal == RankingSignal::GitCochange {
                without_git_history_candidates(raw_candidates.clone())
            } else {
                raw_candidates.clone()
            };
            let ablated =
                top_unique_paths(rerank_with_options(candidates, &ablation_options), limit);
            let ablated_paths = ablated
                .iter()
                .map(|result| result.path.clone())
                .collect::<Vec<_>>();
            let ablated_ranks = expected_path_ranks(&case.expected_paths, &ablated_paths);
            *recall += ratio(
                ablated_ranks.iter().filter(|rank| rank.is_some()).count(),
                case.expected_paths.len(),
            );
            *mrr += reciprocal_rank(&ablated_ranks);
            *ndcg_value += ndcg(&ablated_ranks, limit);
        }

        let context_hits = matching_expected_values(&case.expected_paths, &context_paths);
        let context_recall = ratio(context_hits.len(), case.expected_paths.len());
        context_recall_sum += context_recall;

        let test_hits = matching_expected_strings(&case.expected_tests, &selected_tests);
        let test_recall = ratio(test_hits.len(), case.expected_tests.len());
        test_recall_sum += test_recall;

        let mut notes = Vec::new();
        if search_recall == 0.0 {
            notes.push("expected files were not found in top search results".into());
        }
        if context_recall == 0.0 {
            notes.push("expected files were not grounded in context pack".into());
        }
        if !case.expected_tests.is_empty() && test_recall == 0.0 {
            notes.push("expected tests were not selected".into());
        }
        let confidence = if search_recall > 0.0 && context_recall > 0.0 {
            "grounded"
        } else if search_results.is_empty() || context.primary_files.is_empty() {
            abstention_required += 1;
            "abstain"
        } else {
            abstention_required += 1;
            "weak"
        };

        case_reports.push(EvalCaseReport {
            task: case.task,
            expected_paths: case.expected_paths,
            expected_tests: case.expected_tests,
            search_ranks,
            context_hits,
            test_hits,
            top_search_paths: search_paths.into_iter().take(limit).collect(),
            top_context_paths: context_paths.into_iter().take(limit).collect(),
            top_search_signals: search_results
                .first()
                .map(|result| top_score_signals(result, 3))
                .unwrap_or_default(),
            confidence,
            notes,
        });
    }

    let count = case_reports.len() as f64;
    let fusion = RankingEvalSummary {
        mode: "fusion".into(),
        search_recall_at_k: recall_sum / count,
        search_mrr: mrr_sum / count,
        search_ndcg_at_k: ndcg_sum / count,
    };
    let baseline = RankingEvalSummary {
        mode: "baseline".into(),
        search_recall_at_k: baseline_recall_sum / count,
        search_mrr: baseline_mrr_sum / count,
        search_ndcg_at_k: baseline_ndcg_sum / count,
    };
    let semantic = semantic_ready.then(|| RankingEvalSummary {
        mode: "semantic".into(),
        search_recall_at_k: semantic_recall_sum / count,
        search_mrr: semantic_mrr_sum / count,
        search_ndcg_at_k: semantic_ndcg_sum / count,
    });
    let ablations = ablation_sums
        .into_iter()
        .map(|(signal, recall, mrr, ndcg_value)| {
            let recall_at_k = recall / count;
            let search_mrr = mrr / count;
            let ndcg_at_k = ndcg_value / count;
            RankingAblationReport {
                signal: ranking_signal_name(signal).into(),
                search_recall_at_k: recall_at_k,
                search_mrr,
                search_ndcg_at_k: ndcg_at_k,
                recall_delta_vs_fusion: fusion.search_recall_at_k - recall_at_k,
                mrr_delta_vs_fusion: fusion.search_mrr - search_mrr,
                ndcg_delta_vs_fusion: fusion.search_ndcg_at_k - ndcg_at_k,
            }
        })
        .collect::<Vec<_>>();
    Ok(EvalReport {
        repo,
        limit,
        case_count: case_reports.len(),
        summary: EvalSummary {
            search_recall_at_k: fusion.search_recall_at_k,
            search_mrr: fusion.search_mrr,
            search_ndcg_at_k: fusion.search_ndcg_at_k,
            context_recall_at_k: context_recall_sum / count,
            test_recall_at_k: test_recall_sum / count,
            abstention_required,
        },
        baseline,
        semantic,
        fusion,
        ablations,
        cases: case_reports,
    })
}

fn load_eval_cases(
    values: &[String],
    cases_file: Option<&PathBuf>,
) -> anyhow::Result<Vec<EvalCase>> {
    let mut cases = values
        .iter()
        .map(|value| {
            let (task, expected) = value.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("eval case must use TASK=EXPECTED_PATH[,EXPECTED_PATH]: {value}")
            })?;
            let expected_paths = expected
                .split(',')
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if task.trim().is_empty() || expected_paths.is_empty() {
                anyhow::bail!("eval task and expected paths must be non-empty: {value}");
            }
            Ok(EvalCase {
                task: task.trim().to_string(),
                expected_paths,
                expected_tests: Vec::new(),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if let Some(path) = cases_file {
        let raw = fs::read_to_string(path)?;
        let mut from_file: Vec<EvalCase> = serde_json::from_str(&raw)?;
        cases.append(&mut from_file);
    }
    Ok(cases)
}

fn expected_path_ranks(expected_paths: &[String], actual_paths: &[PathBuf]) -> Vec<Option<usize>> {
    expected_paths
        .iter()
        .map(|expected| {
            let expected = normalize_path_fragment(expected);
            actual_paths
                .iter()
                .position(|path| {
                    normalize_path_fragment(&path.to_string_lossy()).contains(&expected)
                })
                .map(|rank| rank + 1)
        })
        .collect()
}

fn matching_expected_values(expected: &[String], actual: &[PathBuf]) -> Vec<String> {
    expected
        .iter()
        .filter(|expected| {
            let expected = normalize_path_fragment(expected);
            actual
                .iter()
                .any(|path| normalize_path_fragment(&path.to_string_lossy()).contains(&expected))
        })
        .cloned()
        .collect()
}

fn matching_expected_strings(expected: &[String], actual: &[String]) -> Vec<String> {
    expected
        .iter()
        .filter(|expected| {
            let expected = expected.to_ascii_lowercase();
            actual
                .iter()
                .any(|value| value.to_ascii_lowercase().contains(&expected))
        })
        .cloned()
        .collect()
}

fn reciprocal_rank(ranks: &[Option<usize>]) -> f64 {
    ranks
        .iter()
        .flatten()
        .min()
        .map(|rank| 1.0 / *rank as f64)
        .unwrap_or(0.0)
}

fn ndcg(ranks: &[Option<usize>], limit: usize) -> f64 {
    if ranks.is_empty() {
        return 1.0;
    }
    let dcg = ranks
        .iter()
        .flatten()
        .filter(|rank| **rank <= limit)
        .map(|rank| 1.0 / ((*rank as f64) + 1.0).log2())
        .sum::<f64>();
    let ideal = (1..=ranks.len().min(limit))
        .map(|rank| 1.0 / ((rank as f64) + 1.0).log2())
        .sum::<f64>();
    if ideal == 0.0 {
        0.0
    } else {
        dcg / ideal
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn print_eval_report(report: &EvalReport) {
    println!("Open Kioku eval for {}", report.repo.display());
    println!(
        "Search recall@{} {:.3}, MRR {:.3}, nDCG@{} {:.3}",
        report.limit,
        report.summary.search_recall_at_k,
        report.summary.search_mrr,
        report.limit,
        report.summary.search_ndcg_at_k
    );
    println!(
        "Ranking baseline: recall@{} {:.3}, MRR {:.3}, nDCG@{} {:.3}",
        report.limit,
        report.baseline.search_recall_at_k,
        report.baseline.search_mrr,
        report.limit,
        report.baseline.search_ndcg_at_k
    );
    println!(
        "Ranking fusion: recall@{} {:.3}, MRR {:.3}, nDCG@{} {:.3}",
        report.limit,
        report.fusion.search_recall_at_k,
        report.fusion.search_mrr,
        report.limit,
        report.fusion.search_ndcg_at_k
    );
    if let Some(semantic) = &report.semantic {
        println!(
            "Ranking semantic-only: recall@{} {:.3}, MRR {:.3}, nDCG@{} {:.3}",
            report.limit,
            semantic.search_recall_at_k,
            semantic.search_mrr,
            report.limit,
            semantic.search_ndcg_at_k
        );
    }
    if !report.ablations.is_empty() {
        println!("Ranking ablations:");
        for ablation in &report.ablations {
            println!(
                "  - without {}: recall@{} {:.3} (delta {:+.3}), MRR {:.3} (delta {:+.3}), nDCG {:.3} (delta {:+.3})",
                ablation.signal,
                report.limit,
                ablation.search_recall_at_k,
                ablation.recall_delta_vs_fusion,
                ablation.search_mrr,
                ablation.mrr_delta_vs_fusion,
                ablation.search_ndcg_at_k,
                ablation.ndcg_delta_vs_fusion
            );
        }
    }
    println!(
        "Context recall@{} {:.3}, test recall@{} {:.3}, weak/abstain {}",
        report.limit,
        report.summary.context_recall_at_k,
        report.limit,
        report.summary.test_recall_at_k,
        report.summary.abstention_required
    );
    for case in &report.cases {
        println!("\n- {} [{}]", case.task, case.confidence);
        println!("  expected paths: {}", case.expected_paths.join(", "));
        println!("  ranks: {:?}", case.search_ranks);
        if !case.top_search_signals.is_empty() {
            println!(
                "  top ranking signals: {}",
                case.top_search_signals.join(", ")
            );
        }
        if !case.test_hits.is_empty() {
            println!("  test hits: {}", case.test_hits.join(", "));
        }
        for note in &case.notes {
            println!("  note: {note}");
        }
    }
}
