pub async fn run_cli() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("warn").init();
    let cli = Cli::parse();
    let repo = cli.repo.clone();
    match cli.command {
        Command::Init { repo: command_repo } => {
            let repo = resolve_repo(&repo, command_repo);
            std::fs::create_dir_all(repo.join(".ok"))?;
            OkConfig::write_default(repo.join("ok.toml"))?;
            print_text_or_json(
                cli.json,
                "Open Kioku is ready.\n\nNext:\n  ok index\n  ok doctor\n  ok mcp install cursor\n\nIf this is useful, star the repo:\nhttps://github.com/shivyadavus/open-kioku",
                &serde_json::json!({"status":"initialized"}),
            )?;
        }
        Command::Index {
            repo: command_repo,
            with_scip,
            mode,
            workspace,
            from_snapshot,
        } => {
            let repo = resolve_repo(&repo, command_repo);
            let mode = parse_index_mode(&mode)?;
            if mode == IndexMode::CrossProject {
                let workspace = workspace.ok_or_else(|| {
                    anyhow::anyhow!("--workspace is required for cross-project indexing")
                })?;
                let report = build_cross_project_workspace(&workspace)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_workspace_link_report(&report);
                }
                return Ok(());
            }
            if from_snapshot.as_deref() == Some("auto") {
                match snapshot_import(&repo) {
                    Ok(report) => {
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&report)?);
                        } else {
                            println!(
                                "Imported snapshot from {} and rebuilt search index",
                                report.artifact_path.display()
                            );
                            for warning in &report.warnings {
                                println!("warning: {warning}");
                            }
                        }
                        return Ok(());
                    }
                    Err(err) => {
                        eprintln!("snapshot import unavailable; falling back to full index: {err}");
                    }
                }
            }
            let snapshot = index_repo_with_scip_mode(&repo, with_scip.as_deref(), mode)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&snapshot.manifest)?);
            } else {
                println!(
                    "Indexed {} files, {} symbols, {} chunks in {} mode",
                    snapshot.manifest.file_count,
                    snapshot.manifest.symbol_count,
                    snapshot.manifest.chunk_count,
                    snapshot.manifest.index_mode
                );
                if let Some(scip) = &snapshot.scip {
                    println!(
                        "SCIP: mode {:?}, imported {} index(es), {} exact references",
                        scip.mode,
                        scip.imported_paths.len(),
                        scip.exact_references
                    );
                    for attempt in &scip.generator_attempts {
                        println!(
                            "SCIP {}: {:?} - {}",
                            attempt.language, attempt.status, attempt.message
                        );
                    }
                }
            }
        }
        Command::Snapshot { command } => match command {
            SnapshotCommand::Export { quality } => {
                let report = snapshot_export(&repo, quality)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!(
                        "Exported {} snapshot to {}",
                        report.quality,
                        report.artifact_path.display()
                    );
                    println!(
                        "Metadata: {} ({} -> {} bytes)",
                        report.metadata_path.display(),
                        report.metadata.original_size_bytes,
                        report.metadata.compressed_size_bytes
                    );
                }
            }
            SnapshotCommand::Import => {
                let report = snapshot_import(&repo)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!("Imported snapshot from {}", report.artifact_path.display());
                    if report.rebuilt_search {
                        println!("Rebuilt Tantivy search index from imported SQLite index");
                    }
                    for warning in &report.warnings {
                        println!("warning: {warning}");
                    }
                }
            }
            SnapshotCommand::Doctor => {
                let report = snapshot_doctor(&repo);
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_snapshot_doctor_report(&report);
                }
            }
        },
        Command::Watch { repo: command_repo } => {
            let repo = resolve_repo(&repo, command_repo);
            open_kioku_watch::watch_repo(&repo)?;
        }
        Command::Status {
            repo: command_repo,
            markdown,
            write,
            exit_code,
        } => {
            let repo = resolve_repo(&repo, command_repo);
            let manifest = load_index_manifest(&repo)?;
            let doctor = if markdown || write.is_some() || exit_code {
                Some(doctor_report(&repo))
            } else {
                None
            };
            if markdown || write.is_some() {
                let doctor_ref = doctor
                    .as_ref()
                    .expect("doctor report should be available for status snapshot");
                let rendered = render_status_markdown(&repo, manifest.as_ref(), doctor_ref);
                if let Some(path) = write {
                    fs::write(&path, rendered)?;
                    if cli.json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "ok": doctor_ref.ok,
                                "path": path,
                            }))?
                        );
                    } else {
                        println!("Wrote Open Kioku status snapshot to {}", path.display());
                    }
                } else {
                    println!("{rendered}");
                }
            } else if cli.json {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            } else if let Some(manifest) = manifest {
                println!(
                    "Healthy index: {} files, {} symbols, {} skipped, mode {}, indexed at {}",
                    manifest.file_count,
                    manifest.symbol_count,
                    manifest.quality.skipped_paths.len(),
                    manifest.index_mode,
                    manifest.indexed_at
                );
            } else {
                println!("No index found. Run `ok index .`.");
            }
            if exit_code && !doctor.as_ref().map(|report| report.ok).unwrap_or(true) {
                anyhow::bail!("Open Kioku status has failing readiness checks");
            }
        }
        Command::Doctor {
            repo: command_repo,
            format,
        } => {
            let repo = resolve_repo(&repo, command_repo);
            let report = doctor_report(&repo);
            let ok = report.ok;
            if cli.json || format == DoctorFormat::Json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Open Kioku doctor for {}", report.repo.display());
                for check in &report.checks {
                    let marker = match check.status {
                        CheckStatus::Pass => "[ok]",
                        CheckStatus::Warn => "[warn]",
                        CheckStatus::Fail => "[fail]",
                    };
                    println!("{marker:<6} {:<16} {}", check.name, check.message);
                }
                let passes = report
                    .checks
                    .iter()
                    .filter(|c| matches!(c.status, CheckStatus::Pass))
                    .count();
                let warns = report
                    .checks
                    .iter()
                    .filter(|c| matches!(c.status, CheckStatus::Warn))
                    .count();
                let fails = report
                    .checks
                    .iter()
                    .filter(|c| matches!(c.status, CheckStatus::Fail))
                    .count();
                println!(
                    "\n{} checks passed, {} warnings, {} failures",
                    passes, warns, fails
                );

                if !report.next_steps.is_empty() {
                    println!("\nNext steps:");
                    for step in &report.next_steps {
                        println!("- {step}");
                    }
                }
            }
            if !ok {
                std::process::exit(1);
            }
        }
        Command::Setup { command } => match command {
            SetupCommand::Audit {
                repo: command_repo,
                markdown,
                write,
                exit_code,
            } => {
                let repo = resolve_repo(&repo, command_repo);
                let report = setup_audit_report(&repo);
                if markdown || write.is_some() {
                    let rendered = render_setup_audit_markdown(&report);
                    if let Some(path) = write {
                        fs::write(&path, rendered)?;
                        if cli.json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&serde_json::json!({
                                    "ok": report.ok,
                                    "path": path,
                                }))?
                            );
                        } else {
                            println!("Wrote Open Kioku setup audit to {}", path.display());
                        }
                    } else {
                        println!("{rendered}");
                    }
                } else if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_setup_audit_report(&report);
                }
                if exit_code && !report.ok {
                    anyhow::bail!("Open Kioku setup audit has failing checks");
                }
            }
        },
        Command::Graph { command } => match command {
            GraphCommand::Query {
                dsl,
                limit,
                max_depth,
                timeout_ms,
                format,
            } => {
                let store = open_store(&repo)?;
                let ast = open_kioku_graph::query::parse_graph_query(&dsl)?;
                let options = open_kioku_graph::query::GraphQueryOptions {
                    limit,
                    max_depth,
                    deadline_ms: timeout_ms,
                    ..Default::default()
                };
                let result = open_kioku_graph::query::execute_graph_query(
                    &store as &dyn open_kioku_storage::GraphStore,
                    &ast,
                    options,
                )?;
                if format.to_lowercase() == "json" {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("{:?}", result);
                }
            }
            GraphCommand::Schema { format } => {
                let store = open_store(&repo).ok();
                let manifest = store
                    .as_ref()
                    .and_then(|store| open_kioku_storage::MetadataStore::manifest(store).ok())
                    .flatten();
                let schema = open_kioku_graph::schema::current_schema_with_manifest(
                    store
                        .as_ref()
                        .map(|s| s as &dyn open_kioku_storage::GraphStore),
                    manifest.as_ref(),
                );
                if format.to_lowercase() == "markdown" {
                    let mut lines = vec![
                        format!("# Open Kioku Evidence Graph Schema v{}", schema.version),
                        "".to_string(),
                    ];

                    if !schema.feature_flags.is_empty() {
                        lines.push("## Supported Features".to_string());
                        for feature in &schema.feature_flags {
                            lines.push(format!("- `{}`", feature));
                        }
                        lines.push("".to_string());
                    }

                    if !schema.query_features.is_empty() {
                        lines.push("## Query Features".to_string());
                        for feature in &schema.query_features {
                            lines.push(format!("- `{}`", feature));
                        }
                        lines.push("".to_string());
                    }

                    if !schema.evidence_source_types.is_empty() {
                        lines.push("## Evidence Source Types".to_string());
                        for source_type in &schema.evidence_source_types {
                            lines.push(format!("- `{}`", source_type));
                        }
                        lines.push("".to_string());
                    }

                    if !schema.optional_evidence.is_empty() {
                        lines.push("## Optional Evidence Availability".to_string());
                        for evidence in &schema.optional_evidence {
                            lines.push(format!(
                                "- `{}`: {} (count: {})",
                                evidence.name, evidence.status, evidence.evidence_count
                            ));
                            for caveat in &evidence.caveats {
                                lines.push(format!("  - caveat: {}", caveat));
                            }
                        }
                        lines.push("".to_string());
                    }

                    lines.push("## Node Types".to_string());
                    for node in &schema.node_types {
                        let status = if node.stable {
                            "Stable"
                        } else {
                            "Experimental"
                        };
                        lines.push(format!("### {} ({})", node.name, status));
                        lines.push(node.description.clone());
                        if !node.required_fields.is_empty() {
                            lines.push(
                                "- **Required**: ".to_string() + &node.required_fields.join(", "),
                            );
                        }
                        if !node.optional_fields.is_empty() {
                            lines.push(
                                "- **Optional**: ".to_string() + &node.optional_fields.join(", "),
                            );
                        }
                        lines.push("".to_string());
                    }

                    lines.push("## Edge Types".to_string());
                    for edge in &schema.edge_types {
                        let status = if edge.stable {
                            "Stable"
                        } else {
                            "Experimental"
                        };
                        lines.push(format!("### {} ({})", edge.name, status));
                        lines.push(edge.description.clone());
                        lines.push(format!("- **Sources**: {}", edge.source_types.join(", ")));
                        lines.push(format!("- **Targets**: {}", edge.target_types.join(", ")));
                        if !edge.required_evidence.is_empty() {
                            lines.push(format!(
                                "- **Evidence**: {}",
                                edge.required_evidence.join(", ")
                            ));
                        }
                        lines.push("".to_string());
                    }

                    println!("{}", lines.join("\n"));
                } else {
                    println!("{}", serde_json::to_string_pretty(&schema)?);
                }
            }
        },
        Command::Demo { path, force } => {
            let repo = demo_repo_path(path.clone())?;
            let report = build_demo_repo(&repo, force)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                let rel_path = if let Some(ref p) = path {
                    p.display().to_string()
                } else {
                    "./open-kioku-demo".to_string()
                };
                println!("Open Kioku is ready.\n");
                println!("Next:");
                println!("  ok demo --force");
                println!("  ok --repo {} plan token --format markdown\n", rel_path);
                println!("If this is useful, star the repo:");
                println!("https://github.com/shivyadavus/open-kioku");
            }
        }
        Command::Search {
            query,
            limit,
            kind,
            explain_ranking,
            semantic,
            hybrid,
        } => {
            let store = open_store(&repo)?;
            let results = if matches!(kind, SearchKind::Graph) {
                graph_search(&repo, &query, limit)?
            } else if semantic {
                semantic_search(&repo, &store, &query, limit)?
            } else if hybrid {
                hybrid_search(&repo, &store, &query, limit)?
            } else {
                search(&repo, &store, &query, limit)?
            };
            output(cli.json, &results, || {
                for result in &results {
                    println!(
                        "{}:{}  {:.2}  {}",
                        result.path.display(),
                        result.line_range.as_ref().map(|r| r.start).unwrap_or(0),
                        result.score,
                        result.snippet
                    );
                    if explain_ranking {
                        let signals = top_score_signals(result, 3);
                        if signals.is_empty() {
                            println!("  ranking: no dominant signals");
                        } else {
                            println!("  ranking: {}", signals.join(", "));
                        }
                    }
                }
            })?;
        }
        Command::Semantic { command } => match command {
            SemanticCommand::Status { repo: command_repo } => {
                let repo = absolutize(&resolve_repo(&repo, command_repo))?;
                let store = open_store(&repo)?;
                let config = OkConfig::load_from_repo(&repo)?;
                let manager = SemanticIndexManager::new(&repo, &store, &config.semantic);
                let status = manager.status();
                output(cli.json, &status, || print_semantic_status(&status))?;
            }
            SemanticCommand::Index { repo: command_repo } => {
                let repo = absolutize(&resolve_repo(&repo, command_repo))?;
                let store = open_store(&repo)?;
                let mut config = OkConfig::load_from_repo(&repo)?;
                config.semantic.enabled = true;
                let manager = SemanticIndexManager::new(&repo, &store, &config.semantic);
                let report = manager.index()?;
                output(cli.json, &report, || {
                    println!(
                        "Semantic index ready: {} vectors, {} reused, {} embedded",
                        report.status.vector_count, report.reused_embeddings, report.embedded_count
                    );
                })?;
            }
            SemanticCommand::Rebuild { repo: command_repo } => {
                let repo = absolutize(&resolve_repo(&repo, command_repo))?;
                let store = open_store(&repo)?;
                let mut config = OkConfig::load_from_repo(&repo)?;
                config.semantic.enabled = true;
                let manager = SemanticIndexManager::new(&repo, &store, &config.semantic);
                let report = manager.rebuild()?;
                output(cli.json, &report, || {
                    println!(
                        "Semantic index rebuilt: {} vectors, {} embedded",
                        report.status.vector_count, report.embedded_count
                    );
                })?;
            }
            SemanticCommand::Clean {
                repo: command_repo,
                include_cache,
            } => {
                let repo = absolutize(&resolve_repo(&repo, command_repo))?;
                let store = open_store(&repo)?;
                let config = OkConfig::load_from_repo(&repo)?;
                let manager = SemanticIndexManager::new(&repo, &store, &config.semantic);
                manager.clean(include_cache)?;
                println!("Semantic artifacts removed.");
            }
        },
        Command::Symbol { command } => {
            let store = open_store(&repo)?;
            let engine = SymbolEngine::new(&store);
            match command {
                SymbolCommand::Find { name } => output(cli.json, &engine.find(&name, 50)?, || {})?,
                SymbolCommand::Definition { name } => {
                    output(cli.json, &engine.definition(&name)?, || {})?
                }
                SymbolCommand::Refs { name } => {
                    output(cli.json, &engine.references(&name, 50)?, || {})?
                }
            }
        }
        Command::Explain { command } => {
            let store = open_store(&repo)?;
            match command {
                ExplainCommand::File { path } => {
                    let file = store.get_file_by_path(&path)?;
                    let chunks = if let Some(file) = &file {
                        store.chunks_for_file(&file.id)?
                    } else {
                        Vec::new()
                    };
                    let symbols = if let Some(file) = &file {
                        store.symbols_for_file(&file.id)?
                    } else {
                        Vec::new()
                    };
                    output(
                        cli.json,
                        &serde_json::json!({"file": file, "chunks": chunks, "symbols": symbols}),
                        || {
                            if let Some(f) = &file {
                                println!(
                                    "{} ({:?}, {} bytes)",
                                    path.display(),
                                    f.language,
                                    f.size_bytes
                                );
                            }
                            println!("{} chunks, {} symbols indexed", chunks.len(), symbols.len());
                            for symbol in &symbols {
                                let range = symbol
                                    .range
                                    .as_ref()
                                    .map(|r| format!(":{}–{}", r.start, r.end))
                                    .unwrap_or_default();
                                println!("  {:?} {}{}", symbol.kind, symbol.name, range);
                            }
                        },
                    )?;
                }
                ExplainCommand::Symbol { name } => {
                    let symbol = SymbolEngine::new(&store).definition(&name)?;
                    output(cli.json, &symbol, || {})?;
                }
            }
        }
        Command::Impact(args) => {
            let store = open_store(&repo)?;
            let index_dir = default_index_dir(&repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(&index_dir)?)
            } else {
                None
            };
            let engine = ImpactEngine::new(&store)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_history_store(Some(&store));
            let architecture_policy = configured_architecture_policy_report(&repo, &store)?;

            if let Some(since) = args.since.as_deref() {
                let changed = changed_ranges_since(&repo, since)?;
                let mut reports = Vec::new();
                for file in changed.iter().filter_map(|change| change.new_path.as_ref()) {
                    let mut report = engine.for_file(file)?;
                    report.architecture_policy = architecture_policy.clone();
                    reports.push(report);
                }
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "since": since,
                            "changed_files": changed,
                            "impact_reports": reports,
                        }))?
                    );
                } else {
                    println!("Changed files since {since}:");
                    for change in &changed {
                        println!("  {}", render_changed_range(change));
                    }
                    for report in &reports {
                        println!("\nImpact target: {}", report.target);
                        println!(
                            "Risk: {} ({:.2})",
                            report.risk_report.level, report.risk_report.score
                        );
                    }
                }
                return Ok(());
            }

            let mut report = if let Some(path) = args.file {
                let normalized = normalize_to_repo_relative(&repo, &path);
                engine.for_file(&normalized)?
            } else if let Some(symbol) = args.symbol {
                let definition = SymbolEngine::new(&store).definition(&symbol)?;
                let files = store.list_files(usize::MAX, 0)?;
                let file = files.iter().find(|file| file.id == definition.file_id);
                let path_to_use = file
                    .map(|file| file.path.as_path())
                    .unwrap_or(Path::new(&symbol));
                let normalized = normalize_to_repo_relative(&repo, path_to_use);
                engine.for_file(&normalized)?
            } else {
                anyhow::bail!("provide --file or --symbol");
            };
            report.architecture_policy = architecture_policy;
            output(cli.json, &report, || {
                println!("Impact target: {}", report.target);
                println!(
                    "Risk: {} ({:.2})",
                    report.risk_report.level, report.risk_report.score
                );
                println!("\nDirect impacts ({}):", report.direct_impacts.len());
                for result in &report.direct_impacts {
                    println!(
                        "  {}:{} ({:.2})",
                        result.path.display(),
                        result.line_range.as_ref().map(|r| r.start).unwrap_or(0),
                        result.score
                    );
                }
                if !report.indirect_impacts.is_empty() {
                    println!("\nIndirect impacts ({}):", report.indirect_impacts.len());
                    for result in report.indirect_impacts.iter().take(5) {
                        println!(
                            "  {}:{} ({:.2})",
                            result.path.display(),
                            result.line_range.as_ref().map(|r| r.start).unwrap_or(0),
                            result.score
                        );
                    }
                }
            })?;
        }
        Command::Path { from, to } => {
            let store = open_store(&repo)?;
            let from = resolve_graph_node(&store, &from)?;
            let to = resolve_graph_node(&store, &to)?;
            let path = store.shortest_path(&from, &to, 12)?;
            output(cli.json, &path, || {
                if path.is_empty() {
                    println!("No dependency path found.");
                } else {
                    for edge in &path {
                        println!("{} -> {} {:?}", edge.from, edge.to, edge.edge_type);
                    }
                }
            })?;
        }
        Command::Tests { changed } => {
            let store = open_store(&repo)?;
            output(
                cli.json,
                &TestSelector::new(&store).for_changed_path_with_evidence(&changed, 20)?,
                || {},
            )?;
        }
        Command::Context {
            task,
            format,
            compressed,
        } => {
            let store = open_store(&repo)?;
            let pack = build_context_pack(&repo, &store, &task, 20)?;
            if compressed {
                let compressed = ContextHandleStore::open_repo(&repo)?.compress_pack(&pack)?;
                if cli.json || format == ContextPackFormat::Json {
                    println!("{}", serde_json::to_string_pretty(&compressed)?);
                } else if format == ContextPackFormat::Toon {
                    println!(
                        "{}",
                        open_kioku_format::render_compressed_context_toon(&compressed)
                    );
                } else {
                    println!("{}", serde_json::to_string_pretty(&compressed)?);
                }
            } else {
                let rendered = format.render(&pack)?;
                println!("{}", rendered);
            }
        }
        Command::RetrieveContext { handle } => {
            let retrieved =
                ContextHandleStore::open_repo(&repo)?.retrieve(&ContextHandleId::new(handle))?;
            output(cli.json, &retrieved, || {
                if let Some(retrieved) = &retrieved {
                    println!("{}", retrieved.original);
                } else {
                    println!("No context handle found.");
                }
            })?;
        }
        Command::Plan {
            task,
            format,
            limit,
            since,
            verify_evidence,
        } => {
            let store = open_store(&repo)?;
            let task = if let Some(since) = since.as_deref() {
                task_with_changed_ranges(&repo, &task, since)?
            } else {
                task
            };
            let context = build_context_pack(&repo, &store, &task, limit)?;
            let index_dir = default_index_dir(&repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(&index_dir)?)
            } else {
                None
            };
            let report = PlanEngine::new(&store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_history_store(Some(&store))
                .with_memory_facts(RepoMemoryStore::open_repo(&repo)?.search(&task, 8)?)
                .plan_from_context(&task, limit, context)?;
            let format = if cli.json { PlanFormat::Json } else { format };
            println!("{}", format.render(&report)?);
            verify_plan_evidence(&report, verify_evidence)?;
        }
        Command::VerifyBoundary {
            plan,
            changed,
            evidence_refs,
        } => {
            let report = load_saved_plan(&plan)?;
            let outcome = verify_saved_plan_boundary(&report, &changed, &evidence_refs)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                println!(
                    "Boundary verification passed for {} changed file(s)",
                    outcome.changed_files.len()
                );
                for warning in &outcome.warnings {
                    eprintln!("{warning}");
                }
            }
        }
        Command::Verify {
            plan,
            diff,
            git,
            since_plan,
            changed,
            evidence_refs,
            traceability_strict,
            check_api_surface,
            check_deps,
            run_commands,
            write_attestation,
        } => {
            let store = open_store(&repo)?;
            let report = load_saved_plan(&plan)?;
            let mut changed = changed;
            let unified_diff = if let Some(since) = since_plan.as_deref() {
                for change in changed_ranges_since(&repo, since)? {
                    if let Some(path) = change.new_path.or(change.old_path) {
                        changed.push(path);
                    }
                }
                verify_diff_since(&repo, diff.as_deref(), since)?
            } else {
                verify_diff_input(&repo, diff.as_deref(), git)?
            };
            let index_dir = default_index_dir(&repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(&index_dir)?)
            } else {
                None
            };
            let architecture_policy = load_architecture_policy(&repo)?;
            let check_dependency_delta = check_deps || architecture_policy.is_some();
            let contract_store =
                write_attestation.then(|| FsContractStore::new(repo.join(".ok/contracts")));
            let verification = ChangeVerifier::new(&store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_contract_store(
                    contract_store
                        .as_ref()
                        .map(|store| store as &dyn ContractStore),
                )
                .verify(
                    &repo,
                    &report,
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
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&verification)?);
            } else {
                print_verify_report(&verification);
            }
            if matches!(
                verification.verdict,
                open_kioku_patch::VerificationVerdict::Fail
            ) {
                anyhow::bail!("change verification failed");
            }
        }
        Command::Contract { command } => {
            handle_contract_command(cli.json, &repo, command)?;
        }
        Command::Bench(args) => {
            let min_precision = args.quality_min_precision_at_1;
            let report = run_bench(args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_bench_report(&report);
            }
            if let Some(quality) = &report.quality {
                if quality.precision_at_1 < min_precision {
                    anyhow::bail!(
                        "quality precision@1 {:.3} is below required {:.3}",
                        quality.precision_at_1,
                        min_precision
                    );
                }
            }
        }
        Command::WorkflowBench(args) => {
            let min_context_recall = args.min_context_recall;
            let min_verification_accuracy = args.min_verification_accuracy;
            let min_cases = args.min_cases;
            let report = run_workflow_bench(args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_workflow_bench_report(&report);
            }
            if report.case_count < min_cases {
                anyhow::bail!(
                    "workflow benchmark loaded {} cases, below required {}",
                    report.case_count,
                    min_cases
                );
            }
            if report.workflow.context_recall_at_k < min_context_recall {
                anyhow::bail!(
                    "workflow context recall@{} {:.3} is below required {:.3}",
                    report.limit,
                    report.workflow.context_recall_at_k,
                    min_context_recall
                );
            }
            if report.workflow.verification_verdict_accuracy < min_verification_accuracy {
                anyhow::bail!(
                    "workflow verification accuracy {:.3} is below required {:.3}",
                    report.workflow.verification_verdict_accuracy,
                    min_verification_accuracy
                );
            }
        }
        Command::ContractBench(args) => {
            let min_cases = args.min_cases;
            let min_verdict_accuracy = args.min_verdict_accuracy;
            let min_verification_precision = args.min_verification_precision;
            let min_boundary_precision = args.min_boundary_precision;
            let min_boundary_recall = args.min_boundary_recall;
            let min_toon_reduction = args.min_toon_reduction;
            let report = run_contract_bench(args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_contract_bench_report(&report);
            }
            if report.case_count < min_cases {
                anyhow::bail!(
                    "contract benchmark loaded {} cases, below required {}",
                    report.case_count,
                    min_cases
                );
            }
            if !report.failures.is_empty() {
                anyhow::bail!(
                    "contract benchmark failed {} case expectation(s): {}",
                    report.failures.len(),
                    report.failures.join(", ")
                );
            }
            if report.summary.verdict_accuracy < min_verdict_accuracy {
                anyhow::bail!(
                    "contract verdict accuracy {:.3} is below required {:.3}",
                    report.summary.verdict_accuracy,
                    min_verdict_accuracy
                );
            }
            if report.summary.verification_precision < min_verification_precision {
                anyhow::bail!(
                    "contract verification precision {:.3} is below required {:.3}",
                    report.summary.verification_precision,
                    min_verification_precision
                );
            }
            if report.summary.boundary_precision < min_boundary_precision {
                anyhow::bail!(
                    "contract boundary precision {:.3} is below required {:.3}",
                    report.summary.boundary_precision,
                    min_boundary_precision
                );
            }
            if report.summary.boundary_recall < min_boundary_recall {
                anyhow::bail!(
                    "contract boundary recall {:.3} is below required {:.3}",
                    report.summary.boundary_recall,
                    min_boundary_recall
                );
            }
            if report.summary.min_toon_reduction < min_toon_reduction {
                anyhow::bail!(
                    "contract TOON reduction {:.3} is below required {:.3}",
                    report.summary.min_toon_reduction,
                    min_toon_reduction
                );
            }
        }
        Command::Eval(args) => {
            let min_recall = args.min_recall_at_k;
            let min_mrr = args.min_mrr;
            let report = run_eval(args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_eval_report(&report);
            }
            if report.summary.search_recall_at_k < min_recall {
                anyhow::bail!(
                    "eval search recall@{} {:.3} is below required {:.3}",
                    report.limit,
                    report.summary.search_recall_at_k,
                    min_recall
                );
            }
            if report.summary.search_mrr < min_mrr {
                anyhow::bail!(
                    "eval MRR {:.3} is below required {:.3}",
                    report.summary.search_mrr,
                    min_mrr
                );
            }
        }
        Command::Prove(args) => {
            let format = if cli.json {
                ProveFormat::Json
            } else {
                args.format
            };
            let report = run_proof(args)?;
            if matches!(format, ProveFormat::Json) {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", render_proof_markdown(&report));
                println!("\nShareable proof generated.");
                println!("Repo: https://github.com/shivyadavus/open-kioku");
            }
        }
        Command::Architecture { command } => match command {
            ArchitectureCommand::Policy { command } => {
                handle_architecture_policy_command(cli.json, &repo, command)?;
            }
            ArchitectureCommand::Detect => {
                let store = open_store(&repo)?;
                let summary = ArchitectureDetector::new(&store, None).detect()?;
                output(cli.json, &summary, || {})?;
            }
            ArchitectureCommand::Boundaries => {
                let store = open_store(&repo)?;
                let summary = ArchitectureDetector::new(&store, None).detect()?;
                output(cli.json, &summary.components, || {})?;
            }
            ArchitectureCommand::Violations => {
                let store = open_store(&repo)?;
                let summary = ArchitectureDetector::new(&store, None).detect()?;
                output(cli.json, &summary.violations, || {})?;
            }
            ArchitectureCommand::Bench(args) => {
                let min_precision = args.min_precision;
                let min_recall = args.min_recall;
                let max_p95_ms = args.max_p95_ms;
                let report = run_architecture_policy_bench(args)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_architecture_policy_bench_report(&report);
                }
                if report.summary.precision < min_precision {
                    anyhow::bail!(
                        "architecture policy benchmark precision {:.3} is below required {:.3}",
                        report.summary.precision,
                        min_precision
                    );
                }
                if report.summary.recall < min_recall {
                    anyhow::bail!(
                        "architecture policy benchmark recall {:.3} is below required {:.3}",
                        report.summary.recall,
                        min_recall
                    );
                }
                if let Some(max_p95_ms) = max_p95_ms {
                    if report.p95_policy_check_ms > max_p95_ms {
                        anyhow::bail!(
                            "architecture policy p95 {:.2}ms exceeds required {:.2}ms",
                            report.p95_policy_check_ms,
                            max_p95_ms
                        );
                    }
                }
            }
            ArchitectureCommand::Fleet { workspace } => {
                let report = load_fleet_architecture_report(&workspace)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_fleet_architecture_report(&report);
                }
            }
        },
        Command::History { command } => {
            let store = open_store(&repo)?;
            match command {
                HistoryCommand::Similar {
                    task,
                    paths,
                    symbols,
                    limit,
                } => {
                    let query = SimilarChangeQuery {
                        task,
                        paths,
                        symbols,
                    };
                    let report = store.similar_changes(&query, limit)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_similar_change_report(&report);
                    }
                }
                HistoryCommand::Churn {
                    path,
                    module,
                    symbol,
                } => {
                    let provided = usize::from(path.is_some())
                        + usize::from(module.is_some())
                        + usize::from(symbol.is_some());
                    if provided != 1 {
                        anyhow::bail!("provide exactly one of --path, --module, or --symbol");
                    }
                    let summary = if let Some(path) = path {
                        store.churn_for_file(&path)?
                    } else if let Some(module) = module {
                        store.churn_for_module(&module)?
                    } else if let Some(query) = symbol {
                        let symbol = resolve_provenance_symbol(&store, &query)?;
                        store.churn_for_symbol(&symbol.id)?
                    } else {
                        unreachable!("exactly one churn target was checked above");
                    };
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&summary)?);
                    } else {
                        print_churn_summary(&summary);
                    }
                }
                HistoryCommand::Ownership { path } => {
                    let components = ownership_components(&repo, &store, &path)?;
                    let memory_facts = ownership_memory_facts(&repo, &path, &components)?;
                    let report =
                        open_kioku_git::ownership_for_path(open_kioku_git::OwnershipInput {
                            repo: &repo,
                            path: &path,
                            history: &store,
                            memory_facts: &memory_facts,
                            components,
                        })?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_ownership_report(&report);
                    }
                }
                HistoryCommand::Reviewers { path } => {
                    let components = ownership_components(&repo, &store, &path)?;
                    let memory_facts = ownership_memory_facts(&repo, &path, &components)?;
                    let ownership =
                        open_kioku_git::ownership_for_path(open_kioku_git::OwnershipInput {
                            repo: &repo,
                            path: &path,
                            history: &store,
                            memory_facts: &memory_facts,
                            components,
                        })?;
                    let report = open_kioku_git::suggest_reviewers(
                        open_kioku_git::ReviewerSuggestionInput {
                            path: &path,
                            history: &store,
                            ownership: Some(&ownership),
                        },
                    )?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_reviewer_suggestion_report(&report);
                    }
                }
                HistoryCommand::ReviewersBench(args) => {
                    let min_accuracy = args.min_accuracy;
                    let report = run_reviewer_bench(&repo, args)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_reviewer_bench_report(&report);
                    }
                    if report.accuracy < min_accuracy {
                        anyhow::bail!(
                            "reviewer benchmark accuracy {:.3} is below required {:.3}",
                            report.accuracy,
                            min_accuracy
                        );
                    }
                }
                HistoryCommand::SimilarBench(args) => {
                    let min_recall_at_5 = args.min_recall_at_5;
                    let report = run_similar_history_bench(&repo, args)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_similar_history_bench_report(&report);
                    }
                    if report.recall_at_5 < min_recall_at_5 {
                        anyhow::bail!(
                            "similar-history benchmark Top-5 recall {:.3} is below required {:.3}",
                            report.recall_at_5,
                            min_recall_at_5
                        );
                    }
                }
                HistoryCommand::Bench(args) => {
                    let min_reviewer_accuracy = args.min_reviewer_accuracy;
                    let min_similar_recall_at_5 = args.min_similar_recall_at_5;
                    let max_similar_p95_ms = args.max_similar_p95_ms;
                    let max_lookup_p95_ms = args.max_lookup_p95_ms;
                    let report = run_history_bench(&repo, args)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_history_bench_report(&report);
                    }
                    if report.reviewer_accuracy < min_reviewer_accuracy {
                        anyhow::bail!(
                            "history benchmark reviewer accuracy {:.3} is below required {:.3}",
                            report.reviewer_accuracy,
                            min_reviewer_accuracy
                        );
                    }
                    if report.similar_recall_at_5 < min_similar_recall_at_5 {
                        anyhow::bail!(
                            "history benchmark similar-change Top-5 recall {:.3} is below required {:.3}",
                            report.similar_recall_at_5,
                            min_similar_recall_at_5
                        );
                    }
                    if report.similar_p95_ms > max_similar_p95_ms {
                        anyhow::bail!(
                            "history benchmark similar-change p95 latency {:.3} ms exceeds {:.3} ms",
                            report.similar_p95_ms,
                            max_similar_p95_ms
                        );
                    }
                    if report.ownership_churn_p95_ms > max_lookup_p95_ms {
                        anyhow::bail!(
                            "history benchmark ownership/churn p95 latency {:.3} ms exceeds {:.3} ms",
                            report.ownership_churn_p95_ms,
                            max_lookup_p95_ms
                        );
                    }
                    if !report.failures.is_empty() {
                        anyhow::bail!(
                            "history benchmark had {} failing public API case(s)",
                            report.failures.len()
                        );
                    }
                }
                HistoryCommand::Provenance {
                    path,
                    symbol,
                    limit,
                } => {
                    if let Some(path) = path {
                        let provenance = store.provenance_for_path(&path, limit)?;
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&provenance)?);
                        } else {
                            print_file_provenance(&provenance);
                        }
                    } else if let Some(query) = symbol {
                        let symbol = resolve_provenance_symbol(&store, &query)?;
                        let provenance = store.provenance_for_symbol(&symbol.id, limit)?;
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&provenance)?);
                        } else {
                            print_symbol_provenance(&provenance);
                        }
                    }
                }
            }
        }
        Command::Patch { command } => {
            let config = OkConfig::load_from_repo(&repo)?;
            let store = open_store(&repo)?;
            let planner = PatchPlanner::new(&config, &store as &dyn OkStore);
            match command {
                PatchCommand::Plan { task } => output(cli.json, &planner.plan(&task)?, || {})?,
                PatchCommand::Review { id } => {
                    let response = serde_json::json!({
                        "id": id,
                        "status": "requires_stored_patch_plan",
                        "message": "patch review requires a stored patch plan"
                    });
                    print_text_or_json(
                        cli.json,
                        &format!("patch review requires stored patch plan id={id}"),
                        &response,
                    )?;
                }
                PatchCommand::Apply { id, approved } => {
                    anyhow::bail!("patch apply is policy gated and requires a stored diff; id={id} approved={approved}");
                }
            }
        }
        Command::Memory { command } => {
            let memory = RepoMemoryStore::open_repo(&repo)?;
            match command {
                MemoryCommand::Remember {
                    text,
                    source,
                    confidence,
                } => {
                    let fact = memory.remember(&text, &source, confidence.into())?;
                    output(cli.json, &fact, || {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&fact).unwrap_or_default()
                        );
                    })?;
                }
                MemoryCommand::Search { query, limit } => {
                    let results = memory.search(&query, limit)?;
                    output(cli.json, &results, || {
                        if results.is_empty() {
                            println!("No repo memory matched.");
                        } else {
                            for result in &results {
                                println!(
                                    "{:.2} {} [{}]",
                                    result.score, result.fact.text, result.fact.source
                                );
                            }
                        }
                    })?;
                }
                MemoryCommand::Recent { limit } => {
                    let facts = memory.recent(limit)?;
                    output(cli.json, &facts, || {
                        for fact in &facts {
                            println!("{} [{}]", fact.text, fact.source);
                        }
                    })?;
                }
            }
        }
        Command::Mcp { command } => match command {
            McpCommand::Install { client, repo } => {
                let repo = absolutize(&repo)?;
                let snippet = mcp_install_snippet(client, &repo);
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&snippet)?);
                } else {
                    println!("{}", snippet["instructions"].as_str().unwrap_or_default());
                    if let Some(config_text) = snippet["config_text"].as_str() {
                        println!("{config_text}");
                    } else if let Ok(config) = serde_json::to_string_pretty(&snippet["config"]) {
                        println!("{config}");
                    }
                }
            }
            McpCommand::Serve {
                repo,
                read_only,
                allow_write,
                approval_required,
                allow_command,
                deny_network,
                hide_experimental,
            } => {
                let mut config = OkConfig::load_from_repo(&repo)?;
                config.mcp.mode = if read_only && !allow_write {
                    "read-only".into()
                } else {
                    "write".into()
                };
                config.security.allow_write = allow_write;
                config.security.approval_required = approval_required;
                config.security.deny_network = deny_network;
                config.mcp.hide_experimental = hide_experimental;
                if !allow_command.is_empty() {
                    config.commands.allow = allow_command;
                }
                open_kioku_mcp::serve_stdio(repo, config).await?;
            }
        },
        Command::Scip { command } => match command {
            ScipCommand::Doctor { repo: command_repo } => {
                let repo = resolve_repo(&repo, command_repo);
                let config = OkConfig::load_from_repo(&repo)?;
                let snapshot = scip_setup_report(&repo, &config);
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&snapshot)?);
                } else {
                    print_scip_setup_report(&snapshot);
                }
            }
            ScipCommand::Setup { repo: command_repo } => {
                let repo = resolve_repo(&repo, command_repo);
                let config = OkConfig::load_from_repo(&repo)?;
                let snapshot = scip_setup_report(&repo, &config);
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&snapshot)?);
                } else {
                    print_scip_setup_report(&snapshot);
                    println!("\nTo generate where installed:");
                    println!(
                        "  ok index {} --with-scip auto",
                        shell_quote(&repo.display().to_string())
                    );
                    println!("\nOpen Kioku will never install SCIP indexers unless a future explicit install flag enables it.");
                }
            }
        },
    }
    Ok(())
}
