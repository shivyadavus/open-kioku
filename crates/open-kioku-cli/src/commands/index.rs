fn index_repo(repo: &Path) -> anyhow::Result<open_kioku_ingest::IndexSnapshot> {
    index_repo_with_config(repo, OkConfig::load_from_repo(repo)?, IndexMode::Full)
}

fn index_repo_with_scip_mode(
    repo: &Path,
    with_scip: Option<&str>,
    mode: IndexMode,
) -> anyhow::Result<open_kioku_ingest::IndexSnapshot> {
    let mut config = OkConfig::load_from_repo(repo)?;
    if let Some(mode) = with_scip {
        config.scip.enabled = mode != "off";
        config.scip.mode = parse_scip_mode(mode)?;
    }
    index_repo_with_config(repo, config, mode)
}

fn index_repo_with_config(
    repo: &Path,
    config: OkConfig,
    mode: IndexMode,
) -> anyhow::Result<open_kioku_ingest::IndexSnapshot> {
    let reporter = Arc::new(Mutex::new(IndexProgressReporter::new()));
    report_index_stage(
        &reporter,
        "lock",
        "waiting for exclusive index writer lock".to_string(),
    );
    let _lock = IndexWriteLock::acquire(repo, IndexWriteLock::DEFAULT_WAIT)?;
    report_index_stage(&reporter, "lock", "acquired index writer lock".to_string());
    // RI3.6: migrate legacy layouts into the generation layout exactly once, under the
    // write lock (a directory move, not a data copy). Readers resolve both layouts.
    if let Some(generation) = open_kioku_storage::generations::adopt_legacy_layout(repo)? {
        report_index_stage(
            &reporter,
            "generations",
            format!("adopted legacy index layout as generation {generation}"),
        );
    }
    let index_reporter = Arc::clone(&reporter);
    let (mut snapshot, history) = Indexer::default().index_repo_with_history_mode_and_progress(
        repo,
        &config,
        mode,
        move |progress| {
            report_index_progress(&index_reporter, progress);
        },
    )?;
    report_index_stage(
        &reporter,
        "store",
        format!(
            "writing {} files, {} symbols, {} chunks, {} document sections, {} occurrences, {} analysis facts",
            snapshot.files.len(),
            snapshot.symbols.len(),
            snapshot.chunks.len(),
            snapshot.document_sections.len(),
            snapshot.occurrences.len(),
            snapshot.analysis_facts.len()
        ),
    );
    let store = open_store_for_write(repo)?;
    // An index written before secret-value redaction stored data and config values as read.
    // Replacing its rows leaves those bytes in SQLite free pages and its write-ahead log, and
    // the semantic vector store holds the same text, so both are cleared after the new manifest
    // is published. Read before staging, which removes the previous manifest. The new manifest
    // carries the work as outstanding until it succeeds, so a blocked pass is retried by the
    // next run and `ok doctor` reports it meanwhile, instead of being silently forgotten.
    let compact_after_publish = match store.manifest() {
        Ok(previous) => previous.is_some_and(|previous| previous.needs_pre_redaction_compaction()),
        // Failing open here publishes `pending: false` over an index whose state is unknown and
        // never retries. The clearing is idempotent, so assuming it is owed costs one pass.
        Err(_) => true,
    };
    snapshot.manifest.quality.pending_pre_redaction_compaction = compact_after_publish;
    // The manifest is the publication marker, written last (below) so a concurrent reader
    // never opens one whose graph or search index is still being written.
    store.stage_index_with_documents(
        IndexData {
            manifest: &snapshot.manifest,
            files: &snapshot.files,
            symbols: &snapshot.symbols,
            chunks: &snapshot.chunks,
            tests: &snapshot.tests,
            imports: &snapshot.imports,
            occurrences: &snapshot.occurrences,
            analysis_facts: &snapshot.analysis_facts,
            scopes: &snapshot.scopes,
            bindings: &snapshot.bindings,
            call_sites: &snapshot.call_sites,
        },
        &snapshot.document_sections,
    )?;
    report_index_stage(
        &reporter,
        "history",
        format!(
            "writing {} commits, {} file touches, {} cochange edges",
            history.commits.len(),
            history.file_touches.len(),
            history.cochange_edges.len()
        ),
    );
    store.put_history_snapshot(&history)?;
    report_index_stage(&reporter, "graph", "building dependency graph".to_string());
    let graph = InMemoryGraph::from_index_with_resolved_relationships(
        &snapshot.files,
        &snapshot.symbols,
        &snapshot.chunks,
        &snapshot.occurrences,
        &snapshot.imports,
        &snapshot.analysis_facts,
        &snapshot.resolved_relationships,
    );
    report_index_stage(
        &reporter,
        "graph",
        format!(
            "writing {} graph nodes and {} graph edges",
            graph.nodes.len(),
            graph.edges.len()
        ),
    );
    // Move nodes out of the graph once; the previous per-call clones kept up to three copies of
    // the node set alive at the memory peak.
    let mut nodes = graph.nodes.into_values().collect::<Vec<_>>();
    // The graph holds nodes in a hash map whose order differs per process; search documents
    // are added in this order, so it is fixed to node ids.
    nodes.sort_unstable_by(|left, right| left.id.0.cmp(&right.id.0));
    let edges = graph.edges;
    store.replace_graph(&nodes, &edges)?;
    drop(edges);
    report_index_stage(
        &reporter,
        "search",
        format!(
            "rebuilding Tantivy index for {} chunks",
            snapshot.chunks.len()
        ),
    );
    rebuild_disk_index_with_graph(
        default_index_dir(repo),
        &snapshot.chunks,
        &snapshot.files,
        &snapshot.symbols,
        &nodes,
    )?;
    store.put_manifest(&snapshot.manifest)?;
    if compact_after_publish {
        report_index_stage(
            &reporter,
            "compact",
            "clearing bytes stored before secret-value redaction from the database and the \
             semantic vector store"
                .to_string(),
        );
        // The index itself is correct either way, so a reader holding the database never fails
        // this run. It does leave the work outstanding in the published manifest, which is what
        // makes the next run retry it.
        match compact_pre_redaction_bytes(repo, &store) {
            Ok(()) => {
                snapshot.manifest.quality.pending_pre_redaction_compaction = false;
                store.put_manifest(&snapshot.manifest)?;
            }
            Err(err) => report_index_stage(
                &reporter,
                "compact",
                format!(
                    "clearing bytes stored before secret-value redaction failed ({err}); the \
                     manifest records the work as outstanding, `ok doctor` reports it, and the \
                     next `ok index` retries it"
                ),
            ),
        }
    }
    // A database an earlier Open Kioku wrote deleted rows without zeroing them, so it can still
    // hold paths this or an earlier run removed, a newly denied one included (#553). Rewritten
    // once; an unreadable marker is treated as owing it. The index is correct either way, so a
    // failure is reported and left for the next run, which finds the marker still unset.
    if !store.deleted_content_zeroed().unwrap_or(false) {
        report_index_stage(
            &reporter,
            "compact",
            "rewriting the database once so rows earlier runs deleted, including those of \
             paths the index policy now excludes, are not left in its free space"
                .to_string(),
        );
        if let Err(err) = store.vacuum() {
            report_index_stage(
                &reporter,
                "compact",
                format!(
                    "rewriting the database failed ({err}); rows earlier runs deleted may \
                     remain in its free space until the next `ok index` retries it"
                ),
            );
        }
    }
    report_index_stage(&reporter, "complete", "index ready".to_string());
    Ok(snapshot)
}

/// Everything an index written before secret-value redaction left behind: the semantic vector
/// store, whose target text and embedding cache were built from unredacted chunks, and the
/// database's free pages and write-ahead log. Both, or the work stays outstanding.
fn compact_pre_redaction_bytes(
    repo: &Path,
    store: &open_kioku_storage_sqlite::SqliteStore,
) -> anyhow::Result<()> {
    open_kioku_semantic::discard_vector_store(repo)?;
    store.vacuum()?;
    Ok(())
}

fn parse_scip_mode(value: &str) -> anyhow::Result<ScipMode> {
    match value {
        "off" => Ok(ScipMode::Off),
        "consume" => Ok(ScipMode::Consume),
        "auto" => Ok(ScipMode::Auto),
        "required" => Ok(ScipMode::Required),
        other => anyhow::bail!("unsupported SCIP mode: {other}"),
    }
}

fn parse_index_mode(value: &str) -> anyhow::Result<IndexMode> {
    match value {
        "full" => Ok(IndexMode::Full),
        "balanced" => Ok(IndexMode::Balanced),
        "fast" => Ok(IndexMode::Fast),
        "cross-project" | "cross_project" => Ok(IndexMode::CrossProject),
        other => anyhow::bail!(
            "unsupported index mode: {other}; expected full, balanced, fast, or cross-project"
        ),
    }
}

struct IndexProgressReporter {
    started_at: Instant,
    last_emitted_at: Instant,
    last_phase: &'static str,
}

impl IndexProgressReporter {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started_at: now,
            last_emitted_at: now,
            last_phase: "",
        }
    }

    fn emit_progress(&mut self, progress: IndexProgress) {
        let now = Instant::now();
        let phase_changed = self.last_phase != progress.phase;
        let completed = progress
            .total_files
            .map(|total| progress.indexed_files == total)
            .unwrap_or(false);
        if !phase_changed
            && !completed
            && now.duration_since(self.last_emitted_at) < Duration::from_secs(2)
        {
            return;
        }
        self.last_phase = progress.phase;
        self.last_emitted_at = now;
        let elapsed = self.started_at.elapsed().as_secs_f64();
        match progress.total_files {
            Some(total) if total > 0 => {
                let percent = (progress.indexed_files as f64 / total as f64) * 100.0;
                eprintln!(
                    "index[{phase}] {indexed}/{total} files ({percent:.1}%), scanned={scanned}, elapsed={elapsed:.1}s",
                    phase = progress.phase,
                    indexed = progress.indexed_files,
                    scanned = progress.scanned_files,
                );
            }
            _ => {
                eprintln!(
                    "index[{phase}] scanned={scanned}, indexed={indexed}, elapsed={elapsed:.1}s",
                    phase = progress.phase,
                    scanned = progress.scanned_files,
                    indexed = progress.indexed_files,
                );
            }
        }
    }

    fn emit_stage(&mut self, phase: &'static str, detail: String) {
        let elapsed = self.started_at.elapsed().as_secs_f64();
        self.last_phase = phase;
        self.last_emitted_at = Instant::now();
        eprintln!("index[{phase}] {detail}, elapsed={elapsed:.1}s");
    }
}

fn report_index_progress(reporter: &Arc<Mutex<IndexProgressReporter>>, progress: IndexProgress) {
    if let Ok(mut reporter) = reporter.lock() {
        reporter.emit_progress(progress);
    }
}

fn report_index_stage(
    reporter: &Arc<Mutex<IndexProgressReporter>>,
    phase: &'static str,
    detail: String,
) {
    if let Ok(mut reporter) = reporter.lock() {
        reporter.emit_stage(phase, detail);
    }
}

const SEMANTIC_PROGRESS_INTERVAL: Duration = Duration::from_secs(2);

/// Renders semantic embedding progress on stderr so stdout stays the report. An interactive
/// stderr gets one line redrawn in place; anything else gets whole lines, where carriage
/// returns would only garble the capture. "Interactive" is `is_terminal()` minus the two
/// conventional opt-outs, because a CI job that allocates a PTY is a terminal by that test and
/// is exactly where a redrawn line reads worst.
struct SemanticProgressReporter {
    phase: &'static str,
    started_at: Instant,
    last_emitted_at: Option<Instant>,
    terminal: bool,
    line_open: bool,
    finished: bool,
}

impl SemanticProgressReporter {
    fn new(phase: &'static str) -> Self {
        // NO_COLOR is honoured only when non-empty, which is what the convention requires: an
        // empty value is not an opt-out.
        let opted_out = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
            || std::env::var("TERM").is_ok_and(|term| term == "dumb");
        Self {
            phase,
            started_at: Instant::now(),
            last_emitted_at: None,
            terminal: std::io::stderr().is_terminal() && !opted_out,
            line_open: false,
            finished: false,
        }
    }

    fn observe(&mut self, progress: SemanticIndexProgress) {
        if self.finished {
            return;
        }
        let now = Instant::now();
        let finished = progress.embedded >= progress.to_embed;
        if !semantic_progress_is_due(self.last_emitted_at, now, finished) {
            return;
        }
        self.last_emitted_at = Some(now);
        self.finished = finished;
        let line = semantic_progress_line(self.phase, progress, self.started_at.elapsed());
        if self.terminal {
            eprint!("\r\x1b[2K{line}");
            if finished {
                eprintln!();
            }
            self.line_open = !finished;
        } else {
            eprintln!("{line}");
        }
    }
}

impl Drop for SemanticProgressReporter {
    // A build that fails mid-embedding must not glue its error onto a half-drawn line.
    fn drop(&mut self) {
        if self.line_open {
            eprintln!();
        }
    }
}

/// Whether a progress update earns a line: the first one and the last one always do, and the
/// rest are rate-limited to one per [`SEMANTIC_PROGRESS_INTERVAL`]. Split from `observe` so the
/// cadence is testable without a clock or a terminal.
fn semantic_progress_is_due(
    last_emitted_at: Option<Instant>,
    now: Instant,
    finished: bool,
) -> bool {
    finished
        || last_emitted_at.is_none_or(|last| now.duration_since(last) >= SEMANTIC_PROGRESS_INTERVAL)
}

fn semantic_progress_line(
    phase: &str,
    progress: SemanticIndexProgress,
    elapsed: Duration,
) -> String {
    let SemanticIndexProgress {
        embedded,
        to_embed,
        reused,
        total_targets,
    } = progress;
    let elapsed = elapsed.as_secs_f64();
    if to_embed == 0 {
        return format!(
            "semantic[{phase}] nothing to embed, reused={reused}/{total_targets}, elapsed={elapsed:.1}s"
        );
    }
    let percent = (embedded as f64 / to_embed as f64) * 100.0;
    format!(
        "semantic[{phase}] {embedded}/{to_embed} targets embedded ({percent:.1}%), reused={reused}/{total_targets}, elapsed={elapsed:.1}s"
    )
}

#[cfg(test)]
mod semantic_progress_tests {
    use super::*;

    #[test]
    fn semantic_progress_emits_first_and_last_and_rate_limits_between() {
        let start = Instant::now();
        // The first update always prints, which is what makes a long run say something early.
        assert!(semantic_progress_is_due(None, start, false));
        // Inside the interval, an unfinished update is suppressed.
        assert!(!semantic_progress_is_due(
            Some(start),
            start + Duration::from_millis(1_999),
            false
        ));
        // At the interval it prints again.
        assert!(semantic_progress_is_due(
            Some(start),
            start + SEMANTIC_PROGRESS_INTERVAL,
            false
        ));
        // Completion is never rate-limited, so the final line cannot be swallowed by a build
        // that finishes inside the interval — which is every small repository.
        assert!(semantic_progress_is_due(
            Some(start),
            start + Duration::from_millis(1),
            true
        ));
    }

    #[test]
    fn semantic_progress_line_states_embedded_of_total_reused_and_elapsed() {
        let line = semantic_progress_line(
            "index",
            SemanticIndexProgress {
                embedded: 512,
                to_embed: 4_096,
                reused: 380,
                total_targets: 4_476,
            },
            Duration::from_millis(41_300),
        );
        assert_eq!(
            line,
            "semantic[index] 512/4096 targets embedded (12.5%), reused=380/4476, elapsed=41.3s"
        );
    }

    #[test]
    fn semantic_progress_line_says_when_every_target_was_reused() {
        let line = semantic_progress_line(
            "rebuild",
            SemanticIndexProgress {
                embedded: 0,
                to_embed: 0,
                reused: 12,
                total_targets: 12,
            },
            Duration::ZERO,
        );
        assert_eq!(
            line,
            "semantic[rebuild] nothing to embed, reused=12/12, elapsed=0.0s"
        );
    }
}

fn mcp_install_snippet(client: McpClient, repo: &Path) -> serde_json::Value {
    let args = vec![
        "mcp".to_string(),
        "serve".to_string(),
        "--repo".to_string(),
        repo.display().to_string(),
        "--read-only".to_string(),
    ];
    let command_array: Vec<String> = std::iter::once("ok".to_string())
        .chain(args.iter().cloned())
        .collect();
    match client {
        McpClient::Claude => serde_json::json!({
            "client": "claude",
            "instructions": "Add this entry to Claude Desktop's mcpServers config. Open Kioku MCP tools inspect local evidence; apply source edits with your normal editor.",
            "config": {
                "mcpServers": {
                    "open-kioku": {
                        "command": "ok",
                        "args": args
                    }
                }
            }
        }),
        McpClient::Cursor => serde_json::json!({
            "client": "cursor",
            "instructions": "Add this entry to Cursor's MCP config. Open Kioku MCP tools inspect local evidence; apply source edits with your normal editor.",
            "config": {
                "open-kioku": {
                    "command": "ok",
                    "args": args
                }
            }
        }),
        McpClient::Codex => serde_json::json!({
            "client": "codex",
            "instructions": "Add this entry to ~/.codex/config.toml or your trusted project .codex/config.toml.",
            "config_text": format!(
                "[mcp_servers.open-kioku]\ncommand = \"ok\"\nargs = [{}]\nenabled = true\n",
                args.iter()
                    .map(|arg| format!("\"{}\"", toml_escape(arg)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "config": {
                "mcp_servers": {
                    "open-kioku": {
                        "command": "ok",
                        "args": args,
                        "enabled": true
                    }
                }
            }
        }),
        McpClient::Gemini => serde_json::json!({
            "client": "gemini",
            "instructions": "Add this entry to .gemini/settings.json or ~/.gemini/settings.json under mcpServers.",
            "config": {
                "mcpServers": {
                    "open-kioku": {
                        "command": "ok",
                        "args": args,
                        "trust": false
                    }
                }
            }
        }),
        McpClient::Opencode => serde_json::json!({
            "client": "opencode",
            "instructions": "Add this entry to opencode.json or opencode.jsonc.",
            "config": {
                "$schema": "https://opencode.ai/config.json",
                "mcp": {
                    "open-kioku": {
                        "type": "local",
                        "command": command_array,
                        "enabled": true
                    }
                }
            }
        }),
        McpClient::Zed => serde_json::json!({
            "client": "zed",
            "instructions": "Add this entry to Zed settings.json under context_servers.",
            "config": {
                "context_servers": {
                    "open-kioku": {
                        "command": "ok",
                        "args": args,
                        "env": {}
                    }
                }
            }
        }),
        McpClient::Windsurf => serde_json::json!({
            "client": "windsurf",
            "instructions": "Add this entry to ~/.codeium/windsurf/mcp_config.json (or %USERPROFILE%\\.codeium\\windsurf\\mcp_config.json on Windows).",
            "config": {
                "mcpServers": {
                    "open-kioku": {
                        "command": "ok",
                        "args": args
                    }
                }
            }
        }),
        McpClient::Trae => serde_json::json!({
            "client": "trae",
            "instructions": "Add this entry to ~/.trae/mcp.json (or %USERPROFILE%\\.trae\\mcp.json on Windows), or locally in your project's .trae/mcp.json.",
            "config": {
                "mcpServers": {
                    "open-kioku": {
                        "command": "ok",
                        "args": args
                    }
                }
            }
        }),
    }
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
