fn handle_architecture_policy_command(
    json: bool,
    repo: &Path,
    command: ArchitecturePolicyCommand,
) -> anyhow::Result<()> {
    match command {
        ArchitecturePolicyCommand::Validate { path, format } => {
            let (policy, paths) = if let Some(path) = path {
                let path = if path.is_absolute() {
                    path
                } else {
                    repo.join(path)
                };
                (Some(load_architecture_policy_from_path(&path)?), vec![path])
            } else {
                let policy = load_architecture_policy(repo)?;
                let paths = policy
                    .as_ref()
                    .map(|policy| policy.source_paths(repo))
                    .unwrap_or_default();
                (policy, paths)
            };
            let output = architecture_policy_output(policy, paths);
            match architecture_policy_format(json, format) {
                ArchitecturePolicyFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                ArchitecturePolicyFormat::Markdown => {
                    print!("{}", render_architecture_policy_validate_markdown(&output));
                }
                ArchitecturePolicyFormat::Text => {
                    println!("{}", output.message);
                }
            }
        }
        ArchitecturePolicyCommand::Print => {
            let policy = load_architecture_policy(repo)?;
            let paths = policy
                .as_ref()
                .map(|policy| policy.source_paths(repo))
                .unwrap_or_default();
            let output = architecture_policy_output(policy, paths);
            if json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if let Some(policy) = &output.policy {
                println!("# source: {}", output.source.unwrap_or_default());
                for path in &output.paths {
                    println!("# path: {}", path.display());
                }
                print!("{}", policy.to_toml()?);
            } else {
                println!("{}", output.message);
            }
        }
        ArchitecturePolicyCommand::Check { format } => {
            let format = architecture_policy_format(json, format);
            let Some(policy) = load_architecture_policy(repo)? else {
                let report = open_kioku_core::PolicyCheckReport {
                    configured: false,
                    uncertainty: vec![
                        "no architecture policy configured; dependency edges were not evaluated"
                            .into(),
                    ],
                    ..Default::default()
                };
                match format {
                    ArchitecturePolicyFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    }
                    ArchitecturePolicyFormat::Markdown => {
                        print!("{}", render_policy_check_markdown(&report));
                    }
                    ArchitecturePolicyFormat::Text => {
                        println!(
                            "No architecture policy configured; dependency edges were not evaluated."
                        );
                    }
                }
                return Ok(());
            };
            let store = open_store(repo)?;
            let resolver = PolicyResolver::new(&policy)?;
            let report = evaluate_policy(&store, &resolver, &policy)?;
            match format {
                ArchitecturePolicyFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                ArchitecturePolicyFormat::Markdown => {
                    print!("{}", render_policy_check_markdown(&report));
                }
                ArchitecturePolicyFormat::Text => {
                    println!(
                        "Evaluated {} dependency edge(s): {} allowed, {} violation(s), {} unknown.",
                        report.evaluated_edge_count,
                        report.allowed_edges,
                        report.violation_count,
                        report.unknown_edge_count
                    );
                    for violation in &report.violations {
                        println!(
                            "{} {} -> {} via {:?}: {}",
                            violation.severity,
                            violation.source_path.display(),
                            violation.target_path.display(),
                            violation.edge_type,
                            violation.rule_id
                        );
                    }
                    for note in &report.uncertainty {
                        println!("uncertainty: {note}");
                    }
                }
            }
        }
        ArchitecturePolicyCommand::Explain {
            file,
            symbol,
            format,
        } => {
            let format = architecture_policy_format(json, format);
            let Some(policy) = load_architecture_policy(repo)? else {
                let output = ArchitecturePolicyExplainOutput {
                    configured: false,
                    query_kind: if symbol.is_some() {
                        "symbol".into()
                    } else if file.is_some() {
                        "file".into()
                    } else {
                        "repo".into()
                    },
                    query: symbol
                        .clone()
                        .unwrap_or_else(|| file.unwrap_or_else(|| repo.to_path_buf()).display().to_string()),
                    file_path: None,
                    symbol: None,
                    components: Vec::new(),
                    violations: Vec::new(),
                    exemptions: Vec::new(),
                    uncertainty: vec![
                        "no architecture policy configured; public API boundaries were not evaluated"
                            .into(),
                    ],
                    message: "No architecture policy configured; public API boundaries were not evaluated."
                        .into(),
                };
                match format {
                    ArchitecturePolicyFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&output)?);
                    }
                    ArchitecturePolicyFormat::Markdown => {
                        print!("{}", render_policy_explain_markdown(&output));
                    }
                    ArchitecturePolicyFormat::Text => {
                        println!("{}", output.message);
                    }
                }
                return Ok(());
            };
            let store = open_store(repo)?;
            let resolver = PolicyResolver::new(&policy)?;
            let output =
                architecture_policy_explain_output(repo, &store, &resolver, &policy, file, symbol)?;
            match format {
                ArchitecturePolicyFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                ArchitecturePolicyFormat::Markdown => {
                    print!("{}", render_policy_explain_markdown(&output));
                }
                ArchitecturePolicyFormat::Text => {
                    println!("{}", output.message);
                    for component in &output.components {
                        println!(
                            "component: {} via {}",
                            component.component_id, component.matched_glob
                        );
                    }
                    for violation in &output.violations {
                        println!(
                            "{} {} -> {} via {:?}: {}",
                            violation.severity,
                            violation.source_path.display(),
                            violation.target_path.display(),
                            violation.edge_type,
                            violation.rule_id
                        );
                    }
                    for exemption in &output.exemptions {
                        println!(
                            "exempted {} by {} ({}): {} -> {}",
                            exemption.rule_id,
                            exemption.exemption_id,
                            exemption.scope,
                            exemption.source_path.display(),
                            exemption.target_path.display()
                        );
                    }
                    for note in &output.uncertainty {
                        println!("uncertainty: {note}");
                    }
                }
            }
        }
    }
    Ok(())
}

fn architecture_policy_output(
    policy: Option<ArchitecturePolicy>,
    paths: Vec<PathBuf>,
) -> ArchitecturePolicyOutput {
    let source = policy.as_ref().map(|policy| policy.source);
    let configured = policy.is_some();
    let message = if let Some(source) = source {
        format!("Architecture policy is valid ({source}).")
    } else {
        "No architecture policy configured. Heuristic architecture detection remains active.".into()
    };
    ArchitecturePolicyOutput {
        valid: true,
        configured,
        source,
        paths,
        policy,
        message,
    }
}

fn architecture_policy_format(
    global_json: bool,
    requested: Option<ArchitecturePolicyFormat>,
) -> ArchitecturePolicyFormat {
    if global_json {
        ArchitecturePolicyFormat::Json
    } else {
        requested.unwrap_or(ArchitecturePolicyFormat::Text)
    }
}

fn render_architecture_policy_validate_markdown(output: &ArchitecturePolicyOutput) -> String {
    let mut out = String::new();
    out.push_str("# Architecture Policy Validation\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    out.push_str(&format!("| Valid | `{}` |\n", output.valid));
    out.push_str(&format!("| Configured | `{}` |\n", output.configured));
    out.push_str(&format!(
        "| Source | `{}` |\n",
        output.source.unwrap_or_default()
    ));
    out.push_str(&format!("| Paths | `{}` |\n", output.paths.len()));
    out.push('\n');
    out.push_str(&output.message);
    out.push('\n');
    if !output.paths.is_empty() {
        out.push_str("\n## Paths\n\n");
        for path in &output.paths {
            out.push_str(&format!("- `{}`\n", path.display()));
        }
    }
    if let Some(policy) = &output.policy {
        out.push_str("\n## Policy Summary\n\n");
        out.push_str(&format!("- Layers: `{}`\n", policy.layers.len()));
        out.push_str(&format!(
            "- Dependency rules: `{}`\n",
            policy.dependency_rules.len()
        ));
        out.push_str(&format!(
            "- Public API rules: `{}`\n",
            policy.public_api_rules.len()
        ));
        out.push_str(&format!(
            "- Internal-only rules: `{}`\n",
            policy.internal_only_rules.len()
        ));
        out.push_str(&format!("- Exemptions: `{}`\n", policy.exemptions.len()));
    }
    out
}

fn render_policy_check_markdown(report: &open_kioku_core::PolicyCheckReport) -> String {
    let mut out = String::new();
    out.push_str("# Architecture Policy Check\n\n");
    out.push_str("| Metric | Value |\n| --- | ---: |\n");
    out.push_str(&format!("| Configured | `{}` |\n", report.configured));
    out.push_str(&format!(
        "| Evaluated edges | {} |\n",
        report.evaluated_edge_count
    ));
    out.push_str(&format!("| Allowed edges | {} |\n", report.allowed_edges));
    out.push_str(&format!("| Violations | {} |\n", report.violation_count));
    out.push_str(&format!(
        "| Public API violations | {} |\n",
        report.public_api_violation_count
    ));
    out.push_str(&format!(
        "| Unknown edges | {} |\n",
        report.unknown_edge_count
    ));
    if !report.violations.is_empty() {
        out.push_str("\n## Violations\n\n");
        for violation in &report.violations {
            out.push_str(&format!(
                "- `{}` `{}` -> `{}` via `{:?}` ({})\n",
                violation.rule_id,
                violation.source_path.display(),
                violation.target_path.display(),
                violation.edge_type,
                violation.severity
            ));
        }
    }
    if !report.exemptions.is_empty() {
        out.push_str("\n## Exemptions\n\n");
        for exemption in &report.exemptions {
            out.push_str(&format!(
                "- `{}` exempted by `{}` for `{}` -> `{}`\n",
                exemption.rule_id,
                exemption.exemption_id,
                exemption.source_path.display(),
                exemption.target_path.display()
            ));
        }
    }
    if !report.uncertainty.is_empty() {
        out.push_str("\n## Uncertainty\n\n");
        for note in &report.uncertainty {
            out.push_str(&format!("- {}\n", note));
        }
    }
    out
}

fn render_policy_explain_markdown(output: &ArchitecturePolicyExplainOutput) -> String {
    let mut out = String::new();
    out.push_str("# Architecture Policy Explanation\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    out.push_str(&format!("| Configured | `{}` |\n", output.configured));
    out.push_str(&format!("| Query kind | `{}` |\n", output.query_kind));
    out.push_str(&format!("| Query | `{}` |\n", output.query));
    out.push_str(&format!("| Components | `{}` |\n", output.components.len()));
    out.push_str(&format!("| Violations | `{}` |\n", output.violations.len()));
    out.push_str(&format!("| Exemptions | `{}` |\n", output.exemptions.len()));
    out.push('\n');
    out.push_str(&output.message);
    out.push('\n');
    if !output.components.is_empty() {
        out.push_str("\n## Components\n\n");
        for component in &output.components {
            out.push_str(&format!(
                "- `{}` via `{}`\n",
                component.component_id, component.matched_glob
            ));
        }
    }
    if !output.violations.is_empty() {
        out.push_str("\n## Violations\n\n");
        for violation in &output.violations {
            out.push_str(&format!(
                "- `{}` `{}` -> `{}` via `{:?}` ({})\n",
                violation.rule_id,
                violation.source_path.display(),
                violation.target_path.display(),
                violation.edge_type,
                violation.severity
            ));
        }
    }
    if !output.exemptions.is_empty() {
        out.push_str("\n## Exemptions\n\n");
        for exemption in &output.exemptions {
            out.push_str(&format!(
                "- `{}` exempted by `{}` for `{}` -> `{}`\n",
                exemption.rule_id,
                exemption.exemption_id,
                exemption.source_path.display(),
                exemption.target_path.display()
            ));
        }
    }
    if !output.uncertainty.is_empty() {
        out.push_str("\n## Uncertainty\n\n");
        for note in &output.uncertainty {
            out.push_str(&format!("- {}\n", note));
        }
    }
    out
}

fn architecture_policy_explain_output<S>(
    repo: &Path,
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
    file: Option<PathBuf>,
    symbol: Option<String>,
) -> anyhow::Result<ArchitecturePolicyExplainOutput>
where
    S: MetadataStore + GraphStore,
{
    let (query_kind, query, file_path, symbol) = if let Some(symbol_query) = symbol {
        let symbol = SymbolEngine::new(store).definition(&symbol_query)?;
        let file_path = file_path_for_symbol(store, &symbol)?;
        ("symbol".into(), symbol_query, Some(file_path), Some(symbol))
    } else if let Some(path) = file {
        let path = repo_relative_path(repo, &path);
        ("file".into(), path.display().to_string(), Some(path), None)
    } else {
        ("repo".into(), repo.display().to_string(), None, None)
    };

    let mut uncertainty = Vec::new();
    let components = if let Some(file_path) = &file_path {
        match resolver.resolve_node(file_path, symbol.as_ref().map(|symbol| symbol.id.clone())) {
            Ok(resolved) => resolved.components,
            Err(unmapped) => {
                uncertainty.push(format!(
                    "{} did not match any architecture policy component",
                    unmapped.file_path.display()
                ));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let boundary = evaluate_public_api_boundary(store, resolver, policy)?;
    let violations = boundary
        .violations
        .into_iter()
        .filter(|violation| match &file_path {
            Some(file_path) => {
                violation.source_path == *file_path || violation.target_path == *file_path
            }
            None => true,
        })
        .collect::<Vec<_>>();
    let exemptions = boundary
        .exemptions
        .into_iter()
        .filter(|exemption| match &file_path {
            Some(file_path) => {
                exemption.source_path == *file_path || exemption.target_path == *file_path
            }
            None => true,
        })
        .collect::<Vec<_>>();
    uncertainty.extend(boundary.uncertainty);
    if file_path.is_some() && violations.is_empty() && exemptions.is_empty() {
        uncertainty.push("no public API boundary findings matched this query".into());
    }
    uncertainty.sort();
    uncertainty.dedup();
    let message = format!(
        "Architecture policy explanation for {query_kind} `{query}`: {} component match(es), {} violation(s), {} exemption(s).",
        components.len(),
        violations.len(),
        exemptions.len()
    );
    Ok(ArchitecturePolicyExplainOutput {
        configured: true,
        query_kind,
        query,
        file_path,
        symbol,
        components,
        violations,
        exemptions,
        uncertainty,
        message,
    })
}

