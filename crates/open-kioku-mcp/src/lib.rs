use open_kioku_actions::{ActionKind, PolicyGate};
use open_kioku_architecture::ArchitectureDetector;
use open_kioku_config::OkConfig;
use open_kioku_context::ContextPackBuilder;
use open_kioku_impact::ImpactEngine;
use open_kioku_patch::PatchPlanner;
use open_kioku_search_regex::search_chunks;
use open_kioku_search_tantivy::{default_index_dir, TantivySearchIndex};
use open_kioku_storage::{GraphStore, MetadataStore, OkStore, SearchIndex};
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
            let pack = ContextPackBuilder::new(store as &dyn OkStore).build(task, limit(&params))?;
            let format_arg = params.get("format").and_then(Value::as_str).unwrap_or("json");
            if format_arg == "markdown" {
                Ok(json!(open_kioku_context::ContextPackFormat::Markdown.render(&pack)?))
            } else {
                Ok(json!(pack))
            }
        }
        "impact_analysis" => {
            let path = required_str(&params, "path")?;
            Ok(json!(ImpactEngine::new(store).for_file(Path::new(path))?))
        }
        "find_tests_for_change" | "recommend_validation_plan" => {
            let path = required_str(&params, "path")?;
            Ok(json!(
                TestSelector::new(store).for_changed_path(Path::new(path), limit(&params))?
            ))
        }
        "detect_architecture" | "architecture_boundaries" | "architecture_violations" => {
            Ok(json!(ArchitectureDetector::new(store).detect()?))
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
        "get_implementations" | "semantic_search" | "structural_search" => {
            search_tool(repo, store, &params)
        }
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
            Ok(json!(ArchitectureDetector::new(store).detect()?))
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
        "map_stacktrace_to_code" | "find_errors_for_symbol" | "find_recent_failures" => Ok(
            json!({"results": [], "evidence": [], "confidence": "low", "reason": "runtime integrations are not configured"}),
        ),
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
        dispatch(repo, store, config, name, args).await.map(|value| {
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

fn tools(config: &OkConfig) -> (Vec<Value>, Vec<String>) {
    let read_only_tools: &[(&str, &str, Value)] = &[
        ("repo_status", "Return the current index manifest (file count, symbol count, indexed_at)", json!({"type":"object","properties":{}})),
        ("list_files", "List all indexed files", json!({"type":"object","properties":{"limit":{"type":"integer","description":"Max results (default 20, max 100)"}}})),
        ("list_languages", "List all languages detected in the indexed repository", json!({"type":"object","properties":{}})),
        ("list_symbols", "List or search indexed symbols by name", json!({"type":"object","properties":{"query":{"type":"string","description":"Symbol name filter (substring match)"},"limit":{"type":"integer"}}})),
        ("search_symbols", "Search indexed symbols by name", json!({"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}}})),
        ("detect_architecture", "Detect high-level architectural components from file paths", json!({"type":"object","properties":{}})),
        ("architecture_boundaries", "Show architectural component boundaries", json!({"type":"object","properties":{}})),
        ("architecture_violations", "Report architecture boundary violations", json!({"type":"object","properties":{}})),
        ("search_code", "Lexical BM25 search across all indexed code chunks", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"Search query"},"limit":{"type":"integer"}}})),
        ("search_files", "Search indexed files by content", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer"}}})),
        ("regex_search", "Search indexed code using a regex pattern", json!({"type":"object","required":["pattern"],"properties":{"pattern":{"type":"string","description":"Regex pattern"},"limit":{"type":"integer"}}})),
        ("semantic_search", "Semantic similarity search (falls back to lexical when semantic is disabled)", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer"}}})),
        ("structural_search", "Structural search across symbols and chunks", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer"}}})),
        ("get_definition", "Find the definition of a symbol by name", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"Symbol name"}}})),
        ("get_references", "Find all references to a symbol", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer"}}})),
        ("get_implementations", "Find implementations of an interface or trait", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer"}}})),
        ("get_callers", "Find all callers of a symbol", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer"}}})),
        ("get_callees", "Find all symbols called by a given symbol", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer"}}})),
        ("get_symbol_context", "Get full context for a symbol (definition, file, range)", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"}}})),
        ("dependency_path", "Find the dependency path between two files or symbols", json!({"type":"object","required":["from","to"],"properties":{"from":{"type":"string","description":"Source file path or symbol name"},"to":{"type":"string","description":"Target file path or symbol name"}}})),
        ("impact_analysis", "Analyse the blast radius if a file is changed", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"Relative file path"}}})),
        ("module_dependencies", "List direct graph neighbours of a file or symbol node", json!({"type":"object","required":["node"],"properties":{"node":{"type":"string"},"limit":{"type":"integer"}}})),
        ("build_context_pack", "Build a full context pack (primary files, symbols, tests, patch boundaries) for an AI task", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"Natural language task description"},"limit":{"type":"integer"},"format":{"type":"string","enum":["json","markdown"],"description":"Output format"}}})),
        ("explain_file", "Return chunks and metadata for a single file", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"Relative file path"}}})),
        ("explain_symbol", "Return definition and context for a symbol", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"}}})),
        ("explain_flow", "Summarise the high-level architecture", json!({"type":"object","properties":{}})),
        ("summarize_architecture", "Return an architecture summary", json!({"type":"object","properties":{}})),
        ("find_tests_for_change", "Find tests that should be run when a file changes", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"},"limit":{"type":"integer"}}})),
        ("recommend_validation_plan", "Recommend a validation plan for a file change", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"},"limit":{"type":"integer"}}})),
        ("explain_test_coverage", "Explain test coverage for a file", json!({"type":"object","properties":{"path":{"type":"string"},"limit":{"type":"integer"}}})),
        ("propose_patch", "Propose a patch plan for a task (read-only; does not write files)", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"Natural language description of the change"}}})),
        ("review_patch", "Review a patch plan", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string"}}})),
        ("validate_patch", "Validate a patch plan against the index", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string"}}})),
        ("map_stacktrace_to_code", "Map a stack trace to indexed source locations", json!({"type":"object","properties":{"stacktrace":{"type":"string"}}})),
        ("find_errors_for_symbol", "Find runtime errors associated with a symbol", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string"}}})),
        ("find_recent_failures", "Find recent runtime failures in the indexed repository", json!({"type":"object","properties":{"limit":{"type":"integer"}}})),
    ];

    let write_tools: &[(&str, &str, Value)] = &[(
        "apply_patch",
        "Apply an approved patch plan (requires write mode and approval)",
        json!({"type":"object","required":["id","approved"],"properties":{"id":{"type":"string","description":"Patch plan ID"},"approved":{"type":"boolean","description":"Must be true to apply"}}}),
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
        | "structural_search"
        | "get_implementations"
        | "get_callers"
        | "get_callees"
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

        let mut config_write = OkConfig::default();
        config_write.security.allow_write = true;

        let result_rw = dispatch(Path::new("."), &store, &config_write, "tools/list", params)
            .await
            .unwrap();
        let tools_rw = result_rw["tools"].as_array().unwrap();
        assert!(tools_rw.iter().any(|t| t["name"] == "apply_patch"));
    }
}
