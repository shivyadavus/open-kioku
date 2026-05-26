use clap::{Args, Parser, Subcommand, ValueEnum};
use open_kioku_architecture::ArchitectureDetector;
use open_kioku_config::OkConfig;
use open_kioku_context::{ContextPackBuilder, ContextPackFormat};
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
use std::fs;
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
    Demo {
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        force: bool,
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
        #[arg(long, value_enum, default_value_t = ContextPackFormat::Json)]
        format: ContextPackFormat,
    },
    Bench {
        #[arg(long, default_value = ".")]
        repo: PathBuf,
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
        #[arg(long, default_value_t = false)]
        hide_experimental: bool,
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
struct DemoReport {
    repo: PathBuf,
    file_count: usize,
    symbol_count: usize,
    chunk_count: usize,
    commands: Vec<String>,
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
            let snapshot = index_repo(&repo)?;
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
        Command::Demo { path, force } => {
            let repo = demo_repo_path(path)?;
            let report = build_demo_repo(&repo, force)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Demo repo ready: {}", report.repo.display());
                println!(
                    "Indexed {} files, {} symbols, {} chunks",
                    report.file_count, report.symbol_count, report.chunk_count
                );
                println!("\nTry:");
                for command in &report.commands {
                    println!("  {command}");
                }
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
        Command::Context { task, format } => {
            let store = open_store(&repo)?;
            let pack = ContextPackBuilder::new(&store as &dyn OkStore).build(&task, 20)?;
            let rendered = format.render(&pack)?;
            println!("{}", rendered);
        }
        Command::Bench { repo } => {
            let start = std::time::Instant::now();
            let snapshot = index_repo(&repo)?;
            let duration = start.elapsed();
            
            let manifest = snapshot.manifest;
            println!("Indexed {} files and {} symbols in {:?}", manifest.file_count, manifest.symbol_count, duration);
            println!("{:.2} files/sec", manifest.file_count as f64 / duration.as_secs_f64());
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
    }
    Ok(())
}

fn doctor_report(repo: &Path) -> DoctorReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let mut checks = Vec::new();
    let mut next_steps = Vec::new();

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
            status: CheckStatus::Fail,
            message: ".ok/index.sqlite is missing".into(),
        });
        next_steps.push("Run `ok index .` before connecting an MCP client.".into());
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
        next_steps,
    }
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
        "# Open Kioku Demo\n\nSmall repo for trying code search, symbols, impact, and MCP setup.\n",
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
    OkConfig::write_default(repo.join("ok.toml"))?;

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
            format!("ok --repo {repo_display} context \"change token expiry\" --json"),
            format!("ok mcp install claude --repo {repo_display}"),
        ],
    })
}

fn index_repo(repo: &Path) -> anyhow::Result<open_kioku_ingest::IndexSnapshot> {
    let config = OkConfig::load_from_repo(repo)?;
    let snapshot = Indexer::default().index_repo(repo, &config)?;
    let store = open_store(repo)?;
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
        default_index_dir(repo),
        &snapshot.chunks,
        &snapshot.files,
        &snapshot.symbols,
    )?;
    Ok(snapshot)
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
