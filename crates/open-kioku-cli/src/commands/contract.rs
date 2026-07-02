#[derive(Debug, Serialize)]
struct BoundaryVerificationOutcome {
    changed_files: Vec<String>,
    warnings: Vec<String>,
    evidence_refs: Vec<String>,
}

fn load_saved_plan(path: &Path) -> anyhow::Result<PlanReport> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug, Serialize)]
struct ContractCreateOutput {
    contract_id: String,
    stored: bool,
    store_path: Option<PathBuf>,
    contract: ChangeContractV1,
}

#[derive(Debug, Serialize)]
struct ContractExplainOutput {
    contract_id: String,
    task: String,
    primary_files: Vec<String>,
    secondary_files: Vec<String>,
    forbidden_files: Vec<String>,
    architecture_constraints: Vec<String>,
    api_surface_constraints: Vec<String>,
    dependency_delta_constraints: Vec<String>,
    required_tests: Vec<String>,
    validation_commands: Vec<String>,
    traceability: Vec<String>,
    evidence_ref_count: usize,
}

fn handle_contract_command(
    json: bool,
    repo: &Path,
    command: ContractCommand,
) -> anyhow::Result<()> {
    match command {
        ContractCommand::Create {
            task,
            plan,
            plan_json,
            limit,
            no_store,
            format,
        } => {
            let store = open_store(repo)?;
            let plan = contract_plan_from_input(repo, &store, task, plan, plan_json, limit)?;
            let contract = ContractBuilder::from_plan(&plan)?;
            let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
            let stored = !no_store;
            if stored {
                contract_store.save(&contract)?;
            }
            let output = ContractCreateOutput {
                contract_id: contract.id.0.clone(),
                stored,
                store_path: stored.then(|| {
                    repo.join(".ok/contracts")
                        .join(format!("{}.json", contract.id.0))
                }),
                contract,
            };
            print_contract_create_output(&output, effective_contract_format(json, format))?;
        }
        ContractCommand::Verify {
            id,
            contract,
            contract_json,
            diff,
            git,
            since_plan,
            mut changed,
            evidence_refs,
            traceability_strict,
            check_api_surface,
            check_deps,
            run_commands,
            write_attestation,
            format,
        } => {
            let store = open_store(repo)?;
            let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
            let (contract, stored) =
                load_contract_input(&contract_store, id, contract, contract_json)?;
            if write_attestation && !stored {
                anyhow::bail!("--write-attestation requires a stored contract --id");
            }
            let unified_diff = if let Some(since) = since_plan.as_deref() {
                for change in changed_ranges_since(repo, since)? {
                    if let Some(path) = change.new_path.or(change.old_path) {
                        changed.push(path);
                    }
                }
                verify_diff_since(repo, diff.as_deref(), since)?
            } else {
                verify_diff_input(repo, diff.as_deref(), git)?
            };
            let index_dir = default_index_dir(repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(&index_dir)?)
            } else {
                None
            };
            let architecture_policy = load_architecture_policy(repo)?;
            let check_dependency_delta = check_deps || architecture_policy.is_some();
            let verification = ContractVerifier::new(&store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_contract_store(stored.then_some(&contract_store as &dyn ContractStore))
                .verify(
                    repo,
                    &contract,
                    VerifyChangeInput {
                        changed_files: changed,
                        unified_diff,
                        evidence_refs,
                        run_commands,
                        write_attestation,
                        validation_attestations: Vec::new(),
                        traceability_strict,
                        check_api_surface,
                        check_dependency_delta,
                        architecture_policy,
                        suppress_plan_validation_pending: false,
                    },
                )?;
            let failed = matches!(
                verification.decision,
                open_kioku_patch::VerificationDecision::Fail
            );
            print_contract_verification_output(
                &verification,
                effective_contract_format(json, format),
            )?;
            if failed {
                anyhow::bail!("contract verification failed");
            }
        }
        ContractCommand::Explain {
            id,
            contract,
            contract_json,
            format,
        } => {
            let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
            let (contract, _) = load_contract_input(&contract_store, id, contract, contract_json)?;
            let explanation = explain_contract(&contract);
            print_contract_explain_output(&explanation, effective_contract_format(json, format))?;
        }
        ContractCommand::Show { id, format } | ContractCommand::Export { id, format } => {
            let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
            let contract = contract_store.load(&ContractId::new(id))?;
            print_contract_output(&contract, effective_contract_format(json, format))?;
        }
    }
    Ok(())
}

fn effective_contract_format(json: bool, format: ContractFormat) -> ContractFormat {
    if json {
        ContractFormat::Json
    } else {
        format
    }
}

fn contract_plan_from_input(
    repo: &Path,
    store: &SqliteStore,
    task: Option<String>,
    plan: Option<PathBuf>,
    plan_json: Option<String>,
    limit: usize,
) -> anyhow::Result<PlanReport> {
    match (task, plan, plan_json) {
        (None, Some(path), None) => load_saved_plan(&path),
        (None, None, Some(json)) => Ok(serde_json::from_str(&json)?),
        (Some(task), None, None) => {
            let context = build_context_pack(repo, store, &task, limit)?;
            let index_dir = default_index_dir(repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(&index_dir)?)
            } else {
                None
            };
            Ok(PlanEngine::new(store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_history_store(Some(store))
                .with_memory_facts(RepoMemoryStore::open_repo(repo)?.search(&task, 8)?)
                .plan_from_context(&task, limit, context)?)
        }
        _ => anyhow::bail!("provide exactly one of TASK, --plan, or --plan-json"),
    }
}

fn load_contract_input(
    store: &FsContractStore,
    id: Option<String>,
    contract: Option<PathBuf>,
    contract_json: Option<String>,
) -> anyhow::Result<(ChangeContractV1, bool)> {
    match (id, contract, contract_json) {
        (Some(id), None, None) => Ok((store.load(&ContractId::new(id))?, true)),
        (None, Some(path), None) => Ok((parse_contract_json(&fs::read_to_string(path)?)?, false)),
        (None, None, Some(json)) => Ok((parse_contract_json(&json)?, false)),
        _ => anyhow::bail!("provide exactly one of --id, --contract, or --contract-json"),
    }
}

fn parse_contract_json(json: &str) -> anyhow::Result<ChangeContractV1> {
    if let Ok(contract) = serde_json::from_str::<ChangeContractV1>(json) {
        return Ok(contract);
    }
    let record: StoredContractRecord = serde_json::from_str(json)?;
    Ok(record.contract)
}

fn explain_contract(contract: &ChangeContractV1) -> ContractExplainOutput {
    ContractExplainOutput {
        contract_id: contract.id.0.clone(),
        task: contract.task.clone(),
        primary_files: contract_files(&contract.primary_files),
        secondary_files: contract_files(&contract.secondary_files),
        forbidden_files: contract_files(&contract.forbidden_files),
        architecture_constraints: contract
            .architecture_constraints
            .iter()
            .map(|constraint| format!("{} ({:?})", constraint.rule, constraint.severity))
            .collect(),
        api_surface_constraints: contract
            .api_surface_constraints
            .iter()
            .map(|constraint| {
                format!(
                    "{} {:?} ({:?})",
                    constraint.scope, constraint.allowed_changes, constraint.severity
                )
            })
            .collect(),
        dependency_delta_constraints: contract
            .dependency_delta_constraints
            .iter()
            .map(|constraint| {
                format!(
                    "{} -> {} {:?} ({:?})",
                    constraint.source, constraint.target, constraint.action, constraint.severity
                )
            })
            .collect(),
        required_tests: contract
            .required_tests
            .iter()
            .map(|test| format!("{}: {}", test.target, test.reason))
            .collect(),
        validation_commands: contract
            .validation_commands
            .iter()
            .map(|command| format!("{}: {}", command.command, command.reason))
            .collect(),
        traceability: contract
            .traceability
            .iter()
            .map(|trace| format!("{}: {}", trace.field, trace.rationale))
            .collect(),
        evidence_ref_count: contract.evidence_refs.len(),
    }
}

fn contract_files(files: &[open_kioku_contract::ContractFile]) -> Vec<String> {
    files.iter().map(|file| file.as_str().to_string()).collect()
}

fn print_contract_create_output(
    output: &ContractCreateOutput,
    format: ContractFormat,
) -> anyhow::Result<()> {
    match format {
        ContractFormat::Json => println!("{}", serde_json::to_string_pretty(output)?),
        ContractFormat::Markdown => print!("{}", render_contract_create_markdown(output)),
        ContractFormat::Toon => print!("{}", render_contract_create_toon(output)),
    }
    Ok(())
}

fn print_contract_output(
    contract: &ChangeContractV1,
    format: ContractFormat,
) -> anyhow::Result<()> {
    match format {
        ContractFormat::Json => println!("{}", serde_json::to_string_pretty(contract)?),
        ContractFormat::Markdown => print!("{}", render_contract_markdown(contract)),
        ContractFormat::Toon => print!("{}", render_contract_toon(contract)),
    }
    Ok(())
}

fn print_contract_explain_output(
    explanation: &ContractExplainOutput,
    format: ContractFormat,
) -> anyhow::Result<()> {
    match format {
        ContractFormat::Json => println!("{}", serde_json::to_string_pretty(explanation)?),
        ContractFormat::Markdown => print!("{}", render_contract_explain_markdown(explanation)),
        ContractFormat::Toon => print!("{}", render_contract_explain_toon(explanation)),
    }
    Ok(())
}

fn print_contract_verification_output(
    report: &ContractVerificationReport,
    format: ContractFormat,
) -> anyhow::Result<()> {
    match format {
        ContractFormat::Json => println!("{}", serde_json::to_string_pretty(report)?),
        ContractFormat::Markdown => print!("{}", render_contract_verification_markdown(report)),
        ContractFormat::Toon => print!("{}", render_contract_verification_toon(report)),
    }
    Ok(())
}

fn render_contract_create_markdown(output: &ContractCreateOutput) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Change Contract `{}`\n\n", output.contract_id));
    out.push_str(&format!("- Stored: `{}`\n", output.stored));
    if let Some(path) = &output.store_path {
        out.push_str(&format!("- Path: `{}`\n", path.display()));
    }
    out.push('\n');
    out.push_str(&render_contract_markdown(&output.contract));
    out
}

fn render_contract_create_toon(output: &ContractCreateOutput) -> String {
    let path = output
        .store_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    format!(
        "type: contract_create\nid: {}\nstored: {}\npath: {}\n{}",
        output.contract_id,
        output.stored,
        path,
        render_contract_toon(&output.contract)
    )
}

fn render_contract_markdown(contract: &ChangeContractV1) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Change Contract `{}`\n\n", contract.id.0));
    out.push_str(&format!("Task: {}\n\n", contract.task));
    push_markdown_list(
        &mut out,
        "Primary Files",
        &contract_files(&contract.primary_files),
    );
    push_markdown_list(
        &mut out,
        "Secondary Files",
        &contract_files(&contract.secondary_files),
    );
    push_markdown_list(
        &mut out,
        "Forbidden Files",
        &contract_files(&contract.forbidden_files),
    );
    push_markdown_list(
        &mut out,
        "Architecture Constraints",
        &contract
            .architecture_constraints
            .iter()
            .map(|constraint| {
                format!(
                    "{} ({:?}): {}",
                    constraint.rule, constraint.severity, constraint.reason
                )
            })
            .collect::<Vec<_>>(),
    );
    push_markdown_list(
        &mut out,
        "Validation Commands",
        &contract
            .validation_commands
            .iter()
            .map(|command| format!("{}: {}", command.command, command.reason))
            .collect::<Vec<_>>(),
    );
    if let Some(quality) = contract.extensions.get("evidence_quality") {
        out.push_str("\n## Evidence Quality\n\n");
        out.push_str(&format!("```json\n{}\n```\n", quality));
    }
    out.push_str(&format!(
        "\nRisk: `{:?}` {:.2}\nConfidence: `{:?}` {:.2}\nEvidence refs: `{}`\n",
        contract.risk.level,
        contract.risk.score,
        contract.confidence.level,
        contract.confidence.score,
        contract.evidence_refs.len()
    ));
    out
}

fn render_contract_toon(contract: &ChangeContractV1) -> String {
    let mut out = format!(
        "type: change_contract\nid: {}\ntask: {}\nrisk: {:?} {:.2}\nconfidence: {:?} {:.2}\n",
        contract.id.0,
        contract.task,
        contract.risk.level,
        contract.risk.score,
        contract.confidence.level,
        contract.confidence.score
    );
    push_toon_list(
        &mut out,
        "primary_files",
        &contract_files(&contract.primary_files),
    );
    push_toon_list(
        &mut out,
        "architecture_constraints",
        &contract
            .architecture_constraints
            .iter()
            .map(|constraint| constraint.rule.clone())
            .collect::<Vec<_>>(),
    );
    push_toon_list(
        &mut out,
        "validation_commands",
        &contract
            .validation_commands
            .iter()
            .map(|command| command.command.clone())
            .collect::<Vec<_>>(),
    );
    if let Some(quality) = contract.extensions.get("evidence_quality") {
        out.push_str(&format!("evidence_quality: {}\n", quality));
    }
    out
}

fn render_contract_explain_markdown(explanation: &ContractExplainOutput) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Contract Explanation `{}`\n\nTask: {}\n\n",
        explanation.contract_id, explanation.task
    ));
    push_markdown_list(&mut out, "Primary Files", &explanation.primary_files);
    push_markdown_list(
        &mut out,
        "Architecture Constraints",
        &explanation.architecture_constraints,
    );
    push_markdown_list(
        &mut out,
        "API Surface Constraints",
        &explanation.api_surface_constraints,
    );
    push_markdown_list(
        &mut out,
        "Dependency Delta Constraints",
        &explanation.dependency_delta_constraints,
    );
    push_markdown_list(&mut out, "Required Tests", &explanation.required_tests);
    push_markdown_list(
        &mut out,
        "Validation Commands",
        &explanation.validation_commands,
    );
    push_markdown_list(&mut out, "Traceability", &explanation.traceability);
    out.push_str(&format!(
        "\nEvidence refs: `{}`\n",
        explanation.evidence_ref_count
    ));
    out
}

fn render_contract_explain_toon(explanation: &ContractExplainOutput) -> String {
    let mut out = format!(
        "type: contract_explanation\nid: {}\ntask: {}\nevidence_ref_count: {}\n",
        explanation.contract_id, explanation.task, explanation.evidence_ref_count
    );
    push_toon_list(&mut out, "primary_files", &explanation.primary_files);
    push_toon_list(
        &mut out,
        "architecture_constraints",
        &explanation.architecture_constraints,
    );
    push_toon_list(
        &mut out,
        "api_surface_constraints",
        &explanation.api_surface_constraints,
    );
    push_toon_list(
        &mut out,
        "dependency_delta_constraints",
        &explanation.dependency_delta_constraints,
    );
    push_toon_list(&mut out, "traceability", &explanation.traceability);
    out
}

fn render_contract_verification_markdown(report: &ContractVerificationReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Contract Verification `{}`\n\nDecision: `{:?}`\n\n",
        report.contract_id, report.decision
    ));
    push_markdown_list(
        &mut out,
        "Changed Files",
        &report
            .change_report
            .changed_files
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
    );
    push_markdown_list(
        &mut out,
        "Boundary Failures",
        &finding_summaries(&report.change_report.boundary_violations),
    );
    push_markdown_list(
        &mut out,
        "Warnings",
        &finding_summaries(&report.change_report.warnings),
    );
    out.push_str(&format!(
        "\nEvidence quality: mode `{}`, freshness `{}`\n\n",
        report.policy_snapshot.evidence_quality.index_mode,
        report.policy_snapshot.evidence_quality.freshness
    ));
    push_markdown_list(
        &mut out,
        "Dependency Deltas",
        &report
            .change_report
            .dependency_deltas
            .iter()
            .map(|finding| {
                format!(
                    "{:?}: {} -> {} ({})",
                    finding.classification, finding.source, finding.target, finding.reason
                )
            })
            .collect::<Vec<_>>(),
    );
    out
}

fn render_contract_verification_toon(report: &ContractVerificationReport) -> String {
    let mut out = format!(
        "type: contract_verification\nid: {}\ndecision: {:?}\n",
        report.contract_id, report.decision
    );
    push_toon_list(
        &mut out,
        "changed_files",
        &report
            .change_report
            .changed_files
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
    );
    push_toon_list(
        &mut out,
        "dependency_deltas",
        &report
            .change_report
            .dependency_deltas
            .iter()
            .map(|finding| format!("{:?}:{}", finding.classification, finding.reason))
            .collect::<Vec<_>>(),
    );
    out
}

fn push_markdown_list(out: &mut String, title: &str, values: &[String]) {
    out.push_str(&format!("## {title}\n\n"));
    if values.is_empty() {
        out.push_str("- None\n\n");
    } else {
        for value in values {
            out.push_str(&format!("- `{value}`\n"));
        }
        out.push('\n');
    }
}

fn push_toon_list(out: &mut String, name: &str, values: &[String]) {
    out.push_str(&format!("{name}[{}]:\n", values.len()));
    for value in values {
        out.push_str(&format!("  - {value}\n"));
    }
}

fn finding_summaries(findings: &[open_kioku_patch::VerificationFinding]) -> Vec<String> {
    findings
        .iter()
        .map(|finding| format!("{}: {}", finding.kind, finding.reason))
        .collect()
}

fn verify_diff_input(
    repo: &Path,
    diff_path: Option<&Path>,
    include_git_diff: bool,
) -> anyhow::Result<Option<String>> {
    let mut diffs = Vec::new();
    if let Some(path) = diff_path {
        diffs.push(fs::read_to_string(path)?);
    }
    if include_git_diff {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args(["diff", "--unified=0", "--no-ext-diff", "--relative", "HEAD"])
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "git diff failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        diffs.push(String::from_utf8(output.stdout)?);
    }
    if diffs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(diffs.join("\n")))
    }
}

fn verify_diff_since(
    repo: &Path,
    diff_path: Option<&Path>,
    since: &str,
) -> anyhow::Result<Option<String>> {
    let mut diffs = Vec::new();
    if let Some(path) = diff_path {
        diffs.push(fs::read_to_string(path)?);
    }
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff", "--unified=0", "--no-ext-diff", "--relative"])
        .arg(since)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    diffs.push(String::from_utf8(output.stdout)?);
    if diffs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(diffs.join("\n")))
    }
}

fn changed_ranges_since(repo: &Path, since: &str) -> anyhow::Result<Vec<open_kioku_git::DiffFile>> {
    let changes = open_kioku_git::diff_unified_zero_since(repo, since)?;
    Ok(changes
        .into_iter()
        .filter(|change| change.old_path.is_some() || change.new_path.is_some())
        .collect())
}

fn task_with_changed_ranges(repo: &Path, task: &str, since: &str) -> anyhow::Result<String> {
    let changed = changed_ranges_since(repo, since)?;
    if changed.is_empty() {
        return Ok(format!(
            "{task}\n\nGit diff since `{since}` has no changed files."
        ));
    }
    let mut enriched =
        format!("{task}\n\nChanged files and line ranges from `git diff {since} --unified=0`:\n");
    for change in &changed {
        enriched.push_str("- ");
        enriched.push_str(&render_changed_range(change));
        enriched.push('\n');
    }
    Ok(enriched)
}

fn render_changed_range(change: &open_kioku_git::DiffFile) -> String {
    let path = change
        .new_path
        .as_ref()
        .or(change.old_path.as_ref())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unknown>".into());
    let ranges = change
        .hunks
        .iter()
        .filter_map(|hunk| hunk.new_range.as_ref().or(hunk.old_range.as_ref()))
        .map(|range| {
            if range.start == range.end {
                range.start.to_string()
            } else {
                format!("{}-{}", range.start, range.end)
            }
        })
        .collect::<Vec<_>>();
    let score = change
        .rename_score
        .map(|score| format!(" rename_score={score}"))
        .unwrap_or_default();
    if ranges.is_empty() {
        format!("{:?} {}{}", change.status, path, score)
    } else {
        format!(
            "{:?} {} lines {}{}",
            change.status,
            path,
            ranges.join(","),
            score
        )
    }
}

fn print_verify_report(report: &ChangeVerificationReport) {
    println!("Verification: {:?}", report.verdict);
    println!("Changed files: {}", report.changed_files.len());
    for path in &report.changed_files {
        println!("  - {}", path.display());
    }
    if !report.changed_symbols.is_empty() {
        println!("Changed symbols:");
        for symbol in &report.changed_symbols {
            println!("  - {symbol}");
        }
    }
    if !report.traceability.is_empty() {
        println!("Traceability:");
        for trace in &report.traceability {
            let evidence = if trace.evidence_refs.is_empty() {
                "no direct evidence refs".into()
            } else {
                trace.evidence_refs.join(", ")
            };
            println!("  - {}: {} ({})", trace.field, trace.rationale, evidence);
        }
    }
    print_findings("Boundary failures", &report.boundary_violations);
    print_findings("Warnings", &report.warnings);
    print_findings("Missing tests", &report.missing_tests);
    print_findings("Changed impact", &report.changed_impact);
    print_findings("API surface deltas", &report.api_surface_deltas);
    if !report.dependency_deltas.is_empty() {
        println!("Dependency deltas:");
        for finding in &report.dependency_deltas {
            let source_path = finding
                .source_path
                .as_ref()
                .map(|path| path.as_str())
                .unwrap_or("<unknown>");
            let target_path = finding
                .target_path
                .as_ref()
                .map(|path| path.as_str())
                .unwrap_or(&finding.target);
            let evidence = if finding.evidence_refs.is_empty() {
                "no direct evidence refs".into()
            } else {
                finding
                    .evidence_refs
                    .iter()
                    .map(|reference| reference.0.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            println!(
                "  - {:?}: {} -> {} via {} ({}) [{}]",
                finding.classification,
                source_path,
                target_path,
                finding.edge_type,
                finding.reason,
                evidence
            );
        }
    }
    if !report.recommended_tests.is_empty() {
        println!("Recommended tests:");
        for test in &report.recommended_tests {
            let command = test.command.as_deref().unwrap_or("manual validation");
            println!("  - {} via {}", test.name, command);
        }
    }
    if !report.command_results.is_empty() {
        println!("Command results:");
        for result in &report.command_results {
            let attestation = result
                .attestation_id
                .as_deref()
                .map(|id| format!(" attestation={id}"))
                .unwrap_or_default();
            println!(
                "  - {}: {} ({:?}){}",
                result.command, result.status, result.exit_code, attestation
            );
        }
    }
    if let Some(path) = &report.validation_ledger_path {
        println!("Validation ledger: {}", path.display());
    }
}

fn print_findings(label: &str, findings: &[open_kioku_patch::VerificationFinding]) {
    if findings.is_empty() {
        return;
    }
    println!("{label}:");
    for finding in findings {
        let path = finding
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".into());
        println!("  - [{}] {}: {}", finding.kind, path, finding.reason);
    }
}

fn verify_saved_plan_boundary(
    report: &PlanReport,
    changed: &[PathBuf],
    evidence_refs: &[String],
) -> anyhow::Result<BoundaryVerificationOutcome> {
    let boundary = &report.recommended_change_boundary;
    let allowed = boundary
        .allowed_files
        .iter()
        .map(|path| normalize_boundary_path(path))
        .collect::<std::collections::BTreeSet<_>>();
    let caution = boundary
        .caution_files
        .iter()
        .map(|path| normalize_boundary_path(path))
        .collect::<std::collections::BTreeSet<_>>();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let changed_files = changed
        .iter()
        .map(|path| normalize_boundary_path(path))
        .collect::<Vec<_>>();

    for path in &changed_files {
        if let Some(rule) = boundary
            .forbidden_rules
            .iter()
            .find(|rule| boundary_pattern_matches(&rule.pattern, path))
        {
            errors.push(format!(
                "forbidden boundary edit: {path} matches `{}` ({})",
                rule.pattern, rule.reason
            ));
            continue;
        }
        if allowed.contains(path) {
            continue;
        }
        if let Some(rule) = boundary
            .caution_rules
            .iter()
            .find(|rule| normalize_boundary_path(&rule.path) == *path)
        {
            warnings.push(format!(
                "caution boundary edit: {path} ({}) evidence: {}",
                rule.reason,
                rule.evidence_refs.join(", ")
            ));
            continue;
        }
        if caution.contains(path) {
            warnings.push(format!("caution boundary edit: {path}"));
            continue;
        }
        if evidence_refs.is_empty() {
            errors.push(format!(
                "out of saved plan boundary: {path}; boundary expansion requires explicit evidence via --evidence-ref"
            ));
        } else {
            warnings.push(format!(
                "expanded boundary for {path} with explicit evidence refs: {}",
                evidence_refs.join(", ")
            ));
        }
    }

    if !errors.is_empty() {
        anyhow::bail!("boundary verification failed:\n{}", errors.join("\n"));
    }

    Ok(BoundaryVerificationOutcome {
        changed_files,
        warnings,
        evidence_refs: evidence_refs.to_vec(),
    })
}

fn normalize_boundary_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    raw.trim_start_matches("./").to_string()
}

fn boundary_pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./").replace('\\', "/");
    if pattern == path {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        if let Some(middle) = prefix.strip_prefix("**/") {
            return path == middle
                || path.starts_with(&format!("{middle}/"))
                || path.contains(&format!("/{middle}/"));
        }
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if pattern.contains('*') {
        let mut remainder = path;
        for part in pattern.split('*').filter(|part| !part.is_empty()) {
            if let Some(index) = remainder.find(part) {
                remainder = &remainder[index + part.len()..];
            } else {
                return false;
            }
        }
        return true;
    }
    false
}
