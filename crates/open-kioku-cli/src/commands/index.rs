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
    let _lock = IndexWriteLock::acquire(repo, &reporter)?;
    let index_reporter = Arc::clone(&reporter);
    let (snapshot, history) = Indexer::default().index_repo_with_history_mode_and_progress(
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
            "writing {} files, {} symbols, {} chunks, {} occurrences, {} analysis facts",
            snapshot.files.len(),
            snapshot.symbols.len(),
            snapshot.chunks.len(),
            snapshot.occurrences.len(),
            snapshot.analysis_facts.len()
        ),
    );
    let store = open_store(repo)?;
    store.replace_index(IndexData {
        manifest: &snapshot.manifest,
        files: &snapshot.files,
        symbols: &snapshot.symbols,
        chunks: &snapshot.chunks,
        tests: &snapshot.tests,
        imports: &snapshot.imports,
        occurrences: &snapshot.occurrences,
        analysis_facts: &snapshot.analysis_facts,
    })?;
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
    let graph = InMemoryGraph::from_index_with_analysis(
        &snapshot.files,
        &snapshot.symbols,
        &snapshot.chunks,
        &snapshot.occurrences,
        &snapshot.imports,
        &snapshot.analysis_facts,
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
    store.replace_graph(
        &graph.nodes.values().cloned().collect::<Vec<_>>(),
        &graph.edges,
    )?;
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
        &graph.nodes.values().cloned().collect::<Vec<_>>(),
    )?;
    report_index_stage(&reporter, "complete", "index ready".to_string());
    Ok(snapshot)
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

struct IndexWriteLock {
    path: PathBuf,
    _file: fs::File,
}

impl IndexWriteLock {
    fn acquire(repo: &Path, reporter: &Arc<Mutex<IndexProgressReporter>>) -> anyhow::Result<Self> {
        let ok_dir = repo.join(".ok");
        fs::create_dir_all(&ok_dir)?;
        let lock_path = ok_dir.join("index.lock");
        report_index_stage(
            reporter,
            "lock",
            "waiting for exclusive index writer lock".to_string(),
        );
        let started_waiting = Instant::now();
        let file = loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(file) => break file,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if started_waiting.elapsed() > Duration::from_secs(30) {
                        anyhow::bail!(
                            "index is locked by another writer or a stale lock at {}; remove it only if no ok index process is running",
                            lock_path.display()
                        );
                    }
                    thread::sleep(Duration::from_millis(250));
                }
                Err(err) => return Err(err.into()),
            }
        };
        report_index_stage(reporter, "lock", "acquired index writer lock".to_string());
        Ok(Self {
            path: lock_path,
            _file: file,
        })
    }
}

impl Drop for IndexWriteLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
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
            "instructions": "Add this entry to Claude Desktop's mcpServers config. To enable the apply_patch tool, add an \"env\" object with \"OPEN_KIOKU_ALLOW_WRITE\": \"true\".",
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
            "instructions": "Add this entry to Cursor's MCP config. To enable the apply_patch tool, set the environment variable OPEN_KIOKU_ALLOW_WRITE=true.",
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
