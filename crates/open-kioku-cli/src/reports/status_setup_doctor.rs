fn file_path_for_symbol(store: &dyn MetadataStore, symbol: &Symbol) -> anyhow::Result<PathBuf> {
    let files = store.list_files(usize::MAX, 0)?;
    files
        .into_iter()
        .find(|file| file.id == symbol.file_id)
        .map(|file| file.path)
        .with_context(|| {
            format!(
                "indexed symbol `{}` references missing file id `{}`",
                symbol.qualified_name, symbol.file_id.0
            )
        })
}

fn repo_relative_path(repo: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.strip_prefix(repo).unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    }
}

/// The published manifest, or `None` for a repository nobody has indexed. An index being
/// built, one from a newer Open Kioku, or one that cannot be opened is an error carrying the
/// same sentence every other read surface prints for it.
fn load_index_manifest(repo: &Path) -> anyhow::Result<Option<IndexManifest>> {
    match SqliteStore::open_repo_index(repo)? {
        Some(store) => Ok(store.manifest()?),
        None => Ok(None),
    }
}

/// Whether the repository's index has a graph awaiting `ok index`. `false` without an index:
/// there is nothing to rebuild, and the missing index is reported on its own.
fn index_graph_rebuild_required(repo: &Path) -> anyhow::Result<bool> {
    let index_path = open_kioku_storage::generations::resolve_index_location(repo).sqlite_path();
    if !index_path.exists() {
        return Ok(false);
    }
    Ok(SqliteStore::open_existing(&index_path)?.graph_rebuild_required()?)
}

const GRAPH_REBUILD_REQUIRED_MESSAGE: &str =
    "graph awaiting rebuild: run `ok index` (edges were built by an older index format and were discarded on open)";

/// The same pre-check the MCP server runs in front of `impact_analysis` and `plan_change`:
/// `ok impact` and `ok plan` refuse to answer from a graph awaiting `ok index` or built under
/// other analysis semantics, rather than reporting an empty dependent list as if measured.
fn require_authoritative_relationships(store: &SqliteStore) -> anyhow::Result<()> {
    if store.graph_rebuild_required()? {
        anyhow::bail!(
            "authoritative relationship evidence unavailable: {GRAPH_REBUILD_REQUIRED_MESSAGE}"
        );
    }
    let compatibility = analysis_semantics_compatibility_for_manifest(store.manifest()?.as_ref());
    if compatibility.status.allows_authoritative_relationships() {
        return Ok(());
    }
    anyhow::bail!(
        "authoritative relationship evidence unavailable: analysis semantics {:?}: {}; stored={}, current={}; affected components [{}], languages [{}]; {}",
        compatibility.status,
        compatibility.reasons.join("; "),
        compatibility.stored_fingerprint.as_deref().unwrap_or("missing"),
        compatibility.current_fingerprint,
        compatibility.affected_components.join(", "),
        compatibility.affected_languages.join(", "),
        compatibility.recommended_action
    )
}

fn semantic_lifecycle_status(repo: &Path) -> Option<open_kioku_semantic::SemanticStatus> {
    let index_path = open_kioku_storage::generations::resolve_index_location(repo).sqlite_path();
    if !index_path.exists() {
        return None;
    }
    let config = OkConfig::load_from_repo(repo).ok()?;
    let store = SqliteStore::open_existing(&index_path).ok()?;
    Some(SemanticIndexManager::new(repo, &store, &config.semantic).status())
}

fn analysis_semantics_compatibility_for_manifest(
    manifest: Option<&IndexManifest>,
) -> open_kioku_core::AnalysisSemanticsCompatibility {
    open_kioku_core::classify_analysis_semantics(
        manifest.and_then(|manifest| manifest.analysis_semantics.as_ref()),
        &open_kioku_core::AnalysisSemanticsState::current(),
    )
}

const STATUS_QUALITY_NOTES_LIMIT: usize = 100;

/// The serialized `snake_case` name of a note's kind, as `repo_status` groups by.
fn note_kind_label(note: &QualityNote) -> String {
    serde_json::to_value(note.kind)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| format!("{:?}", note.kind))
}

fn append_status_quality_notes(out: &mut String, notes: &[QualityNote], detail: StatusDetail) {
    if notes.is_empty() {
        return;
    }

    let mut by_kind = BTreeMap::new();
    for note in notes {
        *by_kind.entry(note_kind_label(note)).or_insert(0usize) += 1;
    }
    out.push_str(&format!(
        "\nQuality notes ({}): {}\n\n",
        notes.len(),
        by_kind
            .iter()
            .map(|(kind, count)| format!("{kind}: {count}"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    let limit = match detail {
        StatusDetail::Summary => STATUS_QUALITY_NOTES_LIMIT,
        StatusDetail::Full => usize::MAX,
    };
    for note in notes.iter().take(limit) {
        out.push_str(&format!("- [{}] {}\n", note_kind_label(note), note.message));
    }
    let omitted = notes.len().saturating_sub(limit);
    if omitted > 0 {
        out.push_str(&format!(
            "- {omitted} additional quality notes omitted; use `ok status --markdown --full` or `ok status --json --full` for every note.\n"
        ));
    }
}

fn render_status_markdown(
    repo: &Path,
    manifest: Option<&IndexManifest>,
    doctor: &DoctorReport,
    detail: StatusDetail,
) -> String {
    let mut out = String::new();
    out.push_str("# Open Kioku Status\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    out.push_str(&format!("| Repo | `{}` |\n", repo.display()));
    out.push_str(&format!("| Ready | `{}` |\n", doctor.ok));
    out.push_str(&format!(
        "| Index generation | `{}` |\n",
        open_kioku_storage::generations::resolve_index_location(repo)
            .generation_id()
            .unwrap_or("legacy layout")
    ));
    out.push_str("| Generated by | `ok status --markdown` |\n");

    out.push_str("\n## Index\n\n");
    if let Some(manifest) = manifest {
        out.push_str("| Metric | Value |\n| --- | ---: |\n");
        out.push_str(&format!("| Mode | `{}` |\n", manifest.index_mode));
        let semantics = analysis_semantics_compatibility_for_manifest(Some(manifest));
        out.push_str(&format!(
            "| Analysis semantics | `{:?}` |\n",
            semantics.status
        ));
        out.push_str(&format!(
            "| Stored semantics fingerprint | `{}` |\n",
            semantics.stored_fingerprint.as_deref().unwrap_or("missing")
        ));
        out.push_str(&format!(
            "| Current semantics fingerprint | `{}` |\n",
            semantics.current_fingerprint
        ));
        // An unreadable marker is its own finding (the readiness checks below carry the
        // error), not a claim either way about the graph.
        out.push_str(&format!(
            "| Graph rebuild required | `{}` |\n",
            match index_graph_rebuild_required(repo) {
                Ok(required) => required.to_string(),
                Err(_) => "unknown".into(),
            }
        ));
        out.push_str(&format!("| Files | {} |\n", manifest.file_count));
        out.push_str(&format!("| Symbols | {} |\n", manifest.symbol_count));
        out.push_str(&format!("| Chunks | {} |\n", manifest.chunk_count));
        out.push_str(&format!(
            "| Skipped paths | {} |\n",
            manifest.quality.skipped_paths.len()
        ));
        out.push_str(&format!(
            "| Files with redacted values | {} |\n",
            manifest
                .quality
                .redacted_files
                .map_or_else(|| "not recorded by this index".into(), |count| count.to_string())
        ));
        if manifest.quality.pending_pre_redaction_compaction {
            out.push_str("| Pre-redaction bytes | clearing outstanding; run `ok index` |\n");
        }
        if let Some(snapshot) = &manifest.snapshot {
            out.push_str(&format!(
                "| Imported snapshot | {} |\n",
                markdown_cell(&snapshot_provenance_summary(snapshot))
            ));
        }
        if let Some(excluded) = manifest
            .quality
            .coverage
            .as_ref()
            .map(IndexCoverage::excluded_by_policy)
            .filter(|excluded| *excluded > 0)
        {
            out.push_str(&format!("| Excluded by policy | {excluded} |\n"));
        }
        out.push_str(&format!(
            "| Coverage | {} |\n",
            manifest
                .quality
                .coverage
                .as_ref()
                .map(IndexCoverage::summary_line)
                .unwrap_or_else(|| "not recorded by this index".into())
        ));
        let coverage_gaps = manifest
            .quality
            .coverage
            .as_ref()
            .map(IndexCoverage::gaps)
            .unwrap_or_default();
        if !coverage_gaps.is_empty() {
            out.push_str(&format!(
                "| Coverage gaps | {} |\n",
                // The same sentence the text surfaces print: one gap must not read two ways
                // depending on which flag produced the output.
                coverage_gaps
                    .iter()
                    .map(|gap| gap.caveat())
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        let quality = &manifest.quality;
        match &quality.excluded_test_targets {
            Some(excluded) if !excluded.is_empty() => out.push_str(&format!(
                "| Tests | {} |\n",
                tests_evidence(quality.test_count, excluded)
            )),
            _ => out.push_str(&format!("| Tests | {} |\n", quality.test_count)),
        }
        out.push_str(&format!(
            "| Imports | {} |\n",
            manifest.quality.import_count
        ));
        out.push_str(&format!(
            "| SCIP indexes imported | {} |\n",
            manifest.quality.scip_indexes_imported
        ));
        out.push_str(&format!(
            "| SCIP exact references | {} |\n",
            manifest.quality.scip_exact_references
        ));
        out.push_str(&format!(
            "| Static analysis facts | {} |\n",
            manifest.quality.static_analysis_facts
        ));
        if manifest.quality.runtime_analysis_facts > 0 {
            out.push_str(&format!(
                "| Runtime analysis facts | {} |\n",
                manifest.quality.runtime_analysis_facts
            ));
        }
        if manifest.quality.git_history_facts > 0 {
            out.push_str(&format!(
                "| Git history facts | {} |\n",
                manifest.quality.git_history_facts
            ));
        }
        if manifest.quality.codeql_databases > 0 {
            out.push_str(&format!(
                "| CodeQL databases | {} |\n",
                manifest.quality.codeql_databases
            ));
        }
        if manifest.quality.coverage_reports > 0 {
            out.push_str(&format!(
                "| Coverage reports | {} |\n",
                manifest.quality.coverage_reports
            ));
        }
        if manifest.quality.junit_reports > 0 {
            out.push_str(&format!(
                "| JUnit reports | {} |\n",
                manifest.quality.junit_reports
            ));
        }
        out.push_str(&format!("\nIndexed at `{}`.\n", manifest.indexed_at));
        if !manifest.quality.build_systems.is_empty() {
            out.push_str(&format!(
                "\nBuild systems: `{}`.\n",
                manifest.quality.build_systems.join(", ")
            ));
        }
        if !manifest.quality.semantic_provider_notes.is_empty() {
            out.push_str("\nLocal signal notes:\n");
            for note in &manifest.quality.semantic_provider_notes {
                out.push_str(&format!("- {}\n", note));
            }
        }
        append_status_quality_notes(&mut out, &manifest.quality.quality_notes, detail);
        if detail == StatusDetail::Full && !manifest.quality.skipped_paths.is_empty() {
            out.push_str(&format!(
                "\nSkipped paths ({}):\n\n",
                manifest.quality.skipped_paths.len()
            ));
            for skipped in &manifest.quality.skipped_paths {
                out.push_str(&format!(
                    "- `{}` ({}, {})\n",
                    skipped.path.display(),
                    skipped.reason.label(),
                    skipped.source.label()
                ));
            }
        }
    } else {
        out.push_str(
            "No index manifest was found. Run `ok index .` before handing this repo to an agent.\n",
        );
    }

    if let Some(semantic) = semantic_lifecycle_status(repo) {
        if semantic.state != "disabled" {
            out.push_str("\n## Semantic Index\n\n");
            out.push_str("| Metric | Value |\n| --- | ---: |\n");
            out.push_str(&format!("| State | `{}` |\n", semantic.state));
            out.push_str(&format!("| Backend | `{}` |\n", semantic.backend));
            out.push_str(&format!("| ANN active | `{}` |\n", semantic.ann_active));
            if let Some(profile) = &semantic.ann_profile {
                out.push_str(&format!("| ANN profile | `{profile}` |\n"));
            }
            out.push_str(&format!("| Vectors | {} |\n", semantic.vector_count));
            if semantic.failed_count > 0 {
                out.push_str(&format!("| Failed vectors | {} |\n", semantic.failed_count));
            }
            if let Some(ratio) = semantic.stale_ratio {
                out.push_str(&format!("| Stale ratio at last build | {ratio:.3} |\n"));
            }
            out.push_str(&format!(
                "| Last rebuilt | `{}` |\n",
                semantic.last_rebuilt_at.as_deref().unwrap_or("never")
            ));
            out.push_str(&format!(
                "| Rebuild required | `{}` |\n",
                semantic.rebuild_required
            ));
            if !semantic.rebuild_reasons.is_empty() {
                out.push_str("\nRebuild reasons:\n");
                for reason in &semantic.rebuild_reasons {
                    out.push_str(&format!("- {reason}\n"));
                }
            }
        }
    }

    out.push_str("\n## Readiness Checks\n\n");
    out.push_str("| Status | Check | Evidence |\n| --- | --- | --- |\n");
    for check in &doctor.checks {
        out.push_str(&format!(
            "| `{}` | `{}` | {} |\n",
            check.status.label(),
            check.name,
            markdown_cell(&check.message)
        ));
    }

    out.push_str("\n## Next Steps\n\n");
    if doctor.next_steps.is_empty() {
        out.push_str("- No required next steps.\n");
    } else {
        for step in &doctor.next_steps {
            out.push_str(&format!("- {step}\n"));
        }
    }

    out.push_str("\n## Handoff Commands\n\n");
    out.push_str(&format!(
        "- `ok setup audit --repo {}`\n",
        shell_quote(&repo.display().to_string())
    ));
    out.push_str(&format!(
        "- `ok prove {}`\n",
        shell_quote(&repo.display().to_string())
    ));
    out
}

fn setup_audit_report(repo: &Path) -> SetupAuditReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let doctor = doctor_report(&repo);
    let manifest = load_index_manifest(&repo).ok().flatten();
    let config = OkConfig::load_from_repo(&repo).ok();
    let providers = quality_provider_report(&repo, manifest.as_ref());
    let advanced_providers = advanced_quality_provider_report(&repo, manifest.as_ref());
    let mut checks = Vec::new();
    let mut next_steps = doctor.next_steps.clone();

    if let Some(manifest) = &manifest {
        checks.push(SetupAuditCheck {
            name: "index".into(),
            status: CheckStatus::Pass,
            message: format!(
                "{} files, {} symbols, {} chunks",
                manifest.file_count, manifest.symbol_count, manifest.chunk_count
            ),
        });
        if let Some(graph) = doctor
            .checks
            .iter()
            .find(|check| check.name == "graph" && !matches!(check.status, CheckStatus::Pass))
        {
            checks.push(SetupAuditCheck {
                name: "graph".into(),
                status: graph.status,
                message: graph.message.clone(),
            });
        }
        if manifest.quality.scip_exact_references > 0 {
            checks.push(SetupAuditCheck {
                name: "scip".into(),
                status: CheckStatus::Pass,
                message: format!(
                    "{} exact references imported",
                    manifest.quality.scip_exact_references
                ),
            });
        } else {
            checks.push(SetupAuditCheck {
                name: "scip".into(),
                status: CheckStatus::Warn,
                message:
                    "SCIP exact references are unavailable; impact and plan quality are reduced"
                        .into(),
            });
        }
    } else {
        checks.push(SetupAuditCheck {
            name: "index".into(),
            status: CheckStatus::Fail,
            message: "missing .ok/index.sqlite manifest".into(),
        });
        next_steps.push("Run `ok index .` before relying on Open Kioku in an agent.".into());
    }

    match config {
        Some(config) => {
            if !config.security.allow_write
                && config.security.deny_network
                && config.security.approval_required
            {
                checks.push(SetupAuditCheck {
                    name: "security".into(),
                    status: CheckStatus::Pass,
                    message: "read-only source access, network denied, approvals required".into(),
                });
            } else {
                checks.push(SetupAuditCheck {
                    name: "security".into(),
                    status: CheckStatus::Warn,
                    message: format!(
                        "allow_write={}, deny_network={}, approval_required={}",
                        config.security.allow_write,
                        config.security.deny_network,
                        config.security.approval_required
                    ),
                });
                next_steps.push(
                    "Review ok.toml before exposing write, command, or network access to agents."
                        .into(),
                );
            }
        }
        None => {
            checks.push(SetupAuditCheck {
                name: "config".into(),
                status: CheckStatus::Warn,
                message: "ok.toml is missing or invalid; defaults will be used".into(),
            });
            next_steps.push("Run `ok init .` to create an explicit ok.toml.".into());
        }
    }

    let mcp_check = doctor.checks.iter().find(|check| check.name == "mcp");
    checks.push(SetupAuditCheck {
        name: "mcp".into(),
        status: mcp_check
            .map(|check| check.status)
            .unwrap_or(CheckStatus::Warn),
        message: mcp_check
            .map(|check| check.message.clone())
            .unwrap_or_else(|| "MCP server check was not available".into()),
    });

    let plugin_surfaces = plugin_surfaces(&repo);

    next_steps.sort();
    next_steps.dedup();
    let ok = checks
        .iter()
        .all(|check| !matches!(check.status, CheckStatus::Fail));
    SetupAuditReport {
        ok,
        repo: repo.clone(),
        generated_by: "ok setup audit",
        checks,
        providers,
        advanced_providers,
        clients: all_mcp_clients()
            .into_iter()
            .map(|client| ClientInstallReport {
                client: client.as_str(),
                config_format: client.config_format(),
                install_command: format!(
                    "ok mcp install {} --repo {}",
                    client.as_str(),
                    shell_quote(&repo.display().to_string())
                ),
                verify: client_verify_command(client),
                note: client_install_note(client),
            })
            .collect(),
        plugin_surfaces,
        next_steps,
    }
}

fn print_setup_audit_report(report: &SetupAuditReport) {
    println!("Open Kioku setup audit for {}", report.repo.display());
    for check in &report.checks {
        println!(
            "{:<6} {:<12} {}",
            check.status.marker(),
            check.name,
            check.message
        );
    }
    println!("\nMCP clients:");
    for client in &report.clients {
        println!(
            "- {:<8} {:<5} {}",
            client.client, client.config_format, client.install_command
        );
    }
    println!("\nQuality signals:");
    for provider in &report.providers {
        println!(
            "{:<6} {:<12} {}",
            provider.status.marker(),
            provider.name,
            provider.evidence
        );
    }
    println!("\nAdvanced providers (optional):");
    if report.advanced_providers.is_empty() {
        println!("- none detected; not required for default SCIP/indexed-facts workflow");
    } else {
        for provider in &report.advanced_providers {
            println!(
                "{:<6} {:<12} {}",
                provider.status.marker(),
                provider.name,
                provider.evidence
            );
        }
    }
    println!("\nSource checkout surfaces (optional):");
    for surface in &report.plugin_surfaces {
        let status = if surface.present {
            "present"
        } else {
            "missing"
        };
        println!(
            "- {:<14} {:<7} {}",
            surface.name,
            status,
            surface.path.display()
        );
    }
    if !report.next_steps.is_empty() {
        println!("\nNext steps:");
        for step in &report.next_steps {
            println!("- {step}");
        }
    }
}

fn render_setup_audit_markdown(report: &SetupAuditReport) -> String {
    let mut out = String::new();
    out.push_str("# Open Kioku Setup Audit\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    out.push_str(&format!("| Repo | `{}` |\n", report.repo.display()));
    out.push_str(&format!("| Ready | `{}` |\n", report.ok));
    out.push_str(&format!("| Generated by | `{}` |\n", report.generated_by));

    out.push_str("\n## Checks\n\n");
    out.push_str("| Status | Check | Evidence |\n| --- | --- | --- |\n");
    for check in &report.checks {
        out.push_str(&format!(
            "| `{}` | `{}` | {} |\n",
            check.status.label(),
            check.name,
            markdown_cell(&check.message)
        ));
    }

    out.push_str("\n## MCP Client Matrix\n\n");
    out.push_str("| Client | Config | Install command | Verify |\n| --- | --- | --- | --- |\n");
    for client in &report.clients {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | `{}` |\n",
            client.client, client.config_format, client.install_command, client.verify
        ));
    }

    out.push_str("\n## Quality Signals\n\n");
    out.push_str("| Status | Signal | Evidence | Next step |\n| --- | --- | --- | --- |\n");
    for provider in &report.providers {
        out.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            provider.status.label(),
            provider.name,
            markdown_cell(&provider.evidence),
            markdown_cell(provider.next_step.as_deref().unwrap_or("None"))
        ));
    }

    out.push_str("\n## Advanced Providers\n\n");
    out.push_str("Optional only. Missing entries do not reduce default readiness; Open Kioku's primary precision path is local indexed facts plus SCIP when available.\n\n");
    if report.advanced_providers.is_empty() {
        out.push_str("No advanced provider artifacts detected.\n");
    } else {
        out.push_str("| Status | Provider | Evidence | Next step |\n| --- | --- | --- | --- |\n");
        for provider in &report.advanced_providers {
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {} |\n",
                provider.status.label(),
                provider.name,
                markdown_cell(&provider.evidence),
                markdown_cell(provider.next_step.as_deref().unwrap_or("None"))
            ));
        }
    }

    out.push_str("\n## Source Checkout Surfaces\n\n");
    out.push_str("These are expected only when auditing the Open Kioku source checkout, not every indexed target repository.\n\n");
    out.push_str("| Surface | Status | Path | Note |\n| --- | --- | --- | --- |\n");
    for surface in &report.plugin_surfaces {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | {} |\n",
            surface.name,
            if surface.present {
                "present"
            } else {
                "missing"
            },
            surface.path.display(),
            surface.note
        ));
    }

    out.push_str("\n## Next Steps\n\n");
    if report.next_steps.is_empty() {
        out.push_str("- No required next steps.\n");
    } else {
        for step in &report.next_steps {
            out.push_str(&format!("- {step}\n"));
        }
    }
    out
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn all_mcp_clients() -> [McpClient; 8] {
    [
        McpClient::Claude,
        McpClient::Cursor,
        McpClient::Codex,
        McpClient::Gemini,
        McpClient::Opencode,
        McpClient::Zed,
        McpClient::Windsurf,
        McpClient::Trae,
    ]
}

fn client_verify_command(client: McpClient) -> String {
    match client {
        McpClient::Claude => "restart Claude, then inspect MCP server logs".into(),
        McpClient::Cursor => "open Cursor MCP settings and confirm open-kioku is enabled".into(),
        McpClient::Codex => "run /mcp in Codex and confirm open-kioku is listed".into(),
        McpClient::Gemini => "run gemini /mcp and confirm open-kioku is connected".into(),
        McpClient::Opencode => "run opencode and ask it to use the open-kioku MCP tools".into(),
        McpClient::Zed => "open Agent Panel settings and confirm the server is active".into(),
        McpClient::Windsurf => {
            "open Windsurf, click Cascade MCPs icon, and confirm open-kioku is connected".into()
        }
        McpClient::Trae => "open Trae Settings -> MCP, and confirm open-kioku is active".into(),
    }
}

fn client_install_note(client: McpClient) -> &'static str {
    match client {
        McpClient::Claude => "Claude-style mcpServers JSON.",
        McpClient::Cursor => "Cursor MCP JSON entry.",
        McpClient::Codex => "Codex config.toml mcp_servers entry.",
        McpClient::Gemini => "Gemini CLI settings.json mcpServers entry.",
        McpClient::Opencode => "OpenCode opencode.json mcp local server entry.",
        McpClient::Zed => "Zed settings.json context_servers entry.",
        McpClient::Windsurf => "Windsurf mcp_config.json entry.",
        McpClient::Trae => "Trae mcp.json entry.",
    }
}

fn plugin_surfaces(repo: &Path) -> Vec<PluginSurfaceReport> {
    vec![
        PluginSurfaceReport {
            name: "cursor-plugin",
            path: repo.join(".cursor-plugin/plugin.json"),
            present: repo.join(".cursor-plugin/plugin.json").exists(),
            note: "Cursor plugin manifest.",
        },
        PluginSurfaceReport {
            name: "claude-plugin",
            path: repo.join(".claude-plugin/plugin.json"),
            present: repo.join(".claude-plugin/plugin.json").exists(),
            note: "Claude plugin manifest.",
        },
        PluginSurfaceReport {
            name: "codex-plugin",
            path: repo.join(".codex-plugin/plugin.json"),
            present: repo.join(".codex-plugin/plugin.json").exists(),
            note: "Codex plugin manifest.",
        },
        PluginSurfaceReport {
            name: "github-workflows",
            path: repo.join(".github/workflows"),
            present: repo.join(".github/workflows").is_dir(),
            note: "CI and release automation.",
        },
    ]
}

fn quality_provider_report(
    repo: &Path,
    manifest: Option<&IndexManifest>,
) -> Vec<QualityProviderReport> {
    let build_systems = detect_build_systems(repo);
    let test_count = manifest
        .map(|manifest| manifest.quality.test_count)
        .unwrap_or(0);
    // No manifest means nothing is indexed, which the "index test files" advice covers. A
    // manifest without the count predates it, and cannot tell skipped tests from absent ones.
    let excluded_tests = match manifest {
        Some(manifest) => manifest.quality.excluded_test_targets.clone(),
        None => Some(std::collections::BTreeMap::new()),
    };
    // Cross-project mode reads no source, so it examines no test and leaves the count
    // unrecorded; re-indexing in that mode would not change that.
    let cross_project =
        manifest.is_some_and(|manifest| manifest.index_mode == IndexMode::CrossProject);
    let import_count = manifest
        .map(|manifest| manifest.quality.import_count)
        .unwrap_or(0);
    let static_analysis_facts = manifest
        .map(|manifest| manifest.quality.static_analysis_facts)
        .unwrap_or(0);
    let runtime_analysis_facts = manifest
        .map(|manifest| manifest.quality.runtime_analysis_facts)
        .unwrap_or(0);
    let git_history_facts = manifest
        .map(|manifest| manifest.quality.git_history_facts)
        .unwrap_or(0);
    let mut providers = Vec::new();

    providers.push(QualityProviderReport {
        name: "build",
        status: if build_systems.is_empty() {
            CheckStatus::Warn
        } else {
            CheckStatus::Pass
        },
        evidence: if build_systems.is_empty() {
            "no build system files detected".into()
        } else {
            format!("detected {}", build_systems.join(", "))
        },
        next_step: if build_systems.is_empty() {
            Some("Run from the repository root or add ok.toml with the intended root.".into())
        } else {
            None
        },
    });
    providers.push(QualityProviderReport {
        name: "tests",
        status: if test_count > 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence: match &excluded_tests {
            Some(excluded) => tests_evidence(test_count, excluded),
            None if cross_project => format!(
                "{test_count} indexed test target(s); {CROSS_PROJECT_TESTS_NOT_APPLICABLE}"
            ),
            None => format!("{test_count} indexed test target(s)"),
        },
        next_step: tests_next_step(
            test_count,
            &excluded_tests,
            cross_project,
            "Index test files before relying on validation recommendations.",
        ),
    });
    providers.push(QualityProviderReport {
        name: "imports",
        status: if import_count > 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence: format!("{import_count} indexed import edge(s)"),
        next_step: if import_count == 0 {
            Some("Index source files with imports to improve local dependency evidence.".into())
        } else {
            None
        },
    });
    providers.push(QualityProviderReport {
        name: "static",
        status: if manifest.is_some() {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence: format!("{static_analysis_facts} language-specific static analysis fact(s)"),
        next_step: if manifest.is_none() {
            Some("Run `ok index .` to collect language-specific static analysis facts.".into())
        } else {
            None
        },
    });
    if runtime_analysis_facts > 0 {
        providers.push(QualityProviderReport {
            name: "runtime",
            status: CheckStatus::Pass,
            evidence: format!(
                "{runtime_analysis_facts} runtime analysis fact(s) from local artifacts"
            ),
            next_step: None,
        });
    }
    providers.push(QualityProviderReport {
        name: "git-history",
        status: if git_history_facts > 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence: format!("{git_history_facts} git co-change fact(s) from local history"),
        next_step: if git_history_facts == 0 {
            Some("Keep repository history available and enable `[history].enabled = true`, then rerun `ok index .`.".into())
        } else {
            None
        },
    });
    providers.push(QualityProviderReport {
        name: "validation",
        status: if test_count > 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence: if build_systems.iter().any(|system| system == "gradle") && test_count > 0 {
            "Gradle-scoped validation commands enabled for indexed Java test paths".into()
        } else if test_count > 0 {
            "indexed validation candidates available".into()
        } else {
            match &excluded_tests {
                Some(excluded) if !excluded.is_empty() => format!(
                    "no indexed validation candidates; {}",
                    all_tests_excluded(excluded)
                ),
                _ => "no indexed validation candidates".into(),
            }
        },
        next_step: tests_next_step(
            test_count,
            &excluded_tests,
            cross_project,
            "Add or index tests so plans can return concrete validation commands.",
        ),
    });
    providers
}

/// The indexed test count, with the targets withheld from it named by reason: "0 runnable"
/// beside "3 skipped" is a different repository from "0 indexed".
fn tests_evidence(
    test_count: usize,
    excluded: &std::collections::BTreeMap<open_kioku_core::TestExclusionReason, usize>,
) -> String {
    if excluded.is_empty() {
        return format!("{test_count} indexed test target(s)");
    }
    let withheld = excluded
        .iter()
        .map(|(reason, count)| format!("{count} {}", reason.describe()))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{test_count} runnable indexed test target(s); excluded: {withheld}")
}

/// Worded as the context pack words it (`validation_unavailable_reason`), so one index is not
/// described two ways.
fn all_tests_excluded(
    excluded: &std::collections::BTreeMap<open_kioku_core::TestExclusionReason, usize>,
) -> String {
    match excluded.keys().collect::<Vec<_>>().as_slice() {
        [reason] => format!("every indexed test target is a {}", reason.describe()),
        _ => format!(
            "every indexed test target is excluded ({})",
            excluded
                .iter()
                .map(|(reason, count)| format!("{count} {}", reason.describe()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

const CROSS_PROJECT_TESTS_NOT_APPLICABLE: &str =
    "not applicable in cross-project mode; see each linked project's own index";

/// With no runnable target, advise what would change that. Indexing more test files cannot help
/// a repository whose indexed tests are all skipped.
fn tests_next_step(
    test_count: usize,
    excluded: &Option<std::collections::BTreeMap<open_kioku_core::TestExclusionReason, usize>>,
    cross_project: bool,
    no_tests_advice: &str,
) -> Option<String> {
    if test_count > 0 {
        return None;
    }
    let Some(excluded) = excluded else {
        if cross_project {
            return Some(
                "Cross-project mode reads no source, so it indexes no tests; see each linked \
                 project's own index."
                    .into(),
            );
        }
        return Some(format!(
            "{no_tests_advice} This index predates the count of skipped tests: re-index to tell \
             skipped tests from absent ones."
        ));
    };
    if excluded.is_empty() {
        return Some(no_tests_advice.into());
    }
    Some(
        excluded
            .keys()
            .map(|reason| reason.remedy())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn advanced_quality_provider_report(
    repo: &Path,
    manifest: Option<&IndexManifest>,
) -> Vec<QualityProviderReport> {
    let codeql_dbs = detect_codeql_databases(repo);
    let bsp_descriptors = count_named_artifacts(&[repo.join(".bsp")], &[".json"], 2);
    let coverage_reports = manifest
        .map(|manifest| manifest.quality.coverage_reports)
        .unwrap_or_else(|| {
            count_named_artifacts(&analysis_roots(repo), &["jacoco.xml", "coverage.xml"], 5)
        });
    let junit_reports = manifest
        .map(|manifest| manifest.quality.junit_reports)
        .unwrap_or_else(|| count_named_artifacts(&analysis_roots(repo), &["test-", "junit"], 5));
    let lsp = relevant_lsp_servers(repo);
    let mut providers = Vec::new();

    if bsp_descriptors > 0 {
        providers.push(QualityProviderReport {
            name: "bsp",
            status: CheckStatus::Pass,
            evidence: format!("{bsp_descriptors} BSP descriptor(s) under .bsp"),
            next_step: None,
        });
    }
    if codeql_dbs > 0 {
        providers.push(QualityProviderReport {
            name: "codeql",
            status: CheckStatus::Pass,
            evidence: format!("{codeql_dbs} local CodeQL database artifact(s) detected"),
            next_step: None,
        });
    }
    if !lsp.present.is_empty() && lsp.missing.is_empty() {
        providers.push(QualityProviderReport {
            name: "lsp",
            status: CheckStatus::Pass,
            evidence: format!(
                "detected matching language servers: {}",
                lsp.present.join(", ")
            ),
            next_step: None,
        });
    }
    if coverage_reports > 0 {
        providers.push(QualityProviderReport {
            name: "coverage",
            status: CheckStatus::Pass,
            evidence: format!("{coverage_reports} coverage report artifact(s) detected"),
            next_step: None,
        });
    }
    if junit_reports > 0 {
        providers.push(QualityProviderReport {
            name: "junit",
            status: CheckStatus::Pass,
            evidence: format!("{junit_reports} JUnit-style report artifact(s) detected"),
            next_step: None,
        });
    }
    providers
}

fn detect_build_systems(repo: &Path) -> Vec<String> {
    let mut systems = Vec::new();
    for (name, paths) in [
        (
            "gradle",
            &[
                "settings.gradle",
                "settings.gradle.kts",
                "build.gradle",
                "build.gradle.kts",
            ][..],
        ),
        ("maven", &["pom.xml"][..]),
        (
            "bazel",
            &["WORKSPACE", "WORKSPACE.bazel", "MODULE.bazel"][..],
        ),
        ("cargo", &["Cargo.toml"][..]),
        ("npm", &["package.json"][..]),
        ("go", &["go.mod"][..]),
    ] {
        if paths.iter().any(|path| repo.join(path).exists()) {
            systems.push(name.to_string());
        }
    }
    systems
}

fn detect_codeql_databases(repo: &Path) -> usize {
    [
        ".ok/codeql",
        "codeql-db",
        "codeql-database",
        ".codeql/database",
    ]
    .iter()
    .filter(|path| {
        let path = repo.join(path);
        path.is_dir()
            && (path.join("db-java").exists()
                || path.join("codeql-database.yml").exists()
                || path.join("log").exists())
    })
    .count()
}

fn analysis_roots(repo: &Path) -> Vec<PathBuf> {
    vec![
        repo.join(".ok/analysis"),
        repo.join("build/reports"),
        repo.join("target/site"),
        repo.join("coverage"),
    ]
}

fn count_named_artifacts(roots: &[PathBuf], needles: &[&str], max_depth: usize) -> usize {
    let mut count = 0;
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(root)
            .max_depth(max_depth)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path().to_string_lossy().to_ascii_lowercase();
            let file_name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if needles
                .iter()
                .any(|needle| file_name.contains(needle) || path.ends_with(needle))
            {
                count += 1;
            }
        }
    }
    count
}

struct LspProviderInventory {
    present: Vec<String>,
    missing: Vec<String>,
}

fn relevant_lsp_servers(repo: &Path) -> LspProviderInventory {
    let languages = sample_lsp_languages(repo);
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for language in &languages {
        let candidates: &[&str] = match language.as_str() {
            "java" => &["jdtls", "java-language-server"],
            "rust" => &["rust-analyzer"],
            "go" => &["gopls"],
            "python" => &["pyright-langserver", "pylsp"],
            "typescript" | "javascript" => &["typescript-language-server"],
            _ => &[],
        };
        if candidates.is_empty() {
            continue;
        }
        if let Some(found) = candidates.iter().find(|binary| command_exists(binary)) {
            present.push(format!("{language}:{found}"));
        } else {
            missing.push(format!("{} ({})", language, candidates.join(" or ")));
        }
    }
    present.sort();
    present.dedup();
    missing.sort();
    missing.dedup();
    LspProviderInventory { present, missing }
}

fn sample_lsp_languages(repo: &Path) -> Vec<String> {
    let mut languages = Vec::new();
    for entry in walkdir::WalkDir::new(repo)
        .max_depth(6)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            name != ".git"
                && name != ".ok"
                && name != "build"
                && name != "target"
                && name != "node_modules"
        })
        .filter_map(|entry| entry.ok())
        .take(5000)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(ext) = entry.path().extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        let language = match ext {
            "java" => "java",
            "rs" => "rust",
            "go" => "go",
            "py" => "python",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" => "javascript",
            _ => continue,
        };
        if !languages.iter().any(|existing| existing == language) {
            languages.push(language.to_string());
        }
    }
    languages.sort();
    languages
}

fn doctor_report(repo: &Path) -> DoctorReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let mut checks = Vec::new();
    let mut next_steps = Vec::new();
    let mut coverage = None;

    // 1. Rust toolchain version
    match std::process::Command::new("rustc")
        .arg("--version")
        .output()
    {
        Ok(output) if output.status.success() => {
            let version_str = String::from_utf8_lossy(&output.stdout);
            let version = version_str.split_whitespace().nth(1).unwrap_or("");
            if let Some(minor) = version
                .split('.')
                .nth(1)
                .and_then(|s| s.parse::<u32>().ok())
            {
                if minor < 75 {
                    checks.push(DoctorCheck {
                        name: "rustc",
                        status: CheckStatus::Warn,
                        message: format!("found rustc {version}, recommend >= 1.75"),
                    });
                } else {
                    checks.push(DoctorCheck {
                        name: "rustc",
                        status: CheckStatus::Pass,
                        message: format!("found rustc {version}"),
                    });
                }
            } else {
                checks.push(DoctorCheck {
                    name: "rustc",
                    status: CheckStatus::Pass,
                    message: format!("found {version_str}"),
                });
            }
        }
        _ => {
            checks.push(DoctorCheck {
                name: "rustc",
                status: CheckStatus::Warn,
                message: "rustc not found in PATH".into(),
            });
        }
    }

    // 2. .ok/ directory
    let ok_dir = repo.join(".ok");
    if ok_dir.is_dir() {
        checks.push(DoctorCheck {
            name: "repo",
            status: CheckStatus::Pass,
            message: format!("found .ok directory at {}", ok_dir.display()),
        });
    } else {
        checks.push(DoctorCheck {
            name: "repo",
            status: CheckStatus::Fail,
            message: format!(".ok directory missing at {}", ok_dir.display()),
        });
        next_steps.push("Run `ok init .` to create the configuration and data directory.".into());
    }

    // 3. .ok/index.sqlite
    let index_path = open_kioku_storage::generations::resolve_index_location(&repo).sqlite_path();
    match SqliteStore::open_repo_index(&repo).and_then(|store| match store {
        Some(store) => store.manifest(),
        None => Ok(None),
    }) {
        Ok(Some(manifest)) => checks.push(DoctorCheck {
            name: "index",
            status: CheckStatus::Pass,
            message: format!(
                "{} files, {} symbols, indexed at {}",
                manifest.file_count, manifest.symbol_count, manifest.indexed_at
            ),
        }),
        // No database, or a database without a manifest (what a 4.0.0 read surface left
        // behind): the unindexed case, in the unindexed words, not a warning about a manifest.
        Ok(None) => {
            // With the withdrawal reason when there is one, as `repo_status` reports it: the
            // rows survived and the next run rebuilds. A store that cannot report the reason
            // still gets the plain sentence rather than failing the whole diagnostic.
            let message = match SqliteStore::repo_not_indexed_status(&repo) {
                Ok(status) => match status.reason {
                    Some(reason) => format!("{}; {reason}", status.message),
                    None => status.message,
                },
                Err(_) => open_kioku_storage::generations::not_indexed_message(&repo),
            };
            checks.push(DoctorCheck {
                name: "index",
                status: CheckStatus::Fail,
                message,
            });
            next_steps.push(format!(
                "Run `ok index {}` before connecting an MCP client.",
                repo.display()
            ));
        }
        Err(err) => {
            checks.push(DoctorCheck {
                name: "index",
                status: CheckStatus::Fail,
                message: err.to_string(),
            });
            // The store already distinguishes an index being built (the writer holds the
            // lock and the manifest is not published yet) from one that cannot be opened.
            if open_kioku_storage::generations::index_write_in_progress(&repo) {
                next_steps.push(
                    "Wait for the running `ok index` to finish, then run `ok doctor` again.".into(),
                );
            } else {
                next_steps.push("Remove .ok/index.sqlite and run `ok index .` again.".into());
            }
        }
    }

    // The index check above opens the store, which is the call that discards a pre-4.0 edge
    // layout and sets the marker; it still reports file and symbol counts, which are intact.
    // The relationship reads behind impact and plan are what the marker withholds, so it is a
    // failure here, not a warning.
    match index_graph_rebuild_required(&repo) {
        Ok(true) => {
            checks.push(DoctorCheck {
                name: "graph",
                status: CheckStatus::Fail,
                message: GRAPH_REBUILD_REQUIRED_MESSAGE.into(),
            });
            next_steps.push("Run `ok index .` to rebuild the relationship graph.".into());
        }
        Ok(false) => {
            if index_path.exists() {
                checks.push(DoctorCheck {
                    name: "graph",
                    status: CheckStatus::Pass,
                    message: "relationship graph present".into(),
                });
            }
        }
        Err(err) => checks.push(DoctorCheck {
            name: "graph",
            status: CheckStatus::Fail,
            message: format!("could not read the graph rebuild marker: {err}"),
        }),
    }

    if let Ok(Some(manifest)) = load_index_manifest(&repo) {
        let compatibility = analysis_semantics_compatibility_for_manifest(Some(&manifest));
        let compatible = compatibility.status.allows_authoritative_relationships();
        checks.push(DoctorCheck {
            name: "analysis-semantics",
            status: if compatible {
                CheckStatus::Pass
            } else {
                CheckStatus::Fail
            },
            message: if compatible {
                format!(
                    "compatible; fingerprint {}",
                    compatibility.current_fingerprint
                )
            } else {
                format!(
                    "{:?}: {}; stored={}, current={}; affected components [{}], languages [{}]; {}",
                    compatibility.status,
                    compatibility.reasons.join("; "),
                    compatibility
                        .stored_fingerprint
                        .as_deref()
                        .unwrap_or("missing"),
                    compatibility.current_fingerprint,
                    compatibility.affected_components.join(", "),
                    compatibility.affected_languages.join(", "),
                    compatibility.recommended_action
                )
            },
        });
        if !compatible {
            next_steps.push(compatibility.recommended_action.clone());
        }
    }

    {
        let location = open_kioku_storage::generations::resolve_index_location(&repo);
        let classes = open_kioku_storage::generations::classify_generations(&repo);
        let corrupt = classes
            .iter()
            .filter(|class| {
                class.state == open_kioku_storage::generations::GenerationState::Corrupt
            })
            .count();
        let message = match location.generation_id() {
            Some(id) if corrupt == 0 => format!(
                "active generation {id} ({} generation(s) on disk)",
                classes.len()
            ),
            Some(id) => format!(
                "active generation {id}; {corrupt} corrupt/incomplete generation dir(s) present",
            ),
            None => "legacy layout (adopts the generation layout on the next `ok index`)".into(),
        };
        checks.push(DoctorCheck {
            name: "generations",
            status: if corrupt == 0 {
                CheckStatus::Pass
            } else {
                CheckStatus::Warn
            },
            message,
        });
    }

    if let Some(semantic) = semantic_lifecycle_status(&repo) {
        if semantic.state == "disabled" {
            checks.push(DoctorCheck {
                name: "semantic-lifecycle",
                status: CheckStatus::Pass,
                message: "semantic search is disabled in ok.toml (explicit local opt-in)".into(),
            });
        } else if semantic.rebuild_required {
            checks.push(DoctorCheck {
                name: "semantic-lifecycle",
                status: CheckStatus::Warn,
                message: format!(
                    "{}; {}",
                    semantic.state,
                    semantic.rebuild_reasons.join("; ")
                ),
            });
            next_steps.push("Run `ok semantic index` to rebuild the local semantic index.".into());
        } else {
            let backend_note = if semantic.ann_active {
                format!(
                    "ANN active ({})",
                    semantic.ann_profile.as_deref().unwrap_or("unknown profile")
                )
            } else {
                format!("backend {}", semantic.backend)
            };
            checks.push(DoctorCheck {
                name: "semantic-lifecycle",
                status: CheckStatus::Pass,
                message: format!(
                    "ready; {} vectors, {}, last rebuilt {}",
                    semantic.vector_count,
                    backend_note,
                    semantic.last_rebuilt_at.as_deref().unwrap_or("unknown")
                ),
            });
        }
    }

    if index_path.exists() {
        if let Ok(store) = SqliteStore::open_existing(&index_path) {
            if let Ok(Some(manifest)) = store.manifest() {
                let quality = &manifest.quality;
                if quality.scip_indexes_imported > 0 && quality.scip_exact_references > 0 {
                    checks.push(DoctorCheck {
                        name: "quality",
                        status: CheckStatus::Pass,
                        message: format!(
                            "SCIP imported {} index(es), {} exact references, {} tests",
                            quality.scip_indexes_imported,
                            quality.scip_exact_references,
                            quality.test_count
                        ),
                    });
                } else {
                    checks.push(DoctorCheck {
                        name: "quality",
                        status: CheckStatus::Warn,
                        message: format!(
                            "SCIP exact references unavailable; {} tests, {} imports indexed",
                            quality.test_count, quality.import_count
                        ),
                    });
                    next_steps.push(
                    "For better references, impact, tests, and planning: run `ok scip setup .`, then `ok index . --with-scip auto`.".into(),
                );
                }
                let (check, step) = coverage_check(quality.coverage.as_ref(), manifest.index_mode);
                checks.push(check);
                next_steps.extend(step);
                let (check, step) = redaction_check(quality);
                checks.push(check);
                next_steps.extend(step);
                if let Some(snapshot) = &manifest.snapshot {
                    let (check, step) = snapshot_check(snapshot);
                    checks.push(check);
                    next_steps.extend(step);
                }
                coverage = quality.coverage.clone();
            }
        }
    }

    // 4. ok.toml
    let config_path = repo.join("ok.toml");
    if config_path.exists() {
        match OkConfig::load_from_repo(&repo) {
            Ok(_) => checks.push(DoctorCheck {
                name: "config",
                status: CheckStatus::Pass,
                message: format!("loaded {}", config_path.display()),
            }),
            Err(err) => {
                checks.push(DoctorCheck {
                    name: "config",
                    status: CheckStatus::Fail,
                    message: err.to_string(),
                });
                next_steps.push("Fix ok.toml or regenerate it with `ok init .`.".into());
            }
        }
    } else {
        checks.push(DoctorCheck {
            name: "config",
            status: CheckStatus::Warn,
            message: "ok.toml is missing; defaults will be used".into(),
        });
        next_steps.push("Run `ok init .` to create ok.toml.".into());
    }

    // 5. Tree-sitter grammars
    let mut detected_languages = Vec::new();
    let walker = walkdir::WalkDir::new(&repo).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        name != ".ok" && name != "node_modules" && name != "target"
    });
    for entry in walker.filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                match ext {
                    "rs" => detected_languages.push("Rust"),
                    "ts" | "tsx" => detected_languages.push("TypeScript"),
                    "py" => detected_languages.push("Python"),
                    "go" => detected_languages.push("Go"),
                    "java" => detected_languages.push("Java"),
                    "js" | "jsx" => detected_languages.push("JavaScript"),
                    _ => {}
                }
            }
        }
    }
    detected_languages.sort();
    detected_languages.dedup();
    if detected_languages.is_empty() {
        checks.push(DoctorCheck {
            name: "grammars",
            status: CheckStatus::Pass,
            message: "no known source files detected".into(),
        });
    } else {
        checks.push(DoctorCheck {
            name: "grammars",
            status: CheckStatus::Pass,
            message: format!("parsers available for {}", detected_languages.join(", ")),
        });
    }

    // 6. MCP server check
    if let Ok(exe) = std::env::current_exe() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let child = Command::new(&exe)
            .args(["mcp", "serve", "--repo", repo.to_str().unwrap_or(".")])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        if let Ok(mut child_proc) = child {
            let request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
            if let Some(mut stdin) = child_proc.stdin.take() {
                let _ = writeln!(stdin, "{}", request);
            }
            let mut result_buf = String::new();
            if let Some(stdout) = child_proc.stdout.take() {
                use std::io::BufRead;
                let mut reader = std::io::BufReader::new(stdout);
                let _ = reader.read_line(&mut result_buf);
            }
            let _ = child_proc.kill();
            let _ = child_proc.wait();

            if result_buf.contains("\"name\":\"open-kioku\"") {
                checks.push(DoctorCheck {
                    name: "mcp",
                    status: CheckStatus::Pass,
                    message: "server responded to initialize request".into(),
                });
            } else {
                checks.push(DoctorCheck {
                    name: "mcp",
                    status: CheckStatus::Fail,
                    message: "server failed to respond correctly".into(),
                });
            }
        } else {
            checks.push(DoctorCheck {
                name: "mcp",
                status: CheckStatus::Fail,
                message: "failed to spawn mcp server process".into(),
            });
        }
    } else {
        checks.push(DoctorCheck {
            name: "mcp",
            status: CheckStatus::Warn,
            message: "could not determine current executable to test server".into(),
        });
    }

    next_steps.dedup();
    let ok = checks
        .iter()
        .all(|check| !matches!(check.status, CheckStatus::Fail));
    DoctorReport {
        ok,
        repo,
        checks,
        coverage,
        next_steps,
    }
}

/// The redaction count as `ok index`, `ok status`, and `ok doctor` print it.
fn redaction_summary(quality: &open_kioku_core::IndexQuality) -> String {
    let state = match quality.redacted_files {
        // Not "none present": the rules do not cover a bare token in prose, a number under a
        // key that reads as a quantity, or a format the index does not read.
        Some(0) => "no value matched the redaction rules in the indexed data, config, or prose files (see docs/security-model.md for what the rules cover)".to_string(),
        Some(count) => format!(
            "{} data, config, or prose file(s) indexed with secret-like values replaced by [REDACTED]",
            group_thousands(count)
        ),
        None => "not recorded; this index predates secret-value redaction, so its data, config, and prose files were stored as read".to_string(),
    };
    if quality.pending_pre_redaction_compaction {
        return format!(
            "{state}; clearing the bytes an earlier index stored as read is still outstanding, so they may remain in the database, its write-ahead log, and the semantic vector store"
        );
    }
    state
}

/// Redaction sits beside coverage. An index written before redaction existed, or one whose
/// clearing of pre-redaction bytes has not finished, warns rather than fails: it holds values
/// as read, and running the index again is the fix.
/// One line for an index `ok snapshot import` published: the revision it describes and what
/// the local policy removed from it.
fn snapshot_provenance_summary(snapshot: &open_kioku_core::SnapshotProvenance) -> String {
    use open_kioku_core::SnapshotRevisionRelation as Relation;
    let count = |value: Option<usize>| value.map_or_else(|| "unknown".into(), |n| n.to_string());
    let revision = match snapshot.relation {
        Relation::SameCommit => "the checked-out commit".to_string(),
        Relation::Related => format!(
            "{} commit(s) behind and {} ahead of the local HEAD",
            count(snapshot.commits_behind),
            count(snapshot.commits_ahead)
        ),
        Relation::Foreign => "a revision not verified against this checkout".to_string(),
    };
    format!(
        "imported from {} ({revision}); {} changed tracked file(s) at import; {} path(s) removed by local index policy",
        snapshot.imported_from_commit,
        count(snapshot.changed_files),
        snapshot.policy_filtered
    )
}

/// An imported index that does not describe the checkout is a warning: every answer is still
/// served, with the caveat, until `ok index` rebuilds it from source.
fn snapshot_check(snapshot: &open_kioku_core::SnapshotProvenance) -> (DoctorCheck, Option<String>) {
    let caveat = snapshot.caveat();
    let step = caveat.is_some().then(|| {
        "Snapshot: run `ok index .` so the index describes this checkout rather than the revision the snapshot was exported from.".to_string()
    });
    (
        DoctorCheck {
            name: "snapshot",
            status: if caveat.is_some() {
                CheckStatus::Warn
            } else {
                CheckStatus::Pass
            },
            message: snapshot_provenance_summary(snapshot),
        },
        step,
    )
}

fn redaction_check(quality: &open_kioku_core::IndexQuality) -> (DoctorCheck, Option<String>) {
    let outstanding =
        quality.redacted_files.is_none() || quality.pending_pre_redaction_compaction;
    let step = outstanding.then(|| {
        "Redaction: run `ok index .` so data, config, and prose files are stored with secret-like values replaced by [REDACTED], and the bytes an earlier index stored as read are cleared from the database and the semantic vector store.".to_string()
    });
    (
        DoctorCheck {
            name: "redaction",
            status: if outstanding {
                CheckStatus::Warn
            } else {
                CheckStatus::Pass
            },
            message: redaction_summary(quality),
        },
        step,
    )
}

/// Coverage is a warning, never a failure: a low ratio is a fact about the repository
/// that the reader must see, not a broken installation.
fn coverage_check(
    coverage: Option<&IndexCoverage>,
    index_mode: IndexMode,
) -> (DoctorCheck, Option<String>) {
    let Some(coverage) = coverage else {
        // Cross-project mode links already-indexed projects and parses no source, so
        // there is nothing to record and re-indexing would not change that.
        if index_mode == IndexMode::CrossProject {
            return (
                DoctorCheck {
                    name: "coverage",
                    status: CheckStatus::Warn,
                    message:
                        "not applicable in cross-project mode; see each linked project's own index"
                            .into(),
                },
                None,
            );
        }
        return (
            DoctorCheck {
                name: "coverage",
                status: CheckStatus::Warn,
                message: "not recorded by this index; what discovery skipped is unknown".into(),
            },
            Some("Run `ok index .` to record which source files the index skipped and why.".into()),
        );
    };
    if coverage.discovered == 0 {
        return (
            DoctorCheck {
                name: "coverage",
                status: CheckStatus::Warn,
                message: "no source files discovered".into(),
            },
            None,
        );
    }
    let mut message = coverage.headline();
    // The headline ends with the policy count exactly when there is detail to add.
    if let Some(detail) = coverage.policy_exclusion_detail() {
        message.push_str(&format!(" ({detail})"));
    }
    let top = coverage.top_skip_reasons(3);
    if !top.is_empty() {
        message.push_str("; top skip reasons: ");
        message.push_str(&format_skip_reasons(&top));
    }
    for caveat in coverage.blind_spot_caveats() {
        message.push_str("; ");
        message.push_str(&caveat);
    }
    // Discovery found source and policy set every file of it aside (a `.gitignore`d
    // `src/`, a tree under a hidden directory): the ratio has no verdict, and the
    // reader needs the setting that emptied it, not "no source files discovered".
    let (programming_discovered, _) = coverage.programming_policy_totals();
    if coverage.percent().is_none()
        || (programming_discovered > 0 && coverage.programming_percent().is_none())
    {
        return (
            DoctorCheck {
                name: "coverage",
                status: CheckStatus::Warn,
                message,
            },
            Some(policy_exclusion_next_step(coverage)),
        );
    }
    let low = coverage.languages_below_warn_threshold();
    // Git ignore rules (`.gitignore`, `.git/info/exclude`, `core.excludesFile`) are
    // written for git, not for this index, so a language they mostly set aside is a
    // verdict the reader must see even at 100% of what remains. `hidden`, `vendor`,
    // `fast_mode`, `denied`, `[index] exclude` and `.okignore` are this tool's own
    // settings and stay in the detail. With the default `[security] allow_hidden_files =
    // false`, ingest checks `hidden` first, so git-ignored worktrees under `.claude/`
    // count as hidden; with it on they count here, and `[index] exclude`, checked before
    // the git rules, is how an intended exclusion is marked.
    let git_ignored = coverage.languages_mostly_excluded_by(open_kioku_core::SkipSource::GitIgnore);
    let judged_omission =
        coverage.below_warn_threshold() || !low.is_empty() || coverage.walk_errors > 0;
    if judged_omission || !git_ignored.is_empty() {
        if !git_ignored.is_empty() {
            message.push_str(&format!(
                "; mostly git-ignored: {}",
                git_ignored
                    .iter()
                    .take(3)
                    .map(|(language, excluded, considered)| format!(
                        "{language} ({} ignored, {} considered)",
                        group_thousands(*excluded),
                        group_thousands(*considered)
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            if git_ignored.len() > 3 {
                message.push_str(&format!(
                    " and {} more (see the table)",
                    git_ignored.len() - 3
                ));
            }
        }
        // A judged omission keeps its own advice; the git-ignored languages are still
        // named in the message.
        let step = match git_ignored.first() {
            Some(&(language, excluded, considered)) if !judged_omission => {
                git_ignore_next_step(language, excluded, considered, git_ignored.len() - 1)
            }
            _ => coverage_next_step(coverage),
        };
        if !low.is_empty() {
            message.push_str(&format!(
                "; under {INDEX_COVERAGE_WARN_PERCENT:.0}%: {}",
                low.iter()
                    .take(3)
                    .map(|(language, percent, _)| format!("{language} ({percent:.1}%)"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            if low.len() > 3 {
                message.push_str(&format!(" and {} more (see the table)", low.len() - 3));
            }
        }
        return (
            DoctorCheck {
                name: "coverage",
                status: CheckStatus::Warn,
                message,
            },
            Some(step),
        );
    }
    (
        DoctorCheck {
            name: "coverage",
            status: CheckStatus::Pass,
            message,
        },
        None,
    )
}

/// A language git ignore rules mostly set aside: the ratio over what remains can read 100%
/// while most of that language's source is absent from the index. `git check-ignore`
/// applies every rule file git reads, so the step cannot name one file.
fn git_ignore_next_step(language: &str, excluded: usize, considered: usize, more: usize) -> String {
    let mut step = format!(
        "Coverage: git ignore rules (`.gitignore`, `.git/info/exclude`, or `core.excludesFile`) govern the largest share of {language} source ({} git-ignored, {} considered); the index follows them, so an absence among those files is not evidence. Remove the rule if they are source an agent should see, or list the paths under `[index] exclude` if the exclusion is intended, then run `ok index .`.",
        group_thousands(excluded),
        group_thousands(considered)
    );
    if more > 0 {
        step.push_str(&format!(
            " {more} more programming language(s) are mostly git-ignored; see the table."
        ));
    }
    step
}

/// Every discovered source file was set aside by policy: name the setting that did it.
fn policy_exclusion_next_step(coverage: &IndexCoverage) -> String {
    match coverage.dominant_policy_source() {
        Some((source, count)) => match source.governing_setting() {
            Some(setting) => format!(
                "Coverage: every source file discovery found was excluded by policy ({} by {}); {setting} governs that. Change it if those files should be indexed, then run `ok index .`.",
                group_thousands(count),
                source.label()
            ),
            None => format!(
                "Coverage: every source file discovery found was excluded by policy ({} by {}); no ok.toml key governs that source, so review `ok --json status` `quality.skipped_paths`.",
                group_thousands(count),
                source.label()
            ),
        },
        None => "Coverage: every source file discovery found was excluded by policy; the index predates source recording, so run `ok index .` to see which rule.".into(),
    }
}

/// The advice for a coverage warning names the key that governs the dominant omission
/// when one exists. Only `too-large` has one among the reasons the ratio is judged on:
/// `binary`, `error`, and `unsupported-language` are facts about the files, and policy
/// exclusions (`hidden`, `ignored`, ...) are outside the ratio and name their own
/// setting in the check message.
fn coverage_next_step(coverage: &IndexCoverage) -> String {
    match coverage.top_skip_reasons(1).first() {
        Some((open_kioku_core::SkipReason::TooLarge, count)) => format!(
            "Coverage: {} source file(s) were skipped as too-large; `[index] max_file_size` governs that. Raise it if the omissions are unintended, then run `ok index .`.",
            group_thousands(*count)
        ),
        Some((reason, count)) => format!(
            "Coverage: {} source file(s) were skipped as {}; no ok.toml key governs that reason, so review the files under `ok --json status` `coverage.by_language` and `quality.skipped_paths`.",
            group_thousands(*count),
            reason.label()
        ),
        None => "Coverage: review `ok --json status` `coverage.by_language`; the missing files were not attributed to a skip reason, so check the walk errors and per-language counts.".into(),
    }
}

/// What the ratio set aside, after the table: the policy count by reason, where the
/// files live, and which setting governs the largest share.
fn coverage_policy_lines(coverage: &IndexCoverage) -> Vec<String> {
    let excluded = coverage.excluded_by_policy();
    if excluded == 0 {
        return Vec::new();
    }
    let mut lines = vec![format!(
        "\nExcluded by policy: {} ({})",
        group_thousands(excluded),
        format_skip_reasons(&coverage.policy_skip_reasons())
    )];
    let dirs = coverage.top_policy_excluded_dirs(3);
    if !dirs.is_empty() {
        lines.push(format!(
            "  top directories: {}",
            dirs.iter()
                .map(|(dir, count)| format!("{} under {dir}/", group_thousands(*count)))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some((source, count)) = coverage.dominant_policy_source() {
        lines.push(match source.governing_setting() {
            Some(setting) => format!(
                "  governing setting: {setting} ({} files, {})",
                group_thousands(count),
                source.label()
            ),
            None => format!(
                "  largest share: {} files ({}); no ok.toml key governs it",
                group_thousands(count),
                source.label()
            ),
        });
    }
    lines
}

/// Fixed-width rows for the terminal; the same numbers `ok doctor --json` carries.
fn coverage_table_lines(coverage: &IndexCoverage) -> Vec<String> {
    let mut lines = vec![format!(
        "{:<12} {:>10} {:>9} {:>9} {:>8} {:>9}  {}",
        "language", "discovered", "excluded", "indexed", "coverage", "generated", "skipped"
    )];
    for (language, entry) in &coverage.by_language {
        let percent = entry
            .percent()
            .map(|percent| format!("{percent:.1}%"))
            .unwrap_or_else(|| "-".into());
        let flag = if entry
            .percent()
            .is_some_and(|percent| percent < INDEX_COVERAGE_WARN_PERCENT)
        {
            "!"
        } else {
            ""
        };
        let mut skipped = entry
            .skipped
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(reason, count)| (*reason, *count))
            .collect::<Vec<_>>();
        skipped.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let skipped = if skipped.is_empty() {
            "-".to_string()
        } else {
            format_skip_reasons(&skipped)
        };
        lines.push(format!(
            "{:<12} {:>10} {:>9} {:>9} {:>7}{flag:<1} {:>9}  {skipped}",
            language,
            group_thousands(entry.discovered),
            group_thousands(entry.excluded_by_policy()),
            group_thousands(entry.indexed),
            percent,
            group_thousands(entry.generated),
        ));
    }
    lines.push(format!(
        "{:<12} {:>10} {:>9} {:>9} {:>8} {:>9}  {}",
        "total",
        group_thousands(coverage.discovered),
        group_thousands(coverage.excluded_by_policy()),
        group_thousands(coverage.indexed),
        coverage
            .percent()
            .map(|percent| format!("{percent:.1}%"))
            .unwrap_or_else(|| "-".into()),
        group_thousands(coverage.generated),
        {
            let mut all = coverage.top_skip_reasons(usize::MAX);
            all.extend(coverage.policy_skip_reasons());
            if all.is_empty() {
                "-".to_string()
            } else {
                format_skip_reasons(&all)
            }
        }
    ));
    lines
}

fn scip_setup_report(repo: &Path, config: &OkConfig) -> ScipSetupReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let ts_args = if repo.join("tsconfig.json").exists() || repo.join("jsconfig.json").exists() {
        "scip-typescript index --output .ok/indexes/typescript.scip"
    } else {
        "scip-typescript index --output .ok/indexes/typescript.scip --infer-tsconfig"
    };
    let indexers = vec![
        ScipIndexerReport {
            language: "typescript/javascript",
            applicable: repo.join("package.json").exists(),
            installed: command_exists("scip-typescript"),
            command: ts_args.into(),
            output_path: ".ok/indexes/typescript.scip".into(),
            note: "best for TypeScript and JavaScript symbol references".into(),
        },
        ScipIndexerReport {
            language: "go",
            applicable: repo.join("go.mod").exists(),
            installed: command_exists("scip-go"),
            command: "scip-go".into(),
            output_path: "index.scip".into(),
            note: "best for Go definition/reference precision".into(),
        },
        ScipIndexerReport {
            language: "java",
            applicable: repo.join("pom.xml").exists()
                || repo.join("settings.gradle").exists()
                || repo.join("settings.gradle.kts").exists()
                || repo.join("gradlew").exists()
                || repo.join("build.gradle").exists()
                || repo.join("build.gradle.kts").exists(),
            installed: command_exists("scip-java"),
            command: "scip-java index".into(),
            output_path: "index.scip".into(),
            note: "runs Maven or Gradle analysis and may clean compiler caches".into(),
        },
        ScipIndexerReport {
            language: "python",
            applicable: repo.join("pyproject.toml").exists() || repo.join("setup.py").exists(),
            installed: command_exists("scip-python"),
            command: "scip-python index . --project-name <repo> --project-version _ --output .ok/indexes/python.scip".into(),
            output_path: ".ok/indexes/python.scip".into(),
            note: "requires project metadata for best external reference stability".into(),
        },
    ];
    ScipSetupReport {
        repo,
        mode: format!("{:?}", config.scip.mode).to_ascii_lowercase(),
        enabled: config.scip.enabled,
        allow_install: config.scip.allow_install,
        timeout_seconds: config.scip.timeout_seconds,
        indexers,
        configured_paths: config.scip.paths.clone(),
    }
}

#[cfg(test)]
mod scip_setup_tests {
    use super::*;

    #[test]
    fn scip_setup_reports_the_java_default_for_a_gradle_settings_root() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("settings.gradle"),
            "rootProject.name = 'proof'\n",
        )
        .unwrap();

        let report = scip_setup_report(temp.path(), &OkConfig::default());
        let java = report
            .indexers
            .iter()
            .find(|indexer| indexer.language == "java")
            .expect("Java SCIP entry must be present");

        assert!(java.applicable);
        assert_eq!(java.command, "scip-java index");
        assert_eq!(java.output_path, PathBuf::from("index.scip"));
    }
}

fn print_scip_setup_report(report: &ScipSetupReport) {
    println!("SCIP setup for {}", report.repo.display());
    println!(
        "mode={}, enabled={}, timeout={}s",
        report.mode, report.enabled, report.timeout_seconds
    );
    println!("\nConfigured SCIP paths:");
    for path in &report.configured_paths {
        println!("- {}", path.display());
    }
    println!("\nIndexers:");
    for indexer in &report.indexers {
        let applicability = if indexer.applicable {
            "applicable"
        } else {
            "not detected"
        };
        let installed = if indexer.installed {
            "installed"
        } else {
            "missing"
        };
        println!(
            "- {}: {}, {}; {}",
            indexer.language, applicability, installed, indexer.note
        );
        if indexer.applicable {
            println!("  {}", indexer.command);
        }
    }
}

fn command_exists(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(binary))
                .any(|path| path.is_file())
        })
        .unwrap_or(false)
}

fn demo_repo_path(path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    match path {
        Some(path) => absolutize(&path),
        None => Ok(std::env::current_dir()?.join("open-kioku-demo")),
    }
}

fn build_demo_repo(repo: &Path, force: bool) -> anyhow::Result<DemoReport> {
    if repo.exists() {
        if !force {
            anyhow::bail!(
                "{} already exists; pass --force to replace the demo repo",
                repo.display()
            );
        }
        fs::remove_dir_all(repo)?;
    }

    fs::create_dir_all(repo.join("src"))?;
    fs::create_dir_all(repo.join("tests"))?;
    fs::write(
        repo.join("README.md"),
        "# Open Kioku Demo: expired sessions\n\nTry `ok preflight \"change token expiration\"`. The safe path begins in `src/auth.rs` and must preserve the login flow in `src/lib.rs` plus `tests/auth_flow.rs`; changing only the handler is the tempting but incomplete edit.\n",
    )?;
    fs::write(
        repo.join("Cargo.toml"),
        r#"[package]
name = "open-kioku-demo"
version = "0.1.0"
edition = "2021"
"#,
    )?;
    fs::write(
        repo.join("src/lib.rs"),
        r#"pub mod auth;

pub struct RequestContext {
    pub user_id: String,
}

pub fn handle_login(user_id: &str) -> String {
    let context = RequestContext {
        user_id: user_id.to_string(),
    };
    auth::issue_token(&context, 3600)
}
"#,
    )?;
    fs::write(
        repo.join("src/auth.rs"),
        r#"use crate::RequestContext;

pub fn issue_token(context: &RequestContext, ttl_seconds: u64) -> String {
    format!("token:{}:{}", context.user_id, ttl_seconds)
}

pub fn validate_token(token: &str) -> bool {
    token.starts_with("token:")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RequestContext;

    #[test]
    fn issues_token_with_user_id() {
        let context = RequestContext {
            user_id: "demo-user".into(),
        };
        assert!(issue_token(&context, 60).contains("demo-user"));
    }
}
"#,
    )?;
    fs::write(
        repo.join("tests/auth_flow.rs"),
        r#"use open_kioku_demo::{auth, handle_login};

#[test]
fn login_returns_valid_token() {
    let token = handle_login("demo-user");
    assert!(auth::validate_token(&token));
}
"#,
    )?;
    let config_path = repo.join("ok.toml");
    OkConfig::write_default(&config_path)?;
    let config = fs::read_to_string(&config_path)?;
    fs::write(
        &config_path,
        config.replacen(
            "[scip]\nenabled = true\nmode = \"consume\"",
            "[scip]\nenabled = false\nmode = \"off\"",
            1,
        ),
    )?;

    let snapshot = index_repo(repo)?;
    let repo_display = repo.display().to_string();
    Ok(DemoReport {
        repo: repo.to_path_buf(),
        file_count: snapshot.manifest.file_count,
        symbol_count: snapshot.manifest.symbol_count,
        chunk_count: snapshot.manifest.chunk_count,
        commands: vec![
            format!("ok --repo {repo_display} search token"),
            format!("ok --repo {repo_display} symbol find issue_token"),
            format!("ok --repo {repo_display} impact --file src/auth.rs"),
            format!("ok --repo {repo_display} context token --format markdown"),
            format!(
                "ok --repo {repo_display} preflight \"change token expiration\" --format markdown"
            ),
            format!("ok prove {repo_display} --task token"),
            format!("ok mcp install claude --repo {repo_display}"),
        ],
    })
}

#[cfg(test)]
mod tests_provider_tests {
    use super::*;
    use open_kioku_core::TestExclusionReason;
    use std::collections::BTreeMap;

    const ADVICE: &str = "Index test files before relying on validation recommendations.";

    /// Three states, three answers: nothing indexed, everything skipped, and an index too old
    /// to say which. Only the first may advise indexing test files without qualification.
    #[test]
    fn the_tests_next_step_tells_absent_skipped_and_unrecorded_apart() {
        assert_eq!(
            tests_next_step(0, &Some(BTreeMap::new()), false, ADVICE).as_deref(),
            Some(ADVICE)
        );

        let skipped = Some(BTreeMap::from([(TestExclusionReason::Disabled, 3)]));
        let step = tests_next_step(0, &skipped, false, ADVICE).unwrap();
        assert!(step.starts_with("Enable the skipped tests"), "{step}");
        assert!(!step.contains("Index test files"), "{step}");

        let step = tests_next_step(0, &None, false, ADVICE).unwrap();
        assert!(
            step.contains("re-index to tell skipped tests from absent ones"),
            "{step}"
        );

        assert_eq!(tests_next_step(2, &skipped, false, ADVICE), None);
        assert_eq!(tests_next_step(2, &None, false, ADVICE), None);
    }

    /// Cross-project mode leaves the count unrecorded because it reads no source; advising a
    /// re-index there would send the reader round in a circle.
    #[test]
    fn the_tests_next_step_does_not_advise_reindexing_a_cross_project_index() {
        let step = tests_next_step(0, &None, true, ADVICE).unwrap();
        assert!(step.contains("Cross-project mode reads no source"), "{step}");
        assert!(!step.contains("re-index"), "{step}");
        assert!(!step.contains("Index test files"), "{step}");
    }

    #[test]
    fn the_tests_evidence_names_what_was_withheld() {
        assert_eq!(
            tests_evidence(0, &BTreeMap::new()),
            "0 indexed test target(s)"
        );
        assert_eq!(
            tests_evidence(1, &BTreeMap::from([(TestExclusionReason::Disabled, 2)])),
            "1 runnable indexed test target(s); excluded: 2 disabled test the runner skips"
        );
    }
}
