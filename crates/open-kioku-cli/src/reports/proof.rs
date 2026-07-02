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
        languages,
        tasks: task_reports,
        reproduce: reproduce_commands(&repo, &tasks, limit, args.reveal_paths),
        notes: vec![
            "The report includes metrics and path shapes only; it does not include source snippets.",
            "Scores measure whether Open Kioku returned grounded planning context, impact, validation, risk, and agent tool calls.",
            "Use --task to evaluate product-specific workflows and --reveal-paths when repository-relative paths are safe to share.",
        ],
    })
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
