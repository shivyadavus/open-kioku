#[derive(Debug, Clone, Serialize)]
struct ArchitectureTrustReport {
    kind: String,
    component_count: usize,
    dependency_edge_count: usize,
    cycle_count: usize,
    route_service_boundary_count: usize,
    policy_violation_count: usize,
    components: Vec<ArchitectureComponentSummary>,
    dependencies: Vec<ArchitectureDependencySummary>,
    cycles: Vec<String>,
    route_service_boundaries: Vec<String>,
    high_risk_files: Vec<ArchitectureHotspot>,
    high_change_files: Vec<ArchitectureHotspot>,
    missing_tests: Vec<String>,
    runtime_risk_hotspots: Vec<ArchitectureHotspot>,
    validation_requirements: Vec<String>,
    policy_violations: Vec<String>,
    caveats: Vec<String>,
    evidence_ids: Vec<String>,
    reproduce: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ArchitectureComponentSummary {
    id: String,
    name: String,
    file_count: usize,
    sample_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ArchitectureDependencySummary {
    edge_type: String,
    count: usize,
    evidence_available: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ArchitectureHotspot {
    path: String,
    score: f32,
    reasons: Vec<String>,
    evidence_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TrustUiReport {
    task: String,
    steps: Vec<TrustUiStep>,
    caveats: Vec<String>,
    reproduce: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct TrustUiStep {
    label: String,
    status: String,
    evidence: Vec<String>,
}

fn handle_architecture_trust_command(
    json: bool,
    repo: &Path,
    kind: &str,
) -> anyhow::Result<()> {
    let report = build_architecture_trust_report(repo, kind)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render_architecture_trust_markdown(&report));
    }
    Ok(())
}

fn build_architecture_trust_report(
    repo: &Path,
    kind: &str,
) -> anyhow::Result<ArchitectureTrustReport> {
    let store = open_store(repo)?;
    let files = store.list_files(usize::MAX, 0)?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    let tests = store.tests().unwrap_or_default();
    let policy = load_architecture_policy(repo)?;
    let (summary, policy_report) = if let Some(policy) = policy.as_ref() {
        let resolver = PolicyResolver::new(policy)?;
        (
            ArchitectureDetector::new(&store, Some(&resolver)).detect()?,
            Some(evaluate_policy(&store, &resolver, policy)?),
        )
    } else {
        (ArchitectureDetector::new(&store, None).detect()?, None)
    };
    let graph_counts = store.graph_counts().unwrap_or_default();
    let edge_stats = store.edge_type_stats().unwrap_or_default();
    let mut caveats = Vec::new();
    if graph_counts.edges == 0 {
        caveats.push("no persisted graph edges were available; run `ok index .` for dependency evidence".into());
    }
    if policy_report.is_none() {
        caveats.push("no architecture policy configured; report uses heuristic components".into());
    }

    let components = summary
        .components
        .iter()
        .map(|component| ArchitectureComponentSummary {
            id: component.id.clone(),
            name: component.name.clone(),
            file_count: component.paths.len(),
            sample_paths: component.paths.iter().take(5).cloned().collect(),
        })
        .collect::<Vec<_>>();

    let dependencies = edge_stats
        .iter()
        .map(|(edge_type, stats)| ArchitectureDependencySummary {
            edge_type: edge_type.clone(),
            count: stats.count,
            evidence_available: stats.evidence_available,
        })
        .collect::<Vec<_>>();

    let route_service_boundaries = symbols
        .iter()
        .filter(|symbol| matches!(symbol.kind, open_kioku_core::SymbolKind::Endpoint))
        .map(|symbol| format!("{} in {}", symbol.qualified_name, symbol.file_id.0))
        .take(50)
        .collect::<Vec<_>>();

    let missing_tests = missing_test_files(&files, &tests);
    let high_change_files = high_change_hotspots(&store, &files, &mut caveats);
    let high_risk_files = risk_hotspots(&files, &symbols, &missing_tests, &high_change_files);
    let runtime_risk_hotspots = runtime_hotspots(&files);
    let policy_violations = policy_report
        .as_ref()
        .map(|report| {
            report
                .violations
                .iter()
                .take(20)
                .map(|violation| {
                    format!(
                        "{}: {} -> {} via {:?}",
                        violation.rule_id,
                        violation.source_path.display(),
                        violation.target_path.display(),
                        violation.edge_type
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let validation_requirements = validation_requirements_for_architecture(
        &high_risk_files,
        &missing_tests,
        policy_report.as_ref().map(|report| report.violation_count).unwrap_or(0),
    );
    let evidence_ids = architecture_evidence_ids(&dependencies, &policy_report);
    let reproduce = vec![
        "ok index .".into(),
        "ok architecture overview".into(),
        "ok architecture hotspots".into(),
        "ok architecture policy check --format markdown".into(),
    ];

    Ok(ArchitectureTrustReport {
        kind: kind.into(),
        component_count: components.len(),
        dependency_edge_count: graph_counts.edges,
        cycle_count: 0,
        route_service_boundary_count: route_service_boundaries.len(),
        policy_violation_count: policy_report
            .as_ref()
            .map(|report| report.violation_count)
            .unwrap_or(0),
        components,
        dependencies,
        cycles: Vec::new(),
        route_service_boundaries,
        high_risk_files,
        high_change_files,
        missing_tests,
        runtime_risk_hotspots,
        validation_requirements,
        policy_violations,
        caveats,
        evidence_ids,
        reproduce,
    })
}

fn render_architecture_trust_markdown(report: &ArchitectureTrustReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Architecture {}\n\n", title_case(&report.kind)));
    out.push_str("| Signal | Value |\n| --- | --- |\n");
    out.push_str(&format!("| Components | `{}` |\n", report.component_count));
    out.push_str(&format!(
        "| Dependency edges | `{}` |\n",
        report.dependency_edge_count
    ));
    out.push_str(&format!("| Cycles | `{}` |\n", report.cycle_count));
    out.push_str(&format!(
        "| Route/service boundaries | `{}` |\n",
        report.route_service_boundary_count
    ));
    out.push_str(&format!(
        "| Policy violations | `{}` |\n\n",
        report.policy_violation_count
    ));

    if report.kind == "overview" {
        push_arch_components(&mut out, &report.components);
        push_arch_dependencies(&mut out, &report.dependencies);
        push_hotspots(&mut out, "High-risk Files", &report.high_risk_files);
        push_string_list(
            &mut out,
            "Route/Service Boundaries",
            &report.route_service_boundaries,
        );
        push_string_list(&mut out, "Policy Violations", &report.policy_violations);
    } else if report.kind == "clusters" {
        push_arch_components(&mut out, &report.components);
    } else if report.kind == "hotspots" {
        push_hotspots(&mut out, "High-risk Files", &report.high_risk_files);
        push_hotspots(&mut out, "High-change Files", &report.high_change_files);
        push_string_list(&mut out, "Missing Tests", &report.missing_tests);
        push_hotspots(
            &mut out,
            "Runtime-risk Hotspots",
            &report.runtime_risk_hotspots,
        );
    } else if report.kind == "boundaries" {
        push_string_list(
            &mut out,
            "Route/Service Boundaries",
            &report.route_service_boundaries,
        );
        push_string_list(&mut out, "Policy Violations", &report.policy_violations);
    } else if report.kind == "drift" {
        push_string_list(&mut out, "Policy Violations", &report.policy_violations);
        push_string_list(&mut out, "Missing Tests", &report.missing_tests);
        push_string_list(&mut out, "Caveats", &report.caveats);
    }

    push_string_list(
        &mut out,
        "Validation Requirements",
        &report.validation_requirements,
    );
    push_string_list(&mut out, "Evidence Handles", &report.evidence_ids);
    push_string_list(&mut out, "Caveats", &report.caveats);
    push_string_list(&mut out, "Reproduce", &report.reproduce);
    out
}

fn push_arch_components(out: &mut String, components: &[ArchitectureComponentSummary]) {
    out.push_str("## Components\n\n");
    if components.is_empty() {
        out.push_str("- None detected.\n\n");
        return;
    }
    for component in components {
        out.push_str(&format!(
            "- `{}`: {} file(s)",
            component.id, component.file_count
        ));
        if !component.sample_paths.is_empty() {
            out.push_str(&format!("; samples `{}`", component.sample_paths.join("`, `")));
        }
        out.push('\n');
    }
    out.push('\n');
}

fn push_arch_dependencies(out: &mut String, dependencies: &[ArchitectureDependencySummary]) {
    out.push_str("## Dependencies\n\n");
    if dependencies.is_empty() {
        out.push_str("- No dependency edge stats were available.\n\n");
        return;
    }
    for dependency in dependencies.iter().filter(|dependency| dependency.count > 0) {
        out.push_str(&format!(
            "- `{}`: {} edge(s), evidence_available `{}`\n",
            dependency.edge_type, dependency.count, dependency.evidence_available
        ));
    }
    out.push('\n');
}

fn push_hotspots(out: &mut String, title: &str, hotspots: &[ArchitectureHotspot]) {
    out.push_str(&format!("## {title}\n\n"));
    if hotspots.is_empty() {
        out.push_str("- None detected.\n\n");
        return;
    }
    for hotspot in hotspots.iter().take(10) {
        out.push_str(&format!(
            "- `{}` score `{:.2}`: {}\n",
            hotspot.path,
            hotspot.score,
            hotspot.reasons.join("; ")
        ));
        if !hotspot.evidence_ids.is_empty() {
            out.push_str(&format!("  - evidence: `{}`\n", hotspot.evidence_ids.join("`, `")));
        }
    }
    out.push('\n');
}

fn push_string_list(out: &mut String, title: &str, values: &[String]) {
    out.push_str(&format!("## {title}\n\n"));
    if values.is_empty() {
        out.push_str("- None.\n\n");
        return;
    }
    for value in values.iter().take(50) {
        out.push_str(&format!("- `{}`\n", value));
    }
    out.push('\n');
}

fn missing_test_files(files: &[open_kioku_core::File], tests: &[TestTarget]) -> Vec<String> {
    let test_file_ids = tests
        .iter()
        .map(|test| test.file_id.0.clone())
        .collect::<BTreeSet<_>>();
    let test_stems = files
        .iter()
        .filter(|file| is_test_like_path(&file.path))
        .filter_map(|file| file.path.file_stem().and_then(|stem| stem.to_str()))
        .map(normalize_test_stem)
        .collect::<BTreeSet<_>>();
    let mut missing = files
        .iter()
        .filter(|file| !file.is_generated && !file.is_vendor && !is_test_like_path(&file.path))
        .filter(|file| is_source_like_path(&file.path))
        .filter(|file| !test_file_ids.contains(&file.id.0))
        .filter(|file| {
            file.path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(normalize_test_stem)
                .map(|stem| !test_stems.contains(&stem))
                .unwrap_or(true)
        })
        .map(|file| normalize_path_string(&file.path))
        .collect::<Vec<_>>();
    missing.sort();
    missing.truncate(25);
    missing
}

fn high_change_hotspots(
    store: &SqliteStore,
    files: &[open_kioku_core::File],
    caveats: &mut Vec<String>,
) -> Vec<ArchitectureHotspot> {
    let mut unavailable = false;
    let mut hotspots = Vec::new();
    for file in files.iter().filter(|file| !file.is_generated && !file.is_vendor) {
        match store.churn_for_file(&file.path) {
            Ok(churn) if churn.stats.touch_count > 0 => hotspots.push(ArchitectureHotspot {
                path: normalize_path_string(&file.path),
                score: churn.stats.hotspot_score,
                reasons: vec![format!(
                    "{} touch(es), hotspot {:.2}",
                    churn.stats.touch_count, churn.stats.hotspot_score
                )],
                evidence_ids: vec![format!("history-churn:{}", churn.key)],
            }),
            Ok(_) => {}
            Err(_) => unavailable = true,
        }
    }
    if unavailable {
        caveats.push("history churn was unavailable for at least one file".into());
    }
    hotspots.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
    });
    hotspots.truncate(10);
    hotspots
}

fn risk_hotspots(
    files: &[open_kioku_core::File],
    symbols: &[Symbol],
    missing_tests: &[String],
    high_change_files: &[ArchitectureHotspot],
) -> Vec<ArchitectureHotspot> {
    let mut symbol_counts = BTreeMap::<String, usize>::new();
    let file_ids = files
        .iter()
        .map(|file| (file.id.0.clone(), normalize_path_string(&file.path)))
        .collect::<BTreeMap<_, _>>();
    for symbol in symbols {
        if let Some(path) = file_ids.get(&symbol.file_id.0) {
            *symbol_counts.entry(path.clone()).or_default() += 1;
        }
    }
    let missing = missing_tests.iter().cloned().collect::<BTreeSet<_>>();
    let churn_scores = high_change_files
        .iter()
        .map(|hotspot| (hotspot.path.clone(), hotspot.score))
        .collect::<BTreeMap<_, _>>();
    let mut hotspots = Vec::new();
    for file in files.iter().filter(|file| !file.is_generated && !file.is_vendor) {
        let path = normalize_path_string(&file.path);
        let mut score = (file.size_bytes as f32 / 20_000.0).min(0.3);
        let mut reasons = Vec::new();
        if let Some(count) = symbol_counts.get(&path).copied() {
            if count >= 8 {
                score += 0.25;
                reasons.push(format!("{count} indexed symbol(s)"));
            }
        }
        if missing.contains(&path) {
            score += 0.25;
            reasons.push("no adjacent indexed test signal".into());
        }
        if let Some(churn) = churn_scores.get(&path).copied() {
            score += (churn / 5.0).min(0.25);
            reasons.push(format!("history hotspot {:.2}", churn));
        }
        let lower = path.to_ascii_lowercase();
        if ["auth", "security", "config", "database", "db", "mcp", "api"]
            .iter()
            .any(|needle| lower.contains(needle))
        {
            score += 0.2;
            reasons.push("sensitive path naming signal".into());
        }
        if score >= 0.25 || !reasons.is_empty() {
            hotspots.push(ArchitectureHotspot {
                path,
                score: (score * 100.0).round() / 100.0,
                reasons,
                evidence_ids: vec!["architecture:hotspot-ranking".into()],
            });
        }
    }
    hotspots.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
    });
    hotspots.truncate(10);
    hotspots
}

fn runtime_hotspots(files: &[open_kioku_core::File]) -> Vec<ArchitectureHotspot> {
    let mut hotspots = files
        .iter()
        .filter(|file| {
            let path = normalize_path_string(&file.path).to_ascii_lowercase();
            path.contains("runtime") || path.contains("server") || path.contains("worker")
        })
        .map(|file| ArchitectureHotspot {
            path: normalize_path_string(&file.path),
            score: 0.4,
            reasons: vec!["runtime path naming signal".into()],
            evidence_ids: vec!["architecture:runtime-path-signal".into()],
        })
        .collect::<Vec<_>>();
    hotspots.sort_by(|left, right| left.path.cmp(&right.path));
    hotspots.truncate(10);
    hotspots
}

fn validation_requirements_for_architecture(
    high_risk_files: &[ArchitectureHotspot],
    missing_tests: &[String],
    policy_violation_count: usize,
) -> Vec<String> {
    let mut requirements = vec![
        "Run `ok plan --format html <task>` before editing governed areas.".into(),
        "Run `ok verify --format html --plan <plan.json> --changed <path>` after edits.".into(),
    ];
    if !high_risk_files.is_empty() {
        requirements.push("High-risk files require targeted tests before acceptance.".into());
    }
    if !missing_tests.is_empty() {
        requirements.push("Files without indexed tests require manual validation or new tests.".into());
    }
    if policy_violation_count > 0 {
        requirements.push("Architecture policy violations must be resolved or explicitly exempted.".into());
    }
    requirements
}

fn architecture_evidence_ids(
    dependencies: &[ArchitectureDependencySummary],
    policy_report: &Option<PolicyCheckReport>,
) -> Vec<String> {
    let mut ids = dependencies
        .iter()
        .filter(|dependency| dependency.count > 0)
        .map(|dependency| format!("graph-edge-type:{}", dependency.edge_type))
        .collect::<Vec<_>>();
    if policy_report.is_some() {
        ids.push("architecture-policy:summary".into());
    }
    ids.sort();
    ids.dedup();
    ids
}

fn handle_ui_command(json: bool, repo: &Path, args: UiArgs) -> anyhow::Result<()> {
    let report = build_trust_ui_report(repo, args.task.unwrap_or_else(|| "local change".into()))?;
    let format = if json { UiFormat::Json } else { args.format };
    let rendered = match format {
        UiFormat::Json => serde_json::to_string_pretty(&report)?,
        UiFormat::Markdown => render_trust_ui_markdown(&report),
        UiFormat::Html => render_trust_ui_html(&report),
    };
    if let Some(output) = args.output {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(output, rendered)?;
    } else {
        println!("{rendered}");
    }
    Ok(())
}

fn build_trust_ui_report(repo: &Path, task: String) -> anyhow::Result<TrustUiReport> {
    let store = open_store(repo)?;
    let manifest = store.manifest()?;
    let architecture = build_architecture_trust_report(repo, "overview")?;
    let adrs = load_adrs(repo)?;
    let caveats = architecture.caveats.clone();
    Ok(TrustUiReport {
        task,
        steps: vec![
            TrustUiStep {
                label: "Task".into(),
                status: "ready".into(),
                evidence: vec!["user-supplied task".into()],
            },
            TrustUiStep {
                label: "Context".into(),
                status: manifest
                    .as_ref()
                    .map(|manifest| format!("indexed {} file(s)", manifest.file_count))
                    .unwrap_or_else(|| "index unavailable".into()),
                evidence: vec!["index-manifest".into()],
            },
            TrustUiStep {
                label: "Affected files".into(),
                status: format!("{} hotspot candidate(s)", architecture.high_risk_files.len()),
                evidence: vec!["architecture:hotspot-ranking".into()],
            },
            TrustUiStep {
                label: "Affected symbols".into(),
                status: "shown in `ok plan` reports".into(),
                evidence: vec!["plan:relevant_symbols".into()],
            },
            TrustUiStep {
                label: "Tests".into(),
                status: format!("{} missing-test candidate(s)", architecture.missing_tests.len()),
                evidence: vec!["architecture:missing-tests".into()],
            },
            TrustUiStep {
                label: "Runtime evidence".into(),
                status: format!(
                    "{} runtime-risk hotspot candidate(s)",
                    architecture.runtime_risk_hotspots.len()
                ),
                evidence: vec!["architecture:runtime-path-signal".into()],
            },
            TrustUiStep {
                label: "Boundaries".into(),
                status: format!("{} component(s)", architecture.component_count),
                evidence: architecture.evidence_ids.clone(),
            },
            TrustUiStep {
                label: "Contract".into(),
                status: format!("{} ADR(s) available for governance", adrs.len()),
                evidence: adrs.iter().map(|adr| format!("adr:{}", adr.id)).collect(),
            },
            TrustUiStep {
                label: "Verification result".into(),
                status: "run `ok verify --format html` after edits".into(),
                evidence: vec!["verify:pending".into()],
            },
        ],
        caveats,
        reproduce: vec![
            "ok ui".into(),
            "ok plan --format html <task>".into(),
            "ok verify --format html --plan <plan.json> --changed <path>".into(),
            "ok prove --html".into(),
        ],
    })
}

fn render_trust_ui_markdown(report: &TrustUiReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Trust Workflow\n\nTask: `{}`\n\n", report.task));
    for step in &report.steps {
        out.push_str(&format!(
            "- {} -> {} ({})\n",
            step.label,
            step.status,
            step.evidence.join(", ")
        ));
    }
    push_string_list(&mut out, "Caveats", &report.caveats);
    push_string_list(&mut out, "Reproduce", &report.reproduce);
    out
}

fn render_trust_ui_html(report: &TrustUiReport) -> String {
    let mut out = html_document_start("Open Kioku Trust Workflow");
    out.push_str(&format!(
        "<h1>Trust Workflow</h1><section class=\"panel\"><h2>Task</h2><p>{}</p></section>",
        escape_html(&report.task)
    ));
    out.push_str("<section class=\"panel\"><h2>Workflow</h2><ol class=\"steps\">");
    for step in &report.steps {
        out.push_str(&format!(
            "<li><strong>{}</strong><br><span>{}</span><br><small>evidence: {}</small></li>",
            escape_html(&step.label),
            escape_html(&step.status),
            escape_html(&step.evidence.join(", "))
        ));
    }
    out.push_str("</ol></section>");
    html_list_section(&mut out, "Caveats", &report.caveats);
    html_list_section(&mut out, "Reproduce", &report.reproduce);
    out.push_str("<section class=\"panel\"><h2>Safety</h2><p>Reports are source-safe by default and include evidence handles, caveats, validation status, and reproduction commands instead of source snippets.</p></section>");
    html_document_end(&mut out);
    out
}

fn render_plan_report_with_adrs(
    format: PlanFormat,
    report: &PlanReport,
    adrs: &[AdrRecord],
) -> anyhow::Result<String> {
    let mut rendered = format.render(report)?;
    if adrs.is_empty() || matches!(format, PlanFormat::Json | PlanFormat::Toon) {
        return Ok(rendered);
    }
    match format {
        PlanFormat::Html => {
            let section = render_adr_html_section(adrs);
            if let Some(index) = rendered.rfind("</main>") {
                rendered.insert_str(index, &section);
            } else {
                rendered.push_str(&section);
            }
        }
        PlanFormat::Markdown => {
            rendered.push_str(&render_adr_markdown_section(adrs));
        }
        PlanFormat::Text => {
            rendered.push_str("\nADRs governing this plan:\n");
            for adr in adrs {
                rendered.push_str(&format!("  - {} [{}] {}\n", adr.id, adr.status, adr.title));
            }
        }
        PlanFormat::Json | PlanFormat::Toon => {}
    }
    Ok(rendered)
}

fn print_verify_report_with_adrs(report: &ChangeVerificationReport, adrs: &[AdrRecord]) {
    print_verify_report(report);
    if !adrs.is_empty() {
        println!("ADRs governing changed files:");
        for adr in adrs {
            println!("  - {} [{}] {}", adr.id, adr.status, adr.title);
        }
    }
}

fn render_verify_html(report: &ChangeVerificationReport, adrs: &[AdrRecord]) -> String {
    let mut out = html_document_start("Open Kioku Verification");
    out.push_str(&format!(
        "<h1>Verification</h1><section class=\"panel\"><p><strong>Verdict:</strong> {:?}</p><p><strong>Changed files:</strong> {}</p></section>",
        report.verdict,
        report.changed_files.len()
    ));
    html_path_section(&mut out, "Changed Files", &report.changed_files);
    html_string_section(&mut out, "Changed Symbols", &report.changed_symbols);
    html_finding_section(&mut out, "Boundary Failures", &report.boundary_violations);
    html_finding_section(&mut out, "Warnings", &report.warnings);
    html_finding_section(&mut out, "Missing Tests", &report.missing_tests);
    html_finding_section(&mut out, "Changed Impact", &report.changed_impact);
    html_test_section(&mut out, "Recommended Tests", &report.recommended_tests);
    html_list_section(&mut out, "Evidence Handles", &report.evidence_refs);
    if !adrs.is_empty() {
        out.push_str(&render_adr_html_section(adrs));
    }
    out.push_str("<section class=\"panel\"><h2>Reproduce</h2><p><code>ok verify --format html --plan &lt;plan.json&gt; --changed &lt;path&gt;</code></p><p>This report is source-safe by default and does not include source snippets.</p></section>");
    html_document_end(&mut out);
    out
}

fn render_proof_html(report: &ProofReport) -> String {
    let mut out = html_document_start("Open Kioku Proof");
    out.push_str(&format!(
        "<h1>Open Kioku Proof</h1><section class=\"panel\"><p><strong>Repo:</strong> {}</p><p><strong>Tasks:</strong> {} &nbsp; <strong>Average score:</strong> {:.1}</p><p><strong>Privacy:</strong> source snippets included: {}</p></section>",
        escape_html(&report.repo),
        report.summary.tasks_scored,
        report.summary.average_score,
        report.privacy.source_snippets_included
    ));
    out.push_str("<section class=\"panel\"><h2>Tasks</h2><table><thead><tr><th>Task</th><th>Score</th><th>Risk</th><th>Evidence</th></tr></thead><tbody>");
    for task in &report.tasks {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>context {}, validation {}, tools {}</td></tr>",
            escape_html(&task.task),
            task.score,
            escape_html(&task.risk_level),
            task.primary_context_count,
            task.validation_count,
            task.tool_call_count
        ));
    }
    out.push_str("</tbody></table></section>");
    html_list_section(&mut out, "Reproduce", &report.reproduce);
    let notes = report.notes.iter().map(|note| (*note).to_string()).collect::<Vec<_>>();
    html_list_section(&mut out, "Caveats", &notes);
    out.push_str("<section class=\"panel\"><h2>Safety</h2><p>HTML proof reports include metrics, path shapes, evidence handles, caveats, validation status, and reproduction commands. They do not include source snippets unless a future explicit source-reveal mode is added.</p></section>");
    html_document_end(&mut out);
    out
}

fn render_adr_markdown_section(adrs: &[AdrRecord]) -> String {
    let mut out = String::new();
    out.push_str("\n## ADR Governance\n\n");
    for adr in adrs {
        out.push_str(&format!(
            "- `{}` [{}] {} ({})\n",
            adr.id,
            adr.status,
            adr.title,
            adr_links_summary(&adr.links)
        ));
    }
    out
}

fn render_adr_html_section(adrs: &[AdrRecord]) -> String {
    let mut out = String::new();
    out.push_str("<section class=\"panel\"><h2>ADR Governance</h2><ul>");
    for adr in adrs {
        out.push_str(&format!(
            "<li><code>{}</code> [{}] {}<br><small>{}</small></li>",
            escape_html(&adr.id),
            escape_html(&adr.status),
            escape_html(&adr.title),
            escape_html(&adr_links_summary(&adr.links))
        ));
    }
    out.push_str("</ul></section>");
    out
}

fn html_document_start(title: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><title>{}</title><style>body{{font-family:system-ui,-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;line-height:1.45;margin:0;color:#17202a;background:#f8fafc}}main{{max-width:1040px;margin:0 auto;padding:32px 20px}}.panel{{background:#fff;border:1px solid #d8e0ea;border-radius:8px;padding:18px;margin:14px 0}}h1,h2{{line-height:1.2}}code{{background:#edf2f7;border-radius:4px;padding:2px 5px}}table{{border-collapse:collapse;width:100%;font-size:14px}}th,td{{border-bottom:1px solid #e5ebf2;padding:8px;text-align:left;vertical-align:top}}.steps li{{margin:0 0 12px}}</style></head><body><main>",
        escape_html(title)
    )
}

fn html_document_end(out: &mut String) {
    out.push_str("</main></body></html>");
}

fn html_path_section(out: &mut String, title: &str, paths: &[PathBuf]) {
    let values = paths.iter().map(normalize_path_string).collect::<Vec<_>>();
    html_list_section(out, title, &values);
}

fn html_string_section(out: &mut String, title: &str, values: &[String]) {
    html_list_section(out, title, values);
}

fn html_finding_section(out: &mut String, title: &str, findings: &[VerificationFinding]) {
    out.push_str(&format!("<section class=\"panel\"><h2>{}</h2>", escape_html(title)));
    if findings.is_empty() {
        out.push_str("<p>None.</p></section>");
        return;
    }
    out.push_str("<ul>");
    for finding in findings {
        out.push_str(&format!(
            "<li><code>{}</code>: {}<br><small>evidence: {}</small></li>",
            escape_html(&finding.kind),
            escape_html(&finding.reason),
            escape_html(
                &finding
                    .evidence_refs
                    .to_vec()
                    .join(", ")
            )
        ));
    }
    out.push_str("</ul></section>");
}

fn html_test_section(out: &mut String, title: &str, tests: &[TestTarget]) {
    out.push_str(&format!("<section class=\"panel\"><h2>{}</h2>", escape_html(title)));
    if tests.is_empty() {
        out.push_str("<p>None.</p></section>");
        return;
    }
    out.push_str("<ul>");
    for test in tests {
        out.push_str(&format!(
            "<li><code>{}</code> via <code>{}</code><br><small>evidence: {}</small></li>",
            escape_html(&test.name),
            escape_html(test.command.as_deref().unwrap_or("manual validation")),
            escape_html(&test.evidence_refs.join(", "))
        ));
    }
    out.push_str("</ul></section>");
}

fn html_list_section(out: &mut String, title: &str, values: &[String]) {
    out.push_str(&format!("<section class=\"panel\"><h2>{}</h2>", escape_html(title)));
    if values.is_empty() {
        out.push_str("<p>None.</p></section>");
        return;
    }
    out.push_str("<ul>");
    for value in values.iter().take(100) {
        out.push_str(&format!("<li><code>{}</code></li>", escape_html(value)));
    }
    out.push_str("</ul></section>");
}

fn is_test_like_path(path: &Path) -> bool {
    let value = normalize_path_string(path).to_ascii_lowercase();
    value.contains("/test")
        || value.contains("test/")
        || value.ends_with("_test.go")
        || value.ends_with(".test.ts")
        || value.ends_with(".spec.ts")
        || value.ends_with("_test.rs")
}

fn is_source_like_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "kt")
    )
}

fn normalize_test_stem(value: &str) -> String {
    value
        .trim_end_matches("_test")
        .trim_end_matches(".test")
        .trim_end_matches(".spec")
        .replace("_test", "")
        .replace("-test", "")
        .to_ascii_lowercase()
}

fn title_case(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
