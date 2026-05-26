use clap::{Args, Parser, Subcommand, ValueEnum};
use open_kioku_architecture::ArchitectureDetector;
use open_kioku_config::OkConfig;
use open_kioku_context::ContextPackBuilder;
use open_kioku_graph::InMemoryGraph;
use open_kioku_impact::ImpactEngine;
use open_kioku_ingest::Indexer;
use open_kioku_patch::PatchPlanner;
use open_kioku_search_regex::search_chunks;
use open_kioku_search_tantivy::{default_index_dir, rebuild_disk_index, TantivySearchIndex};
use open_kioku_storage::{GraphStore, IndexData, MetadataStore, OkStore, SearchIndex};
use open_kioku_storage_sqlite::SqliteStore;
use open_kioku_symbols::SymbolEngine;
use open_kioku_tests::TestSelector;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "ok", about = "Open Kioku code-intelligence platform")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true, default_value = ".")]
    repo: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Init {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    Index {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    Watch {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    Status {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    Doctor {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Symbol {
        #[command(subcommand)]
        command: SymbolCommand,
    },
    Explain {
        #[command(subcommand)]
        command: ExplainCommand,
    },
    Impact(ImpactArgs),
    Path {
        from: String,
        to: String,
    },
    Tests {
        #[arg(long)]
        changed: PathBuf,
    },
    Context {
        task: String,
    },
    Architecture {
        #[command(subcommand)]
        command: ArchitectureCommand,
    },
    Patch {
        #[command(subcommand)]
        command: PatchCommand,
    },
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
}

#[derive(Subcommand)]
enum SymbolCommand {
    Find { name: String },
    Definition { name: String },
    Refs { name: String },
}

#[derive(Subcommand)]
enum ExplainCommand {
    File { path: PathBuf },
    Symbol { name: String },
}

#[derive(Args)]
struct ImpactArgs {
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long)]
    symbol: Option<String>,
}

#[derive(Subcommand)]
enum ArchitectureCommand {
    Detect,
    Boundaries,
    Violations,
}

#[derive(Subcommand)]
enum PatchCommand {
    Plan {
        task: String,
    },
    Review {
        #[arg(long)]
        id: String,
    },
    Apply {
        #[arg(long)]
        id: String,
        #[arg(long)]
        approved: bool,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    Install {
        client: McpClient,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    Serve {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = true)]
        read_only: bool,
        #[arg(long, default_value_t = false)]
        allow_write: bool,
        #[arg(long, default_value_t = true)]
        approval_required: bool,
        #[arg(long = "allow-command")]
        allow_command: Vec<String>,
        #[arg(long, default_value_t = true)]
        deny_network: bool,
    },
}

#[derive(Clone, ValueEnum)]
enum McpClient {
    Claude,
    Cursor,
}

#[derive(Serialize)]
struct DoctorReport {
    ok: bool,
    repo: PathBuf,
    checks: Vec<DoctorCheck>,
    next_steps: Vec<String>,
}

#[derive(Serialize)]
struct DoctorCheck {
    name: &'static str,
    status: CheckStatus,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
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
                "initialized Open Kioku repository",
                &serde_json::json!({"status":"initialized"}),
            )?;
        }
        Command::Index { repo: command_repo } => {
            let repo = resolve_repo(&repo, command_repo);
            let config = OkConfig::load_from_repo(&repo)?;
            let snapshot = Indexer::default().index_repo(&repo, &config)?;
            let store = open_store(&repo)?;
            store.replace_index(IndexData {
                manifest: &snapshot.manifest,
                files: &snapshot.files,
                symbols: &snapshot.symbols,
                chunks: &snapshot.chunks,
                tests: &snapshot.tests,
                imports: &snapshot.imports,
                occurrences: &snapshot.occurrences,
            })?;
            let graph = InMemoryGraph::from_index_with_occurrences(
                &snapshot.files,
                &snapshot.symbols,
                &snapshot.chunks,
                &snapshot.occurrences,
            );
            store.replace_graph(
                &graph.nodes.values().cloned().collect::<Vec<_>>(),
                &graph.edges,
            )?;
            rebuild_disk_index(
                default_index_dir(&repo),
                &snapshot.chunks,
                &snapshot.files,
                &snapshot.symbols,
            )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&snapshot.manifest)?);
            } else {
                println!(
                    "Indexed {} files, {} symbols, {} chunks",
                    snapshot.manifest.file_count,
                    snapshot.manifest.symbol_count,
                    snapshot.manifest.chunk_count
                );
            }
        }
        Command::Watch { repo: command_repo } => {
            let repo = resolve_repo(&repo, command_repo);
            open_kioku_watch::watch_repo(&repo)?;
        }
        Command::Status { repo: command_repo } => {
            let repo = resolve_repo(&repo, command_repo);
            let store = open_store(&repo)?;
            let manifest = store.manifest()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            } else if let Some(manifest) = manifest {
                println!(
                    "Healthy index: {} files, {} symbols, indexed at {}",
                    manifest.file_count, manifest.symbol_count, manifest.indexed_at
                );
            } else {
                println!("No index found. Run `ok index .`.");
            }
        }
        Command::Doctor { repo: command_repo } => {
            let repo = resolve_repo(&repo, command_repo);
            let report = doctor_report(&repo);
            let ok = report.ok;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Open Kioku doctor for {}", report.repo.display());
                for check in &report.checks {
                    let marker = match check.status {
                        CheckStatus::Pass => "PASS",
                        CheckStatus::Warn => "WARN",
                        CheckStatus::Fail => "FAIL",
                    };
                    println!("{marker} {:<16} {}", check.name, check.message);
                }
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
        Command::Search { query, limit } => {
            let store = open_store(&repo)?;
            let results = search(&repo, &store, &query, limit)?;
            output(cli.json, &results, || {
                for result in &results {
                    println!(
                        "{}:{}  {:.2}  {}",
                        result.path.display(),
                        result.line_range.as_ref().map(|r| r.start).unwrap_or(0),
                        result.score,
                        result.snippet
                    );
                }
            })?;
        }
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
                    output(
                        cli.json,
                        &serde_json::json!({"file": file, "chunks": chunks}),
                        || {
                            println!("{} chunks indexed for {}", chunks.len(), path.display());
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
            let report = if let Some(path) = args.file {
                ImpactEngine::new(&store).for_file(&path)?
            } else if let Some(symbol) = args.symbol {
                let definition = SymbolEngine::new(&store).definition(&symbol)?;
                let files = store.list_files(usize::MAX, 0)?;
                let file = files.iter().find(|file| file.id == definition.file_id);
                ImpactEngine::new(&store).for_file(
                    file.map(|file| file.path.as_path())
                        .unwrap_or(Path::new(&symbol)),
                )?
            } else {
                anyhow::bail!("provide --file or --symbol");
            };
            output(cli.json, &report, || {})?;
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
                &TestSelector::new(&store).for_changed_path(&changed, 20)?,
                || {},
            )?;
        }
        Command::Context { task } => {
            let store = open_store(&repo)?;
            output(
                cli.json,
                &ContextPackBuilder::new(&store as &dyn OkStore).build(&task, 20)?,
                || {},
            )?;
        }
        Command::Architecture { command } => {
            let store = open_store(&repo)?;
            let summary = ArchitectureDetector::new(&store).detect()?;
            match command {
                ArchitectureCommand::Detect => output(cli.json, &summary, || {})?,
                ArchitectureCommand::Boundaries => output(cli.json, &summary.components, || {})?,
                ArchitectureCommand::Violations => output(cli.json, &summary.violations, || {})?,
            }
        }
        Command::Patch { command } => {
            let config = OkConfig::load_from_repo(&repo)?;
            let store = open_store(&repo)?;
            let planner = PatchPlanner::new(&config, &store as &dyn OkStore);
            match command {
                PatchCommand::Plan { task } => output(cli.json, &planner.plan(&task)?, || {})?,
                PatchCommand::Review { id } => {
                    println!("patch review requires stored patch plan id={id}")
                }
                PatchCommand::Apply { id, approved } => {
                    anyhow::bail!("patch apply is policy gated and requires a stored diff; id={id} approved={approved}");
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
                    if let Ok(config) = serde_json::to_string_pretty(&snippet["config"]) {
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
                if !allow_command.is_empty() {
                    config.commands.allow = allow_command;
                }
                open_kioku_mcp::serve_stdio(repo, config).await?;
            }
        },
    }
    Ok(())
}

fn doctor_report(repo: &Path) -> DoctorReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let mut checks = Vec::new();
    let mut next_steps = Vec::new();

    push_check(
        &mut checks,
        "repo",
        repo.is_dir(),
        format!("repository path exists: {}", repo.display()),
        format!("repository path does not exist: {}", repo.display()),
    );

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
        next_steps.push("Run `ok init .` to create ok.toml and .ok/.".into());
    }

    let index_path = repo.join(".ok/index.sqlite");
    if index_path.exists() {
        match SqliteStore::open(&index_path).and_then(|store| store.manifest()) {
            Ok(Some(manifest)) => checks.push(DoctorCheck {
                name: "index",
                status: CheckStatus::Pass,
                message: format!(
                    "{} files, {} symbols, indexed at {}",
                    manifest.file_count, manifest.symbol_count, manifest.indexed_at
                ),
            }),
            Ok(None) => {
                checks.push(DoctorCheck {
                    name: "index",
                    status: CheckStatus::Warn,
                    message: "index database exists but has no manifest".into(),
                });
                next_steps.push("Run `ok index .` to build a fresh index.".into());
            }
            Err(err) => {
                checks.push(DoctorCheck {
                    name: "index",
                    status: CheckStatus::Fail,
                    message: err.to_string(),
                });
                next_steps.push("Remove .ok/index.sqlite and run `ok index .` again.".into());
            }
        }
    } else {
        checks.push(DoctorCheck {
            name: "index",
            status: CheckStatus::Warn,
            message: ".ok/index.sqlite is missing".into(),
        });
        next_steps.push("Run `ok index .` before connecting an MCP client.".into());
    }

    let search_index = default_index_dir(&repo);
    if TantivySearchIndex::exists(&search_index) {
        checks.push(DoctorCheck {
            name: "search",
            status: CheckStatus::Pass,
            message: format!("Tantivy index exists at {}", search_index.display()),
        });
    } else {
        checks.push(DoctorCheck {
            name: "search",
            status: CheckStatus::Warn,
            message: "Tantivy index is missing; regex fallback may be used".into(),
        });
        next_steps.push("Run `ok index .` to build the search index.".into());
    }

    match std::env::current_exe() {
        Ok(path) => checks.push(DoctorCheck {
            name: "binary",
            status: CheckStatus::Pass,
            message: format!("running {}", path.display()),
        }),
        Err(err) => checks.push(DoctorCheck {
            name: "binary",
            status: CheckStatus::Warn,
            message: err.to_string(),
        }),
    }

    next_steps.dedup();
    let ok = checks
        .iter()
        .all(|check| !matches!(check.status, CheckStatus::Fail));
    DoctorReport {
        ok,
        repo,
        checks,
        next_steps,
    }
}

fn push_check(
    checks: &mut Vec<DoctorCheck>,
    name: &'static str,
    passed: bool,
    pass_message: String,
    fail_message: String,
) {
    checks.push(DoctorCheck {
        name,
        status: if passed {
            CheckStatus::Pass
        } else {
            CheckStatus::Fail
        },
        message: if passed { pass_message } else { fail_message },
    });
}

fn mcp_install_snippet(client: McpClient, repo: &Path) -> serde_json::Value {
    let args = vec![
        "mcp".to_string(),
        "serve".to_string(),
        "--repo".to_string(),
        repo.display().to_string(),
        "--read-only".to_string(),
    ];
    match client {
        McpClient::Claude => serde_json::json!({
            "client": "claude",
            "instructions": "Add this entry to Claude Desktop's mcpServers config.",
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
            "instructions": "Add this entry to Cursor's MCP config.",
            "config": {
                "open-kioku": {
                    "command": "ok",
                    "args": args
                }
            }
        }),
    }
}

fn absolutize(path: &Path) -> anyhow::Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.exists() {
        Ok(path.canonicalize()?)
    } else {
        Ok(path)
    }
}

fn open_store(repo: impl AsRef<Path>) -> anyhow::Result<SqliteStore> {
    Ok(SqliteStore::open(repo.as_ref().join(".ok/index.sqlite"))?)
}

fn search(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let index_dir = default_index_dir(repo);
    if TantivySearchIndex::exists(&index_dir) {
        return Ok(TantivySearchIndex::open_or_create(index_dir)?.search(query, limit)?);
    }
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = store.all_chunks()?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    Ok(search_chunks(&chunks, &files, &symbols, query, limit)?)
}

fn resolve_repo(global: &Path, command: PathBuf) -> PathBuf {
    if command == Path::new(".") {
        global.to_path_buf()
    } else {
        command
    }
}

fn resolve_graph_node(store: &dyn MetadataStore, query: &str) -> anyhow::Result<String> {
    if query.starts_with("file:") || query.starts_with("symbol:") {
        return Ok(query.to_string());
    }
    if let Some(file) = store.get_file_by_path(Path::new(query))? {
        return Ok(format!("file:{}", file.path.display()));
    }
    if let Some(symbol) = store
        .list_symbols(Some(query), 10, 0)?
        .into_iter()
        .find(|symbol| symbol.name == query || symbol.qualified_name.ends_with(query))
    {
        return Ok(format!("symbol:{}", symbol.id.0));
    }
    Ok(query.to_string())
}

fn output<T: serde::Serialize>(json: bool, value: &T, human: impl FnOnce()) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        let text = serde_json::to_string_pretty(value)?;
        if text.len() < 4096 {
            println!("{text}");
        } else {
            human();
        }
    }
    Ok(())
}

fn print_text_or_json(json: bool, text: &str, value: &serde_json::Value) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{text}");
    }
    Ok(())
}
