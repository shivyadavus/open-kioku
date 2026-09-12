use anyhow::Context;
use open_kioku_actions::{ActionKind, PolicyGate};
use open_kioku_architecture::{evaluate_policy, ArchitectureDetector, PolicyResolver};
use open_kioku_config::{load_architecture_policy, OkConfig};
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
};
use open_kioku_impact::ImpactEngine;
use open_kioku_memory::RepoMemoryStore;
use open_kioku_patch::{
    ChangeVerifier, ContractVerificationReport, ContractVerifier, PatchPlanner, VerifyChangeInput,
};
use open_kioku_plan::{ContractBuilder, PlanEngine, PlanFormat, PreflightFormat, PreflightReport};
use open_kioku_search_regex::{regex_search_index, search_chunks, MAX_REGEX_SCAN_FILES};
use open_kioku_search_tantivy::{default_index_dir, TantivySearchIndex};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_sentry::{disabled_response, unimplemented_response, SentryConfig};
use open_kioku_storage::generations::{not_indexed_message, not_indexed_status};
use open_kioku_storage::{GraphStore, MetadataStore, OkStore, SearchIndex};
use open_kioku_storage_sqlite::SqliteStore;
use open_kioku_symbols::{SymbolEngine, SYMBOL_CONTEXT_SURROUNDING_LINES};
use open_kioku_tests::TestSelector;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

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
    let builder = ContextPackBuilder::new(store as &dyn OkStore)
        .with_history_store(Some(store))
        .with_abstention_policy(
            open_kioku_core::abstention::AbstentionActivation::load_for_repo(repo)
                .map(|activation| activation.policy),
        );

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
    let builder = builder.with_search_index(
        lexical_index_source
            .as_ref()
            .map(|source| source.index() as &dyn open_kioku_storage::SearchIndex),
    );
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
    serve(
        repo,
        config,
        BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await
}

/// The session loop behind `serve_stdio`, generic over the transport so a test can drive a
/// whole session through in-memory buffers and then inspect the repository on disk.
///
/// A repository that has never been indexed is served without a store: the handshake and the
/// tool inventory still answer, `repo_status` says `indexed: false`, and every other tool
/// call names `ok index`. Nothing is created on disk, so a read-only server cannot leave an
/// empty database behind that later reads as an index awaiting rebuild.
async fn serve<R, W>(
    repo: PathBuf,
    config: OkConfig,
    reader: R,
    mut writer: W,
) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut store = SqliteStore::open_repo_index(&repo)?;
    let mut last_request = Instant::now();
    let mut lines = reader.lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        // Reopen after idling so another process's `ok index` becomes visible, and keep
        // looking while unindexed so an index built after startup is served without a
        // restart; the unindexed probe is one `stat`.
        if store.is_none() || store_idle_expired(last_request) {
            store = SqliteStore::open_repo_index(&repo)?;
        }
        last_request = Instant::now();
        if let Some(response) = handle_line(&repo, store.as_ref(), &config, &line).await {
            writer
                .write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())
                .await?;
            writer.flush().await?;
        }
    }
    Ok(())
}

async fn handle_line(
    repo: &Path,
    store: Option<&SqliteStore>,
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
    store: Option<&SqliteStore>,
    config: &OkConfig,
    request: JsonRpcRequest,
) -> Option<JsonRpcResponse> {
    handle_request_with_timeout(repo, store, config, request, TOOL_TIMEOUT).await
}

async fn handle_request_with_timeout(
    repo: &Path,
    store: Option<&SqliteStore>,
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
        dispatch_request(repo, store, config, method, request.params),
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
    // The fingerprint below did not change in 4.0.0, so it passes on a pre-4.0 index whose
    // edges were discarded on open. The marker is the only record of that discard.
    if store.graph_rebuild_required()? {
        anyhow::bail!(
            "authoritative relationship evidence unavailable: graph edges were built by an \
             older index format and were discarded on open; run `ok index` to rebuild them"
        );
    }
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

async fn dispatch_request(
    repo: &Path,
    store: Option<&SqliteStore>,
    config: &OkConfig,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    match store {
        Some(store) => dispatch(repo, store, config, method, params).await,
        None => dispatch_unindexed(repo, config, method, params),
    }
}

/// What the server answers while the repository has no index. The handshake and the tool
/// inventory still work, so a client can connect and see what would be available;
/// `repo_status` reports `indexed: false` with the command that changes it; every other tool
/// is refused with that same sentence rather than answered from nothing, and a retired or
/// unknown name is still told so, because "not indexed" would not be its fix.
fn dispatch_unindexed(
    repo: &Path,
    config: &OkConfig,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    match method {
        "initialize" => Ok(initialize_response(&params)),
        "tools/list" => Ok(tools_list_response(config)),
        "tools/call" => match params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "repo_status" => Ok(tool_response(json!(not_indexed_status(repo)))),
            other => Err(unindexed_method_error(repo, other)),
        },
        "repo_status" => Ok(json!(not_indexed_status(repo))),
        other => Err(unindexed_method_error(repo, other)),
    }
}

fn unindexed_method_error(repo: &Path, name: &str) -> anyhow::Error {
    if retired_tool_guidance(name).is_none() && tool_category(name).is_some() {
        return anyhow::anyhow!("{}", not_indexed_message(repo));
    }
    unknown_method_error(name)
}

fn unknown_method_error(name: &str) -> anyhow::Error {
    match retired_tool_guidance(name) {
        Some(guidance) => {
            anyhow::anyhow!("`{name}` was retired from the MCP tool surface in 4.0.0: {guidance}")
        }
        None => anyhow::anyhow!("unknown MCP method or tool `{name}`"),
    }
}

fn initialize_response(params: &Value) -> Value {
    let client_version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("2024-11-05");
    json!({
        "protocolVersion": client_version,
        "serverInfo": {"name": "open-kioku", "version": env!("CARGO_PKG_VERSION")},
        "capabilities": {"tools": {}}
    })
}

fn tools_list_response(config: &OkConfig) -> Value {
    let (tool_list, unstable) = tools(config);
    json!({
        "tools": tool_list,
        "_unstable_experimental_tools": unstable
    })
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
        "initialize" => Ok(initialize_response(&params)),
        "tools/list" => Ok(tools_list_response(config)),
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
            // A store without a manifest is not an index. `serve` never hands one over, but
            // the answer for it is the unindexed status, not a serialized `null`.
            let Some(manifest) = store.manifest()? else {
                return Ok(json!(not_indexed_status(repo)));
            };
            let compatibility = open_kioku_core::classify_analysis_semantics(
                manifest.analysis_semantics.as_ref(),
                &open_kioku_core::AnalysisSemanticsState::current(),
            );
            let mut status = serde_json::to_value(&manifest)?;
            if let Some(object) = status.as_object_mut() {
                // The field an agent branches on; the unindexed answer carries `false`.
                object.insert("indexed".into(), Value::Bool(true));
                object.insert(
                    "analysis_semantics_status".into(),
                    serde_json::to_value(compatibility)?,
                );
                // The fingerprint above is unchanged since 3.1.0 and passes on a pre-4.0
                // index whose edges were discarded on open; only the marker records that.
                object.insert(
                    "graph_rebuild_required".into(),
                    Value::Bool(store.graph_rebuild_required()?),
                );
                object.insert(
                    "generation_id".into(),
                    serde_json::to_value(
                        open_kioku_storage::generations::resolve_index_location(repo)
                            .generation_id(),
                    )?,
                );
                // Same numbers as `ok --json status`: null when the manifest predates
                // coverage recording, so absence is never mistaken for 100%.
                let coverage = manifest.quality.coverage.as_ref();
                object.insert("coverage".into(), serde_json::to_value(coverage)?);
                object.insert("languages".into(), json!(indexed_languages(store, coverage)?));
                // The whole semantic status, not a readiness summary. An agent
                // about to trust vector results has to be able to ask which
                // provider, model and artifact produced them, and the retired
                // `semantic_status` tool is the only place that answered.
                let semantic = SemanticIndexManager::new(repo, store, &config.semantic).status();
                object.insert("semantic_lifecycle".into(), serde_json::to_value(semantic)?);
            }
            Ok(status)
        }
        "list_files" => {
            gate.ensure_allowed(ActionKind::Read)?;
            // One `path` turns the inventory into the per-file detail view: the
            // indexed record plus the chunks that cover it. File-level detail
            // and symbol-level detail are different questions, so this is the
            // file-level tool rather than a mode on `get_definition`.
            if let Some(path) = optional_str(&params, "path")? {
                let file = store.get_file_by_path(Path::new(path))?;
                let chunks = match &file {
                    Some(file) => store.chunks_for_file(&file.id)?,
                    None => Vec::new(),
                };
                let caveats = if file.is_none() {
                    vec![format!(
                        "`{path}` is not in the index; it may be excluded, unsupported, or added since the last `ok index`"
                    )]
                } else {
                    Vec::new()
                };
                return Ok(json!({"path": path, "file": file, "chunks": chunks, "caveats": caveats}));
            }
            let limit = limit(&params);
            let offset = offset(&params);
            Ok(paged_overfetch_response(
                "files",
                store.list_files(overfetch_limit(limit), offset)?,
                limit,
                offset,
            )?)
        }
        "search_symbols" => {
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
        "search_code" => match params.get("mode").and_then(Value::as_str).unwrap_or("code") {
            "code" | "graph" => search_tool(repo, store, &params),
            "semantic" => semantic_search_tool(repo, store, config, &params),
            "hybrid" => hybrid_search_tool(repo, store, config, &params),
            other => anyhow::bail!(
                "unknown `mode` `{other}` for search_code; expected one of code, graph, semantic, hybrid"
            ),
        },
        "regex_search" => regex_search_tool(store, &params),
        "build_context_pack" => {
            let task = required_str(&params, "task")?;
            let pack = build_context_for_task(repo, store, config, task, limit(&params))?;
            // `compress` is the only path that writes: it stores the snippets
            // under `.ok` and returns handles instead. The default renders the
            // pack inline and touches nothing.
            if bool_arg(&params, "compress") {
                let compressed = ContextHandleStore::open_repo(repo)?.compress_pack(&pack)?;
                return if format_arg(&params, "json") == "toon" {
                    Ok(json!(open_kioku_format::render_compressed_context_toon(
                        &compressed
                    )))
                } else {
                    Ok(json!(compressed))
                };
            }
            // Markdown is the default because JSON is roughly two orders of
            // magnitude larger for the same answer, and this tool exists to
            // spend an agent's context window well. Callers that parse the
            // response ask for "json" explicitly.
            match format_arg(&params, "markdown") {
                "json" => Ok(json!(pack)),
                "toon" => Ok(json!(open_kioku_format::render_context_pack_toon(&pack))),
                _ => Ok(json!(
                    open_kioku_context::ContextPackFormat::Markdown.render(&pack)?
                )),
            }
        }
        "retrieve_context" => {
            let handle = required_str(&params, "handle")?;
            // An unknown handle is an error, not `null`: a null answer reads as an empty
            // snippet, and the handle either came from this repository's compressed pack
            // or it did not.
            let retrieved = ContextHandleStore::open_repo_existing(repo)?
                .map(|store| store.retrieve(&ContextHandleId::new(handle)))
                .transpose()?
                .flatten()
                .with_context(|| {
                    format!(
                        "no context handle `{handle}` is stored for this repository; handles come from build_context_pack with compress=true"
                    )
                })?;
            Ok(json!(retrieved))
        }
        "plan_change" => {
            // `persist` is the only writing path: it turns the plan into a
            // durable ChangeContractV1 that `verify_change` can later be held
            // to. It accepts a plan the caller already holds so a saved plan
            // becomes a contract without re-planning.
            if bool_arg(&params, "persist") {
                let plan = contract_plan_from_params(repo, store, config, &params)?;
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
                return format_contract_create_output(&output, format_arg(&params, "json"));
            }

            // A plan's relationship claims come from the graph. Refuse up front rather than
            // let a stale index produce a plan whose "structurally proven dependents"
            // sentence is silently absent.
            require_authoritative_relationships(store)?;
            let task = required_str(&params, "task")?;
            let detail = params
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or("plan");
            if detail == "patch" {
                return Ok(json!(
                    PatchPlanner::new(config, store as &dyn OkStore).plan(task)?
                ));
            }
            anyhow::ensure!(
                matches!(detail, "plan" | "preflight"),
                "unknown `detail` `{detail}` for plan_change; expected one of plan, preflight, patch"
            );

            let task = if let Some(since) = params.get("since").and_then(Value::as_str) {
                task_with_changed_ranges(repo, task, since)?
            } else {
                task.to_string()
            };
            let memory_facts = RepoMemoryStore::search_repo(repo, &task, 8)?;
            let limit = limit(&params);
            let context = build_context_for_task(repo, store, config, &task, limit)?;
            let report = PlanEngine::new(store as &dyn OkStore)
                .with_history_store(Some(store))
                .with_memory_facts(memory_facts)
                .with_memory_enabled(config.memory.enabled)
                .plan_from_context(&task, limit, context)?;

            if detail == "preflight" {
                let report = PreflightReport::from_plan(&report);
                return match format_arg(&params, "json") {
                    "markdown" => Ok(json!(PreflightFormat::Markdown.render(&report)?)),
                    "html" => Ok(json!(PreflightFormat::Html.render(&report)?)),
                    "text" => Ok(json!(PreflightFormat::Text.render(&report)?)),
                    _ => Ok(json!(report)),
                };
            }
            match format_arg(&params, "markdown") {
                "json" => Ok(json!(report)),
                "toon" => Ok(json!(PlanFormat::Toon.render(&report)?)),
                _ => Ok(json!(PlanFormat::Markdown.render(&report)?)),
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
            Ok(json!(RepoMemoryStore::search_repo(
                repo,
                query,
                limit(&params)
            )?))
        }
        "impact_analysis" => {
            require_authoritative_relationships(store)?;
            let path = required_str(&params, "path")?;
            let search_dir = default_index_dir(repo);
            let search_index = TantivySearchIndex::exists(&search_dir)
                .then(|| TantivySearchIndex::open_or_create(&search_dir))
                .transpose()?;
            let mut report = ImpactEngine::new(store)
                .with_search_index(
                    search_index
                        .as_ref()
                        .map(|index| index as &dyn open_kioku_storage::SearchIndex),
                )
                .with_history_store(Some(store))
                .with_graph_store(Some(store))
                .for_file(Path::new(path))?;
            report.architecture_policy = configured_architecture_policy_report(repo, store)?;
            Ok(json!(report))
        }
        "find_tests_for_change" => {
            // `path` is optional: without one this reports the repository-wide
            // test evidence the retired `explain_test_coverage` returned. A
            // `path` that is present but not a string is an error, not a
            // silently broader answer.
            let path = optional_str(&params, "path")?.unwrap_or_default();
            Ok(json!(
                TestSelector::new(store).for_changed_path(Path::new(path), limit(&params))?
            ))
        }
        "get_definition" => {
            let query = required_str(&params, "query")?;
            let engine = SymbolEngine::new(store);
            // The record answers "where does this live"; the body answers "what
            // does it say". The body is a strictly larger read, so the record
            // stays the default.
            if bool_arg(&params, "include_body") {
                return Ok(json!(
                    engine.context(query, SYMBOL_CONTEXT_SURROUNDING_LINES)?
                ));
            }
            Ok(json!(engine.definition(query)?))
        }
        "get_references" => {
            require_authoritative_relationships(store)?;
            symbol_evidence_tool(store, &params)
        }
        "dependency_path" => {
            require_authoritative_relationships(store)?;
            let from = required_str(&params, "from")?;
            let from = resolve_graph_node(store, from)?;
            // Without a destination there is no route to trace, so the answer
            // is the node's direct neighbourhood instead.
            let Some(to) = params.get("to").and_then(Value::as_str) else {
                let (nodes, edges) = store.neighbors(&from, limit(&params))?;
                return Ok(json!({
                    "node": from,
                    "nodes": nodes,
                    "edges": edges,
                    "evidence_source": "sqlite_graph_store"
                }));
            };
            let to = resolve_graph_node(store, to)?;
            Ok(json!({
                "from": from,
                "to": to,
                "edges": store.shortest_path(&from, &to, 12)?,
                "evidence_source": "sqlite_graph_store"
            }))
        }
        "explain_flow" => explain_flow_tool(store, &params),
        "verify_change" => {
            // Three inputs, three jobs, one verb. A supplied report is
            // explained rather than re-verified; a contract id or inline
            // contract is verified against the contract; otherwise the saved
            // plan is the boundary the change is held to.
            if params.get("verification").is_some() || params.get("verification_json").is_some() {
                let report = verification_report_from_params(&params)?;
                let explanation = explain_verification_report(&report);
                return format_verification_explanation(&explanation, format_arg(&params, "json"));
            }
            if params.get("contract_id").is_some()
                || params.get("contract").is_some()
                || params.get("contract_json").is_some()
            {
                let verification = verify_change_contract_tool(repo, store, &params)?;
                if !bool_arg(&params, "explain") {
                    return Ok(verification);
                }
                let report: ContractVerificationReport =
                    serde_json::from_value(verification.clone()).context(
                        "contract verification must round-trip into a ContractVerificationReport",
                    )?;
                let explanation = explain_verification_report(&report);
                return format_verification_explanation(&explanation, format_arg(&params, "json"));
            }
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
        "query_evidence_graph"
            if optional_str(&params, "query")
                .ok()
                .flatten()
                .unwrap_or_default()
                .trim()
                .is_empty()
                && params.get("query").is_none_or(Value::is_string) =>
        {
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
            let query_str = optional_str(&params, "query")?.unwrap_or("");
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
            // The same check that decides whether these are advertised decides
            // what they answer, so the config switch is never decorative: a
            // half-configured provider is `configured: false`, and a validated
            // one is told plainly that this build cannot query it rather than
            // returning an empty result that reads as "no runtime errors".
            match runtime_provider(&config.runtime) {
                Some(provider) => Ok(json!(unimplemented_response(method, &provider))),
                None => Ok(json!(disabled_response(method))),
            }
        }
        other => Err(unknown_method_error(other)),
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
            .map(tool_response)
    })
}

/// The `tools/call` envelope around one tool's value.
fn tool_response(value: Value) -> Value {
    let mut text = if let Some(s) = value.as_str() {
        s.to_string()
    } else {
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
    };
    let text_truncated = truncate_utf8(&mut text, MAX_TOOL_TEXT_BYTES);
    let rendered = value.is_string();
    let structured_content = structured_content_for(value, &text, text_truncated);
    // `isError` is optional in the MCP schema and defaults to false, but a client
    // that reads it should not have to infer success from a missing field.
    let mut response = json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": structured_content,
        "isError": false
    });
    if text_truncated {
        response["truncated"] = json!(true);
        response["warnings"] = json!([if rendered {
            format!(
                "rendered text was truncated to {} bytes; request format=json or a lower limit for the full result",
                MAX_TOOL_TEXT_BYTES
            )
        } else {
            format!(
                "tool text content was truncated to {} bytes; structuredContent carries the full result",
                MAX_TOOL_TEXT_BYTES
            )
        }]);
    }
    response
}

/// The `structuredContent` half of a tool response. JSON objects are returned as they are. A
/// rendered string (Markdown, TOON) is not structured output; repeating it here doubled every
/// byte of the most common responses, and the client that matters most reads `content` and
/// ignores `structuredContent`. Every tool declares an `outputSchema`, so the field stays, but
/// for a rendering it carries a pointer to `content`, not the text.
fn structured_content_for(value: Value, text: &str, text_truncated: bool) -> Value {
    match value {
        Value::Object(_) => value,
        Value::String(_) => json!({
            "rendered_in": "content",
            "bytes": text.len(),
            "truncated": text_truncated
        }),
        other => json!({ "value": other }),
    }
}

fn search_tool(repo: &Path, store: &dyn MetadataStore, params: &Value) -> anyhow::Result<Value> {
    let limit = limit(params);
    let offset = offset(params);
    let results = search_results(repo, store, params, search_fetch_limit(limit, offset))?;
    paged_bounded_slice_response("results", results, limit, offset)
}

/// Evaluates the caller's pattern over the indexed corpus instead of handing it
/// to the ranked lexical path. An agent that asks for a regex gets exact
/// single-line matches, and gets told when the bounded walk stopped early.
fn regex_search_tool(store: &dyn MetadataStore, params: &Value) -> anyhow::Result<Value> {
    let pattern = params
        .get("pattern")
        .and_then(Value::as_str)
        .filter(|pattern| !pattern.is_empty())
        .context("`regex_search` requires a non-empty `pattern`")?;
    let limit = limit(params);
    let offset = offset(params);
    let fetched = search_fetch_limit(limit, offset);
    let scan = regex_search_index(store, pattern, fetched)?;

    let has_more = scan.results.len() > offset.saturating_add(limit);
    let mut metadata = PageMetadata::new(limit, offset, has_more);
    metadata.caveats.push(format!(
        "the pattern was evaluated over indexed chunk text from {} file(s), not the working tree; regions the indexer did not chunk were not searched",
        scan.files_scanned
    ));
    // Two independent ways this answer can be short of the truth: the walk gave
    // up on the file budget, or the fetch window was clamped. Both have to show.
    if scan.files_capped {
        metadata.has_more = true;
        metadata.truncated = true;
        metadata.warnings.push(format!(
            "the regex scan stopped after {MAX_REGEX_SCAN_FILES} files; results are incomplete"
        ));
    }
    disclose_fetch_cap(&mut metadata, scan.results.len());
    paged_slice_response_with_metadata("results", scan.results, metadata)
}

/// `query` (or the older `pattern` spelling), refused when blank: an empty query returned an
/// empty success, which read as "nothing in the repository matches".
fn search_query(params: &Value) -> anyhow::Result<&str> {
    params
        .get("query")
        .or_else(|| params.get("pattern"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .context("`search_code` requires a non-empty `query`")
}

fn search_results(
    repo: &Path,
    store: &dyn MetadataStore,
    params: &Value,
    fetch_limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let query = search_query(params)?;
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

/// Names that left the advertised surface and the dispatch table together, each
/// paired with where its capability went.
///
/// Nothing here may come back as a hidden alias: an agent holding a stale name
/// must get a clear error rather than a response whose shape no longer matches
/// the description that name was chosen from. But a bare refusal makes the
/// agent guess, and the mapping is already known here, so the error carries it.
const RETIRED_TOOLS: &[(&str, &str)] = &[
    // Removed before 4.0.0.
    (
        "apply_patch",
        "Open Kioku does not edit source files; apply the edit with your editor, then call `verify_change`",
    ),
    (
        "review_patch",
        "use `plan_change` with `detail: \"patch\"` before the edit and `verify_change` after it",
    ),
    (
        "validate_patch",
        "use `verify_change` with the saved plan or a `contract_id`",
    ),
    // Folded into one of the sixteen in 4.0.0 (#406).
    (
        "list_languages",
        "use `repo_status`; its `languages` field carries the same inventory",
    ),
    ("list_symbols", "use `search_symbols`; `query` is optional"),
    (
        "search_files",
        "use `search_code`; it was the same call under another name",
    ),
    (
        "semantic_status",
        "use `repo_status`; its `semantic_lifecycle` field carries the whole status, and `ok semantic status` is the CLI equivalent",
    ),
    ("semantic_search", "use `search_code` with `mode: \"semantic\"`"),
    ("hybrid_search", "use `search_code` with `mode: \"hybrid\"`"),
    (
        "explain_search_result",
        "use `search_code` with `mode: \"hybrid\"`; every result already carries `score_breakdown` and `evidence_refs`",
    ),
    ("explain_file", "use `list_files` with a `path`"),
    (
        "explain_symbol",
        "use `get_definition`; it was the same call under another name",
    ),
    (
        "get_symbol_context",
        "use `get_definition` with `include_body: true`",
    ),
    ("get_callers", "use `get_references` with `kind: \"callers\"`"),
    ("get_callees", "use `get_references` with `kind: \"callees\"`"),
    (
        "get_implementations",
        "use `get_references` with `kind: \"implementations\"`",
    ),
    (
        "module_dependencies",
        "use `dependency_path` with `from` and no `to`",
    ),
    (
        "build_compressed_context",
        "use `build_context_pack` with `compress: true`",
    ),
    ("preflight_change", "use `plan_change` with `detail: \"preflight\"`"),
    (
        "create_change_contract",
        "use `plan_change` with `persist: true` (add `store: false` for a transient contract)",
    ),
    ("propose_patch", "use `plan_change` with `detail: \"patch\"`"),
    (
        "verify_change_contract",
        "use `verify_change` with `contract_id`, `contract`, or `contract_json`",
    ),
    (
        "explain_verification",
        "use `verify_change` with `verification` or `verification_json`, or `explain: true` on a contract verification",
    ),
    (
        "recommend_validation_plan",
        "use `find_tests_for_change`; it was the same call under another name",
    ),
    (
        "explain_test_coverage",
        "use `find_tests_for_change`; omit `path` for the repository-wide evidence",
    ),
    (
        "get_evidence_schema",
        "call `query_evidence_graph` with no `query`",
    ),
    // Removed with nothing lost: no structural or AST matching exists here.
    (
        "structural_search",
        "use `regex_search` for a literal pattern or `search_code` for ranked lexical search; no structural or AST matching exists in this workspace",
    ),
    // Moved to the CLI in 4.0.0; the capability ships, the MCP name does not.
    ("detect_architecture", "moved to the CLI: `ok architecture detect`"),
    (
        "architecture_boundaries",
        "moved to the CLI: `ok architecture boundaries` or `ok architecture summary`",
    ),
    (
        "architecture_violations",
        "moved to the CLI: `ok architecture violations`, or `ok architecture summary` for the full report it was a view of",
    ),
    (
        "architecture_policy_validate",
        "moved to the CLI: `ok architecture policy validate`",
    ),
    (
        "architecture_policy_check",
        "moved to the CLI: `ok architecture policy check`",
    ),
    (
        "architecture_policy_explain",
        "moved to the CLI: `ok architecture policy explain`",
    ),
    (
        "summarize_architecture",
        "moved to the CLI: `ok architecture summary`",
    ),
    (
        "history_provenance_lookup",
        "moved to the CLI: `ok history provenance --path` or `--symbol`",
    ),
    (
        "churn_analysis",
        "moved to the CLI: `ok history churn --path`, `--module`, or `--symbol`",
    ),
    (
        "history_similar_changes",
        "moved to the CLI: `ok history similar --task`, `--path`, or `--symbol`",
    ),
    (
        "ownership_lookup",
        "moved to the CLI: `ok history ownership --path`",
    ),
    (
        "reviewer_suggestions",
        "moved to the CLI: `ok history reviewers --path`",
    ),
    ("get_change_contract", "moved to the CLI: `ok contract show <id>`"),
];

fn retired_tool_guidance(name: &str) -> Option<&'static str> {
    RETIRED_TOOLS
        .iter()
        .find(|(retired, _)| *retired == name)
        .map(|(_, guidance)| *guidance)
}

fn tool_annotations(name: &str) -> Value {
    let mut read_only = true;
    let destructive = false;
    let mut idempotent = true;
    let mut open_world = false;

    match name {
        // Conditionally writing: `build_context_pack` only with compress=true,
        // `plan_change` only with persist=true. The hint describes what the
        // tool can do, not what the default call does.
        "build_context_pack" | "plan_change" | "remember_fact" => {
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
        "description": "The MCP structuredContent object returned by this Open Kioku tool. JSON tool outputs expose their fields directly. Rendered text outputs (Markdown, TOON) live only in `content` and are described here by {\"rendered_in\": \"content\", \"bytes\": N, \"truncated\": bool} rather than repeated; other scalar outputs are wrapped as {\"value\": ...}.",
        "additionalProperties": true,
        "properties": {
            "value": {
                "description": "Wrapped scalar (non-string, non-object) output.",
            },
            "rendered_in": {
                "type": "string",
                "description": "Where a rendered text output lives (\"content\"); the text is not duplicated into structuredContent.",
            },
            "bytes": {
                "type": "integer",
                "description": "Byte length of the rendered text placed in content.",
            },
            "truncated": {
                "type": "boolean",
                "description": "Whether the rendered text in content was truncated to the response cap.",
            }
        }
    })
}

/// The configured runtime provider, or `None` when the repository has not
/// supplied one the provider itself accepts. `enabled = true` is not enough:
/// the provider validates its own configuration, so a switch that is on but
/// incomplete advertises nothing and answers nothing.
fn runtime_provider(config: &open_kioku_config::RuntimeConfig) -> Option<String> {
    if !config.enabled {
        return None;
    }
    match config.provider.trim() {
        "sentry" => open_kioku_sentry::ensure_configured(&SentryConfig {
            enabled: true,
            organization: config.organization.clone(),
            project: config.project.clone(),
            auth_token_env: config.auth_token_env.clone(),
        })
        .ok()
        .map(|()| "sentry".to_string()),
        // An unknown provider name is an unsupported integration, not a
        // working one; nothing is advertised for it.
        _ => None,
    }
}

/// Routing category of every dispatchable tool, gated ones included; `None` for a name the
/// server does not answer, which is how the unindexed path tells "known tool, no index" from
/// "no such tool".
fn tool_category(name: &str) -> Option<&'static str> {
    Some(match name {
        "repo_status" | "list_files" => "repository",
        "search_code" | "regex_search" => "search",
        "search_symbols" | "get_definition" | "get_references" => "code-intelligence",
        "dependency_path" | "impact_analysis" | "explain_flow" => "dependencies",
        "build_context_pack" | "retrieve_context" => "context",
        "plan_change" => "planning",
        "verify_change" | "find_tests_for_change" => "validation",
        "query_evidence_graph" => "evidence-graph",
        "remember_fact" | "search_memory" => "memory",
        "map_stacktrace_to_code" | "find_errors_for_symbol" | "find_recent_failures" => "runtime",
        _ => return None,
    })
}

fn tool_description(name: &str, base: &str) -> String {
    let guidance = match name {
        "repo_status" => "Use first to check whether the local index exists, how much of the repository it covers, which languages it holds, and whether the semantic index is ready, before calling search, symbol, or graph tools. This is read-only and only inspects repository metadata.",
        "list_files" => "Use for a paginated inventory of what the index holds, or pass `path` for one file's indexed detail: its record plus every chunk covering it. Do NOT use to find files by keyword (use search_code) or for a symbol's definition (use get_definition). This is read-only and returns indexed data only.",
        "search_code" => "Use as the single entry point for finding where something is handled. `mode` selects the evidence: `code` for lexical BM25 over indexed chunks, `graph` for indexed graph-node documents, `semantic` for the local vector index, `hybrid` for both merged and re-sorted. Semantic and hybrid fall back to lexical-only and say so in `semantic_status` when the vector index is not ready, so their extra recall is never assumed. Every result already carries `score_breakdown` and `evidence_refs`; there is no separate explain step. Do NOT use for literal patterns (use regex_search) or for a known symbol (use get_definition). This is read-only.",
        "regex_search" => "Use when the target is a literal pattern and ranked guesses will not do. Every hit is exact, at confidence 1.0, and hits arrive in path order rather than by score. Do NOT use for ranked keyword search (use search_code), natural-language concept search (use search_code with mode=semantic or mode=hybrid), or broad candidate discovery (use search_code). Read the response caveats before concluding a pattern is absent from the repository. This is read-only.",
        "search_symbols" => "Use to browse the indexed symbol table, or filter it by a substring you already know; omit `query` to page through everything. Do NOT use when the query is only approximate: matching is case-insensitive substring, not fuzzy or ranked, so a name that shares no substring will not appear at all. For a symbol's defining record use get_definition, and for its usages use get_references. This is read-only and searches the local index only.",
        "get_definition" => "Use after resolving a symbol name, when knowing where it lives is enough. Set include_body=true when the definition text and the indexed lines around it are what the task needs, and read `caveats` before relying on that body: it names anything the index could not recover. Do NOT use for candidate discovery (use search_symbols) or for usages (use get_references). This is read-only.",
        "get_references" => "Use to answer what else touches one resolved symbol. `kind` selects the evidence: `references` for occurrences, `callers` and `callees` for persisted CALLS edges, `implementations` for persisted IMPLEMENTS facts, `all` for every section at once. Each section names its own `evidence_source` and caveats because absence means different things: no occurrence is not the same claim as no persisted IMPLEMENTS fact. Do NOT flatten the sections together, and do NOT use this for file-level blast radius (use impact_analysis). This is read-only.",
        "dependency_path" => "Use to explain how two files or symbols are connected through indexed dependencies. Omit `to` to get the direct dependency neighbours of `from` instead of a route. Prefer impact_analysis for downstream blast radius and query_evidence_graph for arbitrary traversal. This is read-only.",
        "impact_analysis" => "Use before editing a file to estimate the blast radius: downstream dependent files, caller functions, related test files, and architecture policy impact from the indexed dependency graph. Do NOT use when only test targets are needed (use find_tests_for_change) or for a full evidence-backed pre-edit plan (use plan_change). This is read-only and analyzes the local index only.",
        "explain_flow" => "Use to retrieve graph-backed endpoint-to-call-path evidence from indexed endpoint nodes and directed CALLS edges. Each flow starts at an indexed endpoint and contains a bounded directed call path. Do NOT use for arbitrary graph traversal (use query_evidence_graph) or for a route between two named nodes (use dependency_path). This is read-only; repositories without persisted endpoint or call evidence return no flow entries.",
        "build_context_pack" => "Use before planning or editing to assemble a ranked bundle of relevant files, symbol definitions, test targets, git history evidence, and architecture policy context for a natural-language task. Set compress=true when prompt budget is tight: it stores the snippets under .ok and returns short handles that retrieve_context expands on demand. Do NOT use when only test targets are needed (use find_tests_for_change). With compress=false, the default, this is read-only.",
        "retrieve_context" => "Use only with handles returned by build_context_pack with compress=true, to recover the original snippets. Prefer build_context_pack for a fresh task-level context bundle. This is read-only.",
        "plan_change" => "Use before editing to create an evidence-backed plan with expected files, ranges, impact, and tests. `detail` selects the artifact: `plan` for the full report, `preflight` for one concise start decision, `patch` for a patch plan that writes nothing. Set persist=true to turn the plan into a durable change contract that verify_change can later hold the edit to; it accepts `plan` or `plan_json` so a plan you already hold becomes a contract without re-planning, and store=false keeps it transient. With persist=false, the default, this is read-only.",
        "verify_change" => "Use after code edits to check what actually changed against what was declared. Supply `plan` or `plan_json` to verify against a saved PlanReport, or `contract_id`, `contract`, or `contract_json` to verify against a change contract; supply `verification` or `verification_json` instead to explain a report you already hold without verifying anything. Set explain=true to get the explanation of a contract verification directly. When run_commands=true it executes the plan's validation commands on the local machine, and write_attestation=true persists timestamped records under .ok/contracts. Do NOT use for pre-edit planning (use plan_change). Side effects are conditional on those flags; with all flags false the tool is read-only.",
        "find_tests_for_change" => "Use after identifying a changed file to select the test files that should be run to validate the change. Returns ranked test file paths with relevance scores based on naming conventions, import relationships, and co-change history; omit `path` for the repository-wide test evidence. Do NOT use for a full evidence-backed pre-edit plan (use plan_change) and do NOT read it as proof a test exercises the change. This is read-only and executes nothing.",
        "query_evidence_graph" => "Use for advanced read-only evidence queries when no purpose-built tool fits, and call it with no `query` first to get the schema: node types, edge types, properties, and the versioned relationship-semantic capability matrix. The query language is a constrained Cypher-like DSL, not full Cypher. Prefer the purpose-built tools when they answer the question. This is read-only.",
        "remember_fact" => "Use only for durable, repository-scoped facts (architectural decisions, ownership conventions, known anti-patterns) that an agent should recall across sessions. Appends an immutable record to the local .ok SQLite store; duplicates are not deduplicated. Do NOT use for transient session notes, per-task scratch data, or facts derivable from the live index. Call search_memory first to avoid recording redundant entries. This tool writes to local storage and is not idempotent.",
        "search_memory" => "Use to retrieve stored repository-scoped memory facts by keyword, entity, or text match from the local .ok SQLite store. Returns fact text, source, confidence, timestamp, and associated entities for each match. Do NOT use for current source code search (use search_code) or for live index data (use the search and symbol tools). Use remember_fact to write new entries. This is read-only.",
        "map_stacktrace_to_code" => "Use when runtime stack trace text must be mapped to indexed source locations. Prefer find_errors_for_symbol when the symbol is known and recent stored failures are needed. This tool is read-only and returns disabled status when runtime integration is not configured.",
        "find_errors_for_symbol" => "Use to look up recently stored runtime errors and stack traces for one specific symbol name. Returns error messages, stack frames, and timestamps when runtime integration is configured. Do NOT use for ad-hoc stack trace text mapping (use map_stacktrace_to_code) or for a broad inventory of recent failures (use find_recent_failures). This is read-only and returns a disabled-status response when runtime error integration is not configured in the repository.",
        "find_recent_failures" => "Use to list recently stored runtime failures, errors, and incidents across the repository before beginning a debugging investigation. Returns failure entries with timestamps, error types, and affected symbols when runtime integration is configured. Do NOT use when errors for one specific symbol are needed (use find_errors_for_symbol) or for stack trace mapping (use map_stacktrace_to_code). This is read-only and returns a disabled-status response when runtime error integration is not configured.",
        _ => "Use when this exact indexed repository capability is needed. Prefer narrower sibling tools when they match the task. This tool reports local Open Kioku index data and does not contact external services.",
    };

    format!("{base} {guidance}")
}

/// The advertised tool surface: sixteen tools that each answer one question no
/// other tool answers, plus the two families that are advertised only when the
/// repository has configured them. Retired names are gone from here and from
/// the dispatch table together, so a stale name fails loudly instead of
/// resolving to a shape that no longer matches the description it was chosen
/// from.
fn tools(config: &OkConfig) -> (Vec<Value>, Vec<String>) {
    let read_only_tools: &[(&str, &str, Value)] = &[
        ("repo_status", "Retrieve the current repository index metadata, including file count, symbol count, chunk count, the exact timestamp when the repository was last indexed, the languages the index holds, index coverage (source files discovered versus indexed per language, each discovered file's omission attributed to a skip reason, plus counts of directories pruned by name and walk errors the ratio cannot see; null when the index predates coverage recording), and local semantic index lifecycle health (state, ANN activity, and rebuild requirements).", json!({"type":"object","properties":{}})),
        ("list_files", "List indexed files with relative path, size in bytes, and language, or pass one `path` to get that file's indexed detail instead: its file record plus every code chunk covering it, with line ranges. A path that is not indexed returns a null file and an explicit caveat rather than an empty success.", json!({"type":"object","properties":{"path":{"type":"string","description":"Repository-relative path of a single file to describe in detail (e.g. 'src/main.rs'). When set, `limit` and `offset` are ignored and the response carries the file record and its chunks."},"limit":{"type":"integer","description":"Maximum number of files to return when listing. Defaults to 20, capped at 100."},"offset":{"type":"integer","description":"Number of matching files to skip when listing. Defaults to 0."}}})),
        ("search_code", "Search indexed code through one of four evidence modes: lexical BM25 over code chunks, indexed graph-node documents, the local semantic vector index, or a hybrid merge of lexical and semantic candidates deduplicated by path and re-sorted by combined score. Semantic and hybrid modes report `semantic_status` and fall back to lexical-only results when the vector index is not ready. Every result carries path, line range, snippet, score, per-signal score_breakdown, and evidence_refs.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The search query: terms, identifiers, routes, config keys, or a natural-language description when mode is semantic or hybrid."},"mode":{"type":"string","enum":["code","graph","semantic","hybrid"],"description":"Which evidence to search. 'code' (default) is lexical BM25 over indexed chunks and file paths; 'graph' searches indexed graph-node documents; 'semantic' searches the local vector index; 'hybrid' merges lexical and semantic candidates. An unknown mode is a tool error."},"limit":{"type":"integer","description":"Maximum number of search results to return. Defaults to 20, capped at 100."},"offset":{"type":"integer","description":"Number of matching search results to skip. Defaults to 0."}}})),
        ("regex_search", "Match a regular expression line by line against indexed chunk text, in path order, returning exact single-line hits with file path, line number, and the matching line. Regions the indexer did not chunk are not searched, and the response carries that caveat plus a warning when the bounded walk stopped early.", json!({"type":"object","required":["pattern"],"properties":{"pattern":{"type":"string","description":"A valid regular expression pattern (Rust regex syntax) matched against each indexed source line. An unparseable pattern is returned as a tool error. Example: 'fn\\s+main' to find main function declarations."},"limit":{"type":"integer","description":"Maximum number of matching lines to return. Defaults to 20, capped at 100."},"offset":{"type":"integer","description":"Number of matching lines to skip before returning results. Defaults to 0."}}})),
        ("search_symbols", "List or substring-filter the indexed symbol table (functions, classes, structs, traits, interfaces) with pagination, returning symbol name, kind, file path, and line range. Matching is case-insensitive substring against name and qualified name, ordered by qualified name: it is not fuzzy and the results are not ranked, so an approximate name does not match. Omitting `query` pages through every indexed symbol.", json!({"type":"object","properties":{"query":{"type":"string","description":"Substring matched case-insensitively against symbol names and qualified names. Omit to list all symbols ordered by qualified name. Not fuzzy: a name that shares no substring with the query does not match."},"limit":{"type":"integer","description":"Maximum number of symbols to return. Defaults to 20, capped at 100. Use with offset for pagination."},"offset":{"type":"integer","description":"Number of matching symbols to skip before returning results. Defaults to 0."}}})),
        ("get_definition", "Retrieve the indexed definition record for a symbol (function, class, struct, trait, module) by name: its file, line range, kind, qualified name, confidence, and provenance. With include_body=true it also joins the symbol back to the indexed chunk text covering it, returning the definition body with the line range it spans plus up to ten indexed lines above and below it verbatim; anything that could not be recovered from the index is stated in `caveats` rather than returned as a shorter body.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The exact or partial name of the symbol to find the definition for."},"include_body":{"type":"boolean","description":"Set true to return the definition body and the indexed lines around it alongside the record. Defaults to false, which returns the record only."}}})),
        ("get_references", "Retrieve evidence about how one resolved symbol is used, in sections that keep their provenance apart: `references` returns indexed occurrences, each with its own provenance and confidence; `callers` and `callees` return persisted CALLS graph edges in the named direction; `implementations` returns verified implementation sites from persisted IMPLEMENTS facts with parser provenance. Every section names its own evidence_source and caveats, because an empty occurrence list and an empty IMPLEMENTS list are different claims.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The name of the symbol to gather usage evidence for. For implementations this is the interface, trait, abstract class, or protocol name."},"kind":{"type":"string","enum":["references","callers","callees","implementations","all"],"description":"Which evidence sections to return. Defaults to 'references'. 'all' returns every section in one response, each still labelled with its own evidence_source. An unknown kind is a tool error."},"limit":{"type":"integer","description":"Maximum number of entries per section. Defaults to 20, capped at 100."}}})),
        ("dependency_path", "Trace the shortest dependency or reference path between two files or symbols from the persisted graph, or, when `to` is omitted, list the direct dependency graph neighbours (imports and dependents) of `from` instead.", json!({"type":"object","required":["from"],"properties":{"from":{"type":"string","description":"The starting node path or symbol name."},"to":{"type":"string","description":"The target node path or symbol name. Omit to return the direct neighbours of `from` rather than a route between two nodes."},"limit":{"type":"integer","description":"Maximum number of neighbours to return when `to` is omitted. Defaults to 20, capped at 100."}}})),
        ("impact_analysis", "Analyze the blast radius of a change to one repository-relative file using the indexed dependency graph. Returns ranked downstream dependent files, caller functions, related test files, and architecture policy impact with impact scores and relationship types. Dependents reached through typed relationship edges are additionally split into proven_impact (authoritative structural proof) and possible_impact (heuristic or corroborating only, never presented as fact).", json!({"type":"object","required":["path"],"properties":{"path":{"type":"string","description":"The repository-relative path of the file to analyze for downstream impact (e.g., 'src/auth/handler.rs')."}}})),
        ("explain_flow", "Return graph-backed endpoint-to-call flow evidence, plus a heuristic architecture summary. Each flow contains an indexed endpoint and a bounded directed CALLS path.", json!({"type":"object","properties":{"limit":{"type":"integer","description":"Maximum endpoint flows to return. Defaults to 20, capped at 100."}}})),
        ("build_context_pack", "Assemble a ranked context pack of relevant files, symbol definitions, test targets, git history evidence, and architecture policy context for a natural-language task. Returns Markdown by default, sized for an agent's context window. With compress=true it stores the original snippets under the .ok data directory and returns compact handles instead, which retrieve_context expands on demand.", json!({"type":"object","required":["task"],"properties":{"task":{"type":"string","description":"A natural language description of the task to gather context for (e.g., 'refactor the authentication middleware to support OAuth2')."},"compress":{"type":"boolean","description":"Set true to store snippets locally and return short handles instead of inline source, reducing token count. Defaults to false. This is the only path that writes."},"limit":{"type":"integer","description":"Maximum number of context items to gather. Defaults to 20. Raise it when the pack missed a file you expected; it controls coverage, not rendering cost."},"format":{"type":"string","enum":["json","markdown","toon"],"description":"Output format. Defaults to 'markdown', which carries the same evidence as 'json' at a small fraction of the context cost and is what an agent should read. Ask for 'json' only when the result will be parsed rather than read - for example a plan saved for verify_change. 'toon' is token-optimized notation. With compress=true the default is 'json' and 'markdown' is not produced."}}})),
        ("retrieve_context", "Retrieve the original uncompressed source code snippet associated with a compressed context handle.", json!({"type":"object","required":["handle"],"properties":{"handle":{"type":"string","description":"The handle ID returned by build_context_pack with compress=true."}}})),
        ("plan_change", "Generate an evidence-backed pre-edit plan for a task: primary files to edit, expected impact, changed-line ranges, edit boundaries, and recommended test targets. `detail` chooses between the full plan, a concise preflight decision, and a patch plan that writes nothing. With persist=true the plan is turned into a versioned ChangeContractV1, stored under .ok/contracts by default, which verify_change can later hold the actual edit to.", json!({"type":"object","properties":{"task":{"type":"string","description":"A natural language description of the task or change to plan. Required unless persist=true is given an existing `plan` or `plan_json`."},"detail":{"type":"string","enum":["plan","preflight","patch"],"description":"Which artifact to return. 'plan' (default) is the full evidence-backed report; 'preflight' is one concise start decision with verdict, confirmed edit files, risks, and evidence quality; 'patch' is a patch plan that writes no files. Ignored when persist=true. An unknown value is a tool error."},"persist":{"type":"boolean","description":"Set true to build a versioned change contract from the plan instead of returning the plan itself. Defaults to false. This is the only path that writes."},"store":{"type":"boolean","description":"With persist=true, whether the contract is written under .ok/contracts. Defaults to true; set false for a transient contract that is returned but not stored."},"plan":{"type":"object","description":"With persist=true, an inline PlanReport object to build the contract from instead of planning afresh."},"plan_json":{"type":"string","description":"With persist=true, a JSON-encoded PlanReport to build the contract from instead of planning afresh."},"since":{"type":"string","description":"Optional git revision/range used with git diff --unified=0 to include changed files and line ranges in planning context."},"limit":{"type":"integer","description":"Maximum planning results to generate. Defaults to 20."},"format":{"type":"string","enum":["json","markdown","toon","html","text"],"description":"Output format. The full plan defaults to 'markdown', which is what an agent should read; ask for 'json' when the plan will be saved and passed to verify_change. Preflight defaults to 'json' and also accepts 'markdown', 'html', and 'text'. Contracts default to 'json'."}}})),
        ("verify_change", "Verify what actually changed against what was declared. Checks an actual unified diff or changed file list against a saved PlanReport or against a stored or inline change contract, covering boundary constraints, expected file coverage, API surface stability, and dependency policy. Supplying an existing verification report instead explains that report - decision, boundary failures, warnings, dependency deltas, validation attestations, and recommended tests - without verifying anything. Optionally executes configured validation commands and persists timestamped attestation records.", json!({"type":"object","properties":{"plan":{"type":"object","description":"A JSON object containing the saved PlanReport to verify against."},"plan_json":{"type":"string","description":"A JSON-encoded string representation of the PlanReport to verify against."},"contract_id":{"type":"string","description":"Id of a contract stored under .ok/contracts to verify against. Stored ids append verification records to that contract."},"contract":{"type":"object","description":"Inline ChangeContractV1 or StoredContractRecord object to verify against."},"contract_json":{"type":"string","description":"JSON-encoded ChangeContractV1 or StoredContractRecord to verify against."},"verification":{"type":"object","description":"An existing ContractVerificationReport to explain. When present, nothing is verified."},"verification_json":{"type":"string","description":"A JSON-encoded ContractVerificationReport to explain. When present, nothing is verified."},"explain":{"type":"boolean","description":"Set true with a contract to return the explanation of the resulting verification report rather than the report itself. Defaults to false."},"diff":{"type":"string","description":"The unified diff (git diff format) showing the actual changes to verify."},"since_plan":{"type":"string","description":"Git revision or range (e.g., 'HEAD~1', 'abc123..def456') used with git diff --unified=0 to derive changed files and diff input automatically."},"changed_files":{"type":"array","items":{"type":"string"},"description":"List of repository-relative paths of changed files. Used when diff is not provided."},"evidence_refs":{"type":"array","items":{"type":"string"},"description":"List of evidence reference identifiers supporting the change."},"validation_attestations":{"type":"array","items":{"type":"object"},"description":"Previously recorded validation attestations to replay during contract verification."},"traceability_strict":{"type":"boolean","description":"Set true to reject any evidence references not present in the saved plan or contract, enforcing full traceability. Defaults to false (lenient mode allows extra evidence)."},"check_api_surface":{"type":"boolean","description":"Set true to detect public API surface changes (additions, removals, signature modifications) and flag them as warnings. Defaults to false."},"check_dependency_delta":{"type":"boolean","description":"Set true to detect dependency graph changes and flag forbidden dependency additions based on architecture policy. Defaults to false; a configured policy enables it anyway."},"run_commands":{"type":"boolean","description":"Set true to execute shell validation commands (test runners, linters) defined in the plan or contract on the local machine. Commands run synchronously and their exit codes are recorded. Defaults to false."},"write_attestation":{"type":"boolean","description":"Set true together with run_commands to persist timestamped pass/fail attestation records under .ok/contracts/validation/. With a contract it requires a stored contract_id. Defaults to false."},"format":{"type":"string","enum":["json","markdown","toon"],"description":"Return format for contract verification and for explanations. Defaults to json."}}})),
        ("find_tests_for_change", "Identify the test files that should be run to validate a change, ranked by relevance from naming conventions, import relationships, and co-change history. With a `path` the ranking is for that changed file; without one it reports the repository-wide stored test evidence.", json!({"type":"object","properties":{"path":{"type":"string","description":"Repository-relative path of the file being changed (e.g., 'src/auth/handler.rs'). Omit for the repository-wide test evidence."},"limit":{"type":"integer","description":"Maximum number of test file recommendations to return, ranked by relevance. Defaults to 20."}}})),
        ("query_evidence_graph", "Execute a read-only graph query using a constrained subset of Cypher, or, when called with no `query`, return the versioned evidence schema instead: supported node types, edge types, query properties, and the Tier-1 relationship-semantic capability matrix. (Note: the DSL is NOT full Cypher.) Output rows are JSON arrays aligned with the user-selected variables in `columns`.", json!({"type":"object","properties":{"query":{"type":"string","description":"The graph query string to execute. Omit or leave empty to return the evidence schema instead of running a query."},"limit":{"type":"integer","description":"Maximum rows to return. Defaults to 50, capped at 100."},"offset":{"type":"integer","description":"Number of matching rows to skip. Defaults to 0."}}})),
    ];

    // Advertised only where the feature is configured. Both families stay in
    // the dispatch table either way; what the gate removes is a name an agent
    // would otherwise be taught to reach for and get nothing from.
    let memory_tools: &[(&str, &str, Value)] = &[
        ("remember_fact", "Persist a durable, repository-scoped memory fact into the local .ok SQLite store with optional source attribution and confidence level. The fact is append-only and survives re-indexing.", json!({"type":"object","required":["text"],"properties":{"text":{"type":"string","description":"The fact text to persist. Should be a complete, self-contained statement (e.g., 'The auth module uses JWT tokens with 24h expiry'). Maximum ~4KB."},"source":{"type":"string","description":"Identifier for the source that observed this fact (e.g., 'mcp', 'agent', 'human'). Defaults to 'mcp' when omitted."},"confidence":{"type":"string","enum":["low","medium","high","exact"],"description":"Confidence level indicating reliability of the fact. 'low' for uncertain inferences, 'exact' for verified truths. Defaults to 'medium' when omitted."}}})),
        ("search_memory", "Search the append-only repository memory store for previously recorded facts by keyword and entity match. Returns fact text, source, confidence level, and timestamp for each matching entry.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"Keyword or phrase to match against stored fact text and entity names."},"limit":{"type":"integer","description":"Maximum number of matching facts to return, ordered by relevance. Defaults to 20."}}})),
    ];
    let runtime_tools: &[(&str, &str, Value)] = &[
        ("map_stacktrace_to_code", "Map a runtime stack trace to indexed source locations and file lines.", json!({"type":"object","properties":{"stacktrace":{"type":"string","description":"The stack trace string to analyze."}}})),
        ("find_errors_for_symbol", "Retrieve recently stored runtime errors and stack traces for one specific symbol from the local error store. Returns error messages, stack frames, and timestamps when runtime integration is configured; otherwise returns a disabled-status response.", json!({"type":"object","required":["query"],"properties":{"query":{"type":"string","description":"The exact symbol name to look up stored runtime errors for."}}})),
        ("find_recent_failures", "Retrieve a list of recently stored runtime failures, errors, and incidents from the repository's local error store. Returns failure entries with timestamps, error types, and affected symbols when runtime integration is configured; otherwise returns a disabled-status response.", json!({"type":"object","properties":{"limit":{"type":"integer","description":"Maximum number of failure entries to retrieve, ordered by most recent. Defaults to 20."}}})),
    ];

    let mut advertised = read_only_tools.iter().collect::<Vec<_>>();
    if config.memory.enabled {
        advertised.extend(memory_tools.iter());
    }
    if runtime_provider(&config.runtime).is_some() {
        advertised.extend(runtime_tools.iter());
    }

    let mut tools = Vec::new();
    let mut unstable = Vec::new();

    for (name, description, schema) in advertised {
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
            "annotations": tool_annotations(name),
            "_meta": {
                "io.open-kioku/category": tool_category(name).unwrap_or("repository-intelligence"),
                "io.open-kioku/maturity": maturity
            }
        }));
    }

    (tools, unstable)
}

fn tool_maturity(name: &str) -> &'static str {
    match name {
        "map_stacktrace_to_code" | "find_errors_for_symbol" | "find_recent_failures" => {
            "experimental"
        }
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
    let mut metadata = PageMetadata::new(limit, offset, has_more);
    disclose_fetch_cap(&mut metadata, values.len());
    paged_slice_response_with_metadata(key, values, metadata)
}

/// Reports the `search_fetch_limit` clamp on the response that the clamp shaped.
///
/// `search_fetch_limit` asks for `offset + limit + 1` but never more than
/// `MAX_MCP_FETCH`, while `offset` alone accepts far more than that. Once the
/// clamp bites, the `+1` sentinel `has_more` is derived from was never fetched,
/// so a short page is indistinguishable from the end of the results. Every
/// caller that fetches through `search_fetch_limit` must run its metadata
/// through here, or it will quietly answer "that is all of them".
fn disclose_fetch_cap(metadata: &mut PageMetadata, fetched: usize) {
    let window = metadata
        .offset
        .saturating_add(metadata.limit)
        .saturating_add(1);
    if window <= MAX_MCP_FETCH || fetched < MAX_MCP_FETCH {
        return;
    }
    metadata.has_more = true;
    metadata.truncated = true;
    metadata.warnings.push(format!(
        "search results were scanned up to {MAX_MCP_FETCH} candidates; narrow the query or use a lower offset"
    ));
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

/// An optional string parameter. Absent is `None`; present but not a string is
/// an error, because returning the broader answer a missing parameter selects
/// would answer a question the caller did not ask.
fn optional_str<'a>(params: &'a Value, key: &str) -> anyhow::Result<Option<&'a str>> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.as_str())),
        Some(other) => anyhow::bail!("`{key}` must be a string, got {other}"),
    }
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
    // `since` is caller input on a read-only server. Without the terminator a value such as
    // `--output=<path>` is an option to git, which exits 0 and writes the diff there.
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "diff",
            "--unified=0",
            "--no-ext-diff",
            "--relative",
            "--end-of-options",
        ])
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
    config: &OkConfig,
    params: &Value,
) -> anyhow::Result<PlanReport> {
    let task = params.get("task").and_then(Value::as_str);
    let plan = params.get("plan").filter(|value| !value.is_null());
    let plan_json = params.get("plan_json").and_then(Value::as_str);
    let selectors = task.is_some() as u8 + plan.is_some() as u8 + plan_json.is_some() as u8;
    anyhow::ensure!(
        selectors == 1,
        "plan_change with persist=true requires exactly one of `task`, `plan`, or `plan_json`"
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
    let memory_facts = RepoMemoryStore::search_repo(repo, &task, 8)?;
    let mut context = ContextPackBuilder::new(store as &dyn OkStore)
        .with_history_store(Some(store))
        .build(&task, limit)?;
    context.architecture_policy = configured_architecture_policy_report(repo, store)?;
    Ok(PlanEngine::new(store as &dyn OkStore)
        .with_history_store(Some(store))
        .with_memory_facts(memory_facts)
        .with_memory_enabled(config.memory.enabled)
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

/// Resolve a path, symbol name, or explicit `file:`/`symbol:` node id to a graph node id.
/// A name that resolves to nothing is an error: passing it through produced an empty edge
/// list that read as "these two are unconnected" when the truth was "this is not in the
/// index". The same check backs `ok path`.
fn resolve_graph_node<S>(store: &S, query: &str) -> anyhow::Result<String>
where
    S: MetadataStore + GraphStore + ?Sized,
{
    if query.starts_with("file:") || query.starts_with("symbol:") {
        return match store.node_by_id(query)? {
            Some(_) => Ok(query.to_string()),
            None => anyhow::bail!(
                "`{query}` is not a node in the indexed dependency graph; it may be excluded, unsupported, or added since the last `ok index`"
            ),
        };
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
    anyhow::bail!(
        "`{query}` is not an indexed file path or symbol name; it may be excluded, unsupported, or added since the last `ok index`"
    )
}

/// The three kinds of evidence an agent asks for about one resolved symbol,
/// answered in one response with each kind's provenance kept apart.
///
/// References are occurrence evidence, calls are persisted CALLS edges, and
/// implementations are persisted IMPLEMENTS facts. They are not
/// interchangeable, and absence does not mean the same thing in each: no
/// occurrence is a different claim from no persisted IMPLEMENTS fact. Merging
/// them into one ranked list would move that distinction into the reader's
/// head, so every section instead names its own `evidence_source` and carries
/// its own caveats.
fn symbol_evidence_tool<S>(store: &S, params: &Value) -> anyhow::Result<Value>
where
    S: MetadataStore + GraphStore,
{
    let query = required_str(params, "query")?;
    let kind = params
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("references");
    anyhow::ensure!(
        matches!(
            kind,
            "references" | "callers" | "callees" | "implementations" | "all"
        ),
        "unknown `kind` `{kind}` for get_references; expected one of references, callers, callees, implementations, all"
    );
    let limit = limit(params);
    let engine = SymbolEngine::new(store);
    let wanted = |section: &str| kind == section || kind == "all";

    // Implementation facts are keyed by target name, so a trait the index never
    // recorded a definition for still has answerable IMPLEMENTS evidence.
    // Occurrence and call evidence need a resolved symbol, so an unresolvable
    // name is still an error for those.
    let symbol = match engine.definition(query) {
        Ok(symbol) => Some(symbol),
        Err(err) if matches!(kind, "implementations" | "all") => {
            let mut response = Map::new();
            response.insert("query".into(), json!(query));
            response.insert("kind".into(), json!(kind));
            response.insert("symbol".into(), Value::Null);
            response.insert("symbol_resolution".into(), json!(err.to_string()));
            response.insert(
                "implementations".into(),
                implementation_section(store, query, limit)?,
            );
            if kind == "all" {
                response.insert(
                    "caveats".into(),
                    json!([format!(
                        "`{query}` did not resolve to an indexed symbol, so occurrence and call evidence could not be gathered; only IMPLEMENTS facts, which are keyed by target name, are reported"
                    )]),
                );
            }
            return Ok(Value::Object(response));
        }
        Err(err) => return Err(err.into()),
    };
    let symbol = symbol.context("symbol resolution must have produced a definition")?;

    let mut response = Map::new();
    response.insert("query".into(), json!(query));
    response.insert("kind".into(), json!(kind));
    response.insert("symbol".into(), serde_json::to_value(&symbol)?);

    if wanted("references") {
        let mut occurrences = engine.references(query, overfetch_limit(limit))?;
        let has_more = occurrences.len() > limit;
        occurrences.truncate(limit);
        response.insert(
            "references".into(),
            json!({
                "evidence_source": "symbol_occurrences",
                "occurrences": occurrences,
                "returned": occurrences.len(),
                "limit": limit,
                "has_more": has_more,
                "caveats": [
                    "each occurrence carries its own provenance and confidence; a `lexical` occurrence at low confidence is the name-match fallback used when the index holds no resolved occurrence for this symbol"
                ],
            }),
        );
    }

    for (section, inbound) in [("callers", true), ("callees", false)] {
        if !wanted(section) {
            continue;
        }
        response.insert(
            section.into(),
            call_edge_section(store, &symbol, inbound, limit)?,
        );
    }

    if wanted("implementations") {
        response.insert(
            "implementations".into(),
            implementation_section(store, query, limit)?,
        );
    }

    Ok(Value::Object(response))
}

/// Inbound or outbound CALLS edges for one symbol, from the persisted graph.
fn call_edge_section<S>(
    store: &S,
    symbol: &open_kioku_core::Symbol,
    inbound: bool,
    limit: usize,
) -> anyhow::Result<Value>
where
    S: GraphStore + ?Sized,
{
    let node = format!("symbol:{}", symbol.id.0);
    let (nodes, edges) = store.neighbors(&node, MAX_MCP_FETCH)?;
    let mut edges = edges
        .into_iter()
        .filter(|edge| {
            edge.edge_type == GraphEdgeType::Calls
                && if inbound {
                    edge.to.0 == node
                } else {
                    edge.from.0 == node
                }
        })
        .collect::<Vec<_>>();
    let has_more = edges.len() > limit;
    edges.truncate(limit);
    let related_ids = edges
        .iter()
        .map(|edge| if inbound { &edge.from.0 } else { &edge.to.0 })
        .collect::<std::collections::BTreeSet<_>>();
    let nodes = nodes
        .into_iter()
        .filter(|candidate| related_ids.contains(&candidate.id.0))
        .collect::<Vec<_>>();
    Ok(json!({
        "evidence_source": "sqlite_graph_store",
        "direction": if inbound { "inbound" } else { "outbound" },
        "nodes": nodes,
        "edges": edges,
        "returned": edges.len(),
        "limit": limit,
        "has_more": has_more,
        "caveats": [
            "call edges are persisted CALLS graph edges; a repository without call evidence returns none rather than an inferred path"
        ],
    }))
}

/// Verified implementation sites from persisted IMPLEMENTS facts.
fn implementation_section<S>(store: &S, query: &str, limit: usize) -> anyhow::Result<Value>
where
    S: MetadataStore + ?Sized,
{
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
        "evidence_source": "persisted_implements_facts",
        "implementations": implementations,
        "returned": implementations.len(),
        "limit": limit,
        "has_more": has_more,
        "caveats": [
            "returns only persisted IMPLEMENTS facts; absent language or parser evidence yields no result"
        ],
    }))
}

/// The language inventory `list_languages` used to answer. Index coverage
/// already records one entry per language the last index saw, so the usual
/// path costs nothing extra; the file scan is the fallback for indexes written
/// before coverage recording, where the alternative is reporting no languages.
fn indexed_languages(
    store: &dyn MetadataStore,
    coverage: Option<&open_kioku_core::IndexCoverage>,
) -> anyhow::Result<Vec<String>> {
    if let Some(coverage) = coverage.filter(|coverage| !coverage.by_language.is_empty()) {
        return Ok(coverage.by_language.keys().cloned().collect());
    }
    let mut languages = store
        .list_files(usize::MAX, 0)?
        .into_iter()
        .map(|file| file.language.key().to_string())
        .collect::<Vec<_>>();
    languages.sort_unstable();
    languages.dedup();
    Ok(languages)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_rendered_string_payload_is_not_duplicated_into_structured_content() {
        // A Markdown or TOON rendering used to be sent twice: once in `content`, once
        // wrapped as {"value": ...} in `structuredContent`, so every byte was paid twice
        // and, when the text was capped, the uncapped copy was the larger one.
        let oversized = "x".repeat(MAX_TOOL_TEXT_BYTES + 5_000);
        let value = json!(oversized);
        let mut text = value.as_str().unwrap().to_string();
        let text_truncated = truncate_utf8(&mut text, MAX_TOOL_TEXT_BYTES);
        assert!(text_truncated, "the fixture must actually exceed the cap");
        let structured = structured_content_for(value, &text, text_truncated);
        assert_eq!(structured["rendered_in"], "content");
        assert_eq!(structured["bytes"], text.len());
        assert!(
            text.len() <= MAX_TOOL_TEXT_BYTES + 32,
            "content respects the cap"
        );
        assert_eq!(structured["truncated"], true);
        assert!(
            serde_json::to_string(&structured).unwrap().len() < 128,
            "structuredContent must be a pointer, not a copy of the text"
        );
        // Objects pass through untouched; other scalars are still wrapped.
        let object = json!({"files": 3});
        assert_eq!(structured_content_for(object.clone(), "{}", false), object);
        assert_eq!(
            structured_content_for(json!(7), "7", false),
            json!({"value": 7})
        );
    }
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
            Some(&store),
            &config,
            r#"{"jsonrpc":"2.0","id":"req-1","method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        )
        .await
        .expect("initialize request should return a response");
        assert_eq!(string_id.id, Some(json!("req-1")));
        assert_eq!(string_id.result.unwrap()["protocolVersion"], "2024-11-05");

        let numeric_id = handle_line(
            Path::new("."),
            Some(&store),
            &config,
            r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{}}"#,
        )
        .await
        .expect("numeric request should return a response");
        assert_eq!(numeric_id.id, Some(json!(7)));
        assert!(numeric_id.error.is_none());

        let initialized_notification = handle_line(
            Path::new("."),
            Some(&store),
            &config,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
        )
        .await;
        assert!(initialized_notification.is_none());

        let missing_method = handle_line(
            Path::new("."),
            Some(&store),
            &config,
            r#"{"jsonrpc":"2.0","id":"missing-method","params":{}}"#,
        )
        .await
        .expect("invalid request with id should return an error response");
        assert_eq!(missing_method.id, Some(json!("missing-method")));
        assert_eq!(missing_method.error.unwrap()["code"], -32600);

        let malformed = handle_line(Path::new("."), Some(&store), &config, "{")
            .await
            .expect("parse errors should return an error response");
        assert_eq!(malformed.id, None);
        assert_eq!(malformed.error.unwrap()["code"], -32700);

        let malformed_unicode = handle_line(
            Path::new("."),
            Some(&store),
            &config,
            r#"{"jsonrpc":"2.0","id":"bad-unicode","method":"initialize","params":{"client":"\uD800"}}"#,
        )
        .await
        .expect("parse errors should return an error response");
        assert_eq!(malformed_unicode.id, None);
        assert_eq!(malformed_unicode.error.unwrap()["code"], -32700);

        let unknown_method = handle_line(
            Path::new("."),
            Some(&store),
            &config,
            r#"{"jsonrpc":"2.0","id":"unknown-method","method":"missing_method","params":{}}"#,
        )
        .await
        .expect("unknown request with id should return an error response");
        assert_eq!(unknown_method.id, Some(json!("unknown-method")));
        assert_eq!(unknown_method.error.unwrap()["code"], -32000);

        let tool_error = handle_line(
            Path::new("."),
            Some(&store),
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
            Some(&store),
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
                "evidence_schema.json",
                r#"{"jsonrpc":"2.0","id":"evidence-schema","method":"query_evidence_graph","params":{}}"#,
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
            // The two shapes a successful `tools/call` can take. Only the error envelope was
            // pinned before, so #389 changed the wire shape of every rendered response without
            // a snapshot moving (#392).
            (
                "tools_call_json_tool.json",
                r#"{"jsonrpc":"2.0","id":"tools-call-json","method":"tools/call","params":{"name":"list_files","arguments":{"limit":1}}}"#,
            ),
            (
                "tools_call_rendered_tool.json",
                r#"{"jsonrpc":"2.0","id":"tools-call-rendered","method":"tools/call","params":{"name":"build_context_pack","arguments":{"task":"publish invoice","limit":1,"format":"markdown"}}}"#,
            ),
            (
                "pagination.json",
                r#"{"jsonrpc":"2.0","id":"pagination","method":"list_files","params":{"limit":1,"offset":0}}"#,
            ),
            (
                "list_files_detail.json",
                r#"{"jsonrpc":"2.0","id":"list-files-detail","method":"list_files","params":{"path":"src/billing.rs"}}"#,
            ),
            (
                "semantic_search_not_ready.json",
                r#"{"jsonrpc":"2.0","id":"semantic-search","method":"search_code","params":{"query":"publish invoice","mode":"semantic","limit":1}}"#,
            ),
            (
                "hybrid_search_lexical_fallback.json",
                r#"{"jsonrpc":"2.0","id":"hybrid-search","method":"search_code","params":{"query":"publish invoice","mode":"hybrid","limit":1}}"#,
            ),
            (
                "regex_search.json",
                r#"{"jsonrpc":"2.0","id":"regex-search","method":"regex_search","params":{"pattern":"pub fn publish_\\w+","limit":5}}"#,
            ),
            (
                "regex_search_invalid_pattern.json",
                r#"{"jsonrpc":"2.0","id":"regex-search-invalid","method":"regex_search","params":{"pattern":"pub fn (","limit":5}}"#,
            ),
            (
                "get_definition_with_body.json",
                r#"{"jsonrpc":"2.0","id":"get-definition-body","method":"get_definition","params":{"query":"publish_invoice_event","include_body":true}}"#,
            ),
            (
                "get_references.json",
                r#"{"jsonrpc":"2.0","id":"get-references","method":"get_references","params":{"query":"publish_invoice_event","limit":1}}"#,
            ),
            // Every evidence kind in one response, which is where the
            // provenance of each has to stay legible.
            (
                "get_references_all.json",
                r#"{"jsonrpc":"2.0","id":"get-references-all","method":"get_references","params":{"query":"publish_invoice_event","kind":"all","limit":1}}"#,
            ),
            (
                "get_references_implementations.json",
                r#"{"jsonrpc":"2.0","id":"get-references-impls","method":"get_references","params":{"query":"InvoicePublisher","kind":"implementations","limit":1}}"#,
            ),
            (
                "get_references_callers.json",
                r#"{"jsonrpc":"2.0","id":"get-references-callers","method":"get_references","params":{"query":"archive_invoice_event","kind":"callers","limit":1}}"#,
            ),
            (
                "dependency_path_neighbors.json",
                r#"{"jsonrpc":"2.0","id":"dependency-neighbors","method":"dependency_path","params":{"from":"src/billing.rs","limit":5}}"#,
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
            let response = handle_line(&fixture.repo, Some(&fixture.store), &fixture.config, line)
                .await
                .expect("snapshot request should return a response");
            assert_mcp_snapshot(name, &response);
        }
    }

    /// Every line a session sends is answered from no store at all, and the repository on
    /// disk is exactly as it was: no `.ok`, no database. The handshake and the inventory
    /// still answer so a client can connect and see what indexing would give it.
    #[tokio::test]
    async fn unindexed_repository_is_served_without_creating_an_index() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().to_path_buf();
        fs::write(repo.join("main.rs"), "fn main() {}\n").unwrap();
        let config = OkConfig::default();

        let session = |lines: &[&str]| format!("{}\n", lines.join("\n"));
        let drive = |input: String| {
            let repo = repo.clone();
            let config = config.clone();
            async move {
                let mut output = Vec::new();
                serve(repo, config, BufReader::new(input.as_bytes()), &mut output)
                    .await
                    .expect("an unindexed repository is served, not refused");
                String::from_utf8(output)
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str::<Value>(line).unwrap())
                    .collect::<Vec<_>>()
            }
        };

        let handshake = drive(session(&[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        ]))
        .await;
        assert_eq!(handshake[0]["result"]["serverInfo"]["name"], "open-kioku");
        assert_eq!(
            handshake[1]["result"]["tools"].as_array().unwrap().len(),
            16
        );
        assert!(
            !repo.join(".ok").exists(),
            "initialize and tools/list must not create .ok"
        );

        let calls = drive(session(&[
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"repo_status","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"search_code","arguments":{"query":"main"}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"get_callers","arguments":{"query":"main"}}}"#,
            r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"no_such_tool","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":7,"method":"repo_status","params":{}}"#,
        ]))
        .await;
        assert!(
            !repo.join(".ok").exists(),
            "tool calls on an unindexed repository must not create .ok"
        );
        let expected_next_step = format!("ok index {}", repo.display());

        let status = &calls[0]["result"];
        assert_eq!(status["isError"], false);
        assert_eq!(status["structuredContent"]["indexed"], false);
        assert_eq!(status["structuredContent"]["next_step"], expected_next_step);
        let message = status["structuredContent"]["message"].as_str().unwrap();
        assert!(
            message.starts_with("repository is not indexed"),
            "{message}"
        );
        assert!(message.contains(&expected_next_step), "{message}");
        assert_eq!(
            status["structuredContent"]["index_path"],
            json!(repo.join(".ok/index.sqlite"))
        );

        let search = &calls[1]["error"];
        assert_eq!(search["code"], -32000);
        assert_eq!(search["message"], not_indexed_message(&repo));
        assert!(search["message"]
            .as_str()
            .unwrap()
            .contains(&expected_next_step));

        // Retired and unknown names keep their own answers: "not indexed" is not their fix.
        assert!(calls[2]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("retired from the MCP tool surface"));
        assert!(calls[3]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown MCP method or tool"));
        assert_eq!(calls[4]["result"]["indexed"], false);
        assert_eq!(calls[4]["result"]["next_step"], expected_next_step);
    }

    /// An index without a manifest is what a 4.0.0 read surface left behind; it is served as
    /// unindexed, not as a legacy index awaiting rebuild.
    #[tokio::test]
    async fn empty_index_database_is_served_as_unindexed() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().to_path_buf();
        fs::create_dir_all(repo.join(".ok")).unwrap();
        drop(SqliteStore::open(repo.join(".ok/index.sqlite")).unwrap());
        assert!(SqliteStore::open_repo_index(&repo).unwrap().is_none());

        let mut output = Vec::new();
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_references","arguments":{"query":"main","kind":"all"}}}"#,
            "\n"
        );
        serve(
            repo.clone(),
            OkConfig::default(),
            BufReader::new(input.as_bytes()),
            &mut output,
        )
        .await
        .unwrap();
        let response: Value =
            serde_json::from_str(String::from_utf8(output).unwrap().trim()).unwrap();
        let message = response["error"]["message"].as_str().unwrap();
        assert_eq!(message, not_indexed_message(&repo));
        assert!(!message.contains("legacy index"), "{message}");
    }

    #[tokio::test]
    async fn search_code_refuses_a_blank_query() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();
        for params in [json!({"query": ""}), json!({"query": "   "}), json!({})] {
            let error = dispatch(
                Path::new("."),
                &store,
                &config,
                "search_code",
                params.clone(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("requires a non-empty `query`"),
                "{params}: {error}"
            );
        }
    }

    #[tokio::test]
    async fn dependency_path_refuses_nodes_the_index_does_not_hold() {
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
        for (params, missing) in [
            (
                json!({"from": "nope.rs", "to": "also_nope.rs"}),
                "`nope.rs`",
            ),
            (json!({"from": "file:nope.rs"}), "`file:nope.rs`"),
        ] {
            let error = dispatch(Path::new("."), &store, &config, "dependency_path", params)
                .await
                .unwrap_err()
                .to_string();
            assert!(error.contains(missing), "{error}");
            assert!(error.contains("ok index"), "{error}");
        }
    }

    #[tokio::test]
    async fn retrieve_context_refuses_an_unknown_handle() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();
        let error = dispatch(
            temp.path(),
            &store,
            &config,
            "retrieve_context",
            json!({"handle": "bogus"}),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("no context handle `bogus`"), "{error}");
        assert!(
            !temp.path().join(".ok").exists(),
            "looking up a handle must not create .ok/context.sqlite"
        );
    }

    #[tokio::test]
    async fn impact_analysis_on_an_unindexed_path_reports_unknown_risk() {
        let temp = tempfile::tempdir().unwrap();
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
        let report = dispatch(
            temp.path(),
            &store,
            &config,
            "impact_analysis",
            json!({"path": "does/not/exist.rs"}),
        )
        .await
        .unwrap();
        assert_eq!(report["risk_report"]["level"], "unknown", "{report}");
        assert!(report["risk_report"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap()
                .contains("`does/not/exist.rs` is not in the index")));
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

    /// `search_fetch_limit` clamps the fetch at `MAX_MCP_FETCH` while `offset`
    /// accepts far more, so a deep page loses the `+1` sentinel `has_more` is
    /// derived from. Without disclosure the response reads as the end of the
    /// matches, and an agent paging a broad pattern concludes it has seen every
    /// occurrence in the repository.
    #[tokio::test]
    async fn regex_search_reports_truncation_when_the_fetch_cap_swallows_the_page() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();
        let file = File {
            id: FileId::new("file-wide"),
            repository_id: RepositoryId::new("repo"),
            path: "src/wide.rs".into(),
            language: Language::Rust,
            size_bytes: 4096,
            content_hash: "hash-wide".into(),
            is_generated: false,
            is_vendor: false,
        };
        // Comfortably more matching lines than the fetch cap can return.
        let line_count = MAX_MCP_FETCH + 200;
        let text = (0..line_count)
            .map(|i| format!("fn generated_{i}() {{}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunk = CodeChunk {
            id: "chunk-wide".into(),
            file_id: file.id.clone(),
            range: LineRange {
                start: 1,
                end: line_count as u32,
            },
            language: Language::Rust,
            text,
            symbol_id: None,
        };
        store
            .replace_index(IndexData {
                manifest: &fixture_manifest(),
                files: &[file],
                symbols: &[],
                chunks: &[chunk],
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let deep = dispatch(
            Path::new("."),
            &store,
            &config,
            "regex_search",
            json!({"pattern": "^fn generated_", "limit": 20, "offset": MAX_MCP_FETCH}),
        )
        .await
        .unwrap();

        assert_eq!(deep["returned"], 0, "the clamped page returns nothing");
        assert_eq!(
            deep["truncated"], true,
            "a page the fetch cap swallowed must say so: {deep}"
        );
        assert_eq!(
            deep["has_more"], true,
            "an empty clamped page must not read as the end of the matches: {deep}"
        );
        assert!(
            deep["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning
                    .as_str()
                    .unwrap_or_default()
                    .contains("scanned up to")),
            "the cap needs a warning naming it: {deep}"
        );

        // A page inside the cap keeps reporting honestly rather than warning always.
        let shallow = dispatch(
            Path::new("."),
            &store,
            &config,
            "regex_search",
            json!({"pattern": "^fn generated_", "limit": 5, "offset": 0}),
        )
        .await
        .unwrap();
        assert_eq!(shallow["returned"], 5);
        assert_eq!(shallow["has_more"], true);
        assert_eq!(
            shallow["truncated"], false,
            "an unclamped page is not truncated: {shallow}"
        );
        assert!(shallow["warnings"].as_array().unwrap().is_empty());
    }

    /// A pre-4.0 index has its edges discarded on open and only the marker records that; the
    /// analysis fingerprint is unchanged since 3.1.0, so the fingerprint gate alone let
    /// `impact_analysis` answer `proven_impact: []` and `plan_change` drop its relationship
    /// sentence from such a store.
    #[tokio::test]
    async fn impact_plan_and_context_refuse_a_graph_awaiting_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.sqlite");
        let config = OkConfig::default();
        let manifest = fixture_manifest();
        let file = open_kioku_core::File {
            id: open_kioku_core::FileId::new("f1"),
            repository_id: open_kioku_core::RepositoryId::new("repo"),
            path: PathBuf::from("src/lib.rs"),
            language: open_kioku_core::Language::Rust,
            size_bytes: 10,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        };
        // The context pack only reads the graph once a primary file is selected, so the
        // task must match indexed text or the refusal is never reached.
        let chunk = CodeChunk {
            id: "chunk-worker".into(),
            file_id: file.id.clone(),
            range: LineRange { start: 1, end: 2 },
            language: Language::Rust,
            text: "pub struct Worker;\nimpl Worker { pub fn run(&self) {} }\n".into(),
            symbol_id: None,
        };
        {
            let store = SqliteStore::open(&path).unwrap();
            store
                .replace_index(IndexData {
                    manifest: &manifest,
                    files: &[file],
                    symbols: &[],
                    chunks: &[chunk],
                    tests: &[],
                    imports: &[],
                    occurrences: &[],
                    analysis_facts: &[],
                    scopes: &[],
                    bindings: &[],
                    call_sites: &[],
                })
                .unwrap();
        }
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL); \
                 INSERT OR REPLACE INTO schema_meta(key, value) VALUES('graph_rebuild_required_v4', '1');",
            )
            .unwrap();
        let store = SqliteStore::open(&path).unwrap();

        let status = dispatch(Path::new("."), &store, &config, "repo_status", json!({}))
            .await
            .unwrap();
        assert_eq!(status["graph_rebuild_required"], true);
        assert_eq!(
            status["analysis_semantics_status"]["status"], "compatible",
            "the fingerprint gate alone does not see the marker: {status}"
        );

        for (method, params) in [
            ("impact_analysis", json!({"path": "src/lib.rs"})),
            ("plan_change", json!({"task": "change src/lib.rs"})),
            (
                "plan_change",
                json!({"task": "change src/lib.rs", "detail": "preflight"}),
            ),
            ("build_context_pack", json!({"task": "change Worker::run"})),
        ] {
            let error = dispatch(Path::new("."), &store, &config, method, params)
                .await
                .unwrap_err()
                .to_string();
            assert!(
                error.contains("older index format") && error.contains("ok index"),
                "{method} must name the rebuild, got: {error}"
            );
        }
    }

    /// `since_plan` is caller input on a read-only server: after `--end-of-options` an
    /// option-shaped value is a revision git cannot resolve, not a file it writes.
    #[test]
    fn verify_since_plan_never_reaches_git_as_an_option() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "--quiet"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test User"]);
        git(&["config", "commit.gpgsign", "false"]);
        fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "--quiet", "-m", "one"]);
        fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();

        let sink = temp.path().join("stolen-diff.txt");
        let since = format!("--output={}", sink.display());
        assert!(git_diff_since(repo, &since).is_err());
        assert!(
            !sink.exists(),
            "git must not have written {}",
            sink.display()
        );

        let diff = git_diff_since(repo, "HEAD").unwrap().unwrap();
        assert!(diff.contains("+two"), "{diff}");
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
        assert_eq!(tools_ro.len(), 16, "the documented MCP inventory changed");
        for (retired_tool, _) in RETIRED_TOOLS {
            assert!(
                tools_ro.iter().all(|tool| tool["name"] != *retired_tool),
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
            assert!(
                tool["_meta"]["io.open-kioku/category"]
                    .as_str()
                    .is_some_and(|category| !category.is_empty()),
                "{name} should expose a machine-readable routing category"
            );
            assert_eq!(
                tool["_meta"]["io.open-kioku/maturity"], tool["maturity"],
                "{name} should expose consistent maturity metadata"
            );

            let schema = tool["inputSchema"].as_object().unwrap();
            assert_eq!(schema.get("type"), Some(&json!("object")));
            let properties = schema
                .get("properties")
                .and_then(Value::as_object)
                .expect("tool input schema properties");
            for (property_name, property) in properties {
                assert!(
                    property["description"]
                        .as_str()
                        .is_some_and(|description| !description.is_empty()),
                    "{name}.{property_name} should describe its input"
                );
            }
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for property_name in required {
                    assert!(
                        properties.contains_key(property_name.as_str().unwrap()),
                        "{name} requires an undeclared input property"
                    );
                }
            }
        }
        let references = tools_ro
            .iter()
            .find(|tool| tool["name"] == "get_references")
            .unwrap();
        let description = references["description"].as_str().unwrap();
        assert!(description.contains("persisted IMPLEMENTS facts"));
        assert!(
            description.contains("evidence_source"),
            "the merged tool must advertise that each section names its own provenance"
        );

        // `remember_fact` is only advertised when memory is configured, so the
        // annotation check moves to the configured surface.
        let mut memory_config = OkConfig::default();
        memory_config.memory.enabled = true;
        let (memory_tools, _) = tools(&memory_config);
        let remember_fact = memory_tools
            .iter()
            .find(|tool| tool["name"] == "remember_fact")
            .unwrap();
        assert_eq!(remember_fact["annotations"]["readOnlyHint"], false);
        assert_eq!(remember_fact["annotations"]["destructiveHint"], false);

        // Conditionally writing tools must say so: the compress and persist
        // paths write under .ok, and a readOnlyHint of true would be a lie a
        // client's approval policy would act on.
        for name in ["build_context_pack", "plan_change"] {
            let tool = tools_ro.iter().find(|tool| tool["name"] == name).unwrap();
            assert_eq!(
                tool["annotations"]["readOnlyHint"], false,
                "{name} can write and must not claim otherwise"
            );
        }
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
        for (retired_tool, _) in RETIRED_TOOLS {
            assert!(
                tools_write_enabled
                    .iter()
                    .all(|tool| tool["name"] != *retired_tool),
                "{retired_tool} must not be restored by write configuration"
            );
        }
    }

    #[tokio::test]
    async fn merged_symbol_evidence_keeps_each_provenance_separate() {
        // `get_references` absorbed the call and implementation lookups. The
        // point of the merge is one call; the risk of the merge is that
        // occurrence evidence, graph call edges, and persisted IMPLEMENTS facts
        // read as one undifferentiated list. Each has to keep its own source,
        // its own caveats, and its own meaning of "empty".
        let fixture = McpSnapshotFixture::new();
        let all = dispatch(
            &fixture.repo,
            &fixture.store,
            &fixture.config,
            "get_references",
            json!({"query": "publish_invoice_event", "kind": "all", "limit": 5}),
        )
        .await
        .unwrap();

        assert_eq!(all["kind"], "all");
        assert_eq!(
            all["references"]["evidence_source"], "symbol_occurrences",
            "occurrence evidence must name its own source: {all}"
        );
        assert_eq!(
            all["callers"]["evidence_source"], "sqlite_graph_store",
            "call edges must name the graph store, not the occurrence table: {all}"
        );
        assert_eq!(all["callees"]["evidence_source"], "sqlite_graph_store");
        assert_eq!(all["callers"]["direction"], "inbound");
        assert_eq!(all["callees"]["direction"], "outbound");
        assert_eq!(
            all["implementations"]["evidence_source"], "persisted_implements_facts",
            "IMPLEMENTS facts must not be presented as occurrences: {all}"
        );

        for section in ["references", "callers", "callees", "implementations"] {
            assert!(
                !all[section]["caveats"].as_array().unwrap().is_empty(),
                "{section} must carry the caveat that says what its absence means"
            );
        }
        // No section leaks into another: the payload key differs per kind, so a
        // caller cannot read an implementation as a reference by position.
        assert!(all["references"]["occurrences"].is_array());
        assert!(all["implementations"]["implementations"].is_array());
        assert!(all["references"].get("implementations").is_none());
        assert!(all["implementations"].get("occurrences").is_none());

        // Each per-occurrence record still carries the provenance the fusion
        // rules depend on, so an exact occurrence is never confused with the
        // low-confidence lexical fallback.
        for occurrence in all["references"]["occurrences"].as_array().unwrap() {
            assert!(occurrence["provenance"].is_string(), "{occurrence}");
            assert!(occurrence["confidence"].is_string(), "{occurrence}");
        }

        // A trait the index holds no definition for still has answerable
        // IMPLEMENTS evidence; requiring symbol resolution first would have
        // silently dropped it.
        let implementations = dispatch(
            &fixture.repo,
            &fixture.store,
            &fixture.config,
            "get_references",
            json!({"query": "InvoicePublisher", "kind": "implementations", "limit": 5}),
        )
        .await
        .unwrap();
        assert!(implementations["symbol"].is_null());
        assert_eq!(
            implementations["implementations"]["evidence_source"],
            "persisted_implements_facts"
        );
        assert_eq!(implementations["implementations"]["returned"], 1);

        // `all` degrades the way `implementations` does rather than losing the
        // IMPLEMENTS evidence the specific kind would still have returned for
        // the same name.
        let unresolved_all = dispatch(
            &fixture.repo,
            &fixture.store,
            &fixture.config,
            "get_references",
            json!({"query": "InvoicePublisher", "kind": "all", "limit": 5}),
        )
        .await
        .unwrap();
        assert!(unresolved_all["symbol"].is_null());
        assert_eq!(unresolved_all["implementations"]["returned"], 1);
        assert!(
            unresolved_all["caveats"]
                .as_array()
                .unwrap()
                .iter()
                .any(|caveat| caveat
                    .as_str()
                    .unwrap_or_default()
                    .contains("did not resolve to an indexed symbol")),
            "the sections that could not be gathered must be named: {unresolved_all}"
        );

        // An unknown kind is an error, not a quietly different answer.
        let unknown = dispatch(
            &fixture.repo,
            &fixture.store,
            &fixture.config,
            "get_references",
            json!({"query": "publish_invoice_event", "kind": "callsites"}),
        )
        .await
        .unwrap_err();
        assert!(unknown.to_string().contains("unknown `kind`"));
    }

    #[tokio::test]
    async fn a_present_but_wrong_typed_optional_parameter_is_an_error() {
        // Making a parameter optional in the fold must not turn a type error
        // into a broader answer. `find_tests_for_change` and
        // `query_evidence_graph` required theirs before the fold, so a
        // non-string had been an error; answering repository-wide evidence or
        // the schema instead answers a question the caller did not ask.
        let fixture = McpSnapshotFixture::new();
        for (tool, params) in [
            ("find_tests_for_change", json!({"path": 123})),
            ("list_files", json!({"path": ["src/billing.rs"]})),
            ("query_evidence_graph", json!({"query": 7})),
        ] {
            let error = dispatch(&fixture.repo, &fixture.store, &fixture.config, tool, params)
                .await
                .err()
                .unwrap_or_else(|| {
                    panic!("{tool} answered instead of rejecting a non-string parameter")
                });
            assert!(
                error.to_string().contains("must be a string"),
                "{tool} should name the type it wanted: {error}"
            );
        }

        // An absent parameter still selects the broader answer it was folded in
        // to provide.
        let repo_wide = dispatch(
            &fixture.repo,
            &fixture.store,
            &fixture.config,
            "find_tests_for_change",
            json!({}),
        )
        .await
        .expect("an absent `path` is still the repository-wide question");
        assert!(repo_wide.is_object() || repo_wide.is_array());
    }

    #[tokio::test]
    async fn retired_tool_names_fail_loudly_instead_of_resolving() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();
        for (retired_tool, guidance) in RETIRED_TOOLS {
            let error = dispatch(Path::new("."), &store, &config, retired_tool, json!({}))
                .await
                .expect_err("a retired tool name must not dispatch");
            let message = error.to_string();
            assert!(
                message.contains(retired_tool),
                "the error for `{retired_tool}` must name it: {message}"
            );
            // A bare refusal makes the agent guess. The mapping is known here,
            // so the error is a migration instruction, not just a rejection.
            assert!(
                message.contains(guidance),
                "the error for `{retired_tool}` must say where the capability went: {message}"
            );
        }
    }

    #[tokio::test]
    async fn memory_and_runtime_tools_are_advertised_only_when_configured() {
        let store = SqliteStore::open(":memory:").unwrap();
        let gated = ["remember_fact", "search_memory"];
        let runtime = [
            "map_stacktrace_to_code",
            "find_errors_for_symbol",
            "find_recent_failures",
        ];

        let default_config = OkConfig::default();
        let (advertised, _) = tools(&default_config);
        let names = advertised
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        for name in gated.iter().chain(runtime.iter()) {
            assert!(
                !names.contains(name),
                "{name} must stay off the default surface"
            );
        }
        // Gated off the listing, still answerable: the disabled stub is the
        // honest answer, and hiding the name is not the same as removing it.
        let disabled = dispatch(
            Path::new("."),
            &store,
            &default_config,
            "find_recent_failures",
            json!({}),
        )
        .await
        .unwrap();
        assert_eq!(disabled["configured"], false);

        let mut configured = OkConfig::default();
        configured.memory.enabled = true;
        configured.runtime.enabled = true;
        configured.runtime.provider = "sentry".into();
        configured.runtime.organization = Some("acme".into());
        configured.runtime.project = Some("api".into());
        configured.runtime.auth_token_env = "OK_TEST_RUNTIME_TOKEN".into();
        std::env::set_var("OK_TEST_RUNTIME_TOKEN", "token");
        let (advertised, _) = tools(&configured);
        let names = advertised
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(advertised.len(), 21);
        for name in gated.iter().chain(runtime.iter()) {
            assert!(names.contains(name), "{name} must appear once configured");
        }

        // A validated provider changes the answer too, not only the listing.
        let answered = dispatch(
            Path::new("."),
            &store,
            &configured,
            "find_recent_failures",
            json!({}),
        )
        .await
        .unwrap();
        assert_eq!(answered["configured"], true);
        assert!(answered["reason"]
            .as_str()
            .unwrap()
            .contains("not evidence that no runtime errors exist"));

        // `enabled` alone is not a configured provider: the provider validates
        // its own configuration, so a switch that only advertises cannot exist.
        let mut half_configured = OkConfig::default();
        half_configured.runtime.enabled = true;
        let (advertised, _) = tools(&half_configured);
        assert_eq!(advertised.len(), 16);
        std::env::remove_var("OK_TEST_RUNTIME_TOKEN");
    }

    #[tokio::test]
    async fn query_evidence_graph_without_a_query_returns_the_evidence_schema() {
        let store = SqliteStore::open(":memory:").unwrap();
        let config = OkConfig::default();

        let params = json!({});
        let result = dispatch(
            Path::new("."),
            &store,
            &config,
            "query_evidence_graph",
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
