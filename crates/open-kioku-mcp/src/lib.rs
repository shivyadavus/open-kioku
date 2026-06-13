use open_kioku_actions::{ActionKind, PolicyGate};
use open_kioku_architecture::ArchitectureDetector;
use open_kioku_config::OkConfig;
use open_kioku_context::ContextPackBuilder;
use open_kioku_context_compress::ContextHandleStore;
use open_kioku_core::{Confidence, ContextHandleId, PlanReport, SymbolId};
use open_kioku_impact::ImpactEngine;
use open_kioku_memory::RepoMemoryStore;
use open_kioku_patch::{ChangeVerifier, PatchPlanner, VerifyChangeInput};
use open_kioku_plan::{PlanEngine, PlanFormat};
use open_kioku_search_regex::search_chunks;
use open_kioku_search_tantivy::{default_index_dir, TantivySearchIndex};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_sentry::disabled_response;
use open_kioku_storage::{GraphStore, HistoryStore, MetadataStore, OkStore, SearchIndex};
use open_kioku_storage_sqlite::SqliteStore;
use open_kioku_symbols::SymbolEngine;
use open_kioku_tests::TestSelector;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

pub async fn serve_stdio(repo: PathBuf, config: OkConfig) -> anyhow::Result<()> {
    let store = SqliteStore::open(repo.join(".ok/index.sqlite"))?;
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => handle_request(&repo, &store, &config, request).await,
            Err(err) => JsonRpcResponse {
                jsonrpc: "2.0",
                id: None,
                result: None,
                error: Some(json!({"code": -32700, "message": err.to_string()})),
            },
        };
        stdout
            .write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())
            .await?;
        stdout.flush().await?;
    }
    Ok(())
}

async fn handle_request(
    repo: &Path,
    store: &SqliteStore,
    config: &OkConfig,
    request: JsonRpcRequest,
) -> JsonRpcResponse {
    let id = request.id.clone();
    let result = dispatch(repo, store, config, &request.method, request.params).await;
    match result {
        Ok(value) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(value),
            error: None,
        },
        Err(err) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(json!({"code": -32000, "message": err.to_string()})),
        },
    }
}

async fn dispatch(
    repo: &Path,
    store: &SqliteStore,
    config: &OkConfig,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    let gate = PolicyGate::new(config);
    match method {
        "initialize" => {
            let client_version = params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            Ok(json!({
                "protocolVersion": client_version,
                "serverInfo": {"name": "open-kioku", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"tools": {}}
            }))
        }
        "tools/list" => {
            let (tool_list, unstable) = tools(config);
            Ok(json!({
                "tools": tool_list,
                "_unstable_experimental_tools": unstable
            }))
        }
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            call_tool(repo, store, config, name, args).await
        }
        "repo_status" => Ok(json!(store.manifest()?)),
        "list_languages" => {
            let files = store.list_files(usize::MAX, 0)?;
            let mut languages = files
                .into_iter()
                .map(|file| format!("{:?}", file.language))
                .collect::<Vec<_>>();
            languages.sort();
            languages.dedup();
            Ok(json!({"languages": languages}))
        }
        "list_files" => {
            gate.ensure_allowed(ActionKind::Read)?;
            let limit = limit(&params);
            Ok(json!(store.list_files(limit, 0)?))
        }
        "list_symbols" | "search_symbols" => {
            let query = params.get("query").and_then(Value::as_str);
            Ok(json!(store.list_symbols(query, limit(&params), 0)?))
        }
        "search_code" | "search_files" => search_tool(repo, store, &params),
        "regex_search" => search_tool(repo, store, &params),
        "build_context_pack" => {
            let task = required_str(&params, "task")?;
            let pack =
                ContextPackBuilder::new(store as &dyn OkStore).build(task, limit(&params))?;
            let format_arg = params
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("json");
            match format_arg {
                "markdown" => Ok(json!(
                    open_kioku_context::ContextPackFormat::Markdown.render(&pack)?
                )),
                "toon" => Ok(json!(open_kioku_format::render_context_pack_toon(&pack))),
                _ => Ok(json!(pack)),
            }
        }
        "build_compressed_context" => {
            let task = required_str(&params, "task")?;
            let pack =
                ContextPackBuilder::new(store as &dyn OkStore).build(task, limit(&params))?;
            let compressed = ContextHandleStore::open_repo(repo)?.compress_pack(&pack)?;
            let format_arg = params
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("json");
            if format_arg == "toon" {
                Ok(json!(open_kioku_format::render_compressed_context_toon(
                    &compressed
                )))
            } else {
                Ok(json!(compressed))
            }
        }
        "retrieve_context" => {
            let handle = required_str(&params, "handle")?;
            let retrieved =
                ContextHandleStore::open_repo(repo)?.retrieve(&ContextHandleId::new(handle))?;
            Ok(json!(retrieved))
        }
        "plan_change" => {
            let task = required_str(&params, "task")?;
            let memory_facts = RepoMemoryStore::open_repo(repo)?.search(task, 8)?;
            let report = PlanEngine::new(store as &dyn OkStore)
                .with_memory_facts(memory_facts)
                .plan(task, limit(&params))?;
            let format_arg = params
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("json");
            match format_arg {
                "markdown" => Ok(json!(PlanFormat::Markdown.render(&report)?)),
                "toon" => Ok(json!(PlanFormat::Toon.render(&report)?)),
                _ => Ok(json!(report)),
            }
        }
        "remember_fact" => {
            let text = required_str(&params, "text")?;
            let source = params
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("mcp");
            let confidence = confidence_arg(&params);
            Ok(json!(
                RepoMemoryStore::open_repo(repo)?.remember(text, source, confidence)?
            ))
        }
        "search_memory" => {
            let query = required_str(&params, "query")?;
            Ok(json!(
                RepoMemoryStore::open_repo(repo)?.search(query, limit(&params))?
            ))
        }
        "impact_analysis" => {
            let path = required_str(&params, "path")?;
            Ok(json!(ImpactEngine::new(store).for_file(Path::new(path))?))
        }
        "history_provenance_lookup" => {
            let path = params.get("path").and_then(Value::as_str);
            let symbol = params.get("symbol").and_then(Value::as_str);
            match (path, symbol) {
                (Some(path), None) => Ok(json!(
                    store.provenance_for_path(Path::new(path), limit(&params))?
                )),
                (None, Some(query)) => {
                    let symbol = resolve_history_symbol(store, query)?;
                    Ok(json!(
                        store.provenance_for_symbol(&symbol.id, limit(&params))?
                    ))
                }
                (Some(_), Some(_)) => {
                    anyhow::bail!("provide exactly one of `path` or `symbol`")
                }
                (None, None) => anyhow::bail!("missing required `path` or `symbol` argument"),
            }
        }
        "find_tests_for_change" | "recommend_validation_plan" => {
            let path = required_str(&params, "path")?;
            Ok(json!(
                TestSelector::new(store).for_changed_path(Path::new(path), limit(&params))?
            ))
        }
        "detect_architecture" | "architecture_boundaries" | "architecture_violations" => {
            Ok(json!(ArchitectureDetector::new(store, None).detect()?))
        }
        "get_definition" | "get_symbol_context" | "explain_symbol" => {
            let query = required_str(&params, "query")?;
            Ok(json!(SymbolEngine::new(store).definition(query)?))
        }
        "get_references" => {
            let query = required_str(&params, "query")?;
            Ok(json!(
                SymbolEngine::new(store).references(query, limit(&params))?
            ))
        }
        "get_callers" | "get_callees" => {
            let query = required_str(&params, "query")?;
            let symbol = SymbolEngine::new(store).definition(query)?;
            let node = format!("symbol:{}", symbol.id.0);
            let (nodes, edges) = store.neighbors(&node, limit(&params))?;
            Ok(json!({"symbol": symbol, "nodes": nodes, "edges": edges}))
        }
        "semantic_status" => semantic_status_tool(repo, store, config),
        "semantic_search" => semantic_search_tool(repo, store, config, &params),
        "hybrid_search" => hybrid_search_tool(repo, store, config, &params),
        "explain_search_result" => hybrid_search_tool(repo, store, config, &params),
        "get_implementations" | "structural_search" => search_tool(repo, store, &params),
        "dependency_path" => {
            let from = required_str(&params, "from")?;
            let to = required_str(&params, "to")?;
            let from = resolve_graph_node(store, from)?;
            let to = resolve_graph_node(store, to)?;
            Ok(json!({
                "from": from,
                "to": to,
                "edges": store.shortest_path(&from, &to, 12)?,
                "evidence_source": "sqlite_graph_store"
            }))
        }
        "module_dependencies" => {
            let node = required_str(&params, "node")?;
            let node = resolve_graph_node(store, node)?;
            let (nodes, edges) = store.neighbors(&node, limit(&params))?;
            Ok(json!({"node": node, "nodes": nodes, "edges": edges}))
        }
        "explain_file" => {
            let path = required_str(&params, "path")?;
            let file = store.get_file_by_path(Path::new(path))?;
            let chunks = if let Some(file) = &file {
                store.chunks_for_file(&file.id)?
            } else {
                Vec::new()
            };
            Ok(json!({"file": file, "chunks": chunks}))
        }
        "explain_flow" | "summarize_architecture" => {
            Ok(json!(ArchitectureDetector::new(store, None).detect()?))
        }
        "explain_test_coverage" => {
            let path = params
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(json!(
                TestSelector::new(store).for_changed_path(Path::new(path), limit(&params))?
            ))
        }
        "propose_patch" | "review_patch" | "validate_patch" => {
            let task = params
                .get("task")
                .and_then(Value::as_str)
                .unwrap_or("review requested patch");
            Ok(json!(
                PatchPlanner::new(config, store as &dyn OkStore).plan(task)?
            ))
        }
        "verify_change" => {
            let plan = plan_from_params(&params)?;
            let changed_files = params
                .get("changed_files")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(PathBuf::from)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let evidence_refs = params
                .get("evidence_refs")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let unified_diff = params
                .get("diff")
                .and_then(Value::as_str)
                .map(str::to_string);
            let run_commands = params
                .get("run_commands")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let index_dir = default_index_dir(repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(index_dir)?)
            } else {
                None
            };
            Ok(json!(ChangeVerifier::new(store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .verify(
                    repo,
                    &plan,
                    VerifyChangeInput {
                        changed_files,
                        unified_diff,
                        evidence_refs,
                        run_commands,
                    },
                )?))
        }
        "apply_patch" => {
            gate.ensure_allowed(ActionKind::ApplyPatch)?;
            if std::env::var("OPEN_KIOKU_ALLOW_WRITE").unwrap_or_default() != "true" {
                return Ok(
                    json!({"denied": true, "reason": "apply_patch requires OPEN_KIOKU_ALLOW_WRITE=true in the server environment"}),
                );
            }
            Ok(
                json!({"denied": true, "reason": "apply_patch requires explicit stored patch approval flow"}),
            )
        }
        "map_stacktrace_to_code" | "find_errors_for_symbol" | "find_recent_failures" => {
            Ok(json!(disabled_response(method)))
        }
        other => anyhow::bail!("unknown MCP method or tool `{other}`"),
    }
}

fn call_tool<'a>(
    repo: &'a Path,
    store: &'a SqliteStore,
    config: &'a OkConfig,
    name: &'a str,
    args: Value,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send + 'a>> {
    Box::pin(async move {
        dispatch(repo, store, config, name, args)
            .await
            .map(|value| {
                let text = if let Some(s) = value.as_str() {
                    s.to_string()
                } else {
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
                };
                json!({"content": [{"type": "text", "text": text}], "structuredContent": value})
            })
    })
}

fn search_tool(repo: &Path, store: &dyn MetadataStore, params: &Value) -> anyhow::Result<Value> {
    let query = params
        .get("query")
        .or_else(|| params.get("pattern"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let index_dir = default_index_dir(repo);
    if TantivySearchIndex::exists(&index_dir) {
        let index = TantivySearchIndex::open_or_create(index_dir)?;
        return Ok(json!(index.search(query, limit(params))?));
    }
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = store.all_chunks()?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    Ok(json!(search_chunks(
        &chunks,
        &files,
        &symbols,
        query,
        limit(params)
    )?))
}

fn semantic_search_tool(
    repo: &Path,
    store: &dyn MetadataStore,
    config: &OkConfig,
    params: &Value,
) -> anyhow::Result<Value> {
    let query = required_str(params, "query")?;
    let mut semantic_config = config.semantic.clone();
    semantic_config.enabled = true;
    let manager = SemanticIndexManager::new(repo, store, &semantic_config);
    let status = manager.status();
    if status.ready {
        return Ok(json!({
            "semantic_status": status,
            "results": manager.search(query, limit(params))?,
        }));
    }
    Ok(json!({
        "semantic_status": status,
        "results": [],
        "error": "semantic index is not ready; run `ok semantic index` first"
    }))
}

fn semantic_status_tool(
    repo: &Path,
    store: &dyn MetadataStore,
    config: &OkConfig,
) -> anyhow::Result<Value> {
    let manager = SemanticIndexManager::new(repo, store, &config.semantic);
    Ok(json!(manager.status()))
}

fn hybrid_search_tool(
    repo: &Path,
    store: &dyn MetadataStore,
    config: &OkConfig,
    params: &Value,
) -> anyhow::Result<Value> {
    let query = required_str(params, "query")?;
    let mut results = search_tool(repo, store, params)?
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect::<Vec<open_kioku_core::SearchResult>>();
    let mut semantic_config = config.semantic.clone();
    semantic_config.enabled = true;
    let manager = SemanticIndexManager::new(repo, store, &semantic_config);
    let status = manager.status();
    if status.ready {
        merge_semantic_results(&mut results, manager.search(query, limit(params))?);
    }
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
    });
    results.truncate(limit(params));
    Ok(json!({
        "semantic_status": status,
        "results": results,
    }))
}

fn merge_semantic_results(
    results: &mut Vec<open_kioku_core::SearchResult>,
    semantic_results: Vec<open_kioku_core::SearchResult>,
) {
    for semantic in semantic_results {
        if let Some(existing) = results
            .iter_mut()
            .find(|result| result.path == semantic.path)
        {
            for evidence in semantic.evidence {
                if !existing.evidence.contains(&evidence) {
                    existing.evidence.push(evidence);
                }
            }
            for evidence_ref in semantic.evidence_refs {
                if !existing.evidence_refs.contains(&evidence_ref) {
                    existing.evidence_refs.push(evidence_ref);
                }
            }
            for component in semantic.score_breakdown {
                if !existing
                    .score_breakdown
                    .iter()
                    .any(|existing| existing.signal == component.signal)
                {
                    existing.score_breakdown.push(component);
                }
            }
            existing.reconcile_score_breakdown();
        } else {
            results.push(semantic);
        }
    }
}

fn tools(config: &OkConfig) -> (Vec<Value>, Vec<String>) {
    let read_only_tools: &[(&str, &str, Value)] = &[
        ("repo_status", "Retrieve the current repository index metadata, including file count, symbol count, chunk count, and the exact timestamp when the repository was last indexed.", json!({"type":"object","properties":{}})),
        ("list_files", "List all indexed files within the repository. Returns metadata such as relative path, size in bytes, and language. Useful for codebase structure discovery.", json!({"type":"object","properties":{"limit":{"type":"integer","description":"Maximum number of files to return. Defaults to 20, capped at 100."}}})),
        ("list_languages", "List all programming languages detected and indexed in the repository, alongside support status.", json!({"type":"object","properties":{}})),
        ("list_symbols", "List or search indexed code symbols (such as classes, structs, functions, and interfaces) by name. Supports substring filtering.", json!({"type":"object","properties":{"query":{"type":"string","description":"Substring query to filter symbol names. If omitted, returns all symbols."},"limit":{"type":"integer","description":"Maximum number of symbols to return. Defaults to 20, capped at 100."}}})),
        ("search_symbols", "Search indexed code symbols by name using exact or fuzzy matching. Essential for finding definitions.", json!({"type":"object","properties":{"query":{"type":"string","description":"Fuzzy or exact search query for symbol names."},"limit":{"type":"integer","description":"Maximum number of results to return. Defaults to 20, capped at 100."}}})),
        ("detect_architecture", "Detect high-level architectural components and directories in the repository based on file layouts.", json!({"type":"object","properties":{}})),
        ("architecture_boundaries", "Show the configured or inferred boundaries between architectural components, useful to understand import constraints.", json!({"type":"object","properties":{}})),
        ("architecture_violations", "Report any import or boundary violations that deviate from the defined codebase architecture rules.", json!({"type":"object","properties":{}})),
        ("search_code", "Perform a lexical BM25 search across all indexed code chunks. Returns snippet matches, line numbers, and relevance scores. Best for general code search.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The search query containing terms, code patterns, or identifiers."},"limit":{"type":"integer","description":"Maximum number of search results to return. Defaults to 20, capped at 100."}}})),
        ("search_files", "Search indexed file names and contents for specific keywords or file path patterns.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The search query to match against file paths and file contents."},"limit":{"type":"integer","description":"Maximum number of results to return. Defaults to 20, capped at 100."}}})),
        ("regex_search", "Search indexed code using a regular expression pattern. Returns exact line matching snippets.", json!({"type":"object","required":["pattern"],"properties":{"pattern":{"type":"string","description":"A valid regular expression pattern to match against source code."},"limit":{"type":"integer","description":"Maximum number of results to return. Defaults to 20, capped at 100."}}})),
        ("semantic_status", "Report the current status, readiness, and staleness of the local semantic vector index.", json!({"type":"object","properties":{}})),
        ("semantic_search", "Search the local semantic vector index using natural language queries to retrieve conceptually related code snippets.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"Natural language search query expressing the concept or functionality you are looking for."},"limit":{"type":"integer","description":"Maximum number of results to return. Defaults to 20, capped at 100."}}})),
        ("hybrid_search", "Perform a hybrid search combining lexical BM25 candidates and semantic vector candidates to produce ranked, context-rich results.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The search query to match lexically and conceptually against the codebase."},"limit":{"type":"integer","description":"Maximum number of results to return. Defaults to 20, capped at 100."}}})),
        ("explain_search_result", "Run a hybrid search and return detailed, explainable ranking scores and evidence for the top retrieved code snippets.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The search query to analyze and explain results for."},"limit":{"type":"integer","description":"Maximum number of explained results. Defaults to 20, capped at 100."}}})),
        ("structural_search", "Perform a structural search across both symbol trees and code chunks to find structural matching syntax.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The query to run against structure trees and chunk indices."},"limit":{"type":"integer","description":"Maximum number of structural matches to return. Defaults to 20, capped at 100."}}})),
        ("get_definition", "Retrieve the definition location, file range, and body of a symbol (function, class, struct, trait, module) by its name.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The exact or partial name of the symbol to find the definition for."}}})),
        ("get_references", "Retrieve all references, usages, and call-sites of a given symbol throughout the indexed codebase.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The name of the symbol to find references for."},"limit":{"type":"integer","description":"Maximum number of references to return. Defaults to 20, capped at 100."}}})),
        ("get_implementations", "Find all implementations of a given interface, trait, or abstract class in the repository.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The name of the interface or trait."},"limit":{"type":"integer","description":"Maximum number of implementations to return. Defaults to 20, capped at 100."}}})),
        ("get_callers", "Find all parent functions, methods, or modules that call the target symbol.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The name of the symbol whose callers you want to find."},"limit":{"type":"integer","description":"Maximum number of callers to return. Defaults to 20, capped at 100."}}})),
        ("get_callees", "Find all functions, methods, or symbols called by the target symbol.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The name of the symbol whose calls you want to trace."},"limit":{"type":"integer","description":"Maximum number of callees to return. Defaults to 20, capped at 100."}}})),
        ("get_symbol_context", "Retrieve the comprehensive context of a symbol, including definition details, range, file context, and documentation if available.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The name of the symbol."}}})),
        ("dependency_path", "Trace the shortest dependency or reference path between two files or symbols, illustrating how they are connected.", json!({"type":"object","required":["from","to"],"properties":{"from":{"type":"string","description":"The starting node path or symbol name."},"to":{"type":"string","description":"The target node path or symbol name."}}})),
        ("impact_analysis", "Analyze the potential blast radius of a change to a file. Identifies downstream dependents, callers, and related test files.", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"The repository-relative path of the file to analyze."}}})),
        ("history_provenance_lookup", "Look up bounded commit provenance for exactly one repository-relative path or indexed symbol. Returns first-seen, last-touched, recent touches, confidence, and explicit uncertainty.", json!({"type":"object","properties":{"path":{"type":"string","description":"Repository-relative path to inspect."},"symbol":{"type":"string","description":"Exact symbol name, qualified name, or symbol ID to inspect."},"limit":{"type":"integer","description":"Maximum recent touches to return. Defaults to 20, capped at 100."}},"oneOf":[{"required":["path"]},{"required":["symbol"]}]})),
        ("module_dependencies", "List the direct dependency graph neighbors (imports and dependents) of a given file or symbol node.", json!({"type":"object","required":["node"],"properties":{"node":{"type":"string","description":"The file path or symbol node identifier."},"limit":{"type":"integer","description":"Maximum number of neighbors to return. Defaults to 20, capped at 100."}}})),
        ("build_context_pack", "Assemble a comprehensive, token-efficient context pack of files, symbols, and tests relevant to a natural language task description.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"A natural language description of the task to gather context for."},"limit":{"type":"integer","description":"Maximum number of context results to include. Defaults to 20."},"format":{"type":"string","enum":["json","markdown","toon"],"description":"The output format of the context pack."}}})),
        ("build_compressed_context", "Build a reversible compressed context pack with references and handles. Allows retrieving original snippets later to save prompt space.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"A natural language description of the task."},"limit":{"type":"integer","description":"Maximum number of context items. Defaults to 20."},"format":{"type":"string","enum":["json","toon"],"description":"The output format."}}})),
        ("retrieve_context", "Retrieve the original uncompressed source code snippet associated with a compressed context handle.", json!({"type":"object","required":["handle"],"properties":{"handle":{"type":"string","description":"The handle ID returned by build_compressed_context."}}})),
        ("plan_change", "Generate an evidence-backed pre-edit plan for a task, including primary files to edit, expected impact, and recommended test targets.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"A natural language description of the task or change to plan."},"limit":{"type":"integer","description":"Maximum planning results to generate. Defaults to 20."},"format":{"type":"string","enum":["json","markdown","toon"],"description":"The format of the plan."}}})),
        ("remember_fact", "Record a repository-scoped memory fact with metadata, entity links, and confidence parameters.", json!({"type":"object","required":["text"],"properties":{"text":{"type":"string","description":"The fact text to remember."},"source":{"type":"string","description":"The source or tool that observed the fact. Defaults to 'mcp'."},"confidence":{"type":"string","enum":["low","medium","high","exact"],"description":"The confidence level of the fact."}}})),
        ("search_memory", "Search through the append-only repository memory facts using keyword and entity matches.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The search query to match against facts."},"limit":{"type":"integer","description":"Maximum number of facts to return. Defaults to 20."}}})),
        ("explain_file", "Retrieve the metadata, syntax parsing status, and all code chunks for a single repository file.", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"The repository-relative path of the file."}}})),
        ("explain_symbol", "Retrieve definition, qualified name, range, and direct structural context for a given symbol name.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The symbol name to explain."}}})),
        ("explain_flow", "Summarize the high-level architecture flows and directory boundaries within the repository.", json!({"type":"object","properties":{}})),
        ("summarize_architecture", "Return a structured summary of the codebase architecture, including layer constraints and violation checks.", json!({"type":"object","properties":{}})),
        ("find_tests_for_change", "Analyze a file path and identify the test files that should be run to validate changes to it.", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"The repository-relative path of the file being changed."},"limit":{"type":"integer","description":"Maximum test recommendations to return. Defaults to 20."}}})),
        ("recommend_validation_plan", "Recommend a comprehensive validation plan (test targets, coverage checks, static checks) for a file change.", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"The repository-relative file path."},"limit":{"type":"integer","description":"Maximum recommendations to return. Defaults to 20."}}})),
        ("explain_test_coverage", "Retrieve and explain the test coverage metrics and associated test suites for a given file.", json!({"type":"object","properties":{"path":{"type":"string","description":"The repository-relative path of the file."},"limit":{"type":"integer","description":"Maximum coverage elements to return. Defaults to 20."}}})),
        ("propose_patch", "Propose a patch plan (file edits, context bounds) for a task. Read-only; does not write any files.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"A natural language description of the changes to propose."}}})),
        ("review_patch", "Review a proposed patch plan for safety, target constraints, and completeness.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"The task name or identifier associated with the patch."}}})),
        ("validate_patch", "Validate a patch plan against codebase boundaries and references to detect warnings.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"The task name or identifier."}}})),
        ("verify_change", "Verify an actual diff or set of changed files against a saved pre-edit plan. Validates constraints and runs test commands.", json!({"type":"object","properties":{"plan":{"type":"object","description":"A JSON object containing the saved PlanReport."},"plan_json":{"type":"string","description":"A JSON-encoded string representation of the PlanReport."},"diff":{"type":"string","description":"The unified diff showing the actual changes."},"changed_files":{"type":"array","items":{"type":"string"},"description":"List of repository-relative paths of changed files."},"evidence_refs":{"type":"array","items":{"type":"string"},"description":"List of evidence reference identifiers."},"run_commands":{"type":"boolean","description":"Set true to execute the validation commands defined in the plan."}}})),
        ("map_stacktrace_to_code", "Map a runtime stack trace to indexed source locations and file lines.", json!({"type":"object","properties":{"stacktrace":{"type":"string","description":"The stack trace string to analyze."}}})),
        ("find_errors_for_symbol", "Retrieve recent runtime errors and stack traces associated with a given symbol.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The symbol name to look up errors for."}}})),
        ("find_recent_failures", "Retrieve a list of recent runtime failures, errors, or incidents recorded in the repository.", json!({"type":"object","properties":{"limit":{"type":"integer","description":"Maximum number of failure entries to retrieve. Defaults to 20."}}})),
    ];

    let write_tools: &[(&str, &str, Value)] = &[(
        "apply_patch",
        "Apply an approved patch plan to the codebase. Requires write mode enabled and approval.",
        json!({"type":"object","required":["id","approved"],"properties":{"id":{"type":"string","description":"The identifier of the patch plan to apply."},"approved":{"type":"boolean","description":"Must be true to authorize applying the patch."}}}),
    )];

    let mut tools = Vec::new();
    let mut unstable = Vec::new();

    for (name, description, schema) in read_only_tools {
        let maturity = tool_maturity(name);
        if maturity == "experimental" {
            unstable.push(name.to_string());
            if config.mcp.hide_experimental {
                continue;
            }
        }
        tools.push(json!({
            "name": name,
            "description": description,
            "maturity": maturity,
            "experimental": maturity == "experimental",
            "inputSchema": schema
        }));
    }

    if config.security.allow_write {
        for (name, description, schema) in write_tools {
            let maturity = tool_maturity(name);
            if maturity == "experimental" {
                unstable.push(name.to_string());
                if config.mcp.hide_experimental {
                    continue;
                }
            }
            tools.push(json!({
                "name": name,
                "description": description,
                "maturity": maturity,
                "experimental": maturity == "experimental",
                "inputSchema": schema
            }));
        }
    }

    (tools, unstable)
}

fn tool_maturity(name: &str) -> &'static str {
    match name {
        "semantic_search"
        | "semantic_status"
        | "hybrid_search"
        | "explain_search_result"
        | "structural_search"
        | "get_implementations"
        | "get_callers"
        | "get_callees"
        | "history_provenance_lookup"
        | "explain_flow"
        | "map_stacktrace_to_code"
        | "find_errors_for_symbol"
        | "find_recent_failures"
        | "apply_patch" => "experimental",
        _ => "stable",
    }
}

fn limit(params: &Value) -> usize {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(100) as usize
}

fn required_str<'a>(params: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required string argument `{key}`"))
}

fn plan_from_params(params: &Value) -> anyhow::Result<PlanReport> {
    if let Some(plan) = params.get("plan") {
        return Ok(serde_json::from_value(plan.clone())?);
    }
    if let Some(plan_json) = params.get("plan_json").and_then(Value::as_str) {
        return Ok(serde_json::from_str(plan_json)?);
    }
    anyhow::bail!("verify_change requires `plan` object or `plan_json` string")
}

fn confidence_arg(params: &Value) -> Confidence {
    match params
        .get("confidence")
        .and_then(Value::as_str)
        .unwrap_or("medium")
        .to_ascii_lowercase()
        .as_str()
    {
        "low" => Confidence::Low,
        "high" => Confidence::High,
        "exact" => Confidence::Exact,
        _ => Confidence::Medium,
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

fn resolve_history_symbol(
    store: &dyn MetadataStore,
    query: &str,
) -> anyhow::Result<open_kioku_core::Symbol> {
    if let Some(symbol) = store.symbol_by_id(&SymbolId::new(query))? {
        return Ok(symbol);
    }
    let candidates = store.list_symbols(Some(query), 101, 0)?;
    let exact = candidates
        .iter()
        .filter(|symbol| symbol.name == query || symbol.qualified_name == query)
        .cloned()
        .collect::<Vec<_>>();
    match exact.as_slice() {
        [symbol] => Ok(symbol.clone()),
        [] if candidates.len() == 1 => Ok(candidates[0].clone()),
        [] if candidates.is_empty() => anyhow::bail!("symbol not found: {query}"),
        matches => {
            let ambiguous = if matches.is_empty() {
                &candidates
            } else {
                matches
            };
            let names = ambiguous
                .iter()
                .take(10)
                .map(|symbol| format!("{} [{}]", symbol.qualified_name, symbol.id.0))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!(
                "symbol query `{query}` is ambiguous; use a qualified name or symbol ID: {names}"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_config::OkConfig;
    use open_kioku_storage_sqlite::SqliteStore;
    use serde_json::json;
    use std::path::Path;

    #[tokio::test]
    async fn test_initialize_negotiates_version() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();

        let params = json!({"protocolVersion": "2024-11-05"});
        let result = dispatch(Path::new("."), &store, &config, "initialize", params)
            .await
            .unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "open-kioku");

        let params_other = json!({"protocolVersion": "2023-01-01"});
        let result_other = dispatch(Path::new("."), &store, &config, "initialize", params_other)
            .await
            .unwrap();
        assert_eq!(result_other["protocolVersion"], "2023-01-01");
    }

    #[tokio::test]
    async fn test_tools_list_respects_security_config() {
        let store = SqliteStore::open(":memory:").unwrap();

        let mut config_read_only = OkConfig::default();
        config_read_only.security.allow_write = false;

        let params = json!({});
        let result_ro = dispatch(
            Path::new("."),
            &store,
            &config_read_only,
            "tools/list",
            params.clone(),
        )
        .await
        .unwrap();
        let tools_ro = result_ro["tools"].as_array().unwrap();
        assert!(tools_ro.iter().all(|t| t["name"] != "apply_patch"));
        let provenance = tools_ro
            .iter()
            .find(|tool| tool["name"] == "history_provenance_lookup")
            .unwrap();
        assert_eq!(provenance["maturity"], "experimental");

        let mut config_write = OkConfig::default();
        config_write.security.allow_write = true;

        let result_rw = dispatch(Path::new("."), &store, &config_write, "tools/list", params)
            .await
            .unwrap();
        let tools_rw = result_rw["tools"].as_array().unwrap();
        assert!(tools_rw.iter().any(|t| t["name"] == "apply_patch"));
    }
}
