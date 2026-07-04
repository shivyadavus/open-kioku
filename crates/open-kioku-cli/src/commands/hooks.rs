const HOOK_SCHEMA_VERSION: u32 = 1;
const HOOK_MAX_DEADLINE_MS: u64 = 2_000;
const HOOK_MANIFEST_REL: &str = ".open-kioku/agent-hooks.toml";
const HOOK_BEGIN: &str = "<!-- OPEN-KIOKU-HOOKS:BEGIN -->";
const HOOK_END: &str = "<!-- OPEN-KIOKU-HOOKS:END -->";

#[derive(Clone, Copy)]
struct HookTarget {
    name: &'static str,
    rel_path: &'static str,
    note: &'static str,
}

fn install_hooks(
    repo: &Path,
    mode: HookMode,
    dry_run: bool,
    deadline_ms: u64,
) -> anyhow::Result<HookInstallReport> {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    validate_hook_deadline(deadline_ms)?;
    let (policy_gated, policy_warnings) = hook_policy_gate(&repo);
    if mode == HookMode::Enforce && !policy_gated {
        anyhow::bail!(
            "enforce hooks require policy-gated ok.toml: security.allow_write=false, security.deny_network=true, security.approval_required=true"
        );
    }
    let mut warnings = if mode == HookMode::Enforce {
        policy_warnings
    } else {
        Vec::new()
    };

    let targets = hook_targets();
    let manifest_path = repo.join(HOOK_MANIFEST_REL);
    let existing_manifest = read_hook_manifest(&manifest_path).ok().flatten();
    let managed_files: Vec<String> = std::iter::once(HOOK_MANIFEST_REL.to_string())
        .chain(targets.iter().map(|target| target.rel_path.to_string()))
        .collect();
    let generated_at = existing_manifest
        .as_ref()
        .filter(|manifest| {
            manifest.mode == mode
                && manifest.deadline_ms == deadline_ms
                && manifest.managed_files == managed_files
        })
        .map(|manifest| manifest.generated_at.clone())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let manifest = HookManifest {
        schema_version: HOOK_SCHEMA_VERSION,
        mode,
        deadline_ms,
        fail_open: mode != HookMode::Enforce,
        enforce_fail_closed: mode == HookMode::Enforce,
        policy_gated,
        generated_at,
        managed_files,
    };

    let mut changed_files = Vec::new();
    let mut unchanged_files = Vec::new();
    let removed_files = Vec::new();

    let manifest_content = toml::to_string_pretty(&manifest)?;
    stage_file_content(
        &manifest_path,
        &manifest_content,
        dry_run,
        &mut changed_files,
        &mut unchanged_files,
    )?;

    let block = render_hook_instruction_block(mode, deadline_ms);
    for target in targets {
        let path = repo.join(target.rel_path);
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let next = upsert_marker_block(&existing, &block);
        stage_file_content(
            &path,
            &next,
            dry_run,
            &mut changed_files,
            &mut unchanged_files,
        )?;
    }

    if mode == HookMode::Enforce {
        warnings.push(
            "enforce mode is explicit and reversible; read-only operations must continue to fail open"
                .into(),
        );
    }

    Ok(HookInstallReport {
        ok: true,
        repo,
        mode,
        dry_run,
        deadline_ms,
        changed_files,
        unchanged_files,
        removed_files,
        warnings,
    })
}

fn uninstall_hooks(repo: &Path, dry_run: bool) -> anyhow::Result<HookInstallReport> {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let mut changed_files = Vec::new();
    let mut unchanged_files = Vec::new();
    let mut removed_files = Vec::new();
    let mut warnings = Vec::new();

    let manifest_path = repo.join(HOOK_MANIFEST_REL);
    let manifest = read_hook_manifest(&manifest_path).ok().flatten();
    let mut rel_paths: BTreeSet<String> = hook_targets()
        .iter()
        .map(|target| target.rel_path.to_string())
        .collect();
    if let Some(manifest) = &manifest {
        rel_paths.extend(
            manifest
                .managed_files
                .iter()
                .filter(|path| path.as_str() != HOOK_MANIFEST_REL)
                .cloned(),
        );
    }

    for rel_path in rel_paths {
        let path = repo.join(&rel_path);
        if !path.exists() {
            unchanged_files.push(path);
            continue;
        }
        let existing = fs::read_to_string(&path)?;
        let Some(next) = remove_marker_block(&existing) else {
            warnings.push(format!(
                "{} has no Open Kioku marker block; preserving user content",
                path.display()
            ));
            unchanged_files.push(path);
            continue;
        };
        if next.trim().is_empty() {
            if !dry_run {
                fs::remove_file(&path)?;
            }
            removed_files.push(path);
        } else {
            stage_file_content(
                &path,
                &next,
                dry_run,
                &mut changed_files,
                &mut unchanged_files,
            )?;
        }
    }

    if manifest_path.exists() {
        if !dry_run {
            fs::remove_file(&manifest_path)?;
        }
        removed_files.push(manifest_path);
    } else {
        unchanged_files.push(manifest_path);
    }

    Ok(HookInstallReport {
        ok: true,
        repo,
        mode: manifest
            .as_ref()
            .map(|manifest| manifest.mode)
            .unwrap_or(HookMode::Advisory),
        dry_run,
        deadline_ms: manifest
            .as_ref()
            .map(|manifest| manifest.deadline_ms)
            .unwrap_or(750),
        changed_files,
        unchanged_files,
        removed_files,
        warnings,
    })
}

fn hook_doctor_report(repo: &Path) -> HookDoctorReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let manifest_path = repo.join(HOOK_MANIFEST_REL);
    let manifest = read_hook_manifest(&manifest_path).ok().flatten();
    let mut checks = Vec::new();
    let mut next_steps = Vec::new();
    let mut managed_files = Vec::new();

    match &manifest {
        Some(manifest) => {
            checks.push(DoctorCheck {
                name: "manifest",
                status: CheckStatus::Pass,
                message: format!(
                    "mode={}, deadline={}ms, fail_open={}",
                    manifest.mode, manifest.deadline_ms, manifest.fail_open
                ),
            });
            if manifest.schema_version == HOOK_SCHEMA_VERSION {
                checks.push(DoctorCheck {
                    name: "schema",
                    status: CheckStatus::Pass,
                    message: format!("hook schema v{}", manifest.schema_version),
                });
            } else {
                checks.push(DoctorCheck {
                    name: "schema",
                    status: CheckStatus::Warn,
                    message: format!(
                        "hook schema v{} is not the current v{}",
                        manifest.schema_version, HOOK_SCHEMA_VERSION
                    ),
                });
            }
            if manifest.deadline_ms == 0 || manifest.deadline_ms > HOOK_MAX_DEADLINE_MS {
                checks.push(DoctorCheck {
                    name: "deadline",
                    status: CheckStatus::Fail,
                    message: format!(
                        "deadline {}ms exceeds max {}ms",
                        manifest.deadline_ms, HOOK_MAX_DEADLINE_MS
                    ),
                });
            } else {
                checks.push(DoctorCheck {
                    name: "deadline",
                    status: CheckStatus::Pass,
                    message: format!("short hook deadline: {}ms", manifest.deadline_ms),
                });
            }
            let (policy_gated, _) = hook_policy_gate(&repo);
            if manifest.mode == HookMode::Enforce && !policy_gated {
                checks.push(DoctorCheck {
                    name: "policy",
                    status: CheckStatus::Fail,
                    message: "enforce mode requires secure ok.toml policy gate".into(),
                });
                next_steps.push(
                    "Run `ok init .` and keep security.allow_write=false, security.deny_network=true, approval_required=true before enforce mode.".into(),
                );
            } else if manifest.mode == HookMode::Enforce {
                checks.push(DoctorCheck {
                    name: "policy",
                    status: CheckStatus::Pass,
                    message: "enforce mode is policy-gated".into(),
                });
            }
            for rel_path in &manifest.managed_files {
                if rel_path == HOOK_MANIFEST_REL {
                    continue;
                }
                let path = repo.join(rel_path);
                managed_files.push(path.clone());
                match fs::read_to_string(&path) {
                    Ok(content) if has_marker_block(&content) => checks.push(DoctorCheck {
                        name: "managed-file",
                        status: CheckStatus::Pass,
                        message: format!("{} contains Open Kioku marker block", path.display()),
                    }),
                    Ok(_) => checks.push(DoctorCheck {
                        name: "managed-file",
                        status: CheckStatus::Warn,
                        message: format!("{} exists without marker block", path.display()),
                    }),
                    Err(_) => checks.push(DoctorCheck {
                        name: "managed-file",
                        status: CheckStatus::Warn,
                        message: format!("{} is missing", path.display()),
                    }),
                }
            }
        }
        None => {
            checks.push(DoctorCheck {
                name: "manifest",
                status: CheckStatus::Warn,
                message: format!("{} is missing", manifest_path.display()),
            });
            next_steps.push("Run `ok hooks install --mode advisory .`.".into());
        }
    }

    next_steps.sort();
    next_steps.dedup();
    let ok = checks
        .iter()
        .all(|check| !matches!(check.status, CheckStatus::Fail));
    HookDoctorReport {
        ok,
        repo,
        checks,
        manifest,
        managed_files,
        next_steps,
    }
}

fn agent_doctor_report(repo: &Path) -> AgentDoctorReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let mut checks = Vec::new();
    let mut next_steps = Vec::new();
    let surfaces: Vec<AgentSurfaceReport> = agent_surfaces()
        .into_iter()
        .map(|surface| {
            let path = repo.join(surface.rel_path);
            let present = path.exists();
            let managed = fs::read_to_string(&path)
                .map(|content| has_marker_block(&content))
                .unwrap_or(false);
            AgentSurfaceReport {
                name: surface.name,
                path,
                present,
                managed,
                note: surface.note,
            }
        })
        .collect();

    let present_count = surfaces.iter().filter(|surface| surface.present).count();
    let managed_count = surfaces.iter().filter(|surface| surface.managed).count();
    if present_count == 0 {
        checks.push(DoctorCheck {
            name: "agents",
            status: CheckStatus::Warn,
            message: "no local agent guidance surfaces found".into(),
        });
        next_steps.push("Run `ok hooks install --mode advisory .`.".into());
    } else {
        checks.push(DoctorCheck {
            name: "agents",
            status: CheckStatus::Pass,
            message: format!("{present_count} local agent guidance surface(s) found"),
        });
    }
    if managed_count == 0 {
        checks.push(DoctorCheck {
            name: "hooks",
            status: CheckStatus::Warn,
            message: "no Open Kioku managed hook marker blocks found".into(),
        });
        next_steps.push("Run `ok hooks install --mode advisory .`.".into());
    } else {
        checks.push(DoctorCheck {
            name: "hooks",
            status: CheckStatus::Pass,
            message: format!("{managed_count} Open Kioku managed hook surface(s) found"),
        });
    }

    next_steps.sort();
    next_steps.dedup();
    let ok = checks
        .iter()
        .all(|check| !matches!(check.status, CheckStatus::Fail));
    AgentDoctorReport {
        ok,
        repo,
        checks,
        surfaces,
        next_steps,
    }
}

fn print_hook_install_report(report: &HookInstallReport, verb: &str) {
    println!(
        "Open Kioku hooks {verb}: mode={}, deadline={}ms{}",
        report.mode,
        report.deadline_ms,
        if report.dry_run { " (dry run)" } else { "" }
    );
    print_path_group(
        if report.dry_run {
            "Would change"
        } else {
            "Changed"
        },
        &report.changed_files,
    );
    print_path_group("Unchanged", &report.unchanged_files);
    print_path_group(
        if report.dry_run {
            "Would remove"
        } else {
            "Removed"
        },
        &report.removed_files,
    );
    for warning in &report.warnings {
        println!("[warn] {warning}");
    }
}

fn print_hook_doctor_report(report: &HookDoctorReport) {
    println!("Open Kioku hooks doctor for {}", report.repo.display());
    for check in &report.checks {
        println!(
            "{:<6} {:<14} {}",
            check.status.marker(),
            check.name,
            check.message
        );
    }
    if !report.next_steps.is_empty() {
        println!("\nNext steps:");
        for step in &report.next_steps {
            println!("- {step}");
        }
    }
}

fn print_agent_doctor_report(report: &AgentDoctorReport) {
    println!("Open Kioku agents doctor for {}", report.repo.display());
    for check in &report.checks {
        println!(
            "{:<6} {:<10} {}",
            check.status.marker(),
            check.name,
            check.message
        );
    }
    println!("\nAgent surfaces:");
    for surface in &report.surfaces {
        let status = if surface.managed {
            "managed"
        } else if surface.present {
            "present"
        } else {
            "missing"
        };
        println!(
            "- {:<12} {:<8} {} ({})",
            surface.name,
            status,
            surface.path.display(),
            surface.note
        );
    }
    if !report.next_steps.is_empty() {
        println!("\nNext steps:");
        for step in &report.next_steps {
            println!("- {step}");
        }
    }
}

fn print_path_group(label: &str, paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }
    println!("{label}:");
    for path in paths {
        println!("- {}", path.display());
    }
}

fn hook_targets() -> [HookTarget; 3] {
    [
        HookTarget {
            name: "agents",
            rel_path: ".open-kioku/AGENTS.md",
            note: "repo-local Open Kioku agent guidance",
        },
        HookTarget {
            name: "cursor",
            rel_path: ".cursor/rules/open-kioku-hooks.mdc",
            note: "Cursor rules guidance",
        },
        HookTarget {
            name: "claude",
            rel_path: ".claude/CLAUDE.md",
            note: "Claude Code guidance",
        },
    ]
}

fn agent_surfaces() -> [HookTarget; 6] {
    [
        HookTarget {
            name: "agents",
            rel_path: "AGENTS.md",
            note: "generic agent instructions",
        },
        HookTarget {
            name: "ok-agents",
            rel_path: ".open-kioku/AGENTS.md",
            note: "Open Kioku managed guidance",
        },
        HookTarget {
            name: "cursor",
            rel_path: ".cursor/rules/open-kioku-hooks.mdc",
            note: "Cursor rules guidance",
        },
        HookTarget {
            name: "claude",
            rel_path: ".claude/CLAUDE.md",
            note: "Claude Code guidance",
        },
        HookTarget {
            name: "codex",
            rel_path: ".codex-plugin/plugin.json",
            note: "Codex plugin metadata",
        },
        HookTarget {
            name: "mcp",
            rel_path: ".cursor-plugin/plugin.json",
            note: "Cursor plugin metadata",
        },
    ]
}

fn validate_hook_deadline(deadline_ms: u64) -> anyhow::Result<()> {
    if deadline_ms == 0 {
        anyhow::bail!("hook deadline must be greater than 0ms");
    }
    if deadline_ms > HOOK_MAX_DEADLINE_MS {
        anyhow::bail!(
            "hook deadline {}ms exceeds the maximum short deadline of {}ms",
            deadline_ms,
            HOOK_MAX_DEADLINE_MS
        );
    }
    Ok(())
}

fn hook_policy_gate(repo: &Path) -> (bool, Vec<String>) {
    let config_path = repo.join("ok.toml");
    if !config_path.exists() {
        return (
            false,
            vec![format!(
                "explicit ok.toml policy gate is missing at {}",
                config_path.display()
            )],
        );
    }
    match OkConfig::load_from_repo(repo) {
        Ok(config) => {
            let gated = !config.security.allow_write
                && config.security.deny_network
                && config.security.approval_required;
            if gated {
                (true, Vec::new())
            } else {
                (
                    false,
                    vec![format!(
                        "ok.toml is not enforce-ready: allow_write={}, deny_network={}, approval_required={}",
                        config.security.allow_write,
                        config.security.deny_network,
                        config.security.approval_required
                    )],
                )
            }
        }
        Err(err) => (
            false,
            vec![format!("ok.toml policy gate unavailable: {err}")],
        ),
    }
}

fn render_hook_instruction_block(mode: HookMode, deadline_ms: u64) -> String {
    let behavior = match mode {
        HookMode::Advisory => {
            "Advisory: before editing, prefer Open Kioku evidence. If Open Kioku is unavailable, continue without blocking and mention the missing signal."
        }
        HookMode::Warn => {
            "Warn: before editing, warn when there is no fresh Open Kioku plan or change contract. Continue only after surfacing the warning."
        }
        HookMode::Enforce => {
            "Enforce: do not perform edit actions until a fresh Open Kioku plan or change contract exists. Read/search/status operations must never be blocked."
        }
    };
    format!(
        "{HOOK_BEGIN}\n\
         # Open Kioku Agent Hooks\n\n\
         Mode: `{mode}`\n\
         Deadline: `{deadline_ms}ms`\n\n\
         {behavior}\n\n\
         Required pre-edit routine:\n\
         1. Check `repo_status`.\n\
         2. Use `search_code`, `get_definition`, `get_references`, `impact_analysis`, and `find_tests_for_change` as relevant.\n\
         3. Build a plan with `plan_change` before editing.\n\
         4. Verify after editing with `verify_change` or a stored change contract.\n\n\
         Safety rules:\n\
         - Never block basic read operations.\n\
         - Keep hook output short.\n\
         - Fail open unless enforce mode explicitly applies to an edit action.\n\
         - Do not write source files without policy-backed evidence.\n\
         {HOOK_END}\n"
    )
}

fn stage_file_content(
    path: &Path,
    content: &str,
    dry_run: bool,
    changed_files: &mut Vec<PathBuf>,
    unchanged_files: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    if fs::read_to_string(path).ok().as_deref() == Some(content) {
        unchanged_files.push(path.to_path_buf());
        return Ok(());
    }
    if !dry_run {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
    }
    changed_files.push(path.to_path_buf());
    Ok(())
}

fn upsert_marker_block(existing: &str, block: &str) -> String {
    if let Some((start, end)) = marker_bounds(existing) {
        let mut next = String::new();
        next.push_str(&existing[..start]);
        next.push_str(block);
        next.push_str(&existing[end..]);
        return normalize_trailing_newline(next);
    }
    if existing.trim().is_empty() {
        return normalize_trailing_newline(block.to_string());
    }
    let mut next = existing.to_string();
    if !next.ends_with('\n') {
        next.push('\n');
    }
    next.push('\n');
    next.push_str(block);
    normalize_trailing_newline(next)
}

fn remove_marker_block(existing: &str) -> Option<String> {
    let (start, end) = marker_bounds(existing)?;
    let mut next = String::new();
    next.push_str(&existing[..start]);
    next.push_str(&existing[end..]);
    while next.contains("\n\n\n") {
        next = next.replace("\n\n\n", "\n\n");
    }
    Some(normalize_trailing_newline(next.trim_matches('\n').to_string()))
}

fn marker_bounds(existing: &str) -> Option<(usize, usize)> {
    let start = existing.find(HOOK_BEGIN)?;
    let end_start = existing[start..].find(HOOK_END)? + start;
    let mut end = end_start + HOOK_END.len();
    if existing[end..].starts_with('\n') {
        end += 1;
    }
    Some((start, end))
}

fn has_marker_block(content: &str) -> bool {
    marker_bounds(content).is_some()
}

fn normalize_trailing_newline(mut content: String) -> String {
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content
}

fn read_hook_manifest(path: &Path) -> anyhow::Result<Option<HookManifest>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)?;
    Ok(Some(toml::from_str(&raw)?))
}

#[cfg(test)]
mod hook_tests {
    use super::*;

    #[test]
    fn marker_block_replacement_preserves_user_content() {
        let original = "keep\n\n<!-- OPEN-KIOKU-HOOKS:BEGIN -->\nold\n<!-- OPEN-KIOKU-HOOKS:END -->\n\nalso keep\n";
        let replaced = upsert_marker_block(original, "BLOCK\n");
        assert!(replaced.contains("keep"));
        assert!(replaced.contains("also keep"));
        assert!(replaced.contains("BLOCK"));
        assert!(!replaced.contains("old"));
    }

    #[test]
    fn marker_block_removal_preserves_user_content() {
        let original = "keep\n\n<!-- OPEN-KIOKU-HOOKS:BEGIN -->\nold\n<!-- OPEN-KIOKU-HOOKS:END -->\n\nalso keep\n";
        let removed = remove_marker_block(original).unwrap();
        assert!(removed.contains("keep"));
        assert!(removed.contains("also keep"));
        assert!(!removed.contains("old"));
    }
}
