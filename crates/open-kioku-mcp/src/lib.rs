use anyhow::Context;
use open_kioku_actions::{ActionKind, PolicyGate};
use open_kioku_architecture::{
    evaluate_policy, evaluate_public_api_boundary, ArchitectureDetector, PolicyResolver,
};
use open_kioku_config::{load_architecture_policy, load_architecture_policy_from_path, OkConfig};
use open_kioku_context::{
    candidates::{
        CandidateRequest, CandidateStream, ContextCandidateSource, SearchIndexCandidateSource,
        StreamCandidate, UnavailableCandidateSource,
    },
    ContextPackBuilder,
};
use open_kioku_context_compress::ContextHandleStore;
use open_kioku_contract::{
    ChangeContractV1, ContractId, ContractStore, FsContractStore, StoredContractRecord,
};
use open_kioku_core::{
    Confidence, ContextHandleId, GraphEdgeType, GraphNodeType, PlanReport, PolicyCheckReport,
    PolicyComponentMatch, SimilarChangeQuery, SymbolId,
};
use open_kioku_impact::ImpactEngine;
use open_kioku_memory::RepoMemoryStore;
use open_kioku_patch::{
    ChangeVerifier, ContractVerificationReport, ContractVerifier, PatchPlanner, VerifyChangeInput,
};
use open_kioku_plan::{ContractBuilder, PlanEngine, PlanFormat, PreflightFormat, PreflightReport};
use open_kioku_search_regex::search_chunks;
use open_kioku_search_tantivy::{default_index_dir, TantivySearchIndex};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_sentry::disabled_response;
use open_kioku_storage::{GraphStore, HistoryStore, MetadataStore, OkStore, SearchIndex};
use open_kioku_storage_sqlite::SqliteStore;
use open_kioku_symbols::SymbolEngine;
use open_kioku_tests::TestSelector;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const MAX_MCP_LIMIT: usize = 100;
const MAX_MCP_FETCH: usize = 500;
const MAX_TOOL_TEXT_BYTES: usize = 120_000;
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const STORE_IDLE_TTL: Duration = Duration::from_secs(300);
const CONTINUATION_TTL_SECS: u64 = 900;

struct SemanticContextCandidateSource<'a> {
    manager: SemanticIndexManager<'a>,
}

impl<'a> ContextCandidateSource for SemanticContextCandidateSource<'a> {
    fn source(&self) -> open_kioku_core::RetrievalSourceKind {
        open_kioku_core::RetrievalSourceKind::SemanticVector
    }

    fn retrieve(&self, request: &CandidateRequest) -> open_kioku_errors::Result<CandidateStream> {
        let report = self.manager.search_with_path_prefixes(
            &request.task,
            request.limit,
            &request.scope.path_prefixes,
        )?;
        let rationale = format!(
            "local semantic-vector similarity; backend={} eligible={}/{} selectivity={} reason={}",
            report.routing.selected_backend,
            report.routing.eligible_candidate_count,
            report.routing.total_vector_count,
            report.routing.filter_selectivity,
            report.routing.routing_reason,
        );
        let mut stream = CandidateStream::success(
            open_kioku_core::RetrievalSourceKind::SemanticVector,
            report
                .results
                .into_iter()
                .map(|result| {
                    StreamCandidate::from_result(
                        result,
                        open_kioku_core::RetrievalAuthority::Heuristic,
                        rationale.clone(),
                    )
                })
                .collect(),
        );
        stream.caveats.extend(report.routing.caveats);
        Ok(stream)
    }
}

fn build_context_for_task(
    repo: &Path,
    store: &SqliteStore,
    config: &OkConfig,
    task: &str,
    limit: usize,
) -> anyhow::Result<open_kioku_core::ContextPack> {
    let search_dir = default_index_dir(repo);
    let builder = ContextPackBuilder::new(store as &dyn OkStore).with_history_store(Some(store));

    let mut lexical_index_source = None;
    let mut lexical_failure_source = None;
    if TantivySearchIndex::exists(&search_dir) {
        match TantivySearchIndex::open_or_create(&search_dir) {
            Ok(index) => lexical_index_source = Some(SearchIndexCandidateSource::new(index)),
            Err(err) => {
                lexical_failure_source = Some(UnavailableCandidateSource::new(
                    open_kioku_core::RetrievalSourceKind::Lexical,
                    format!("Tantivy lexical index unavailable; using regex fallback: {err}"),
                ));
            }
        }
    }
    let semantic_source = config
        .semantic
        .enabled
        .then(|| SemanticContextCandidateSource {
            manager: SemanticIndexManager::new(repo, store as &dyn MetadataStore, &config.semantic),
        });

    let mut sources = Vec::<&dyn ContextCandidateSource>::new();
    if let Some(source) = lexical_index_source.as_ref() {
        sources.push(source);
    } else if let Some(source) = lexical_failure_source.as_ref() {
        sources.push(source);
    }
    if let Some(source) = semantic_source.as_ref() {
        sources.push(source);
    }

    let mut pack = builder.build_with_sources(task, limit, &sources)?;
    pack.architecture_policy = configured_architecture_policy_report(repo, store)?;
    Ok(pack)
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
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
    let store_path = repo.join(".ok/index.sqlite");
    let mut store = SqliteStore::open(&store_path)?;
    let mut last_request = Instant::now();
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if store_idle_expired(last_request) {
            store = SqliteStore::open(&store_path)?;
        }
        last_request = Instant::now();
        if let Some(response) = handle_line(&repo, &store, &config, &line).await {
            stdout
                .write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())
                .await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

async fn handle_line(
    repo: &Path,
    store: &SqliteStore,
    config: &OkConfig,
    line: &str,
) -> Option<JsonRpcResponse> {
    match serde_json::from_str::<JsonRpcRequest>(line) {
        Ok(request) => handle_request(repo, store, config, request).await,
        Err(err) => Some(JsonRpcResponse {
            jsonrpc: "2.0",
            id: None,
            result: None,
            error: Some(json!({"code": -32700, "message": err.to_string()})),
        }),
    }
}

async fn handle_request(
    repo: &Path,
    store: &SqliteStore,
    config: &OkConfig,
    request: JsonRpcRequest,
) -> Option<JsonRpcResponse> {
    handle_request_with_timeout(repo, store, config, request, TOOL_TIMEOUT).await
}

async fn handle_request_with_timeout(
    repo: &Path,
    store: &SqliteStore,
    config: &OkConfig,
    request: JsonRpcRequest,
    timeout: Duration,
) -> Option<JsonRpcResponse> {
    let id = request.id.clone();
    id.as_ref()?;
    let Some(method) = request.method.as_deref() else {
        return Some(JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(json!({"code": -32600, "message": "missing required JSON-RPC method"})),
        });
    };
    let result = tokio::time::timeout(
        timeout,
        dispatch(repo, store, config, method, request.params),
    )
    .await;
    match result {
        Ok(Ok(value)) => Some(JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(value),
            error: None,
        }),
        Ok(Err(err)) => Some(JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(json!({"code": -32000, "message": err.to_string()})),
        }),
        Err(_) => Some(JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(
                json!({"code": -32001, "message": format!("MCP method `{method}` timed out after {}s", timeout.as_secs())}),
            ),
        }),
    }
}

fn store_idle_expired(last_request: Instant) -> bool {
    last_request.elapsed() > STORE_IDLE_TTL
}

fn analysis_semantics_compatibility_for_store(
    store: &SqliteStore,
) -> anyhow::Result<open_kioku_core::AnalysisSemanticsCompatibility> {
    let manifest = store.manifest()?;
    Ok(open_kioku_core::classify_analysis_semantics(
        manifest
            .as_ref()
            .and_then(|manifest| manifest.analysis_semantics.as_ref()),
        &open_kioku_core::AnalysisSemanticsState::current(),
    ))
}

fn require_authoritative_relationships(store: &SqliteStore) -> anyhow::Result<()> {
    let compatibility = analysis_semantics_compatibility_for_store(store)?;
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
        #[cfg(test)]
        "__test_sleep" => {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(json!({"slept": true}))
        }
        "repo_status" => {
            let manifest = store.manifest()?;
            let compatibility = analysis_semantics_compatibility_for_store(store)?;
            let mut status = serde_json::to_value(&manifest)?;
            if let Some(object) = status.as_object_mut() {
                object.insert(
                    "analysis_semantics_status".into(),
                    serde_json::to_value(compatibility)?,
                );
            }
            Ok(status)
        }
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
            let offset = offset(&params);
            Ok(paged_overfetch_response(
                "files",
                store.list_files(overfetch_limit(limit), offset)?,
                limit,
                offset,
            )?)
        }
        "list_symbols" | "search_symbols" => {
            let query = params.get("query").and_then(Value::as_str);
            let limit = limit(&params);
            let offset = offset(&params);
            Ok(paged_overfetch_response(
                "symbols",
                store.list_symbols(query, overfetch_limit(limit), offset)?,
                limit,
                offset,
            )?)
        }
        "search_code" | "search_files" => search_tool(repo, store, &params),
        "regex_search" => search_tool(repo, store, &params),
        "build_context_pack" => {
            let task = required_str(&params, "task")?;
            let pack = build_context_for_task(repo, store, config, task, limit(&params))?;
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
            let pack = build_context_for_task(repo, store, config, task, limit(&params))?;
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
            let task = if let Some(since) = params.get("since").and_then(Value::as_str) {
                task_with_changed_ranges(repo, task, since)?
            } else {
                task.to_string()
            };
            let memory_facts = RepoMemoryStore::open_repo(repo)?.search(&task, 8)?;
            let limit = limit(&params);
            let context = build_context_for_task(repo, store, config, &task, limit)?;
            let report = PlanEngine::new(store as &dyn OkStore)
                .with_history_store(Some(store))
                .with_memory_facts(memory_facts)
                .plan_from_context(&task, limit, context)?;
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
        "preflight_change" => {
            let task = required_str(&params, "task")?;
            let task = if let Some(since) = params.get("since").and_then(Value::as_str) {
                task_with_changed_ranges(repo, task, since)?
            } else {
                task.to_string()
            };
            let memory_facts = RepoMemoryStore::open_repo(repo)?.search(&task, 8)?;
            let limit = limit(&params);
            let context = build_context_for_task(repo, store, config, &task, limit)?;
            let plan = PlanEngine::new(store as &dyn OkStore)
                .with_history_store(Some(store))
                .with_memory_facts(memory_facts)
                .plan_from_context(&task, limit, context)?;
            let report = PreflightReport::from_plan(&plan);
            let format_arg = params
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("json");
            match format_arg {
                "markdown" => Ok(json!(PreflightFormat::Markdown.render(&report)?)),
                "html" => Ok(json!(PreflightFormat::Html.render(&report)?)),
                "text" => Ok(json!(PreflightFormat::Text.render(&report)?)),
                _ => Ok(json!(report)),
            }
        }
        "create_change_contract" => {
            let plan = contract_plan_from_params(repo, store, &params)?;
            let contract = ContractBuilder::from_plan(&plan)?;
            let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
            let should_store = params.get("store").and_then(Value::as_bool).unwrap_or(true);
            if should_store {
                contract_store.save(&contract)?;
            }
            let output = ContractCreateToolOutput {
                contract_id: contract.id.0.clone(),
                stored: should_store,
                store_path: should_store.then(|| {
                    repo.join(".ok/contracts")
                        .join(format!("{}.json", contract.id.0))
                }),
                contract,
            };
            Ok(format_contract_create_output(
                &output,
                format_arg(&params, "json"),
            )?)
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
            require_authoritative_relationships(store)?;
            let path = required_str(&params, "path")?;
            let mut report = ImpactEngine::new(store)
                .with_history_store(Some(store))
                .for_file(Path::new(path))?;
            report.architecture_policy = configured_architecture_policy_report(repo, store)?;
            Ok(json!(report))
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
        "churn_analysis" => {
            let path = params.get("path").and_then(Value::as_str);
            let module = params.get("module").and_then(Value::as_str);
            let symbol = params.get("symbol").and_then(Value::as_str);
            let provided = usize::from(path.is_some())
                + usize::from(module.is_some())
                + usize::from(symbol.is_some());
            if provided != 1 {
                anyhow::bail!("provide exactly one of `path`, `module`, or `symbol`");
            }
            if let Some(path) = path {
                Ok(json!(store.churn_for_file(Path::new(path))?))
            } else if let Some(module) = module {
                Ok(json!(store.churn_for_module(Path::new(module))?))
            } else if let Some(query) = symbol {
                let symbol = resolve_history_symbol(store, query)?;
                Ok(json!(store.churn_for_symbol(&symbol.id)?))
            } else {
                unreachable!("exactly one churn target was checked above");
            }
        }
        "history_similar_changes" => {
            let query = similar_change_query_from_params(&params)?;
            Ok(json!(store.similar_changes(&query, limit(&params))?))
        }
        "ownership_lookup" => {
            let path = Path::new(required_str(&params, "path")?);
            let components = ownership_components(repo, store, path)?;
            let memory_facts = ownership_memory_facts(repo, path, &components)?;
            Ok(json!(open_kioku_git::ownership_for_path(
                open_kioku_git::OwnershipInput {
                    repo,
                    path,
                    history: store,
                    memory_facts: &memory_facts,
                    components,
                }
            )?))
        }
        "reviewer_suggestions" => {
            let path = Path::new(required_str(&params, "path")?);
            let components = ownership_components(repo, store, path)?;
            let memory_facts = ownership_memory_facts(repo, path, &components)?;
            let ownership = open_kioku_git::ownership_for_path(open_kioku_git::OwnershipInput {
                repo,
                path,
                history: store,
                memory_facts: &memory_facts,
                components,
            })?;
            Ok(json!(open_kioku_git::suggest_reviewers(
                open_kioku_git::ReviewerSuggestionInput {
                    path,
                    history: store,
                    ownership: Some(&ownership),
                }
            )?))
        }
        "find_tests_for_change" | "recommend_validation_plan" => {
            let path = required_str(&params, "path")?;
            Ok(json!(
                TestSelector::new(store).for_changed_path(Path::new(path), limit(&params))?
            ))
        }
        "detect_architecture" => Ok(json!(ArchitectureDetector::new(store, None).detect()?)),
        "architecture_boundaries" | "architecture_violations" => {
            architecture_summary_tool(repo, store)
        }
        "architecture_policy_validate" => architecture_policy_validate_tool(repo, &params),
        "architecture_policy_check" => {
            let Some(policy) = load_architecture_policy(repo)? else {
                return Ok(json!(open_kioku_core::PolicyCheckReport {
                    configured: false,
                    uncertainty: vec![
                        "no architecture policy configured; dependency edges were not evaluated"
                            .into()
                    ],
                    ..Default::default()
                }));
            };
            let resolver = PolicyResolver::new(&policy)?;
            Ok(json!(evaluate_policy(store, &resolver, &policy)?))
        }
        "architecture_policy_explain" => {
            let Some(policy) = load_architecture_policy(repo)? else {
                return Ok(json!({
                    "configured": false,
                    "violations": [],
                    "exemptions": [],
                    "uncertainty": ["no architecture policy configured; public API boundaries were not evaluated"]
                }));
            };
            let resolver = PolicyResolver::new(&policy)?;
            architecture_policy_explain_tool(repo, store, &resolver, &policy, &params)
        }
        "get_definition" | "get_symbol_context" | "explain_symbol" => {
            let query = required_str(&params, "query")?;
            Ok(json!(SymbolEngine::new(store).definition(query)?))
        }
        "get_references" => {
            require_authoritative_relationships(store)?;
            let query = required_str(&params, "query")?;
            Ok(json!(
                SymbolEngine::new(store).references(query, limit(&params))?
            ))
        }
        "get_callers" | "get_callees" => {
            require_authoritative_relationships(store)?;
            let query = required_str(&params, "query")?;
            let symbol = SymbolEngine::new(store).definition(query)?;
            let node = format!("symbol:{}", symbol.id.0);
            let (nodes, edges) = store.neighbors(&node, MAX_MCP_FETCH)?;
            let callers = method == "get_callers";
            let edges = edges
                .into_iter()
                .filter(|edge| {
                    edge.edge_type == GraphEdgeType::Calls
                        && if callers {
                            edge.to.0 == node
                        } else {
                            edge.from.0 == node
                        }
                })
                .take(limit(&params))
                .collect::<Vec<_>>();
            let related_ids = edges
                .iter()
                .map(|edge| if callers { &edge.from.0 } else { &edge.to.0 })
                .collect::<std::collections::BTreeSet<_>>();
            let nodes = nodes
                .into_iter()
                .filter(|candidate| related_ids.contains(&candidate.id.0))
                .collect::<Vec<_>>();
            Ok(json!({"symbol": symbol, "nodes": nodes, "edges": edges}))
        }
        "semantic_status" => semantic_status_tool(repo, store, config),
        "semantic_search" => semantic_search_tool(repo, store, config, &params),
        "hybrid_search" => hybrid_search_tool(repo, store, config, &params),
        "explain_search_result" => hybrid_search_tool(repo, store, config, &params),
        "structural_search" => search_tool(repo, store, &params),
        "get_implementations" => {
            require_authoritative_relationships(store)?;
            implementation_lookup_tool(store, &params)
        }
        "dependency_path" => {
            require_authoritative_relationships(store)?;
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
            require_authoritative_relationships(store)?;
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
        "explain_flow" => explain_flow_tool(store, &params),
        "summarize_architecture" => architecture_summary_tool(repo, store),
        "explain_test_coverage" => {
            let path = params
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(json!(
                TestSelector::new(store).for_changed_path(Path::new(path), limit(&params))?
            ))
        }
        "propose_patch" => {
            let task = required_str(&params, "task")?;
            Ok(json!(
                PatchPlanner::new(config, store as &dyn OkStore).plan(task)?
            ))
        }
        "verify_change" => {
            let plan = plan_from_params(&params)?;
            let mut changed_files = params
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
            let mut unified_diff = params
                .get("diff")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(since) = params.get("since_plan").and_then(Value::as_str) {
                for change in changed_ranges_since(repo, since)? {
                    if let Some(path) = change.new_path.or(change.old_path) {
                        changed_files.push(path);
                    }
                }
                if unified_diff.is_none() {
                    unified_diff = git_diff_since(repo, since)?;
                }
            }
            let run_commands = params
                .get("run_commands")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let traceability_strict = params
                .get("traceability_strict")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let check_api_surface = params
                .get("check_api_surface")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let check_dependency_delta = params
                .get("check_dependency_delta")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let write_attestation = params
                .get("write_attestation")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let architecture_policy = load_architecture_policy(repo)?;
            let check_dependency_delta = check_dependency_delta || architecture_policy.is_some();
            let index_dir = default_index_dir(repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(index_dir)?)
            } else {
                None
            };
            let contract_store =
                write_attestation.then(|| FsContractStore::new(repo.join(".ok/contracts")));
            Ok(json!(ChangeVerifier::new(store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_contract_store(
                    contract_store
                        .as_ref()
                        .map(|store| store as &dyn ContractStore),
                )
                .verify(
                    repo,
                    &plan,
                    VerifyChangeInput {
                        changed_files,
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
                )?))
        }
        "verify_change_contract" => verify_change_contract_tool(repo, store, &params),
        "get_change_contract" => {
            let contract_id = required_str(&params, "contract_id")?;
            let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
            let contract = contract_store.load(&ContractId::new(contract_id))?;
            Ok(format_contract_output(
                &contract,
                format_arg(&params, "json"),
            )?)
        }
        "explain_verification" => {
            let report = verification_report_from_params(&params)?;
            let explanation = explain_verification_report(&report);
            Ok(format_verification_explanation(
                &explanation,
                format_arg(&params, "json"),
            )?)
        }
        "get_evidence_schema" => {
            let manifest = store.manifest().ok().flatten();
            let schema = open_kioku_graph::schema::current_schema_with_manifest(
                Some(store as &dyn open_kioku_storage::GraphStore),
                manifest.as_ref(),
            );
            let mut schema = serde_json::to_value(schema)?;
            let capabilities = [
                open_kioku_core::Language::Rust,
                open_kioku_core::Language::TypeScript,
                open_kioku_core::Language::JavaScript,
                open_kioku_core::Language::Python,
                open_kioku_core::Language::Java,
                open_kioku_core::Language::Go,
            ]
            .iter()
            .filter_map(open_kioku_resolution::semantic_capabilities_for)
            .collect::<Vec<_>>();
            let object = schema
                .as_object_mut()
                .context("evidence schema must serialize as a JSON object")?;
            object.insert(
                "relationship_semantic_capability_version".into(),
                json!(open_kioku_resolution::LANGUAGE_SEMANTIC_CAPABILITY_VERSION),
            );
            object.insert(
                "relationship_semantic_capabilities".into(),
                json!(capabilities),
            );
            object.insert(
                "analysis_semantics_status".into(),
                serde_json::to_value(analysis_semantics_compatibility_for_store(store)?)?,
            );
            Ok(schema)
        }
        "query_evidence_graph" => {
            require_authoritative_relationships(store)?;
            let query_str = params
                .get("query")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let limit = params
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .map(|n| n as usize);
            let offset = params
                .get("offset")
                .and_then(serde_json::Value::as_u64)
                .map(|n| n as usize);

            let ast = match open_kioku_graph::query::parse_graph_query(query_str) {
                Ok(ast) => ast,
                Err(e) => {
                    let (kind, message) = match e {
                        open_kioku_graph::query::GraphQueryError::ParseError(m) => {
                            ("parse_error", m)
                        }
                        open_kioku_graph::query::GraphQueryError::QueryRejected(m) => {
                            ("query_rejected", m)
                        }
                        open_kioku_graph::query::GraphQueryError::UnknownNodeType(m) => {
                            ("unknown_node_type", m)
                        }
                        open_kioku_graph::query::GraphQueryError::UnknownEdgeType(m) => {
                            ("unknown_edge_type", m)
                        }
                        open_kioku_graph::query::GraphQueryError::UnsupportedFilter(m) => {
                            ("unsupported_filter", m.clone())
                        }
                        open_kioku_graph::query::GraphQueryError::DepthLimitExceeded(requested) => {
                            (
                                "depth_limit_exceeded",
                                format!("requested {} exceeds limit", requested),
                            )
                        }
                        open_kioku_graph::query::GraphQueryError::LimitExceeded(requested) => (
                            "limit_exceeded",
                            format!("requested {} exceeds limit", requested),
                        ),
                        open_kioku_graph::query::GraphQueryError::UnboundVariable(m) => {
                            ("unbound_variable", m.clone())
                        }
                        open_kioku_graph::query::GraphQueryError::Timeout => {
                            ("timeout", "Query execution timed out".to_string())
                        }
                        open_kioku_graph::query::GraphQueryError::Storage(e) => {
                            ("storage_error", e.to_string())
                        }
                        open_kioku_graph::query::GraphQueryError::Serde(e) => {
                            ("serde_error", e.to_string())
                        }
                    };
                    return Ok(serde_json::json!({
                        "error": {
                            "kind": kind,
                            "message": message,
                        }
                    }));
                }
            };

            let mut options = open_kioku_graph::query::GraphQueryOptions::default();
            if let Some(l) = limit {
                options.limit = l.min(MAX_MCP_LIMIT);
            }
            if let Some(offset) = offset {
                options.offset = offset;
            }

            match open_kioku_graph::query::execute_graph_query(
                store as &dyn open_kioku_storage::GraphStore,
                &ast,
                options,
            ) {
                Ok(result) => graph_query_response(query_str, &params, result),
                Err(e) => {
                    let (kind, message) = match e {
                        open_kioku_graph::query::GraphQueryError::ParseError(m) => {
                            ("parse_error", m)
                        }
                        open_kioku_graph::query::GraphQueryError::QueryRejected(m) => {
                            ("query_rejected", m)
                        }
                        open_kioku_graph::query::GraphQueryError::UnknownNodeType(m) => {
                            ("unknown_node_type", m)
                        }
                        open_kioku_graph::query::GraphQueryError::UnknownEdgeType(m) => {
                            ("unknown_edge_type", m)
                        }
                        open_kioku_graph::query::GraphQueryError::UnsupportedFilter(m) => {
                            ("unsupported_filter", m.clone())
                        }
                        open_kioku_graph::query::GraphQueryError::DepthLimitExceeded(requested) => {
                            (
                                "depth_limit_exceeded",
                                format!("requested {} exceeds limit", requested),
                            )
                        }
                        open_kioku_graph::query::GraphQueryError::LimitExceeded(requested) => (
                            "limit_exceeded",
                            format!("requested {} exceeds limit", requested),
                        ),
                        open_kioku_graph::query::GraphQueryError::UnboundVariable(m) => {
                            ("unbound_variable", m.clone())
                        }
                        open_kioku_graph::query::GraphQueryError::Timeout => {
                            ("timeout", "Query execution timed out".to_string())
                        }
                        open_kioku_graph::query::GraphQueryError::Storage(e) => {
                            ("storage_error", e.to_string())
                        }
                        open_kioku_graph::query::GraphQueryError::Serde(e) => {
                            ("serde_error", e.to_string())
                        }
                    };
                    Ok(serde_json::json!({
                        "error": {
                            "kind": kind,
                            "message": message,
                        }
                    }))
                }
            }
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
                let mut text = if let Some(s) = value.as_str() {
                    s.to_string()
                } else {
                    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
                };
                let text_truncated = truncate_utf8(&mut text, MAX_TOOL_TEXT_BYTES);
                let structured_content = match value {
                    Value::Object(_) => value,
                    other => json!({ "value": other }),
                };
                let mut response = json!({
                    "content": [{"type": "text", "text": text}],
                    "structuredContent": structured_content
                });
                if text_truncated {
                    response["truncated"] = json!(true);
                    response["warnings"] = json!([format!(
                        "tool text content was truncated to {} bytes; structuredContent remains available",
                        MAX_TOOL_TEXT_BYTES
                    )]);
                }
                response
            })
    })
}

fn search_tool(repo: &Path, store: &dyn MetadataStore, params: &Value) -> anyhow::Result<Value> {
    let limit = limit(params);
    let offset = offset(params);
    let results = search_results(repo, store, params, search_fetch_limit(limit, offset))?;
    paged_bounded_slice_response("results", results, limit, offset)
}

fn search_results(
    repo: &Path,
    store: &dyn MetadataStore,
    params: &Value,
    fetch_limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let query = params
        .get("query")
        .or_else(|| params.get("pattern"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mode = params.get("mode").and_then(Value::as_str).unwrap_or("code");
    let index_dir = default_index_dir(repo);
    if TantivySearchIndex::exists(&index_dir) {
        let index = TantivySearchIndex::open_or_create(index_dir)?;
        if mode == "graph" {
            return Ok(index.search_graph(query, fetch_limit)?);
        }
        return Ok(index.search(query, fetch_limit)?);
    }
    if mode == "graph" {
        anyhow::bail!("graph search index is missing; run `ok index .` first");
    }
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = store.all_chunks()?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    Ok(search_chunks(
        &chunks,
        &files,
        &symbols,
        query,
        fetch_limit,
    )?)
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
        let limit = limit(params);
        let offset = offset(params);
        let results = manager.search(query, search_fetch_limit(limit, offset))?;
        let mut response = paged_bounded_slice_response("results", results, limit, offset)?;
        response["semantic_status"] = json!(status);
        return Ok(response);
    }
    let mut response = paged_slice_response::<open_kioku_core::SearchResult>(
        "results",
        Vec::new(),
        limit(params),
        offset(params),
    )?;
    response["semantic_status"] = json!(status);
    response["error"] = json!("semantic index is not ready; run `ok semantic index` first");
    Ok(response)
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
    let limit = limit(params);
    let offset = offset(params);
    let mut results = search_results(repo, store, params, search_fetch_limit(limit, offset))?;
    let mut semantic_config = config.semantic.clone();
    semantic_config.enabled = true;
    let manager = SemanticIndexManager::new(repo, store, &semantic_config);
    let status = manager.status();
    if status.ready {
        merge_semantic_results(
            &mut results,
            manager.search(query, search_fetch_limit(limit, offset))?,
        );
    }
    results.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.path.cmp(&right.path))
    });
    let mut response = paged_bounded_slice_response("results", results, limit, offset)?;
    response["semantic_status"] = json!(status);
    Ok(response)
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

fn tool_title(name: &str) -> String {
    let words = name
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("Open Kioku {words}")
}

fn tool_annotations(name: &str) -> Value {
    let mut read_only = true;
    let destructive = false;
    let mut idempotent = true;
    let mut open_world = false;

    match name {
        "build_compressed_context"
        | "create_change_contract"
        | "remember_fact"
        | "verify_change_contract" => {
            read_only = false;
            idempotent = false;
        }
        "verify_change" => {
            read_only = false;
            idempotent = false;
            open_world = true;
        }
        _ => {}
    }

    json!({
        "readOnlyHint": read_only,
        "destructiveHint": destructive,
        "idempotentHint": idempotent,
        "openWorldHint": open_world
    })
}

fn tool_output_schema() -> Value {
    json!({
        "type": "object",
        "description": "The MCP structuredContent object returned by this Open Kioku tool. Most tools expose their JSON fields directly; string or scalar tool outputs are wrapped as {\"value\": ...}.",
        "additionalProperties": true,
        "properties": {
            "value": {
                "description": "Wrapped non-object output, used for Markdown, TOON, or scalar responses.",
            }
        }
    })
}

fn tool_description(name: &str, base: &str) -> String {
    let guidance = match name {
        "repo_status" => "Use first to check whether the local index exists and is fresh enough before calling search, symbol, or graph tools. This is read-only and only inspects repository metadata.",
        "list_files" => "Use for broad repository inventory and pagination over indexed paths; use search_files when you already have a keyword or path fragment. This is read-only and returns indexed data only.",
        "list_languages" => "Use for a quick language/support overview before choosing language-specific investigation paths. This is read-only and does not rescan the repository.",
        "list_symbols" => "Use to browse the indexed symbol table or filter by exact substring match. Supports pagination via limit and offset. Do NOT use for fuzzy or ranked symbol search (use search_symbols), for retrieving a symbol's full definition body (use get_definition), or for comprehensive symbol context (use get_symbol_context). This is read-only and reads from the local index only.",
        "search_symbols" => "Use when an agent has an approximate or partial symbol name and needs fuzzy-ranked candidates from the indexed symbol table. Do NOT use for exact substring inventory (use list_symbols), retrieving a symbol's full body (use get_definition), or comprehensive symbol context with docs (use get_symbol_context). This is read-only and searches the local index only.",
        "detect_architecture" => "Use to infer high-level architectural components and layers from the repository directory structure using heuristic analysis. Do NOT use when enforced policy rules are needed (use architecture_policy_check), when specific boundary constraints are needed (use architecture_boundaries), or when file-level policy membership must be explained (use architecture_policy_explain). This is read-only and performs heuristic detection only.",
        "architecture_boundaries" => "Use to inspect configured architecture-policy components, path mappings, and dependency rules before planning or validating a change. When no policy exists, this returns heuristic components plus an explicit caveat; it does not invent constraints. Do NOT use for a policy syntax check (use architecture_policy_validate), a direct policy-only evaluation (use architecture_policy_check), or file-level membership evidence (use architecture_policy_explain). This is read-only.",
        "architecture_violations" => "Use to inspect evaluated architecture-policy violations with their configured component mappings and bounded unknown-edge evidence. When no policy exists, this returns an explicit unavailable result rather than inferred violations. Do NOT use to validate policy syntax (use architecture_policy_validate), for a policy-only report (use architecture_policy_check), or to explain a specific finding (use architecture_policy_explain). This is read-only.",
        "architecture_policy_validate" => "Use before architecture_policy_check when editing or debugging policy TOML syntax and component resolution. It validates policy shape only and does not evaluate graph edges.",
        "architecture_policy_check" => "Use to enforce architecture policy against indexed import/reference/call edges. Use architecture_policy_validate first for policy syntax errors and architecture_policy_explain for why a specific file or symbol matched. This is read-only.",
        "architecture_policy_explain" => "Use after a policy check to explain component membership, public API boundaries, or exemptions for one file, one symbol, or the whole repo. This is read-only and does not change policy.",
        "search_code" => "Use for lexical BM25 code search when exact identifiers, terms, routes, or config keys are known. Prefer semantic_search for conceptual queries, hybrid_search when both lexical and semantic evidence are needed, and regex_search for exact patterns. This is read-only.",
        "search_files" => "Use when the target is a file path, filename, or file-content keyword rather than a code symbol. Set mode=graph to search indexed graph-node documents. Do NOT use for browsing all files without a query (use list_files), for code snippet search (use search_code), or for exact regex line matching (use regex_search). This is read-only and searches the local Tantivy index only.",
        "regex_search" => "Use for exact regular-expression pattern matching against indexed source code lines. Best used after lexical or semantic search has narrowed the search area. Do NOT use for ranked keyword search (use search_code), natural-language concept search (use semantic_search or hybrid_search), or broad lexical candidate discovery (use search_code, or structural_search for a structure-shaped phrase). This is read-only.",
        "semantic_status" => "Use before calling semantic_search or hybrid_search to confirm that the local vector index exists, is ready, and is not stale. Do NOT use to build or rebuild the index (run `ok semantic index` via the CLI instead). This is read-only, inspects only local metadata, and never generates embeddings or contacts external services.",
        "semantic_search" => "Use for natural-language concepts when exact identifiers are unknown. Prefer search_code for exact terms and hybrid_search when results need both semantic recall and lexical precision. This is read-only.",
        "hybrid_search" => "Use as the default investigative search when both keyword precision and semantic recall are needed — merges candidates from the Tantivy BM25 index and the local semantic vector index, deduplicates by file path, and sorts by combined score. Do NOT use when only exact lexical matches are needed (use search_code), when only conceptual similarity matters (use semantic_search), or when regex patterns are required (use regex_search). Falls back to lexical-only results when the semantic index is not ready. This is read-only.",
        "explain_search_result" => "Use when search results need transparent justification before being relied upon in a plan — returns per-result BM25 and semantic score breakdowns, matched evidence spans, and ranking signal weights. Do NOT use for basic search without explanation (use hybrid_search) or when only lexical matches are needed (use search_code). This is read-only and combines the local Tantivy lexical index with the semantic vector index when available.",
        "structural_search" => "Use for experimental local candidate discovery with structure-shaped queries. It currently returns lexical candidates from the local index; inspect each result's match_reason and do not treat it as an AST-pattern match. Use regex_search for literal patterns, get_definition for a known symbol, or search_code for ordinary ranked keyword search. This is read-only.",
        "get_definition" => "Use after resolving a symbol name to retrieve its defining range and body. Prefer search_symbols for candidate discovery and get_symbol_context when surrounding references and documentation are needed. This is read-only.",
        "get_references" => "Use to find usages of a resolved symbol across the index. Prefer get_callers for caller-only relationships and impact_analysis for broader file-level blast radius. This is read-only.",
        "get_implementations" => "Use to retrieve verified implementation sites for a trait, interface, abstract class, or protocol from persisted IMPLEMENTS facts. Each result includes the implementing symbol when available plus parser provenance and confidence. Do NOT use for lexical candidates (use search_code) or all usages (use get_references). This is read-only and returns no result when the current index has no implementation evidence.",
        "get_callers" => "Use to trace inbound call-sites to a function, method, or callable symbol from the indexed call graph. Do NOT use for all reference types including imports and type usages (use get_references), for outbound calls (use get_callees), or for finding a route between two nodes (use dependency_path). Accuracy depends on tree-sitter and optional SCIP indexing depth. This is read-only.",
        "get_callees" => "Use to trace outbound calls made by a function or method. Prefer module_dependencies for file/module neighbors and dependency_path for a specific connection. This is read-only.",
        "get_symbol_context" => "Use to retrieve a comprehensive context bundle for one symbol, including its definition body, file location, enclosing scope, documentation comments, and surrounding code context when indexed. Returns more detail than get_definition. Do NOT use for simple definition lookup only (use get_definition), for fuzzy symbol search (use search_symbols), or for cross-reference tracing (use get_references). This is read-only and reads from the local index.",
        "dependency_path" => "Use to explain how two files or symbols are connected through indexed dependencies. Prefer module_dependencies for local neighbors and impact_analysis for downstream blast radius. This is read-only.",
        "impact_analysis" => "Use before editing a file to estimate the blast radius: downstream dependent files, caller functions, related test files, and architecture policy impact from the indexed dependency graph. Do NOT use when only test targets are needed (use find_tests_for_change) or for a comprehensive validation plan including static checks (use recommend_validation_plan). This is read-only and analyzes the local index only.",
        "history_provenance_lookup" => "Use when authorship timing or first/last-touch evidence is needed for exactly one path or symbol. Prefer churn_analysis for hotspot metrics and ownership_lookup for maintainers. This is read-only and reports uncertainty.",
        "churn_analysis" => "Use to identify hotspots for one path, module, or symbol using materialized git statistics. Prefer history_provenance_lookup for commit provenance and history_similar_changes for prior change examples. This is read-only.",
        "history_similar_changes" => "Use during planning to find prior commits related to a task, path, or symbol. Prefer churn_analysis for risk scoring and ownership_lookup for reviewer ownership. This is read-only.",
        "ownership_lookup" => "Use to identify likely owners for one path from CODEOWNERS, git history, and memory facts. Prefer reviewer_suggestions when choosing reviewers for a proposed change. This is read-only.",
        "reviewer_suggestions" => "Use when a change needs human reviewers and a path is known. Prefer ownership_lookup for ownership evidence without reviewer ranking. This is read-only.",
        "module_dependencies" => "Use to inspect direct dependency neighbors for one file or symbol. Prefer dependency_path for a route between two nodes and architecture_policy_check for rule violations. This is read-only.",
        "build_context_pack" => "Use before planning or editing to assemble a ranked bundle of relevant files, symbol definitions, test targets, git history evidence, and architecture policy context for a natural-language task description. Returns results in the specified format (JSON, Markdown, or TOON). Do NOT use when prompt token budget is tight (use build_compressed_context with retrievable handles instead) or when only test recommendations are needed (use find_tests_for_change). This is read-only and queries the local index and git history only.",
        "build_compressed_context" => "Use when a context pack is needed but prompt token budget is constrained. Returns compressed references with short handles instead of full source snippets, significantly reducing token count. Call retrieve_context later with a handle to expand the original snippet. Do NOT use when full inline context is acceptable (use build_context_pack) or when only test targets are needed (use find_tests_for_change). This tool writes reusable context handles under the .ok data directory and is not idempotent.",
        "retrieve_context" => "Use only with handles returned by build_compressed_context to recover original snippets. Prefer build_context_pack for a fresh task-level context bundle. This is read-only.",
        "plan_change" => "Use before editing to create an evidence-backed plan with expected files, ranges, impact, and tests. Prefer create_change_contract when the plan must be persisted and verified later. This is read-only.",
        "preflight_change" => "Use before a multi-file or risky edit to get one concise, evidence-backed start decision: confirmed edit files, likely impact, tests, risks, caveats, and evidence quality. This is read-only. Use plan_change when the full detailed plan is needed.",
        "create_change_contract" => "Use when a plan needs a durable verification contract for later review or CI evidence. By default it writes a contract under .ok/contracts; set store=false for a transient contract.",
        "get_change_contract" => "Use to retrieve a previously stored contract by id before verification or explanation. Prefer create_change_contract for new contracts. This is read-only.",
        "remember_fact" => "Use only for durable, repository-scoped facts (architectural decisions, ownership conventions, known anti-patterns) that an agent should recall across sessions. Appends an immutable record to the local .ok SQLite store; duplicates are not deduplicated. Do NOT use for transient session notes, per-task scratch data, or facts derivable from the live index. Call search_memory first to avoid recording redundant entries. This tool writes to local storage and is not idempotent.",
        "search_memory" => "Use to retrieve stored repository-scoped memory facts by keyword, entity, or text match from the local .ok SQLite store. Returns fact text, source, confidence, timestamp, and associated entities for each match. Do NOT use for current source code search (use search_code or hybrid_search) or for live index data (use the search and symbol tools). Use remember_fact to write new entries. This is read-only.",
        "explain_file" => "Use to retrieve comprehensive indexed metadata for one known repository-relative file, including language detection, syntax parsing status, all code chunks with line ranges, and associated symbols. Do NOT use to discover files by keyword (use search_files), for symbol-level context (use explain_symbol or get_symbol_context), or for architecture-level file analysis (use architecture_policy_explain). This is read-only and returns only previously indexed data.",
        "explain_symbol" => "Use for a concise explanation of one known symbol, returning its definition range, qualified name, kind, and direct structural relationships. Do NOT use for a comprehensive context bundle with docs and surrounding code (use get_symbol_context), for fuzzy symbol search (use search_symbols), or for cross-reference tracing (use get_references). This is read-only and reads from the local index.",
        "explain_flow" => "Use to retrieve graph-backed flow evidence from indexed endpoint nodes and directed CALLS edges, alongside heuristic architecture components. Each flow starts at an indexed endpoint and contains a bounded directed call path. Do NOT use for enforceable violations (use architecture_policy_check), for a components-only architecture report (use summarize_architecture), or for arbitrary graph traversal (use query_evidence_graph). This is read-only; repositories without persisted endpoint or call evidence return no flow entries.",
        "summarize_architecture" => "Use for a structured architecture overview with layer constraints and violation checks. Prefer explain_flow for a narrative and architecture_policy_explain for file-level evidence. This is read-only.",
        "find_tests_for_change" => "Use after identifying a changed file to select relevant test files that should be run to validate the change. Returns ranked test file paths with relevance scores based on naming conventions, import relationships, and co-change history. Do NOT use when a broader validation plan including static checks and coverage actions is needed (use recommend_validation_plan) or when checking existing coverage data (use explain_test_coverage). This is read-only.",
        "recommend_validation_plan" => "Use before finalizing a change to choose tests, static checks, and coverage actions for one path. Prefer find_tests_for_change for test-only recommendations. This is read-only.",
        "explain_test_coverage" => "Use to inspect stored test-coverage evidence for one file path or the whole repository. Returns associated test suites, coverage percentage when available, and test file associations from indexed metadata. Do NOT use to decide what tests to run next (use recommend_validation_plan) or to find tests for a specific change (use find_tests_for_change). This is read-only, does not execute any tests, and reports only previously indexed or stored coverage data.",
        "propose_patch" => "Use to draft a patch plan without modifying files. Prefer plan_change for evidence-backed planning, then apply approved source edits with the normal editor before using verify_change. This is read-only.",
        "verify_change" => "Use after code edits to compare an actual unified diff or changed file list against a PlanReport produced by plan_change. When run_commands=true, executes shell commands listed in the plan's validation section (test runners, linters) on the local machine. When write_attestation=true, persists timestamped pass/fail records under .ok/contracts/validation/. Do NOT use for pre-edit planning (use plan_change) or contract-based verification (use verify_change_contract). Side effects are conditional on boolean flags; with all flags false the tool is read-only.",
        "verify_change_contract" => "Use after edits to verify a diff against a stored or inline change contract. Stored contract ids append verification records under .ok/contracts, and command execution is opt-in via run_commands.",
        "explain_verification" => "Use after verify_change_contract to translate a verification report into a decision, failures, warnings, and next tests. This is read-only.",
        "map_stacktrace_to_code" => "Use when runtime stack trace text must be mapped to indexed source locations. Prefer find_errors_for_symbol when the symbol is known and recent stored failures are needed. This tool is read-only and returns disabled status when runtime integration is not configured.",
        "find_errors_for_symbol" => "Use to look up recently stored runtime errors and stack traces for one specific symbol name. Returns error messages, stack frames, and timestamps when runtime integration is configured. Do NOT use for ad-hoc stack trace text mapping (use map_stacktrace_to_code) or for a broad inventory of recent failures (use find_recent_failures). This is read-only and returns a disabled-status response when runtime error integration is not configured in the repository.",
        "find_recent_failures" => "Use to list recently stored runtime failures, errors, and incidents across the repository before beginning a debugging investigation. Returns failure entries with timestamps, error types, and affected symbols when runtime integration is configured. Do NOT use when errors for one specific symbol are needed (use find_errors_for_symbol) or for stack trace mapping (use map_stacktrace_to_code). This is read-only and returns a disabled-status response when runtime error integration is not configured.",
        "get_evidence_schema" => "Use before query_evidence_graph to learn available graph node types, edge types, properties, and the versioned Tier-1 relationship-semantic capability matrix. This is read-only and does not query graph data.",
        "query_evidence_graph" => "Use for advanced read-only evidence queries after inspecting get_evidence_schema. The query language is a constrained Cypher-like DSL, not full Cypher; prefer purpose-built tools when available.",
        _ => "Use when this exact indexed repository capability is needed. Prefer narrower sibling tools when they match the task. This tool reports local Open Kioku index data and does not contact external services.",
    };

    format!("{base} {guidance}")
}

fn tools(config: &OkConfig) -> (Vec<Value>, Vec<String>) {
    let read_only_tools: &[(&str, &str, Value)] = &[
        ("repo_status", "Retrieve the current repository index metadata, including file count, symbol count, chunk count, and the exact timestamp when the repository was last indexed.", json!({"type":"object","properties":{}})),
        ("list_files", "List all indexed files within the repository. Returns metadata such as relative path, size in bytes, and language. Useful for codebase structure discovery.", json!({"type":"object","properties":{"limit":{"type":"integer","description":"Maximum number of files to return. Defaults to 20, capped at 100."},"offset":{"type":"integer","description":"Number of matching files to skip. Defaults to 0."}}})),
        ("list_languages", "List all programming languages detected and indexed in the repository, alongside support status.", json!({"type":"object","properties":{}})),
        ("list_symbols", "List or substring-filter all indexed code symbols (functions, classes, structs, traits, interfaces) with pagination. Returns symbol name, kind, file path, and line range for each entry.", json!({"type":"object","properties":{"query":{"type":"string","description":"Substring query to filter symbol names by exact match. If omitted, returns all symbols ordered by name."},"limit":{"type":"integer","description":"Maximum number of symbols to return. Defaults to 20, capped at 100. Use with offset for pagination."},"offset":{"type":"integer","description":"Number of matching symbols to skip before returning results. Defaults to 0."}}})),
        ("search_symbols", "Search indexed code symbols (functions, classes, structs, traits, interfaces) by name using fuzzy matching. Returns ranked candidates with symbol name, kind, file path, and line range.", json!({"type":"object","properties":{"query":{"type":"string","description":"Fuzzy or exact search query for symbol names. Supports partial names and approximate matches."},"limit":{"type":"integer","description":"Maximum number of ranked results to return. Defaults to 20, capped at 100."},"offset":{"type":"integer","description":"Number of matching symbols to skip before returning results. Defaults to 0."}}})),
        ("detect_architecture", "Infer high-level architectural components and layers from the repository directory structure and file layout. Returns detected component names, directory paths, and inferred layer assignments using heuristic analysis.", json!({"type":"object","properties":{}})),
        ("architecture_boundaries", "Return configured architecture-policy components, path mappings, dependency rules, and the evaluated policy report. Without a policy, returns heuristic components and an explicit caveat.", json!({"type":"object","properties":{}})),
        ("architecture_violations", "Return evaluated configured architecture-policy violations, component mappings, and bounded unknown-edge evidence. Without a policy, returns an explicit unavailable result.", json!({"type":"object","properties":{}})),
        ("architecture_policy_validate", "Validate the resolved repository architecture policy, or an explicit policy TOML path, without evaluating indexed graph edges.", json!({"type":"object","properties":{"path":{"type":"string","description":"Optional repository-relative or absolute path to a standalone architecture policy TOML file."}}})),
        ("architecture_policy_check", "Evaluate repository-owned architecture policy dependency rules against indexed import, reference, and call graph edges. Returns allowed, forbidden, and unknown edge counts with bounded unknown samples.", json!({"type":"object","properties":{}})),
        ("architecture_policy_explain", "Explain architecture policy component, public API boundary, and exemption evidence for one indexed file, symbol, or the whole repository.", json!({"type":"object","properties":{"file":{"type":"string","description":"Repository-relative file path to explain."},"symbol":{"type":"string","description":"Indexed symbol name or qualified name to explain."},"scope":{"type":"string","enum":["repo"],"description":"Use `repo` to return repository-wide public API boundary findings."}},"oneOf":[{"required":["file"]},{"required":["symbol"]},{"required":["scope"]}]})),
        ("search_code", "Perform a lexical BM25 search across indexed code chunks. Set mode=graph to search indexed graph-node identifiers, qualified names, routes, config keys, and properties.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The search query containing terms, code patterns, identifiers, graph entity names, routes, or config keys."},"mode":{"type":"string","enum":["code","graph"],"description":"Search mode. Defaults to code; graph searches indexed graph-node documents."},"limit":{"type":"integer","description":"Maximum number of search results to return. Defaults to 20, capped at 100."},"offset":{"type":"integer","description":"Number of matching search results to skip. Defaults to 0."}}})),
        ("search_files", "Search indexed file names and contents for specific keywords or file path patterns, returning ranked file matches with path, size, and language metadata. Set mode=graph to search graph-node documents through the same index.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The search query to match against file paths and file contents. Accepts filenames, directory fragments, or content keywords."},"mode":{"type":"string","enum":["code","graph"],"description":"Search mode. 'code' (default) searches file names and contents; 'graph' searches indexed graph-node documents including entity names and properties."},"limit":{"type":"integer","description":"Maximum number of file results to return. Defaults to 20, capped at 100."},"offset":{"type":"integer","description":"Number of matching file results to skip before returning. Defaults to 0."}}})),
        ("regex_search", "Search indexed source code using a regular expression pattern, returning exact line-level matches with file path, line number, and matching line content for each hit.", json!({"type":"object","required":["pattern"],"properties":{"pattern":{"type":"string","description":"A valid regular expression pattern (Rust regex syntax) to match against indexed source code lines. Example: 'fn\\s+main' to find main function declarations."},"limit":{"type":"integer","description":"Maximum number of matching lines to return. Defaults to 20, capped at 100."},"offset":{"type":"integer","description":"Number of matching lines to skip before returning results. Defaults to 0."}}})),
        ("semantic_status", "Report the current readiness, document count, staleness, and configured embedding provider of the local semantic vector index. Returns a status object indicating whether semantic_search and hybrid_search can produce vector results.", json!({"type":"object","properties":{}})),
        ("semantic_search", "Search the local semantic vector index using natural language queries to retrieve conceptually related code snippets.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"Natural language search query expressing the concept or functionality you are looking for."},"limit":{"type":"integer","description":"Maximum number of results to return. Defaults to 20, capped at 100."},"offset":{"type":"integer","description":"Number of matching search results to skip. Defaults to 0."}}})),
        ("hybrid_search", "Perform a hybrid search that merges ranked candidates from the Tantivy BM25 lexical index and the local semantic vector index, deduplicates by file path, and returns combined-score-sorted results with evidence spans. Falls back to lexical-only when the semantic index is unavailable.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"Natural-language or keyword query to match both lexically (BM25) and semantically (vector similarity) against the indexed codebase. Supports identifiers, phrases, and conceptual descriptions."},"limit":{"type":"integer","description":"Maximum number of merged results to return. Defaults to 20, capped at 100."},"offset":{"type":"integer","description":"Number of merged results to skip before returning. Defaults to 0."}}})),
        ("explain_search_result", "Run a hybrid search and return per-result ranking breakdowns including BM25 text relevance, semantic similarity, graph proximity, and evidence spans for each retrieved code snippet.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The search query whose results should be explained with full score breakdowns and evidence."},"limit":{"type":"integer","description":"Maximum number of explained results to return. Defaults to 20, capped at 100."}}})),
        ("structural_search", "Use for experimental local candidate discovery with structure-shaped queries. Currently returns lexical candidates from the local index rather than guaranteed AST-pattern matches; inspect match_reason on each result.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"Structure-shaped query used for lexical candidate discovery; this is not yet a guaranteed AST pattern."},"limit":{"type":"integer","description":"Maximum number of candidate results to return. Defaults to 20, capped at 100."}}})),
        ("get_definition", "Retrieve the definition location, file range, and body of a symbol (function, class, struct, trait, module) by its name.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The exact or partial name of the symbol to find the definition for."}}})),
        ("get_references", "Retrieve all references, usages, and call-sites of a given symbol throughout the indexed codebase.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The name of the symbol to find references for."},"limit":{"type":"integer","description":"Maximum number of references to return. Defaults to 20, capped at 100."}}})),
        ("get_implementations", "Retrieve verified implementation sites for a trait, interface, abstract class, or protocol from persisted IMPLEMENTS facts. Each result includes parser provenance and confidence.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"Name of the interface, trait, abstract class, or protocol whose persisted implementation evidence is needed."},"limit":{"type":"integer","description":"Maximum number of verified implementation results to return. Defaults to 20, capped at 100."}}})),
        ("get_callers", "Find all inbound call-sites to a function, method, or callable symbol from the indexed call graph. Returns caller symbol name, file path, and line range for each call-site found.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The exact or partial name of the symbol whose inbound callers you want to find."},"limit":{"type":"integer","description":"Maximum number of caller entries to return. Defaults to 20, capped at 100."}}})),
        ("get_callees", "Find all functions, methods, or symbols called by the target symbol.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The name of the symbol whose calls you want to trace."},"limit":{"type":"integer","description":"Maximum number of callees to return. Defaults to 20, capped at 100."}}})),
        ("get_symbol_context", "Retrieve a comprehensive context bundle for one symbol, including its full definition body, file location and range, enclosing scope, documentation comments, and surrounding code context from the local index.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The exact or partial name of the symbol to retrieve comprehensive context for."}}})),
        ("dependency_path", "Trace the shortest dependency or reference path between two files or symbols, illustrating how they are connected.", json!({"type":"object","required":["from","to"],"properties":{"from":{"type":"string","description":"The starting node path or symbol name."},"to":{"type":"string","description":"The target node path or symbol name."}}})),
        ("impact_analysis", "Analyze the blast radius of a change to one repository-relative file using the indexed dependency graph. Returns ranked downstream dependent files, caller functions, related test files, and architecture policy impact with impact scores and relationship types.", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"The repository-relative path of the file to analyze for downstream impact (e.g., 'src/auth/handler.rs')."}}})),
        ("history_provenance_lookup", "Look up bounded commit provenance for exactly one repository-relative path or indexed symbol. Returns first-seen, last-touched, recent touches, confidence, and explicit uncertainty.", json!({"type":"object","properties":{"path":{"type":"string","description":"Repository-relative path to inspect."},"symbol":{"type":"string","description":"Exact symbol name, qualified name, or symbol ID to inspect."},"limit":{"type":"integer","description":"Maximum recent touches to return. Defaults to 20, capped at 100."}},"oneOf":[{"required":["path"]},{"required":["symbol"]}]})),
        ("churn_analysis", "Return materialized churn and hotspot stats for exactly one repository-relative path, module directory, or indexed symbol. Includes all-time, 30-day, 90-day, recency-weighted, hotspot score, confidence, and uncertainty without scanning raw commit history.", json!({"type":"object","properties":{"path":{"type":"string","description":"Repository-relative file path to inspect."},"module":{"type":"string","description":"Repository-relative module or directory path to inspect."},"symbol":{"type":"string","description":"Exact symbol name, qualified name, or symbol ID to inspect."}},"oneOf":[{"required":["path"]},{"required":["module"]},{"required":["symbol"]}]})),
        ("history_similar_changes", "Retrieve ranked similar historical commits using task text, paths, symbols, co-change neighborhoods, churn, and commit metadata. Returns evidence and confidence for each hit.", json!({"type":"object","properties":{"task":{"type":"string","description":"Natural-language task or change description."},"path":{"type":"string","description":"Single repository-relative path to match."},"paths":{"type":"array","items":{"type":"string"},"description":"Repository-relative paths to match."},"symbol":{"type":"string","description":"Single symbol name, qualified name, or symbol ID to match."},"symbols":{"type":"array","items":{"type":"string"},"description":"Symbol names, qualified names, or symbol IDs to match."},"limit":{"type":"integer","description":"Maximum similar changes to return. Defaults to 20, capped at 100."}}})),
        ("ownership_lookup", "Resolve ranked owner suggestions for one repository-relative path from CODEOWNERS, persisted local git history, and secondary repo memory facts. Returns source breakdown, confidence, staleness, component matches, and explicit uncertainty.", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"Repository-relative path to inspect."}}})),
        ("reviewer_suggestions", "Suggest ranked reviewers for one repository-relative path from stored review evidence when available, otherwise explicit ownership and git-author inference. Returns source type, rationale, confidence, availability, and fallback fields.", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"Repository-relative path to inspect."}}})),
        ("module_dependencies", "List the direct dependency graph neighbors (imports and dependents) of a given file or symbol node.", json!({"type":"object","required":["node"],"properties":{"node":{"type":"string","description":"The file path or symbol node identifier."},"limit":{"type":"integer","description":"Maximum number of neighbors to return. Defaults to 20, capped at 100."}}})),
        ("build_context_pack", "Assemble a ranked, token-efficient context pack of relevant files, symbol definitions, test targets, git history evidence, and architecture policy context for a natural-language task. Returns results in JSON, Markdown, or TOON format.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"A natural language description of the task to gather context for (e.g., 'refactor the authentication middleware to support OAuth2')."},"limit":{"type":"integer","description":"Maximum number of context items to include in the pack. Defaults to 20. Higher values increase completeness but also token cost."},"format":{"type":"string","enum":["json","markdown","toon"],"description":"Output format. 'json' returns structured objects, 'markdown' returns human-readable text, 'toon' returns token-optimized notation. Defaults to 'json' when omitted."}}})),
        ("build_compressed_context", "Build a compressed context pack with short handles instead of full source snippets, reducing token count for prompt-constrained scenarios. Use retrieve_context with the returned handles to expand original snippets on demand.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"A natural language description of the task to gather compressed context for."},"limit":{"type":"integer","description":"Maximum number of context items to compress. Defaults to 20. Higher values increase completeness but also stored handle count."},"format":{"type":"string","enum":["json","toon"],"description":"Output format. 'json' returns structured handle objects, 'toon' returns token-optimized notation with handles. Defaults to 'json' when omitted."}}})),
        ("retrieve_context", "Retrieve the original uncompressed source code snippet associated with a compressed context handle.", json!({"type":"object","required":["handle"],"properties":{"handle":{"type":"string","description":"The handle ID returned by build_compressed_context."}}})),
        ("plan_change", "Generate an evidence-backed pre-edit plan for a task, including primary files to edit, expected impact, changed-line ranges, and recommended test targets.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"A natural language description of the task or change to plan."},"since":{"type":"string","description":"Optional git revision/range used with git diff --unified=0 to include changed files and line ranges in planning context."},"limit":{"type":"integer","description":"Maximum planning results to generate. Defaults to 20."},"format":{"type":"string","enum":["json","markdown","toon"],"description":"The format of the plan."}}})),
        ("preflight_change", "Return a concise pre-edit decision with confirmed edit files, likely affected files, validation commands, risks, caveats, evidence references, and evidence quality.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"A natural language description of the change to assess before editing."},"since":{"type":"string","description":"Optional git revision/range used with git diff --unified=0 to include changed files and ranges in planning context."},"limit":{"type":"integer","description":"Maximum planning results to consider. Defaults to 20."},"format":{"type":"string","enum":["json","markdown","html","text"],"description":"The preflight rendering. Defaults to json."}}})),
        ("create_change_contract", "Create and optionally store a versioned change contract from a task or saved plan while preserving plan_change for backwards compatibility.", json!({"type":"object","properties":{"task":{"type":"string","description":"Natural language task used to build a fresh plan before contract creation."},"plan":{"type":"object","description":"Inline PlanReport object used as the source plan."},"plan_json":{"type":"string","description":"JSON-encoded PlanReport used as the source plan."},"since":{"type":"string","description":"Optional git revision/range used with git diff --unified=0 when planning from task."},"limit":{"type":"integer","description":"Maximum planning results to generate when task is provided. Defaults to 20."},"store":{"type":"boolean","description":"Persist the contract under .ok/contracts. Defaults to true."},"format":{"type":"string","enum":["json","markdown","toon"],"description":"Return format. Defaults to json."}},"oneOf":[{"required":["task"]},{"required":["plan"]},{"required":["plan_json"]}]})),
        ("get_change_contract", "Retrieve a stored change contract by id and optionally export it as JSON, Markdown, or TOON.", json!({"type":"object","required":["contract_id"],"properties":{"contract_id":{"type":"string","description":"Stored contract id."},"format":{"type":"string","enum":["json","markdown","toon"],"description":"Return format. Defaults to json."}}})),
        ("remember_fact", "Persist a durable, repository-scoped memory fact into the local .ok SQLite store with optional source attribution and confidence level. The fact is append-only and survives re-indexing.", json!({"type":"object","required":["text"],"properties":{"text":{"type":"string","description":"The fact text to persist. Should be a complete, self-contained statement (e.g., 'The auth module uses JWT tokens with 24h expiry'). Maximum ~4KB."},"source":{"type":"string","description":"Identifier for the source that observed this fact (e.g., 'mcp', 'agent', 'human'). Defaults to 'mcp' when omitted."},"confidence":{"type":"string","enum":["low","medium","high","exact"],"description":"Confidence level indicating reliability of the fact. 'low' for uncertain inferences, 'exact' for verified truths. Defaults to 'medium' when omitted."}}})),
        ("search_memory", "Search the append-only repository memory store for previously recorded facts by keyword and entity match. Returns fact text, source, confidence level, and timestamp for each matching entry.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"Keyword or phrase to match against stored fact text and entity names."},"limit":{"type":"integer","description":"Maximum number of matching facts to return, ordered by relevance. Defaults to 20."}}})),
        ("explain_file", "Retrieve comprehensive indexed metadata for one repository-relative file, including language detection, syntax parsing status, all code chunks with line ranges, and associated symbol definitions.", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"Repository-relative path of the file to explain (e.g., 'src/main.rs' or 'lib/auth/handler.ts')."}}})),
        ("explain_symbol", "Retrieve a concise explanation of one indexed symbol, including its definition range, qualified name, kind (function, class, struct, trait), and direct structural relationships within the codebase.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The exact or partial name of the symbol to explain. Matches against indexed symbol names and qualified names."}}})),
        ("explain_flow", "Return graph-backed endpoint-to-call flow evidence, plus a heuristic architecture summary. Each flow contains an indexed endpoint and a bounded directed CALLS path.", json!({"type":"object","properties":{"limit":{"type":"integer","description":"Maximum endpoint flows to return. Defaults to 20, capped at 100."}}})),
        ("summarize_architecture", "Return a structured summary of the codebase architecture, including layer constraints and violation checks.", json!({"type":"object","properties":{}})),
        ("find_tests_for_change", "Identify test files that should be run to validate changes to one repository-relative file. Returns ranked test file paths with relevance scores based on naming conventions, import relationships, and co-change history.", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"Repository-relative path of the file being changed (e.g., 'src/auth/handler.rs')."},"limit":{"type":"integer","description":"Maximum number of test file recommendations to return, ranked by relevance. Defaults to 20."}}})),
        ("recommend_validation_plan", "Recommend a comprehensive validation plan (test targets, coverage checks, static checks) for a file change.", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"The repository-relative file path."},"limit":{"type":"integer","description":"Maximum recommendations to return. Defaults to 20."}}})),
        ("explain_test_coverage", "Retrieve stored test-coverage evidence and associated test suites for one repository-relative file or the whole repository. Returns coverage percentages, test file associations, and suite metadata from indexed data.", json!({"type":"object","properties":{"path":{"type":"string","description":"Repository-relative path of the file to inspect coverage for. If omitted, returns repository-wide coverage summary."},"limit":{"type":"integer","description":"Maximum number of coverage entries to return. Defaults to 20."}}})),
        ("propose_patch", "Propose a patch plan (file edits, context bounds) for a task. Read-only; does not write any files.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"A natural language description of the changes to propose."}}})),
        ("verify_change", "Verify an actual unified diff or set of changed files against a saved PlanReport, checking boundary constraints, expected file coverage, API surface stability, and dependency policy. Optionally executes configured validation commands and persists timestamped attestation records.", json!({"type":"object","properties":{"plan":{"type":"object","description":"A JSON object containing the saved PlanReport to verify against."},"plan_json":{"type":"string","description":"A JSON-encoded string representation of the PlanReport to verify against."},"diff":{"type":"string","description":"The unified diff (git diff format) showing the actual changes to verify."},"since_plan":{"type":"string","description":"Git revision or range (e.g., 'HEAD~1', 'abc123..def456') used with git diff --unified=0 to derive changed files and diff input automatically."},"changed_files":{"type":"array","items":{"type":"string"},"description":"List of repository-relative paths of changed files. Used when diff is not provided."},"evidence_refs":{"type":"array","items":{"type":"string"},"description":"List of evidence reference identifiers supporting the change."},"traceability_strict":{"type":"boolean","description":"Set true to reject any evidence references not present in the saved plan, enforcing full traceability. Defaults to false (lenient mode allows extra evidence)."},"check_api_surface":{"type":"boolean","description":"Set true to detect public API surface changes (additions, removals, signature modifications) and flag them as warnings. Defaults to false."},"check_dependency_delta":{"type":"boolean","description":"Set true to detect dependency graph changes and flag forbidden dependency additions based on architecture policy. Defaults to false."},"run_commands":{"type":"boolean","description":"Set true to execute shell validation commands (test runners, linters) defined in the plan on the local machine. Commands run synchronously and their exit codes are recorded. Defaults to false."},"write_attestation":{"type":"boolean","description":"Set true together with run_commands to persist timestamped pass/fail attestation records under .ok/contracts/validation/. Has no effect when run_commands is false. Defaults to false."}}})),
        ("verify_change_contract", "Verify changed files or a diff against a stored or inline change contract. Stored contract ids append verification records to .ok/contracts.", json!({"type":"object","properties":{"contract_id":{"type":"string","description":"Stored contract id."},"contract":{"type":"object","description":"Inline ChangeContractV1 or StoredContractRecord object."},"contract_json":{"type":"string","description":"JSON-encoded ChangeContractV1 or StoredContractRecord."},"diff":{"type":"string","description":"The unified diff showing the actual changes."},"since_plan":{"type":"string","description":"Optional git revision/range used with git diff --unified=0 to derive changed files and diff input."},"changed_files":{"type":"array","items":{"type":"string"},"description":"List of repository-relative paths of changed files."},"evidence_refs":{"type":"array","items":{"type":"string"},"description":"List of evidence reference identifiers."},"traceability_strict":{"type":"boolean","description":"Set true to reject supplied evidence references that are not present in the contract."},"check_api_surface":{"type":"boolean","description":"Set true to detect public API additions, removals, and signature changes during verification."},"check_dependency_delta":{"type":"boolean","description":"Set true to detect dependency graph deltas and flag forbidden dependency additions."},"run_commands":{"type":"boolean","description":"Set true to execute validation commands defined in the contract."},"write_attestation":{"type":"boolean","description":"Set true with run_commands and a stored contract id to persist validation attestations."},"validation_attestations":{"type":"array","items":{"type":"object"},"description":"Previously recorded validation attestations to replay during verification."},"format":{"type":"string","enum":["json","markdown","toon"],"description":"Return format. Defaults to json."}},"oneOf":[{"required":["contract_id"]},{"required":["contract"]},{"required":["contract_json"]}]})),
        ("explain_verification", "Explain a contract verification report, including the decision, boundary failures, warnings, dependency deltas, validation attestations, and recommended tests.", json!({"type":"object","properties":{"verification":{"type":"object","description":"Inline ContractVerificationReport object."},"verification_json":{"type":"string","description":"JSON-encoded ContractVerificationReport."},"format":{"type":"string","enum":["json","markdown","toon"],"description":"Return format. Defaults to json."}},"oneOf":[{"required":["verification"]},{"required":["verification_json"]}]})),
        ("map_stacktrace_to_code", "Map a runtime stack trace to indexed source locations and file lines.", json!({"type":"object","properties":{"stacktrace":{"type":"string","description":"The stack trace string to analyze."}}})),
        ("find_errors_for_symbol", "Retrieve recently stored runtime errors and stack traces for one specific symbol from the local error store. Returns error messages, stack frames, and timestamps when runtime integration is configured; otherwise returns a disabled-status response.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The exact symbol name to look up stored runtime errors for."}}})),
        ("find_recent_failures", "Retrieve a list of recently stored runtime failures, errors, and incidents from the repository's local error store. Returns failure entries with timestamps, error types, and affected symbols when runtime integration is configured; otherwise returns a disabled-status response.", json!({"type":"object","properties":{"limit":{"type":"integer","description":"Maximum number of failure entries to retrieve, ordered by most recent. Defaults to 20."}}})),
        ("get_evidence_schema", "Retrieve the versioned schema defining supported graph types, query properties, and the Tier-1 relationship-semantic capability matrix.", json!({"type":"object","properties":{}})),
        ("query_evidence_graph", "Execute a read-only graph query using a constrained subset of Cypher. Call get_evidence_schema first to see available node/edge types. (Note: The DSL is NOT full Cypher). Output rows are JSON arrays aligned with the user-selected variables in `columns`.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The graph query string to execute."},"limit":{"type":"integer","description":"Maximum rows to return. Defaults to 50, capped at 100."},"offset":{"type":"integer","description":"Number of matching rows to skip. Defaults to 0."}}})),
    ];

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
            "title": tool_title(name),
            "description": tool_description(name, description),
            "maturity": maturity,
            "experimental": maturity == "experimental",
            "inputSchema": schema,
            "outputSchema": tool_output_schema(),
            "annotations": tool_annotations(name)
        }));
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
        | "churn_analysis"
        | "history_similar_changes"
        | "ownership_lookup"
        | "reviewer_suggestions"
        | "map_stacktrace_to_code"
        | "find_errors_for_symbol"
        | "find_recent_failures" => "experimental",
        _ => "stable",
    }
}

fn limit(params: &Value) -> usize {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .min(MAX_MCP_LIMIT as u64) as usize
}

fn offset(params: &Value) -> usize {
    params
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10_000) as usize
}

fn overfetch_limit(limit: usize) -> usize {
    limit.saturating_add(1).min(MAX_MCP_LIMIT + 1)
}

fn search_fetch_limit(limit: usize, offset: usize) -> usize {
    offset
        .saturating_add(limit)
        .saturating_add(1)
        .min(MAX_MCP_FETCH)
}

fn paged_overfetch_response<T>(
    key: &str,
    mut values: Vec<T>,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value>
where
    T: Serialize,
{
    let has_more = values.len() > limit;
    if has_more {
        values.truncate(limit);
    }
    paged_response(key, values, PageMetadata::new(limit, offset, has_more))
}

fn paged_slice_response<T>(
    key: &str,
    values: Vec<T>,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value>
where
    T: Serialize,
{
    let has_more = values.len() > offset.saturating_add(limit);
    paged_slice_response_with_metadata(key, values, PageMetadata::new(limit, offset, has_more))
}

fn paged_bounded_slice_response<T>(
    key: &str,
    values: Vec<T>,
    limit: usize,
    offset: usize,
) -> anyhow::Result<Value>
where
    T: Serialize,
{
    let has_more = values.len() > offset.saturating_add(limit);
    let fetch_was_capped = offset.saturating_add(limit).saturating_add(1) > MAX_MCP_FETCH
        && values.len() >= MAX_MCP_FETCH;
    let mut metadata = PageMetadata::new(limit, offset, has_more || fetch_was_capped);
    if fetch_was_capped {
        metadata.truncated = true;
        metadata.warnings.push(format!(
            "search results were scanned up to {MAX_MCP_FETCH} candidates; narrow the query or use a lower offset"
        ));
    }
    paged_slice_response_with_metadata(key, values, metadata)
}

fn paged_slice_response_with_metadata<T>(
    key: &str,
    values: Vec<T>,
    metadata: PageMetadata,
) -> anyhow::Result<Value>
where
    T: Serialize,
{
    let values = values
        .into_iter()
        .skip(metadata.offset)
        .take(metadata.limit)
        .collect::<Vec<_>>();
    paged_response(key, values, metadata)
}

struct PageMetadata {
    limit: usize,
    offset: usize,
    has_more: bool,
    truncated: bool,
    warnings: Vec<String>,
    caveats: Vec<String>,
}

impl PageMetadata {
    fn new(limit: usize, offset: usize, has_more: bool) -> Self {
        Self {
            limit,
            offset,
            has_more,
            truncated: false,
            warnings: Vec::new(),
            caveats: Vec::new(),
        }
    }
}

fn paged_response<T>(key: &str, values: Vec<T>, metadata: PageMetadata) -> anyhow::Result<Value>
where
    T: Serialize,
{
    let returned = values.len();
    let mut map = Map::new();
    map.insert(key.to_string(), serde_json::to_value(values)?);
    map.insert("returned".into(), json!(returned));
    map.insert("limit".into(), json!(metadata.limit));
    map.insert("offset".into(), json!(metadata.offset));
    map.insert("has_more".into(), json!(metadata.has_more));
    map.insert("truncated".into(), json!(metadata.truncated));
    map.insert("warnings".into(), json!(metadata.warnings));
    map.insert("caveats".into(), json!(metadata.caveats));
    Ok(Value::Object(map))
}

fn graph_query_response(
    query: &str,
    params: &Value,
    result: open_kioku_graph::query::GraphQueryResult,
) -> anyhow::Result<Value> {
    let has_more = result.has_more;
    let next_offset = result.offset.saturating_add(result.returned);
    let limit = result.limit;
    let mut value = serde_json::to_value(result)?;
    value["truncated"] = json!(false);
    if has_more {
        value["continuation"] = json!(continuation_handle("query_evidence_graph", params));
        value["expires_at"] = json!(continuation_expires_at());
        value["next"] = json!({
            "method": "query_evidence_graph",
            "arguments": {
                "query": query,
                "limit": limit,
                "offset": next_offset
            }
        });
    }
    Ok(value)
}

fn explain_flow_tool<S>(store: &S, params: &Value) -> anyhow::Result<Value>
where
    S: MetadataStore + GraphStore,
{
    let architecture = ArchitectureDetector::new(store, None).detect()?;
    let mut endpoints = store.nodes_by_type(GraphNodeType::Endpoint, MAX_MCP_FETCH, 0)?;
    endpoints.sort_by(|left, right| left.id.0.cmp(&right.id.0));

    let mut call_edges = store.edges_by_type(GraphEdgeType::Calls, MAX_MCP_FETCH, 0)?;
    call_edges.sort_by(|left, right| left.id.0.cmp(&right.id.0));

    let flow_candidates = endpoints
        .into_iter()
        .filter_map(|endpoint| {
            let call_path = endpoint
                .symbol_id
                .as_ref()
                .map(|symbol_id| flow_call_path(&format!("symbol:{}", symbol_id.0), &call_edges))
                .unwrap_or_default();
            (!call_path.is_empty()).then(|| json!({"entrypoint": endpoint, "call_path": call_path}))
        })
        .collect::<Vec<_>>();
    let endpoint_limit = limit(params);
    let has_more = flow_candidates.len() > endpoint_limit;
    let flows = flow_candidates
        .into_iter()
        .take(endpoint_limit)
        .collect::<Vec<_>>();

    let mut response = serde_json::to_value(architecture)?;
    {
        let response = response
            .as_object_mut()
            .context("architecture response must serialize as an object")?;
        response.insert("flows".into(), json!(flows));
        response.insert("returned".into(), json!(flows.len()));
        response.insert("limit".into(), json!(endpoint_limit));
        response.insert("has_more".into(), json!(has_more));
        response.insert(
            "caveats".into(),
            json!([
                "flows are derived only from persisted endpoint nodes and directed CALLS graph edges",
                "each flow includes up to four directed call hops; repositories without endpoint or call evidence return no flow entries"
            ]),
        );
    }
    Ok(response)
}

fn flow_call_path(
    start: &str,
    call_edges: &[open_kioku_core::GraphEdge],
) -> Vec<open_kioku_core::GraphEdge> {
    const MAX_FLOW_CALL_HOPS: usize = 4;

    let mut call_path = Vec::new();
    let mut current = start.to_string();
    for _ in 0..MAX_FLOW_CALL_HOPS {
        let Some(edge) = call_edges.iter().find(|edge| edge.from.0 == current) else {
            break;
        };
        current = edge.to.0.clone();
        call_path.push(edge.clone());
    }
    call_path
}

fn continuation_handle(method: &str, params: &Value) -> String {
    let mut hasher = DefaultHasher::new();
    method.hash(&mut hasher);
    serde_json::to_string(params)
        .unwrap_or_default()
        .hash(&mut hasher);
    format!("okc_{:016x}", hasher.finish())
}

fn continuation_expires_at() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_add(CONTINUATION_TTL_SECS)
}

fn truncate_utf8(text: &mut String, max_bytes: usize) -> bool {
    if text.len() <= max_bytes {
        return false;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.truncate(end);
    text.push_str("\n...[truncated]");
    true
}

fn configured_architecture_policy_report<S>(
    repo: &Path,
    store: &S,
) -> anyhow::Result<Option<PolicyCheckReport>>
where
    S: MetadataStore + GraphStore + ?Sized,
{
    let Some(policy) = load_architecture_policy(repo)? else {
        return Ok(None);
    };
    let resolver = PolicyResolver::new(&policy)?;
    Ok(Some(evaluate_policy(store, &resolver, &policy)?))
}

fn architecture_summary_tool(repo: &Path, store: &SqliteStore) -> anyhow::Result<Value> {
    let Some(policy) = load_architecture_policy(repo)? else {
        let mut summary = serde_json::to_value(ArchitectureDetector::new(store, None).detect()?)?;
        let response = summary
            .as_object_mut()
            .context("architecture summary must serialize as an object")?;
        response.insert("configured".into(), json!(false));
        response.insert("policy".into(), Value::Null);
        response.insert(
            "policy_check".into(),
            json!(PolicyCheckReport {
                configured: false,
                uncertainty: vec![
                    "no architecture policy configured; component boundaries are heuristic and dependency edges were not evaluated".into()
                ],
                ..PolicyCheckReport::default()
            }),
        );
        response.insert(
            "caveats".into(),
            json!([
                "configure .open-kioku/architecture.toml to evaluate declared component boundaries and violations"
            ]),
        );
        return Ok(summary);
    };

    let resolver = PolicyResolver::new(&policy)?;
    let mut summary = ArchitectureDetector::new(store, Some(&resolver)).detect()?;
    let policy_check = evaluate_policy(store, &resolver, &policy)?;
    summary.violations = policy_check.violations.clone();
    let mut summary = serde_json::to_value(summary)?;
    let response = summary
        .as_object_mut()
        .context("architecture summary must serialize as an object")?;
    response.insert("configured".into(), json!(true));
    response.insert("policy_source".into(), json!(policy.source.to_string()));
    response.insert("policy".into(), serde_json::to_value(policy)?);
    response.insert("policy_check".into(), serde_json::to_value(policy_check)?);
    response.insert("caveats".into(), json!([]));
    Ok(summary)
}

fn ownership_components(
    repo: &Path,
    store: &dyn MetadataStore,
    path: &Path,
) -> anyhow::Result<Vec<PolicyComponentMatch>> {
    if let Some(policy) = load_architecture_policy(repo)? {
        let resolver = PolicyResolver::new(&policy)?;
        return Ok(resolver.resolve_file(path));
    }

    let path_text = path.display().to_string();
    let summary = ArchitectureDetector::new(store, None).detect()?;
    Ok(summary
        .components
        .into_iter()
        .filter(|component| {
            component
                .paths
                .iter()
                .any(|candidate| candidate == &path_text)
        })
        .map(|component| PolicyComponentMatch {
            component_id: component.id,
            matched_glob: "inferred_component_path".into(),
        })
        .collect())
}

fn ownership_memory_facts(
    repo: &Path,
    path: &Path,
    components: &[PolicyComponentMatch],
) -> anyhow::Result<Vec<open_kioku_core::MemorySearchResult>> {
    let mut query_terms = vec![
        "ownership".to_string(),
        "owner".to_string(),
        "owners".to_string(),
        "maintainer".to_string(),
        path.display().to_string(),
    ];
    query_terms.extend(
        components
            .iter()
            .map(|component| component.component_id.clone()),
    );
    Ok(RepoMemoryStore::open_repo(repo)?.search(&query_terms.join(" "), 20)?)
}

fn architecture_policy_validate_tool(repo: &Path, params: &Value) -> anyhow::Result<Value> {
    let (policy, paths) = if let Some(path) = params.get("path").and_then(Value::as_str) {
        let path = Path::new(path);
        let path = if path.is_absolute() {
            path.to_path_buf()
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
    let source = policy.as_ref().map(|policy| policy.source);
    let configured = policy.is_some();
    let message = if let Some(source) = source {
        format!("Architecture policy is valid ({source}).")
    } else {
        "No architecture policy configured. Heuristic architecture detection remains active.".into()
    };
    Ok(json!({
        "valid": true,
        "configured": configured,
        "source": source,
        "paths": paths,
        "policy": policy,
        "message": message,
    }))
}

fn architecture_policy_explain_tool(
    repo: &Path,
    store: &SqliteStore,
    resolver: &PolicyResolver,
    policy: &open_kioku_config::ArchitecturePolicy,
    params: &Value,
) -> anyhow::Result<Value> {
    let file = params.get("file").and_then(Value::as_str);
    let symbol = params.get("symbol").and_then(Value::as_str);
    let scope = params.get("scope").and_then(Value::as_str);
    if let Some(scope) = scope {
        anyhow::ensure!(
            scope == "repo",
            "unsupported architecture policy scope `{scope}`"
        );
    }
    let selectors = file.is_some() as u8 + symbol.is_some() as u8 + scope.is_some() as u8;
    anyhow::ensure!(
        selectors <= 1,
        "provide exactly one of `file`, `symbol`, or `scope`"
    );
    let (query_kind, query, file_path, symbol_value) = match (file, symbol, scope) {
        (Some(path), None, None) => {
            let path = repo_relative_path(repo, Path::new(path));
            ("file", path.display().to_string(), Some(path), Value::Null)
        }
        (None, Some(query), None) => {
            let symbol = SymbolEngine::new(store).definition(query)?;
            let file_path = file_path_for_symbol(store, &symbol)?;
            (
                "symbol",
                query.to_string(),
                Some(file_path),
                serde_json::to_value(symbol)?,
            )
        }
        (None, None, Some("repo")) | (None, None, None) => {
            ("repo", repo.display().to_string(), None, Value::Null)
        }
        _ => unreachable!("selector count was validated above"),
    };
    let mut uncertainty = Vec::new();
    let components = if let Some(file_path) = &file_path {
        match resolver.resolve_node(file_path, None) {
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
    Ok(json!({
        "configured": true,
        "query_kind": query_kind,
        "query": query,
        "file_path": file_path,
        "symbol": symbol_value,
        "components": components,
        "violations": violations,
        "exemptions": exemptions,
        "uncertainty": uncertainty,
        "message": format!(
            "Architecture policy explanation for {query_kind} `{query}`: {} component match(es), {} violation(s), {} exemption(s).",
            components.len(),
            violations.len(),
            exemptions.len()
        )
    }))
}

fn file_path_for_symbol(
    store: &dyn MetadataStore,
    symbol: &open_kioku_core::Symbol,
) -> anyhow::Result<PathBuf> {
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

fn similar_change_query_from_params(params: &Value) -> anyhow::Result<SimilarChangeQuery> {
    let task = params
        .get("task")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut paths = Vec::new();
    if let Some(path) = params.get("path").and_then(Value::as_str) {
        paths.push(PathBuf::from(path));
    }
    if let Some(values) = params.get("paths").and_then(Value::as_array) {
        for value in values {
            let Some(path) = value.as_str() else {
                anyhow::bail!("`paths` must contain only strings");
            };
            paths.push(PathBuf::from(path));
        }
    }
    let mut symbols = Vec::new();
    if let Some(symbol) = params.get("symbol").and_then(Value::as_str) {
        symbols.push(symbol.to_string());
    }
    if let Some(values) = params.get("symbols").and_then(Value::as_array) {
        for value in values {
            let Some(symbol) = value.as_str() else {
                anyhow::bail!("`symbols` must contain only strings");
            };
            symbols.push(symbol.to_string());
        }
    }
    if task.is_none() && paths.is_empty() && symbols.is_empty() {
        anyhow::bail!("provide at least one of `task`, `path`/`paths`, or `symbol`/`symbols`");
    }
    Ok(SimilarChangeQuery {
        task,
        paths,
        symbols,
    })
}

fn required_str<'a>(params: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing required string argument `{key}`"))
}

fn changed_ranges_since(repo: &Path, since: &str) -> anyhow::Result<Vec<open_kioku_git::DiffFile>> {
    Ok(open_kioku_git::diff_unified_zero_since(repo, since)?)
}

fn task_with_changed_ranges(repo: &Path, task: &str, since: &str) -> anyhow::Result<String> {
    let changed = changed_ranges_since(repo, since)?;
    if changed.is_empty() {
        return Ok(format!(
            "{task}\n\nGit diff since `{since}` has no changed files."
        ));
    }
    let mut enriched =
        format!("{task}\n\nChanged files and line ranges from `git diff {since} --unified=0`:\n");
    for change in &changed {
        enriched.push_str("- ");
        enriched.push_str(&render_changed_range(change));
        enriched.push('\n');
    }
    Ok(enriched)
}

fn render_changed_range(change: &open_kioku_git::DiffFile) -> String {
    let path = change
        .new_path
        .as_ref()
        .or(change.old_path.as_ref())
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unknown>".into());
    let ranges = change
        .hunks
        .iter()
        .filter_map(|hunk| hunk.new_range.as_ref().or(hunk.old_range.as_ref()))
        .map(|range| {
            if range.start == range.end {
                range.start.to_string()
            } else {
                format!("{}-{}", range.start, range.end)
            }
        })
        .collect::<Vec<_>>();
    if ranges.is_empty() {
        format!("{:?} {}", change.status, path)
    } else {
        format!("{:?} {} lines {}", change.status, path, ranges.join(","))
    }
}

fn git_diff_since(repo: &Path, since: &str) -> anyhow::Result<Option<String>> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff", "--unified=0", "--no-ext-diff", "--relative"])
        .arg(since)
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(Some(String::from_utf8(output.stdout)?))
}

#[derive(Debug, Serialize)]
struct ContractCreateToolOutput {
    contract_id: String,
    stored: bool,
    store_path: Option<PathBuf>,
    contract: ChangeContractV1,
}

#[derive(Debug, Serialize)]
struct VerificationExplanationOutput {
    contract_id: String,
    decision: String,
    changed_files: Vec<String>,
    boundary_failures: Vec<String>,
    warnings: Vec<String>,
    dependency_deltas: Vec<String>,
    api_surface_findings: Vec<String>,
    validation_attestations: Vec<String>,
    recommended_tests: Vec<String>,
    evidence_refs: Vec<String>,
}

fn contract_plan_from_params(
    repo: &Path,
    store: &SqliteStore,
    params: &Value,
) -> anyhow::Result<PlanReport> {
    let task = params.get("task").and_then(Value::as_str);
    let plan = params.get("plan").filter(|value| !value.is_null());
    let plan_json = params.get("plan_json").and_then(Value::as_str);
    let selectors = task.is_some() as u8 + plan.is_some() as u8 + plan_json.is_some() as u8;
    anyhow::ensure!(
        selectors == 1,
        "create_change_contract requires exactly one of `task`, `plan`, or `plan_json`"
    );

    if let Some(plan) = plan {
        return Ok(serde_json::from_value(plan.clone())?);
    }
    if let Some(plan_json) = plan_json {
        return Ok(serde_json::from_str(plan_json)?);
    }

    let mut task = task.unwrap_or_default().to_string();
    if let Some(since) = params.get("since").and_then(Value::as_str) {
        task = task_with_changed_ranges(repo, &task, since)?;
    }
    let limit = limit(params);
    let memory_facts = RepoMemoryStore::open_repo(repo)?.search(&task, 8)?;
    let mut context = ContextPackBuilder::new(store as &dyn OkStore)
        .with_history_store(Some(store))
        .build(&task, limit)?;
    context.architecture_policy = configured_architecture_policy_report(repo, store)?;
    Ok(PlanEngine::new(store as &dyn OkStore)
        .with_history_store(Some(store))
        .with_memory_facts(memory_facts)
        .plan_from_context(&task, limit, context)?)
}

fn verify_change_contract_tool(
    repo: &Path,
    store: &SqliteStore,
    params: &Value,
) -> anyhow::Result<Value> {
    let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
    let (contract, stored) = contract_from_params(&contract_store, params)?;
    let write_attestation = bool_arg(params, "write_attestation");
    if write_attestation && !stored {
        anyhow::bail!("write_attestation requires a stored `contract_id`");
    }

    let mut changed_files = path_array_arg(params, "changed_files")
        .or_else(|| path_array_arg(params, "changed"))
        .unwrap_or_default();
    let mut unified_diff = params
        .get("diff")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(since) = params.get("since_plan").and_then(Value::as_str) {
        for change in changed_ranges_since(repo, since)? {
            if let Some(path) = change.new_path.or(change.old_path) {
                changed_files.push(path);
            }
        }
        if unified_diff.is_none() {
            unified_diff = git_diff_since(repo, since)?;
        }
    }

    let architecture_policy = load_architecture_policy(repo)?;
    let check_dependency_delta = bool_arg(params, "check_dependency_delta")
        || bool_arg(params, "check_deps")
        || architecture_policy.is_some();
    let validation_attestations = params
        .get("validation_attestations")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()?
        .unwrap_or_default();
    let index_dir = default_index_dir(repo);
    let search_index = if TantivySearchIndex::exists(&index_dir) {
        Some(TantivySearchIndex::open_or_create(index_dir)?)
    } else {
        None
    };

    let report = ContractVerifier::new(store as &dyn OkStore)
        .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
        .with_contract_store(if stored {
            Some(&contract_store as &dyn ContractStore)
        } else {
            None
        })
        .verify(
            repo,
            &contract,
            VerifyChangeInput {
                changed_files,
                unified_diff,
                evidence_refs: string_array_arg(params, "evidence_refs").unwrap_or_default(),
                run_commands: bool_arg(params, "run_commands"),
                write_attestation,
                validation_attestations,
                traceability_strict: bool_arg(params, "traceability_strict"),
                check_api_surface: bool_arg(params, "check_api_surface"),
                check_dependency_delta,
                architecture_policy,
                suppress_plan_validation_pending: false,
            },
        )?;
    format_contract_verification_output(&report, format_arg(params, "json"))
}

fn contract_from_params(
    store: &FsContractStore,
    params: &Value,
) -> anyhow::Result<(ChangeContractV1, bool)> {
    let contract_id = params
        .get("contract_id")
        .or_else(|| params.get("id"))
        .and_then(Value::as_str);
    let contract = params.get("contract").filter(|value| !value.is_null());
    let contract_json = params.get("contract_json").and_then(Value::as_str);
    let selectors =
        contract_id.is_some() as u8 + contract.is_some() as u8 + contract_json.is_some() as u8;
    anyhow::ensure!(
        selectors == 1,
        "contract input requires exactly one of `contract_id`, `contract`, or `contract_json`"
    );

    if let Some(id) = contract_id {
        return Ok((store.load(&ContractId::new(id))?, true));
    }
    if let Some(contract) = contract {
        return Ok((contract_from_value(contract.clone())?, false));
    }
    Ok((
        contract_from_json(contract_json.unwrap_or_default())?,
        false,
    ))
}

fn contract_from_value(value: Value) -> anyhow::Result<ChangeContractV1> {
    if let Ok(contract) = serde_json::from_value::<ChangeContractV1>(value.clone()) {
        return Ok(contract);
    }
    let record: StoredContractRecord = serde_json::from_value(value)?;
    Ok(record.contract)
}

fn contract_from_json(json: &str) -> anyhow::Result<ChangeContractV1> {
    if let Ok(contract) = serde_json::from_str::<ChangeContractV1>(json) {
        return Ok(contract);
    }
    let record: StoredContractRecord = serde_json::from_str(json)?;
    Ok(record.contract)
}

fn verification_report_from_params(params: &Value) -> anyhow::Result<ContractVerificationReport> {
    if let Some(report) = params
        .get("verification")
        .or_else(|| params.get("report"))
        .filter(|value| !value.is_null())
    {
        return Ok(serde_json::from_value(report.clone())?);
    }
    if let Some(json) = params.get("verification_json").and_then(Value::as_str) {
        return Ok(serde_json::from_str(json)?);
    }
    anyhow::bail!("explain_verification requires `verification` object or `verification_json`")
}

fn explain_verification_report(
    report: &ContractVerificationReport,
) -> VerificationExplanationOutput {
    VerificationExplanationOutput {
        contract_id: report.contract_id.clone(),
        decision: format!("{:?}", report.decision),
        changed_files: report
            .change_report
            .changed_files
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        boundary_failures: verification_finding_summaries(
            &report.change_report.boundary_violations,
        ),
        warnings: verification_finding_summaries(&report.change_report.warnings),
        dependency_deltas: report
            .change_report
            .dependency_deltas
            .iter()
            .map(|finding| {
                format!(
                    "{:?}: {} -> {} ({})",
                    finding.classification, finding.source, finding.target, finding.reason
                )
            })
            .collect(),
        api_surface_findings: report
            .api_surface
            .as_ref()
            .map(|surface| verification_finding_summaries(&surface.findings))
            .unwrap_or_default(),
        validation_attestations: report
            .change_report
            .validation_attestations
            .iter()
            .map(|attestation| attestation.id.clone())
            .collect(),
        recommended_tests: report
            .change_report
            .recommended_tests
            .iter()
            .map(|test| format!("{}: {}", test.name, test.reason))
            .collect(),
        evidence_refs: report.change_report.evidence_refs.clone(),
    }
}

fn format_arg<'a>(params: &'a Value, default: &'a str) -> &'a str {
    params
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or(default)
}

fn bool_arg(params: &Value, key: &str) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn path_array_arg(params: &Value, key: &str) -> Option<Vec<PathBuf>> {
    params.get(key).and_then(Value::as_array).map(|values| {
        values
            .iter()
            .filter_map(Value::as_str)
            .map(PathBuf::from)
            .collect()
    })
}

fn string_array_arg(params: &Value, key: &str) -> Option<Vec<String>> {
    params.get(key).and_then(Value::as_array).map(|values| {
        values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

fn format_contract_create_output(
    output: &ContractCreateToolOutput,
    format: &str,
) -> anyhow::Result<Value> {
    match format {
        "markdown" => Ok(json!(render_contract_create_markdown(output))),
        "toon" => Ok(json!(render_contract_create_toon(output))),
        "json" => Ok(json!(output)),
        other => anyhow::bail!("unsupported contract format `{other}`"),
    }
}

fn format_contract_output(contract: &ChangeContractV1, format: &str) -> anyhow::Result<Value> {
    match format {
        "markdown" => Ok(json!(render_contract_markdown(contract))),
        "toon" => Ok(json!(render_contract_toon(contract))),
        "json" => Ok(json!(contract)),
        other => anyhow::bail!("unsupported contract format `{other}`"),
    }
}

fn format_contract_verification_output(
    report: &ContractVerificationReport,
    format: &str,
) -> anyhow::Result<Value> {
    match format {
        "markdown" => Ok(json!(render_contract_verification_markdown(report))),
        "toon" => Ok(json!(render_contract_verification_toon(report))),
        "json" => Ok(json!(report)),
        other => anyhow::bail!("unsupported contract format `{other}`"),
    }
}

fn format_verification_explanation(
    explanation: &VerificationExplanationOutput,
    format: &str,
) -> anyhow::Result<Value> {
    match format {
        "markdown" => Ok(json!(render_verification_explanation_markdown(explanation))),
        "toon" => Ok(json!(render_verification_explanation_toon(explanation))),
        "json" => Ok(json!(explanation)),
        other => anyhow::bail!("unsupported verification explanation format `{other}`"),
    }
}

fn render_contract_create_markdown(output: &ContractCreateToolOutput) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Change Contract `{}`\n\n", output.contract_id));
    out.push_str(&format!("- Stored: `{}`\n", output.stored));
    if let Some(path) = &output.store_path {
        out.push_str(&format!("- Path: `{}`\n", path.display()));
    }
    out.push('\n');
    out.push_str(&render_contract_markdown(&output.contract));
    out
}

fn render_contract_create_toon(output: &ContractCreateToolOutput) -> String {
    let path = output
        .store_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    format!(
        "type: contract_create\nid: {}\nstored: {}\npath: {}\n{}",
        output.contract_id,
        output.stored,
        path,
        render_contract_toon(&output.contract)
    )
}

fn render_contract_markdown(contract: &ChangeContractV1) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Change Contract `{}`\n\n", contract.id.0));
    out.push_str(&format!("Task: {}\n\n", contract.task));
    push_markdown_list(
        &mut out,
        "Primary Files",
        &contract_files(&contract.primary_files),
    );
    push_markdown_list(
        &mut out,
        "Secondary Files",
        &contract_files(&contract.secondary_files),
    );
    push_markdown_list(
        &mut out,
        "Forbidden Files",
        &contract_files(&contract.forbidden_files),
    );
    push_markdown_list(
        &mut out,
        "Architecture Constraints",
        &contract
            .architecture_constraints
            .iter()
            .map(|constraint| {
                format!(
                    "{} ({:?}): {}",
                    constraint.rule, constraint.severity, constraint.reason
                )
            })
            .collect::<Vec<_>>(),
    );
    push_markdown_list(
        &mut out,
        "Validation Commands",
        &contract
            .validation_commands
            .iter()
            .map(|command| format!("{}: {}", command.command, command.reason))
            .collect::<Vec<_>>(),
    );
    if let Some(quality) = contract.extensions.get("evidence_quality") {
        out.push_str("\n## Evidence Quality\n\n");
        out.push_str(&format!("```json\n{}\n```\n", quality));
    }
    out.push_str(&format!(
        "\nRisk: `{:?}` {:.2}\nConfidence: `{:?}` {:.2}\nEvidence refs: `{}`\n",
        contract.risk.level,
        contract.risk.score,
        contract.confidence.level,
        contract.confidence.score,
        contract.evidence_refs.len()
    ));
    out
}

fn render_contract_toon(contract: &ChangeContractV1) -> String {
    let mut out = format!(
        "type: change_contract\nid: {}\ntask: {}\nrisk: {:?} {:.2}\nconfidence: {:?} {:.2}\n",
        contract.id.0,
        contract.task,
        contract.risk.level,
        contract.risk.score,
        contract.confidence.level,
        contract.confidence.score
    );
    push_toon_list(
        &mut out,
        "primary_files",
        &contract_files(&contract.primary_files),
    );
    push_toon_list(
        &mut out,
        "architecture_constraints",
        &contract
            .architecture_constraints
            .iter()
            .map(|constraint| constraint.rule.clone())
            .collect::<Vec<_>>(),
    );
    push_toon_list(
        &mut out,
        "validation_commands",
        &contract
            .validation_commands
            .iter()
            .map(|command| command.command.clone())
            .collect::<Vec<_>>(),
    );
    if let Some(quality) = contract.extensions.get("evidence_quality") {
        out.push_str(&format!("evidence_quality: {}\n", quality));
    }
    out
}

fn render_contract_verification_markdown(report: &ContractVerificationReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Contract Verification `{}`\n\nDecision: `{:?}`\n\n",
        report.contract_id, report.decision
    ));
    push_markdown_list(
        &mut out,
        "Changed Files",
        &report
            .change_report
            .changed_files
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
    );
    push_markdown_list(
        &mut out,
        "Boundary Failures",
        &verification_finding_summaries(&report.change_report.boundary_violations),
    );
    push_markdown_list(
        &mut out,
        "Warnings",
        &verification_finding_summaries(&report.change_report.warnings),
    );
    out.push_str(&format!(
        "\nEvidence quality: mode `{}`, freshness `{}`\n\n",
        report.policy_snapshot.evidence_quality.index_mode,
        report.policy_snapshot.evidence_quality.freshness
    ));
    push_markdown_list(
        &mut out,
        "Dependency Deltas",
        &report
            .change_report
            .dependency_deltas
            .iter()
            .map(|finding| {
                format!(
                    "{:?}: {} -> {} ({})",
                    finding.classification, finding.source, finding.target, finding.reason
                )
            })
            .collect::<Vec<_>>(),
    );
    out
}

fn render_contract_verification_toon(report: &ContractVerificationReport) -> String {
    let mut out = format!(
        "type: contract_verification\nid: {}\ndecision: {:?}\n",
        report.contract_id, report.decision
    );
    push_toon_list(
        &mut out,
        "changed_files",
        &report
            .change_report
            .changed_files
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
    );
    push_toon_list(
        &mut out,
        "dependency_deltas",
        &report
            .change_report
            .dependency_deltas
            .iter()
            .map(|finding| format!("{:?}:{}", finding.classification, finding.reason))
            .collect::<Vec<_>>(),
    );
    out
}

fn render_verification_explanation_markdown(explanation: &VerificationExplanationOutput) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Verification Explanation `{}`\n\nDecision: `{}`\n\n",
        explanation.contract_id, explanation.decision
    ));
    push_markdown_list(&mut out, "Changed Files", &explanation.changed_files);
    push_markdown_list(
        &mut out,
        "Boundary Failures",
        &explanation.boundary_failures,
    );
    push_markdown_list(&mut out, "Warnings", &explanation.warnings);
    push_markdown_list(
        &mut out,
        "Dependency Deltas",
        &explanation.dependency_deltas,
    );
    push_markdown_list(
        &mut out,
        "API Surface Findings",
        &explanation.api_surface_findings,
    );
    push_markdown_list(
        &mut out,
        "Validation Attestations",
        &explanation.validation_attestations,
    );
    push_markdown_list(
        &mut out,
        "Recommended Tests",
        &explanation.recommended_tests,
    );
    out
}

fn render_verification_explanation_toon(explanation: &VerificationExplanationOutput) -> String {
    let mut out = format!(
        "type: verification_explanation\nid: {}\ndecision: {}\n",
        explanation.contract_id, explanation.decision
    );
    push_toon_list(&mut out, "changed_files", &explanation.changed_files);
    push_toon_list(
        &mut out,
        "boundary_failures",
        &explanation.boundary_failures,
    );
    push_toon_list(&mut out, "warnings", &explanation.warnings);
    push_toon_list(
        &mut out,
        "dependency_deltas",
        &explanation.dependency_deltas,
    );
    out
}

fn contract_files(files: &[open_kioku_contract::ContractFile]) -> Vec<String> {
    files.iter().map(|file| file.as_str().to_string()).collect()
}

fn verification_finding_summaries(
    findings: &[open_kioku_patch::VerificationFinding],
) -> Vec<String> {
    findings
        .iter()
        .map(|finding| format!("{}: {}", finding.kind, finding.reason))
        .collect()
}

fn push_markdown_list(out: &mut String, title: &str, values: &[String]) {
    out.push_str(&format!("## {title}\n\n"));
    if values.is_empty() {
        out.push_str("- None\n\n");
    } else {
        for value in values {
            out.push_str(&format!("- `{value}`\n"));
        }
        out.push('\n');
    }
}

fn push_toon_list(out: &mut String, name: &str, values: &[String]) {
    out.push_str(&format!("{name}[{}]:\n", values.len()));
    for value in values {
        out.push_str(&format!("  - {value}\n"));
    }
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

fn implementation_lookup_tool(store: &dyn MetadataStore, params: &Value) -> anyhow::Result<Value> {
    let query = required_str(params, "query")?;
    let limit = limit(params);
    let matching_facts = store.implementation_facts_for_target(query, overfetch_limit(limit))?;
    let has_more = matching_facts.len() > limit;
    let mut implementations = Vec::new();
    for fact in matching_facts.into_iter().take(limit) {
        let implementation = fact
            .symbol_id
            .as_ref()
            .map(|symbol_id| store.symbol_by_id(symbol_id))
            .transpose()?;
        implementations.push(json!({
            "implementation": implementation,
            "evidence": fact,
        }));
    }
    Ok(json!({
        "query": query,
        "implementations": implementations,
        "returned": implementations.len(),
        "limit": limit,
        "has_more": has_more,
        "caveats": [
            "returns only persisted IMPLEMENTS facts; absent language or parser evidence yields no result"
        ],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use open_kioku_config::OkConfig;
    use open_kioku_core::{
        AnalysisFact, CodeChunk, Confidence, EdgeId, EvidenceSourceType, File, FileId,
        GitChangeKind, GitCommitId, GitCommitRecord, GitFileTouch, GraphEdge, GraphEdgeType,
        GraphNode, GraphNodeType, HistoryRecordId, HistorySnapshot, IndexManifest, Language,
        LineRange, NodeId, Owner, RepositoryId, Symbol, SymbolId, SymbolKind,
        HISTORY_SCHEMA_VERSION,
    };
    use open_kioku_search_tantivy::{default_index_dir, rebuild_disk_index_with_graph};
    use open_kioku_storage::{GraphStore, HistoryStore, IndexData, MetadataStore};
    use open_kioku_storage_sqlite::SqliteStore;
    use serde_json::{json, Value};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static SNAPSHOT_REPO_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    async fn json_rpc_protocol_edges_are_stable() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();

        let string_id = handle_line(
            Path::new("."),
            &store,
            &config,
            r#"{"jsonrpc":"2.0","id":"req-1","method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        )
        .await
        .expect("initialize request should return a response");
        assert_eq!(string_id.id, Some(json!("req-1")));
        assert_eq!(string_id.result.unwrap()["protocolVersion"], "2024-11-05");

        let numeric_id = handle_line(
            Path::new("."),
            &store,
            &config,
            r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{}}"#,
        )
        .await
        .expect("numeric request should return a response");
        assert_eq!(numeric_id.id, Some(json!(7)));
        assert!(numeric_id.error.is_none());

        let initialized_notification = handle_line(
            Path::new("."),
            &store,
            &config,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
        )
        .await;
        assert!(initialized_notification.is_none());

        let missing_method = handle_line(
            Path::new("."),
            &store,
            &config,
            r#"{"jsonrpc":"2.0","id":"missing-method","params":{}}"#,
        )
        .await
        .expect("invalid request with id should return an error response");
        assert_eq!(missing_method.id, Some(json!("missing-method")));
        assert_eq!(missing_method.error.unwrap()["code"], -32600);

        let malformed = handle_line(Path::new("."), &store, &config, "{")
            .await
            .expect("parse errors should return an error response");
        assert_eq!(malformed.id, None);
        assert_eq!(malformed.error.unwrap()["code"], -32700);

        let malformed_unicode = handle_line(
            Path::new("."),
            &store,
            &config,
            r#"{"jsonrpc":"2.0","id":"bad-unicode","method":"initialize","params":{"client":"\uD800"}}"#,
        )
        .await
        .expect("parse errors should return an error response");
        assert_eq!(malformed_unicode.id, None);
        assert_eq!(malformed_unicode.error.unwrap()["code"], -32700);

        let unknown_method = handle_line(
            Path::new("."),
            &store,
            &config,
            r#"{"jsonrpc":"2.0","id":"unknown-method","method":"missing_method","params":{}}"#,
        )
        .await
        .expect("unknown request with id should return an error response");
        assert_eq!(unknown_method.id, Some(json!("unknown-method")));
        assert_eq!(unknown_method.error.unwrap()["code"], -32000);

        let tool_error = handle_line(
            Path::new("."),
            &store,
            &config,
            r#"{"jsonrpc":"2.0","id":"tool-error","method":"tools/call","params":{"name":"missing_tool","arguments":{}}}"#,
        )
        .await
        .expect("tool error request should return an error response");
        assert_eq!(tool_error.id, Some(json!("tool-error")));
        let error = tool_error.error.unwrap();
        assert_eq!(error["code"], -32000);
        assert!(error["message"]
            .as_str()
            .unwrap()
            .contains("unknown MCP method or tool"));
    }

    #[tokio::test]
    async fn tool_timeout_and_idle_store_edges_are_stable() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();
        let timeout = handle_request_with_timeout(
            Path::new("."),
            &store,
            &config,
            JsonRpcRequest {
                id: Some(json!("timeout")),
                method: Some("__test_sleep".into()),
                params: json!({}),
            },
            Duration::from_millis(1),
        )
        .await
        .expect("timeout request should return an error response");
        assert_eq!(timeout.id, Some(json!("timeout")));
        assert_eq!(timeout.error.unwrap()["code"], -32001);

        let stale = Instant::now() - STORE_IDLE_TTL - Duration::from_secs(1);
        assert!(store_idle_expired(stale));
        assert!(!store_idle_expired(Instant::now()));
    }

    #[tokio::test]
    async fn golden_mcp_protocol_snapshots_are_stable() {
        let fixture = McpSnapshotFixture::new();
        for (name, line) in [
            (
                "repo_status.json",
                r#"{"jsonrpc":"2.0","id":"repo-status","method":"repo_status","params":{}}"#,
            ),
            (
                "get_evidence_schema.json",
                r#"{"jsonrpc":"2.0","id":"get-evidence-schema","method":"get_evidence_schema","params":{}}"#,
            ),
            (
                "query_evidence_graph.json",
                r#"{"jsonrpc":"2.0","id":"query-evidence-graph","method":"query_evidence_graph","params":{"query":"MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s LIMIT 1"}}"#,
            ),
            (
                "broad_graph_search.json",
                r#"{"jsonrpc":"2.0","id":"broad-graph-search","method":"search_code","params":{"query":"invoice id publish","mode":"graph","limit":1}}"#,
            ),
            (
                "malformed_request.json",
                r#"{"jsonrpc":"2.0","id":"malformed","method":"initialize","params":{"unterminated":}"#,
            ),
            (
                "tool_error.json",
                r#"{"jsonrpc":"2.0","id":"tool-error","method":"tools/call","params":{"name":"missing_tool","arguments":{}}}"#,
            ),
            (
                "pagination.json",
                r#"{"jsonrpc":"2.0","id":"pagination","method":"list_files","params":{"limit":1,"offset":0}}"#,
            ),
            (
                "history_provenance.json",
                r#"{"jsonrpc":"2.0","id":"history-provenance","method":"history_provenance_lookup","params":{"path":"src/billing.rs","limit":5}}"#,
            ),
            (
                "churn_analysis.json",
                r#"{"jsonrpc":"2.0","id":"churn-analysis","method":"churn_analysis","params":{"path":"src/billing.rs"}}"#,
            ),
            (
                "history_similar_changes.json",
                r#"{"jsonrpc":"2.0","id":"history-similar-changes","method":"history_similar_changes","params":{"task":"publish invoice","path":"src/billing.rs","limit":5}}"#,
            ),
            (
                "ownership_lookup.json",
                r#"{"jsonrpc":"2.0","id":"ownership-lookup","method":"ownership_lookup","params":{"path":"src/billing.rs"}}"#,
            ),
            (
                "reviewer_suggestions.json",
                r#"{"jsonrpc":"2.0","id":"reviewer-suggestions","method":"reviewer_suggestions","params":{"path":"src/billing.rs"}}"#,
            ),
            (
                "semantic_status.json",
                r#"{"jsonrpc":"2.0","id":"semantic-status","method":"semantic_status","params":{}}"#,
            ),
            (
                "semantic_search_not_ready.json",
                r#"{"jsonrpc":"2.0","id":"semantic-search","method":"semantic_search","params":{"query":"publish invoice","limit":1}}"#,
            ),
            (
                "hybrid_search_lexical_fallback.json",
                r#"{"jsonrpc":"2.0","id":"hybrid-search","method":"hybrid_search","params":{"query":"publish invoice","limit":1}}"#,
            ),
            (
                "explain_search_result_lexical_fallback.json",
                r#"{"jsonrpc":"2.0","id":"explain-search","method":"explain_search_result","params":{"query":"publish invoice","limit":1}}"#,
            ),
            (
                "structural_search.json",
                r#"{"jsonrpc":"2.0","id":"structural-search","method":"structural_search","params":{"query":"publish_invoice_event","limit":1}}"#,
            ),
            (
                "get_implementations.json",
                r#"{"jsonrpc":"2.0","id":"get-implementations","method":"get_implementations","params":{"query":"InvoicePublisher","limit":1}}"#,
            ),
            (
                "get_callees.json",
                r#"{"jsonrpc":"2.0","id":"get-callees","method":"get_callees","params":{"query":"publish_invoice_event","limit":1}}"#,
            ),
            (
                "get_callers.json",
                r#"{"jsonrpc":"2.0","id":"get-callers","method":"get_callers","params":{"query":"archive_invoice_event","limit":1}}"#,
            ),
            (
                "explain_flow.json",
                r#"{"jsonrpc":"2.0","id":"explain-flow","method":"explain_flow","params":{}}"#,
            ),
            (
                "map_stacktrace_to_code_disabled.json",
                r#"{"jsonrpc":"2.0","id":"map-stacktrace","method":"map_stacktrace_to_code","params":{"stacktrace":"Error at billing::publish_invoice_event"}}"#,
            ),
            (
                "find_errors_for_symbol_disabled.json",
                r#"{"jsonrpc":"2.0","id":"find-errors","method":"find_errors_for_symbol","params":{"query":"publish_invoice_event"}}"#,
            ),
            (
                "find_recent_failures_disabled.json",
                r#"{"jsonrpc":"2.0","id":"recent-failures","method":"find_recent_failures","params":{"limit":1}}"#,
            ),
        ] {
            let response = handle_line(&fixture.repo, &fixture.store, &fixture.config, line)
                .await
                .expect("snapshot request should return a response");
            assert_mcp_snapshot(name, &response);
        }
    }

    #[tokio::test]
    async fn architecture_summary_uses_configured_policy_for_components_and_violations() {
        let fixture = McpSnapshotFixture::new();
        let policy_dir = fixture.repo.join(".open-kioku");
        fs::create_dir_all(&policy_dir).unwrap();
        fs::write(
            policy_dir.join("architecture.toml"),
            r#"
version = "v1"

[[layers]]
id = "billing"
paths = ["src/**"]
"#,
        )
        .unwrap();

        let response = handle_line(
            &fixture.repo,
            &fixture.store,
            &fixture.config,
            r#"{"jsonrpc":"2.0","id":"architecture-summary","method":"summarize_architecture","params":{}}"#,
        )
        .await
        .expect("architecture summary should return a response");
        let result = response
            .result
            .expect("architecture summary should succeed");
        assert_eq!(result["configured"], true);
        assert_eq!(result["policy_source"], "canonical");
        assert_eq!(result["components"][0]["id"], "billing");
        assert_eq!(result["components"][0]["paths"][0], "src/billing.rs");
        assert_eq!(result["policy_check"]["configured"], true);
    }

    struct McpSnapshotFixture {
        repo: PathBuf,
        store: SqliteStore,
        config: OkConfig,
    }

    impl McpSnapshotFixture {
        fn new() -> Self {
            let repo = unique_snapshot_repo();
            fs::create_dir_all(repo.join(".github")).unwrap();
            fs::write(
                repo.join(".github/CODEOWNERS"),
                "src/billing.rs billing@example.com\n",
            )
            .unwrap();
            let store = SqliteStore::open(":memory:").unwrap();
            let manifest = fixture_manifest();
            let file = File {
                id: FileId::new("file-billing"),
                repository_id: RepositoryId::new("repo"),
                path: "src/billing.rs".into(),
                language: Language::Rust,
                size_bytes: 128,
                content_hash: "hash-billing".into(),
                is_generated: false,
                is_vendor: false,
            };
            let other_file = File {
                id: FileId::new("file-routes"),
                repository_id: RepositoryId::new("repo"),
                path: "src/routes.rs".into(),
                language: Language::Rust,
                size_bytes: 96,
                content_hash: "hash-routes".into(),
                is_generated: false,
                is_vendor: false,
            };
            let symbol = Symbol {
                id: SymbolId::new("symbol-publish"),
                name: "publish_invoice_event".into(),
                qualified_name: "billing::routes::publish_invoice_event".into(),
                kind: SymbolKind::Function,
                file_id: file.id.clone(),
                range: Some(LineRange::single(7)),
                language: Language::Rust,
                confidence: Confidence::Exact,
                provenance: EvidenceSourceType::Scip,
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility: open_kioku_core::Visibility::Unknown,
            };
            let secondary_symbol = Symbol {
                id: SymbolId::new("symbol-archive"),
                name: "archive_invoice_event".into(),
                qualified_name: "billing::routes::archive_invoice_event".into(),
                kind: SymbolKind::Function,
                file_id: file.id.clone(),
                range: Some(LineRange::single(12)),
                language: Language::Rust,
                confidence: Confidence::High,
                provenance: EvidenceSourceType::TreeSitter,
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility: open_kioku_core::Visibility::Unknown,
            };
            let implementation_symbol = Symbol {
                id: SymbolId::new("symbol-invoice-publisher-impl"),
                name: "InvoicePublisherImpl".into(),
                qualified_name: "billing::InvoicePublisherImpl".into(),
                kind: SymbolKind::Class,
                file_id: file.id.clone(),
                range: Some(LineRange::single(15)),
                language: Language::Java,
                confidence: Confidence::High,
                provenance: EvidenceSourceType::StaticAnalysis,
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility: open_kioku_core::Visibility::Unknown,
            };
            let chunk = CodeChunk {
                id: "chunk-publish".into(),
                file_id: file.id.clone(),
                range: LineRange { start: 7, end: 9 },
                language: Language::Rust,
                text: "pub fn publish_invoice_event() {}".into(),
                symbol_id: Some(symbol.id.clone()),
            };
            let files = vec![file.clone(), other_file];
            let symbols = vec![
                symbol.clone(),
                secondary_symbol.clone(),
                implementation_symbol.clone(),
            ];
            let chunks = vec![chunk];
            let analysis_facts = vec![AnalysisFact {
                id: "invoice-publisher-implementation".into(),
                file_id: file.id.clone(),
                symbol_id: Some(implementation_symbol.id.clone()),
                target: "InvoicePublisher".into(),
                target_kind: GraphNodeType::Interface,
                edge_type: GraphEdgeType::Implements,
                range: Some(LineRange::single(15)),
                confidence: Confidence::High,
                source: "fixture-static-analysis".into(),
                source_type: EvidenceSourceType::StaticAnalysis,
                message: "fixture implementation evidence".into(),
            }];
            store
                .replace_index(IndexData {
                    manifest: &manifest,
                    files: &files,
                    symbols: &symbols,
                    chunks: &chunks,
                    tests: &[],
                    imports: &[],
                    occurrences: &[],
                    analysis_facts: &analysis_facts,
                    scopes: &[],
                    bindings: &[],
                    call_sites: &[],
                })
                .unwrap();

            let file_node = GraphNode {
                id: NodeId::new("file:file-billing"),
                node_type: GraphNodeType::File,
                label: "src/billing.rs".into(),
                file_id: Some(file.id.clone()),
                properties: std::collections::BTreeMap::from([(
                    "path".into(),
                    json!("src/billing.rs"),
                )]),
                ..Default::default()
            };
            let symbol_node = GraphNode {
                id: NodeId::new("symbol:symbol-publish"),
                node_type: GraphNodeType::Function,
                label: "publish_invoice_event".into(),
                file_id: Some(file.id.clone()),
                symbol_id: Some(symbol.id.clone()),
                properties: std::collections::BTreeMap::from([(
                    "qualified_name".into(),
                    json!("billing::routes::publish_invoice_event"),
                )]),
                ..Default::default()
            };
            let endpoint_node = GraphNode {
                id: NodeId::new("route:publish-invoice"),
                node_type: GraphNodeType::Endpoint,
                label: "POST /api/v1/invoices/{invoiceId}/publish".into(),
                file_id: Some(file.id.clone()),
                symbol_id: Some(symbol.id.clone()),
                properties: std::collections::BTreeMap::from([
                    (
                        "route_path".into(),
                        json!("/api/v1/invoices/{invoiceId}/publish"),
                    ),
                    (
                        "qualified_name".into(),
                        json!("billing::routes::publish_invoice_event"),
                    ),
                ]),
                ..Default::default()
            };
            let secondary_symbol_node = GraphNode {
                id: NodeId::new("symbol:symbol-archive"),
                node_type: GraphNodeType::Function,
                label: "archive_invoice_event".into(),
                file_id: Some(file.id.clone()),
                symbol_id: Some(secondary_symbol.id.clone()),
                properties: std::collections::BTreeMap::from([(
                    "qualified_name".into(),
                    json!("billing::routes::archive_invoice_event"),
                )]),
                ..Default::default()
            };
            let graph_nodes = vec![
                file_node.clone(),
                symbol_node,
                secondary_symbol_node,
                endpoint_node,
            ];
            let graph_edges = vec![
                GraphEdge {
                    id: EdgeId::new("edge-file-defines-symbol"),
                    from: file_node.id.clone(),
                    to: NodeId::new("symbol:symbol-publish"),
                    edge_type: GraphEdgeType::Defines,
                    ..Default::default()
                },
                GraphEdge {
                    id: EdgeId::new("edge-file-defines-archive"),
                    from: file_node.id.clone(),
                    to: NodeId::new("symbol:symbol-archive"),
                    edge_type: GraphEdgeType::Defines,
                    ..Default::default()
                },
                GraphEdge {
                    id: EdgeId::new("edge-publish-calls-archive"),
                    from: NodeId::new("symbol:symbol-publish"),
                    to: NodeId::new("symbol:symbol-archive"),
                    edge_type: GraphEdgeType::Calls,
                    ..Default::default()
                },
            ];
            store.replace_graph(&graph_nodes, &graph_edges).unwrap();
            store
                .put_history_snapshot(&fixture_history_snapshot())
                .unwrap();
            rebuild_disk_index_with_graph(
                default_index_dir(&repo),
                &chunks,
                &files,
                &symbols,
                &graph_nodes,
            )
            .unwrap();

            Self {
                repo,
                store,
                config: OkConfig::default(),
            }
        }
    }

    impl Drop for McpSnapshotFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.repo);
        }
    }

    fn unique_snapshot_repo() -> PathBuf {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = SNAPSHOT_REPO_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let repo = std::env::temp_dir().join(format!(
            "open-kioku-mcp-snapshots-{}-{now}-{sequence}",
            std::process::id(),
        ));
        fs::create_dir_all(&repo).unwrap();
        repo
    }

    fn fixture_manifest() -> IndexManifest {
        let mut manifest: IndexManifest = serde_json::from_value(json!({
            "repository": {
                "id": "repo",
                "name": "mcp-fixture",
                "root": ".",
                "branch": "main",
                "commit": "abc123",
                "indexed_at": "2026-01-01T00:00:00Z"
            },
            "file_count": 2,
            "symbol_count": 2,
            "chunk_count": 1,
            "indexed_at": "2026-01-01T00:00:00Z",
            "schema_version": 1,
            "index_mode": "full",
            "phase_reports": []
        }))
        .unwrap();
        manifest.analysis_semantics = Some(open_kioku_core::AnalysisSemanticsState::current());
        manifest
    }

    fn fixture_history_snapshot() -> HistorySnapshot {
        let touched_at = Utc.with_ymd_and_hms(2026, 1, 2, 12, 0, 0).unwrap();
        let commit_id = GitCommitId::new("publish-invoice");
        HistorySnapshot {
            schema_version: HISTORY_SCHEMA_VERSION,
            commits: vec![GitCommitRecord {
                id: commit_id.clone(),
                parent_ids: Vec::new(),
                author: Owner {
                    name: "Billing Developer".into(),
                    email: Some("billing@example.com".into()),
                },
                committer: None,
                authored_at: touched_at,
                committed_at: touched_at,
                summary: "Publish invoice event".into(),
                message: "Publish invoice event from the billing route".into(),
                file_count: 1,
            }],
            file_touches: vec![GitFileTouch {
                id: HistoryRecordId::new("billing-touch"),
                commit_id,
                path: "src/billing.rs".into(),
                previous_path: None,
                change_kind: GitChangeKind::Added,
                additions: Some(12),
                deletions: Some(0),
                touched_at,
            }],
            symbol_touches: Vec::new(),
            cochange_edges: Vec::new(),
            reviewer_evidence: Vec::new(),
        }
    }

    fn assert_mcp_snapshot(name: &str, response: &JsonRpcResponse) {
        let mut value = serde_json::to_value(response).unwrap();
        normalize_mcp_snapshot(&mut value);
        let formatted = format!("{}\n", serde_json::to_string_pretty(&value).unwrap());
        let snapshot_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("snapshots/mcp");
        fs::create_dir_all(&snapshot_dir).unwrap();
        let snapshot_file = snapshot_dir.join(name);
        if snapshot_file.exists() && std::env::var("UPDATE_GOLDEN_SNAPSHOTS").is_err() {
            let expected = fs::read_to_string(&snapshot_file).unwrap();
            assert_eq!(
                expected.trim(),
                formatted.trim(),
                "MCP snapshot mismatch: {}",
                snapshot_file.display()
            );
        } else {
            fs::write(snapshot_file, formatted).unwrap();
        }
    }

    fn normalize_mcp_snapshot(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, value) in map.iter_mut() {
                    if key == "expires_at" {
                        *value = json!("<expires_at>");
                    } else if key == "freshness" {
                        *value = json!("<freshness>");
                    } else if key == "generated_at" {
                        *value = json!("<generated_at>");
                    } else if key == "indexed_at" {
                        *value = json!("<indexed_at>");
                    } else if key == "observed_at" {
                        *value = json!("<observed_at>");
                    } else if key == "current_dir" {
                        *value = json!("<current_dir>");
                    } else if matches!(
                        key.as_str(),
                        "score"
                            | "confidence"
                            | "raw_value"
                            | "normalized_value"
                            | "weight"
                            | "contribution"
                    ) && value.is_number()
                        || (key.ends_with("_score") && value.is_number())
                    {
                        *value = json!("<score>");
                    } else {
                        normalize_mcp_snapshot(value);
                    }
                }
            }
            Value::Array(values) => {
                for value in values {
                    normalize_mcp_snapshot(value);
                }
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn legacy_analysis_semantics_are_reported_and_relationship_reads_fail_closed() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();
        let mut manifest = fixture_manifest();
        manifest.analysis_semantics = None;
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[],
                symbols: &[],
                chunks: &[],
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let status = dispatch(Path::new("."), &store, &config, "repo_status", json!({}))
            .await
            .unwrap();
        assert_eq!(
            status["analysis_semantics_status"]["status"],
            "rebuild_required"
        );
        assert!(status["analysis_semantics_status"]["recommended_action"]
            .as_str()
            .unwrap()
            .contains("ok index"));

        let graph = dispatch(
            Path::new("."),
            &store,
            &config,
            "query_evidence_graph",
            json!({"query": "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s LIMIT 1"}),
        )
        .await
        .unwrap_err();
        let message = graph.to_string();
        assert!(message.contains("authoritative relationship evidence unavailable"));
        assert!(message.contains("RebuildRequired"));

        let files = dispatch(Path::new("."), &store, &config, "list_files", json!({}))
            .await
            .unwrap();
        assert_eq!(files["returned"], 0);
    }

    #[tokio::test]
    async fn query_evidence_graph_returns_metadata_and_continuation() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();
        let manifest = fixture_manifest();
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &[],
                symbols: &[],
                chunks: &[],
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();
        let root = GraphNode {
            id: NodeId::new("file:root"),
            node_type: GraphNodeType::File,
            label: "root".into(),
            ..Default::default()
        };
        let mut nodes = vec![root.clone()];
        let mut edges = Vec::new();
        for idx in 0..3 {
            let node = GraphNode {
                id: NodeId::new(format!("symbol:fn{idx}")),
                node_type: GraphNodeType::Function,
                label: format!("fn{idx}"),
                ..Default::default()
            };
            edges.push(GraphEdge {
                id: EdgeId::new(format!("edge:{idx}")),
                from: root.id.clone(),
                to: node.id.clone(),
                edge_type: GraphEdgeType::Defines,
                ..Default::default()
            });
            nodes.push(node);
        }
        store.replace_graph(&nodes, &edges).unwrap();

        let result = dispatch(
            Path::new("."),
            &store,
            &config,
            "query_evidence_graph",
            json!({
                "query": "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s LIMIT 2"
            }),
        )
        .await
        .unwrap();

        assert_eq!(result["returned"], 2);
        assert_eq!(result["limit"], 2);
        assert_eq!(result["offset"], 0);
        assert_eq!(result["has_more"], true);
        assert_eq!(result["truncated"], false);
        assert!(result["continuation"].as_str().unwrap().starts_with("okc_"));
        assert_eq!(result["next"]["arguments"]["offset"], 2);
    }

    #[test]
    fn paged_response_metadata_and_utf8_truncation_are_stable() {
        let page = paged_slice_response("results", vec![1, 2, 3, 4], 2, 1).unwrap();
        assert_eq!(page["results"], json!([2, 3]));
        assert_eq!(page["returned"], 2);
        assert_eq!(page["limit"], 2);
        assert_eq!(page["offset"], 1);
        assert_eq!(page["has_more"], true);
        assert_eq!(page["warnings"], json!([]));
        assert_eq!(page["caveats"], json!([]));

        let capped =
            paged_bounded_slice_response("results", (0..MAX_MCP_FETCH).collect(), 2, 499).unwrap();
        assert_eq!(capped["results"], json!([499]));
        assert_eq!(capped["returned"], 1);
        assert_eq!(capped["has_more"], true);
        assert_eq!(capped["truncated"], true);
        assert!(capped["warnings"][0]
            .as_str()
            .unwrap()
            .contains("scanned up to"));

        let mut text = "é".repeat(10);
        assert!(truncate_utf8(&mut text, 7));
        assert!(std::str::from_utf8(text.as_bytes()).is_ok());
        assert!(text.ends_with("...[truncated]"));
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
        for retired_tool in ["apply_patch", "review_patch", "validate_patch"] {
            assert!(
                tools_ro.iter().all(|tool| tool["name"] != retired_tool),
                "{retired_tool} must not be advertised"
            );
        }
        for tool in tools_ro {
            let name = tool["name"].as_str().unwrap();
            let title = tool["title"].as_str().unwrap_or_default();
            let description = tool["description"].as_str().unwrap_or_default();
            assert!(
                title.starts_with("Open Kioku ") && title.len() > name.len(),
                "{name} should expose a meaningful MCP title"
            );
            assert!(
                description.contains("Use ") && description.len() >= 160,
                "{name} should include TDQS usage guidance"
            );
            assert!(
                tool["inputSchema"].is_object(),
                "{name} should expose an input schema"
            );
            assert!(
                tool["outputSchema"].is_object(),
                "{name} should expose an output schema"
            );
            assert!(
                tool["annotations"]["readOnlyHint"].is_boolean(),
                "{name} should expose MCP annotations"
            );
            assert!(
                tool["annotations"]["destructiveHint"].is_boolean(),
                "{name} should expose destructiveHint"
            );
            assert!(
                tool["annotations"]["idempotentHint"].is_boolean(),
                "{name} should expose idempotentHint"
            );
            assert!(
                tool["annotations"]["openWorldHint"].is_boolean(),
                "{name} should expose openWorldHint"
            );
        }
        let provenance = tools_ro
            .iter()
            .find(|tool| tool["name"] == "history_provenance_lookup")
            .unwrap();
        assert_eq!(provenance["maturity"], "experimental");
        let churn = tools_ro
            .iter()
            .find(|tool| tool["name"] == "churn_analysis")
            .unwrap();
        assert_eq!(churn["maturity"], "experimental");
        let similar = tools_ro
            .iter()
            .find(|tool| tool["name"] == "history_similar_changes")
            .unwrap();
        assert_eq!(similar["maturity"], "experimental");
        let ownership = tools_ro
            .iter()
            .find(|tool| tool["name"] == "ownership_lookup")
            .unwrap();
        assert_eq!(ownership["maturity"], "experimental");
        let reviewer_suggestions = tools_ro
            .iter()
            .find(|tool| tool["name"] == "reviewer_suggestions")
            .unwrap();
        assert_eq!(reviewer_suggestions["maturity"], "experimental");
        let structural_search = tools_ro
            .iter()
            .find(|tool| tool["name"] == "structural_search")
            .unwrap();
        assert_eq!(structural_search["maturity"], "experimental");
        assert!(structural_search["description"]
            .as_str()
            .unwrap()
            .contains("lexical candidates"));
        let implementations = tools_ro
            .iter()
            .find(|tool| tool["name"] == "get_implementations")
            .unwrap();
        assert_eq!(implementations["maturity"], "experimental");
        assert!(implementations["description"]
            .as_str()
            .unwrap()
            .contains("persisted IMPLEMENTS facts"));
        let remember_fact = tools_ro
            .iter()
            .find(|tool| tool["name"] == "remember_fact")
            .unwrap();
        assert_eq!(remember_fact["annotations"]["readOnlyHint"], false);
        assert_eq!(remember_fact["annotations"]["destructiveHint"], false);
        let verify_change = tools_ro
            .iter()
            .find(|tool| tool["name"] == "verify_change")
            .unwrap();
        assert_eq!(verify_change["annotations"]["readOnlyHint"], false);
        assert_eq!(verify_change["annotations"]["openWorldHint"], true);

        let mut config_write_enabled = OkConfig::default();
        config_write_enabled.security.allow_write = true;

        let result_write_enabled = dispatch(
            Path::new("."),
            &store,
            &config_write_enabled,
            "tools/list",
            params,
        )
        .await
        .unwrap();
        let tools_write_enabled = result_write_enabled["tools"].as_array().unwrap();
        for retired_tool in ["apply_patch", "review_patch", "validate_patch"] {
            assert!(
                tools_write_enabled
                    .iter()
                    .all(|tool| tool["name"] != retired_tool),
                "{retired_tool} must not be restored by write configuration"
            );
        }
    }

    #[tokio::test]
    async fn test_get_evidence_schema_shape() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();

        let params = json!({});
        let result = dispatch(
            Path::new("."),
            &store,
            &config,
            "get_evidence_schema",
            params,
        )
        .await
        .unwrap();

        // Check top-level properties
        assert!(result.get("version").is_some());
        assert!(result.get("node_types").is_some());
        assert!(result.get("edge_types").is_some());
        assert!(result.get("property_specs").is_some());
        assert!(result.get("feature_flags").is_some());
        assert!(result.get("evidence_source_types").is_some());
        assert!(result.get("query_features").is_some());
        assert!(result.get("optional_evidence").is_some());
        assert_eq!(result["relationship_semantic_capability_version"], 1);
        let semantic_capabilities = result["relationship_semantic_capabilities"]
            .as_array()
            .expect("Tier-1 relationship semantic capabilities");
        assert_eq!(semantic_capabilities.len(), 6);
        let javascript = semantic_capabilities
            .iter()
            .find(|descriptor| descriptor["language"] == "java_script")
            .expect("JavaScript semantic capability descriptor");
        assert_eq!(
            javascript["capabilities"]["types_annotation"],
            "unsupported"
        );
        let java = semantic_capabilities
            .iter()
            .find(|descriptor| descriptor["language"] == "java")
            .expect("Java semantic capability descriptor");
        assert_eq!(
            java["capabilities"]["calls_instance_member"],
            "supported_authoritative"
        );
        assert_eq!(
            java["capabilities"]["calls_dynamic_dispatch"],
            "unsupported"
        );

        // Check arrays
        let node_types = result["node_types"].as_array().unwrap();
        assert!(!node_types.is_empty(), "node_types should not be empty");

        let edge_types = result["edge_types"].as_array().unwrap();
        assert!(!edge_types.is_empty(), "edge_types should not be empty");

        let evidence_source_types = result["evidence_source_types"].as_array().unwrap();
        assert!(evidence_source_types
            .iter()
            .any(|source_type| source_type == "git_history"));

        let query_features = result["query_features"].as_array().unwrap();
        assert!(query_features
            .iter()
            .any(|feature| feature == "bounded_multi_hop_traversal"));

        let optional_evidence = result["optional_evidence"].as_array().unwrap();
        assert!(optional_evidence
            .iter()
            .any(|evidence| evidence["name"] == "scip" && evidence["status"] == "unknown"));
    }
}
