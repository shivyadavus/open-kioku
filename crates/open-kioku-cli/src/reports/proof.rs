const DEFAULT_PROOF_TASKS: &[&str] = &[
    "authentication",
    "configuration",
    "tests",
    "security",
    "database",
    "api",
    "mcp",
    "context pack",
    "impact analysis",
    "search code",
    "symbol lookup",
    "release workflow",
    "npm package",
    "policy",
    "validation",
];

fn run_proof(args: ProveArgs) -> anyhow::Result<ProofReport> {
    let repo = absolutize(&args.path)?;
    let limit = args.limit.clamp(1, 100);
    let retrieval_quality = proof_retrieval_quality(args.retrieval_report.as_deref());
    let snapshot = index_repo(&repo)?;
    let store = open_store(&repo)?;
    let files = store.list_files(usize::MAX, 0)?;
    let languages = language_counts(&files);
    let tasks = if args.tasks.is_empty() {
        choose_proof_tasks(&repo, &store, 3)?
    } else {
        args.tasks
            .iter()
            .map(|task| task.trim())
            .filter(|task| !task.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    if tasks.is_empty() {
        anyhow::bail!("no proof tasks were provided or discovered");
    }

    let index_dir = default_index_dir(&repo);
    let search_index = if TantivySearchIndex::exists(&index_dir) {
        Some(TantivySearchIndex::open_or_create(&index_dir)?)
    } else {
        None
    };
    let planner = PlanEngine::new(&store as &dyn OkStore)
        .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
        .with_history_store(Some(&store));
    let mut task_reports = Vec::with_capacity(tasks.len());
    for task in &tasks {
        let plan = planner.plan(task, limit)?;
        let top_results = search(&repo, &store, task, 5)?;
        task_reports.push(score_proof_task(
            &repo,
            task,
            &plan,
            &top_results,
            args.reveal_paths,
        ));
    }

    let scores = task_reports
        .iter()
        .map(|task| task.score)
        .collect::<Vec<_>>();
    let total = scores.iter().sum::<u32>();
    let tasks_scored = task_reports.len();
    let average_score = if tasks_scored > 0 {
        total as f64 / tasks_scored as f64
    } else {
        0.0
    };
    let pass_rate_70 = if tasks_scored > 0 {
        100.0 * scores.iter().filter(|score| **score >= 70).count() as f64 / tasks_scored as f64
    } else {
        0.0
    };

    Ok(ProofReport {
        repo: if args.reveal_paths {
            repo.display().to_string()
        } else {
            "local repository".into()
        },
        generated_by: "ok prove",
        privacy: ProofPrivacy {
            source_snippets_included: false,
            local_root_included: args.reveal_paths,
            path_mode: if args.reveal_paths {
                "repository_relative"
            } else {
                "redacted_shapes"
            },
        },
        summary: ProofSummary {
            indexed_files: snapshot.manifest.file_count,
            indexed_symbols: snapshot.manifest.symbol_count,
            indexed_chunks: snapshot.manifest.chunk_count,
            tasks_scored,
            average_score: round1(average_score),
            min_score: scores.iter().min().copied().unwrap_or(0),
            max_score: scores.iter().max().copied().unwrap_or(0),
            pass_rate_70: round1(pass_rate_70),
        },
        retrieval_quality,
        languages,
        tasks: task_reports,
        reproduce: reproduce_commands(&repo, &tasks, limit, args.reveal_paths),
        notes: vec![
            "The report includes metrics and path shapes only; it does not include source snippets.",
            "Scores measure whether Open Kioku returned grounded planning context, impact, validation, risk, and agent tool calls.",
            "Use --task to evaluate product-specific workflows and --reveal-paths when repository-relative paths are safe to share.",
            "Retrieval benchmark metrics, when supplied, describe the frozen benchmark artifact only and are never presented as measurements of the private repository.",
        ],
    })
}

fn proof_retrieval_quality(report_path: Option<&Path>) -> ProofRetrievalQuality {
    let Some(report_path) = report_path else {
        return unavailable_proof_retrieval_quality(
            "no frozen retrieval benchmark report was supplied; repository proof scores are not retrieval benchmark metrics",
        );
    };
    let raw = match fs::read_to_string(report_path) {
        Ok(raw) => raw,
        Err(err) => {
            return unavailable_proof_retrieval_quality(format!(
                "retrieval benchmark report could not be read: {err}"
            ));
        }
    };
    let value = match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(value) => value,
        Err(err) => {
            return unavailable_proof_retrieval_quality(format!(
                "retrieval benchmark report is not valid JSON: {err}"
            ));
        }
    };
    parse_proof_retrieval_quality(&value)
        .unwrap_or_else(unavailable_proof_retrieval_quality)
}

fn parse_proof_retrieval_quality(
    value: &serde_json::Value,
) -> Result<ProofRetrievalQuality, String> {
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "retrieval benchmark report is missing schema_version".to_string())?;
    if schema_version != RETRIEVAL_BENCH_SCHEMA_VERSION {
        return Err(format!(
            "retrieval benchmark schema `{schema_version}` is incompatible with supported schema `{RETRIEVAL_BENCH_SCHEMA_VERSION}`"
        ));
    }
    let report_version = required_json_string(value, &["report_version"])?;
    const LEGACY_RETRIEVAL_REPORT_VERSION: &str = "1.2.0";
    if report_version != LEGACY_RETRIEVAL_REPORT_VERSION
        && report_version != RETRIEVAL_REPORT_VERSION
    {
        return Err(format!(
            "retrieval report version `{report_version}` is incompatible with supported report versions `{LEGACY_RETRIEVAL_REPORT_VERSION}` and `{RETRIEVAL_REPORT_VERSION}`"
        ));
    }
    let corpus_id = required_json_string(value, &["corpus_id"])?;
    let corpus_revision = required_json_string(value, &["provenance", "corpus_revision"])?;
    let cases_sha256 = required_json_string(value, &["provenance", "cases_sha256"])?;
    let fixtures_verified = json_path(value, &["provenance", "frozen_fixture_revisions_verified"])
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| {
            "retrieval benchmark report is missing frozen fixture verification status".to_string()
        })?;
    if !fixtures_verified {
        return Err(
            "retrieval benchmark report did not verify frozen fixture revisions; metrics are not proof-grade"
                .into(),
        );
    }

    let strategies = value
        .get("strategies")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "retrieval benchmark report is missing strategies".to_string())?;
    let fusion = strategies
        .iter()
        .find(|strategy| strategy.get("strategy").and_then(serde_json::Value::as_str) == Some("fusion"))
        .ok_or_else(|| "retrieval benchmark report is missing the fusion strategy".to_string())?;
    let quality = json_path(fusion, &["by_split", "holdout", "quality"])
        .ok_or_else(|| "retrieval benchmark report is missing fusion holdout quality".to_string())?;

    let algorithm = required_json_string(
        value,
        &["strategy_identities", "fusion", "algorithm"],
    )?;

    Ok(ProofRetrievalQuality {
        available: true,
        scope: "frozen_retrieval_benchmark_artifact",
        applies_to_repository: false,
        corpus_id: Some(corpus_id),
        corpus_revision: Some(corpus_revision),
        cases_sha256: Some(cases_sha256),
        report_version: Some(report_version),
        strategy: Some("fusion".into()),
        strategy_algorithm: Some(algorithm),
        split: Some("holdout".into()),
        recall_at_10: Some(required_json_f64(quality, &["recall_at_10"])?),
        mean_reciprocal_rank: Some(required_json_f64(
            quality,
            &["mean_reciprocal_rank"],
        )?),
        file_f1_at_10: Some(required_json_f64(quality, &["file_f1_at_10"])?),
        no_gold_false_positive_rate: Some(required_json_f64(
            quality,
            &["no_gold_false_positive_rate"],
        )?),
        token_budget_gold_yield_2000: optional_json_f64(
            quality,
            &["token_budget_gold_yield", "2000"],
        )?,
        caveats: vec![
            "metrics come from a frozen retrieval benchmark artifact and are not measurements of the repository evaluated by ok prove"
                .into(),
        ],
    })
}

fn unavailable_proof_retrieval_quality(reason: impl Into<String>) -> ProofRetrievalQuality {
    ProofRetrievalQuality {
        available: false,
        scope: "frozen_retrieval_benchmark_artifact",
        applies_to_repository: false,
        corpus_id: None,
        corpus_revision: None,
        cases_sha256: None,
        report_version: None,
        strategy: None,
        strategy_algorithm: None,
        split: None,
        recall_at_10: None,
        mean_reciprocal_rank: None,
        file_f1_at_10: None,
        no_gold_false_positive_rate: None,
        token_budget_gold_yield_2000: None,
        caveats: vec![reason.into()],
    }
}

fn json_path<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
    path.iter().try_fold(value, |current, key| current.get(*key))
}

fn required_json_string(value: &serde_json::Value, path: &[&str]) -> Result<String, String> {
    json_path(value, path)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("retrieval benchmark report is missing `{}`", path.join(".")))
}

fn required_json_f64(value: &serde_json::Value, path: &[&str]) -> Result<f64, String> {
    let number = json_path(value, path)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| format!("retrieval benchmark report is missing `{}`", path.join(".")))?;
    if !number.is_finite() || !(0.0..=1.0).contains(&number) {
        return Err(format!(
            "retrieval benchmark metric `{}` must be a finite value in [0, 1]",
            path.join(".")
        ));
    }
    Ok(number)
}

fn optional_json_f64(
    value: &serde_json::Value,
    path: &[&str],
) -> Result<Option<f64>, String> {
    let Some(raw) = json_path(value, path) else {
        return Ok(None);
    };
    let number = raw.as_f64().ok_or_else(|| {
        format!(
            "retrieval benchmark metric `{}` must be numeric",
            path.join(".")
        )
    })?;
    if !number.is_finite() || !(0.0..=1.0).contains(&number) {
        return Err(format!(
            "retrieval benchmark metric `{}` must be a finite value in [0, 1]",
            path.join(".")
        ));
    }
    Ok(Some(number))
}

fn choose_proof_tasks(
    repo: &Path,
    store: &dyn MetadataStore,
    max_tasks: usize,
) -> anyhow::Result<Vec<String>> {
    let mut tasks = Vec::new();
    for candidate in DEFAULT_PROOF_TASKS {
        if !search(repo, store, candidate, 1)?.is_empty() {
            tasks.push((*candidate).to_string());
        }
        if tasks.len() >= max_tasks {
            return Ok(tasks);
        }
    }

    for symbol in store.list_symbols(None, max_tasks, 0)? {
        if !symbol.name.trim().is_empty() {
            tasks.push(symbol.name);
        }
        if tasks.len() >= max_tasks {
            break;
        }
    }
    tasks.sort();
    tasks.dedup();
    Ok(tasks)
}

fn score_proof_task(
    repo: &Path,
    task: &str,
    plan: &open_kioku_core::PlanReport,
    top_results: &[open_kioku_core::SearchResult],
    reveal_paths: bool,
) -> ProofTaskReport {
    let primary_paths = plan
        .primary_context
        .iter()
        .map(|result| result.path.as_path())
        .collect::<Vec<_>>();
    let existing_paths = primary_paths
        .iter()
        .filter(|path| repo.join(path).exists())
        .count();
    let source_context_count = primary_paths
        .iter()
        .filter(|path| is_source_path(path))
        .count();
    let impact_count = plan.impact.direct_impacts.len() + plan.impact.indirect_impacts.len();

    let mut checks = BTreeMap::new();
    checks.insert("primary_context", !plan.primary_context.is_empty());
    checks.insert(
        "paths_exist",
        !primary_paths.is_empty() && existing_paths == primary_paths.len(),
    );
    checks.insert("source_context", source_context_count > 0);
    checks.insert("impact_candidates", impact_count > 0);
    checks.insert("validation_candidates", !plan.validation.is_empty());
    checks.insert("agent_tool_calls", plan.tool_calls.len() >= 3);
    checks.insert("known_risk", plan.risk.level != "unknown");

    let mut score = 0;
    for (name, weight) in [
        ("primary_context", 25),
        ("paths_exist", 15),
        ("source_context", 15),
        ("impact_candidates", 15),
        ("validation_candidates", 15),
        ("agent_tool_calls", 10),
        ("known_risk", 5),
    ] {
        if checks.get(name).copied().unwrap_or(false) {
            score += weight;
        }
    }

    ProofTaskReport {
        task: task.into(),
        score,
        checks,
        primary_context_count: plan.primary_context.len(),
        source_context_count,
        impact_count,
        validation_count: plan.validation.len(),
        tool_call_count: plan.tool_calls.len(),
        risk_level: plan.risk.level.clone(),
        sample_paths: redact_paths(primary_paths, reveal_paths),
        top_search_paths: redact_paths(
            top_results
                .iter()
                .map(|result| result.path.as_path())
                .collect::<Vec<_>>(),
            reveal_paths,
        ),
    }
}

fn language_counts(files: &[open_kioku_core::File]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for file in files {
        *counts
            .entry(format!("{:?}", file.language).to_ascii_lowercase())
            .or_insert(0) += 1;
    }
    counts
}

fn is_source_path(path: &Path) -> bool {
    !is_doc_path(path) && !is_test_path(path)
}

fn is_doc_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("md" | "mdx" | "txt" | "rst")
    ) || path
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        == Some("docs")
}

fn is_test_path(path: &Path) -> bool {
    let value = normalize_path_fragment(&path.to_string_lossy());
    value.contains("/test")
        || value.contains("test/")
        || value.contains("/spec")
        || value.ends_with("_test.go")
        || value.ends_with(".test.ts")
        || value.ends_with(".spec.ts")
}

fn redact_paths(paths: Vec<&Path>, reveal_paths: bool) -> Vec<String> {
    let mut values = paths
        .into_iter()
        .map(|path| proof_path(path, reveal_paths))
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values.truncate(5);
    values
}

fn proof_path(path: &Path, reveal_paths: bool) -> String {
    if reveal_paths {
        return path.display().to_string();
    }
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("file");
    if path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .is_none()
    {
        return format!("**/*.{ext}");
    }
    let top = path
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("repo");
    format!("{top}/**/*.{ext}")
}

fn reproduce_commands(
    repo: &Path,
    tasks: &[String],
    limit: usize,
    reveal_paths: bool,
) -> Vec<String> {
    let repo_arg = if reveal_paths {
        repo.display().to_string()
    } else {
        "/path/to/repo".into()
    };
    let mut command = format!("ok prove {repo_arg} --limit {limit}");
    for task in tasks {
        command.push_str(" --task ");
        command.push_str(&shell_quote(task));
    }
    vec![
        format!("ok init {repo_arg}"),
        format!("ok index {repo_arg}"),
        command,
        format!("ok mcp install cursor --repo {repo_arg}"),
        format!("ok mcp install claude --repo {repo_arg}"),
    ]
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/' | '.'))
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn render_proof_markdown(report: &ProofReport) -> String {
    let mut out = String::new();
    out.push_str("# Open Kioku Proof\n\n");
    out.push_str("Generated by `ok prove` against a real local repository.\n\n");
    out.push_str("This report is designed to be shared: it records metrics and path shapes, not source snippets.\n\n");

    out.push_str("## Summary\n\n");
    out.push_str("| Metric | Value |\n");
    out.push_str("| --- | ---: |\n");
    out.push_str(&format!(
        "| Indexed files | {} |\n",
        report.summary.indexed_files
    ));
    out.push_str(&format!(
        "| Indexed symbols | {} |\n",
        report.summary.indexed_symbols
    ));
    out.push_str(&format!(
        "| Indexed chunks | {} |\n",
        report.summary.indexed_chunks
    ));
    out.push_str(&format!(
        "| Tasks scored | {} |\n",
        report.summary.tasks_scored
    ));
    out.push_str(&format!(
        "| Average proof score | {:.1}/100 |\n",
        report.summary.average_score
    ));
    out.push_str(&format!(
        "| Pass rate at 70+ | {:.1}% |\n",
        report.summary.pass_rate_70
    ));

    push_proof_retrieval_quality_markdown(&mut out, &report.retrieval_quality);

    out.push_str("\n## Languages\n\n");
    for (language, count) in &report.languages {
        out.push_str(&format!("- `{language}`: {count}\n"));
    }

    out.push_str("\n## Task Scores\n\n");
    out.push_str("| Task | Score | Context | Impact | Validation | Risk | Sample paths |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | --- | --- |\n");
    for task in &report.tasks {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            escape_table_cell(&task.task),
            task.score,
            task.primary_context_count,
            task.impact_count,
            task.validation_count,
            escape_table_cell(&task.risk_level),
            escape_table_cell(&task.sample_paths.join(", "))
        ));
    }

    out.push_str("\n## What Was Checked\n\n");
    out.push_str("- Primary context exists for each task.\n");
    out.push_str("- Returned paths exist in the indexed repository.\n");
    out.push_str("- At least one source file appears in context when available.\n");
    out.push_str(
        "- Impact candidates, validation candidates, risk, and agent tool calls are produced.\n",
    );

    out.push_str("\n## Reproduce\n\n");
    out.push_str("```sh\n");
    for command in &report.reproduce {
        out.push_str(command);
        out.push('\n');
    }
    out.push_str("```\n");

    out.push_str("\n## Privacy\n\n");
    out.push_str(&format!(
        "- Source snippets included: `{}`\n",
        report.privacy.source_snippets_included
    ));
    out.push_str(&format!(
        "- Local root included: `{}`\n",
        report.privacy.local_root_included
    ));
    out.push_str(&format!("- Path mode: `{}`\n", report.privacy.path_mode));
    out.push_str("\n---\n\nIf Open Kioku helps your AI coding workflow, please consider starring the repository:\nhttps://github.com/shivyadavus/open-kioku\n");
    out
}

fn push_proof_retrieval_quality_markdown(out: &mut String, quality: &ProofRetrievalQuality) {
    out.push_str("\n## Retrieval Quality\n\n");
    out.push_str(
        "Scope: frozen benchmark artifact only. These metrics do **not** measure the repository evaluated by `ok prove`.\n\n",
    );
    if !quality.available {
        out.push_str("Status: `unavailable`\n\n");
        for caveat in &quality.caveats {
            out.push_str(&format!("- {}\n", escape_table_cell(caveat)));
        }
        return;
    }

    out.push_str("| Metric | Value |\n| --- | ---: |\n");
    if let Some(corpus_id) = &quality.corpus_id {
        out.push_str(&format!("| Corpus | `{}` |\n", escape_table_cell(corpus_id)));
    }
    if let Some(revision) = &quality.corpus_revision {
        out.push_str(&format!("| Corpus revision | `{}` |\n", escape_table_cell(revision)));
    }
    if let Some(value) = quality.recall_at_10 {
        out.push_str(&format!("| Fusion holdout Recall@10 | {:.3} |\n", value));
    }
    if let Some(value) = quality.mean_reciprocal_rank {
        out.push_str(&format!("| Fusion holdout MRR | {:.3} |\n", value));
    }
    if let Some(value) = quality.file_f1_at_10 {
        out.push_str(&format!("| Fusion holdout file F1@10 | {:.3} |\n", value));
    }
    if let Some(value) = quality.no_gold_false_positive_rate {
        out.push_str(&format!("| Fusion holdout no-gold FP rate | {:.3} |\n", value));
    }
    if let Some(value) = quality.token_budget_gold_yield_2000 {
        out.push_str(&format!("| Gold yield @ 2k tokens | {:.3} |\n", value));
    }
    if !quality.caveats.is_empty() {
        out.push('\n');
        for caveat in &quality.caveats {
            out.push_str(&format!("- {}\n", escape_table_cell(caveat)));
        }
    }
}

fn escape_table_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn time_searches(
    iterations: usize,
    mut run: impl FnMut() -> open_kioku_errors::Result<()>,
) -> anyhow::Result<Vec<Duration>> {
    let mut times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        run()?;
        times.push(started.elapsed());
    }
    Ok(times)
}

fn median_duration(mut values: Vec<Duration>) -> Duration {
    values.sort();
    values[values.len() / 2]
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}


#[cfg(test)]
mod proof_retrieval_quality_tests {
    use super::*;

    fn valid_report() -> serde_json::Value {
        serde_json::json!({
            "schema_version": RETRIEVAL_BENCH_SCHEMA_VERSION,
            "report_version": "1.2.0",
            "provenance": {
                "corpus_revision": "fixture-rev-1",
                "cases_sha256": "abc123",
                "frozen_fixture_revisions_verified": true
            },
            "corpus_id": "open-kioku-retrieval-v1",
            "strategy_identities": {
                "fusion": { "algorithm": "reciprocal_rank_fusion" }
            },
            "strategies": [{
                "strategy": "fusion",
                "by_split": {
                    "holdout": {
                        "quality": {
                            "recall_at_10": 0.9,
                            "mean_reciprocal_rank": 0.8,
                            "file_f1_at_10": 0.7,
                            "no_gold_false_positive_rate": 0.2,
                            "token_budget_gold_yield": { "2000": 0.6 }
                        }
                    }
                }
            }]
        })
    }

    #[test]
    fn proof_retrieval_quality_keeps_benchmark_scope_separate_from_private_repo() {
        let quality = parse_proof_retrieval_quality(&valid_report()).unwrap();
        assert!(quality.available);
        assert!(!quality.applies_to_repository);
        assert_eq!(quality.scope, "frozen_retrieval_benchmark_artifact");
        assert_eq!(quality.recall_at_10, Some(0.9));
        assert_eq!(quality.token_budget_gold_yield_2000, Some(0.6));
        assert!(quality
            .caveats
            .iter()
            .any(|caveat| caveat.contains("not measurements of the repository")));
    }

    #[test]
    fn proof_retrieval_quality_fails_closed_when_fixture_revisions_are_unverified() {
        let mut report = valid_report();
        report["provenance"]["frozen_fixture_revisions_verified"] = serde_json::json!(false);
        let error = parse_proof_retrieval_quality(&report).unwrap_err();
        assert!(error.contains("did not verify frozen fixture revisions"));

        let unavailable = unavailable_proof_retrieval_quality(error);
        assert!(!unavailable.available);
        assert_eq!(unavailable.recall_at_10, None);
        assert_eq!(unavailable.mean_reciprocal_rank, None);
    }

    #[test]
    fn proof_retrieval_quality_rejects_out_of_range_metrics() {
        let mut report = valid_report();
        report["strategies"][0]["by_split"]["holdout"]["quality"]["recall_at_10"] =
            serde_json::json!(1.1);
        let error = parse_proof_retrieval_quality(&report).unwrap_err();
        assert!(error.contains("must be a finite value in [0, 1]"));
    }

    #[test]
    fn proof_retrieval_quality_accepts_current_report_version() {
        let mut report = valid_report();
        report["report_version"] = serde_json::json!(RETRIEVAL_REPORT_VERSION);
        let quality = parse_proof_retrieval_quality(&report).unwrap();
        assert_eq!(
            quality.report_version.as_deref(),
            Some(RETRIEVAL_REPORT_VERSION)
        );
    }

    #[test]
    fn proof_retrieval_quality_rejects_incompatible_report_versions() {
        let mut report = valid_report();
        report["report_version"] = serde_json::json!("999.0.0");
        let error = parse_proof_retrieval_quality(&report).unwrap_err();
        assert!(error.contains("incompatible with supported report version"));
    }

    #[test]
    fn proof_retrieval_quality_rejects_out_of_range_optional_token_yield() {
        let mut report = valid_report();
        report["strategies"][0]["by_split"]["holdout"]["quality"]
            ["token_budget_gold_yield"]["2000"] = serde_json::json!(-0.1);
        let error = parse_proof_retrieval_quality(&report).unwrap_err();
        assert!(error.contains("token_budget_gold_yield.2000"));
        assert!(error.contains("finite value in [0, 1]"));
    }

    #[test]
    fn proof_markdown_states_benchmark_metrics_do_not_measure_target_repo() {
        let quality = parse_proof_retrieval_quality(&valid_report()).unwrap();
        let mut markdown = String::new();
        push_proof_retrieval_quality_markdown(&mut markdown, &quality);
        assert!(markdown.contains("do **not** measure the repository evaluated by `ok prove`"));
        assert!(markdown.contains("Fusion holdout Recall@10"));
    }
}
