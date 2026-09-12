#[derive(Parser)]
#[command(name = "ok", version, about = "Open Kioku code-intelligence platform")]
struct Cli {
    /// Print machine-readable JSON instead of text, for every command that supports it.
    #[arg(long, global = true)]
    json: bool,
    /// Repository root to operate on. A command's own repository argument overrides it when
    /// that argument is not the default `.`.
    #[arg(long, global = true, default_value = ".")]
    repo: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create ok.toml and the .ok data directory for a repository.
    Init {
        /// Repository to initialize.
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    /// Build or rebuild the local index: symbols, references, graph, tests, and lexical search.
    #[command(after_help = "Examples:
  ok index .
  ok index . --with-scip auto
  ok index . --mode fast
  ok index . --mode cross-project --workspace ../workspace")]
    Index {
        /// Repository to index.
        #[arg(default_value = ".")]
        repo: PathBuf,
        /// SCIP handling: off, consume an existing index, auto (generate when a generator is
        /// installed), or required (fail without one). Defaults to the ok.toml setting.
        #[arg(long = "with-scip", value_parser = ["off", "consume", "auto", "required"])]
        with_scip: Option<String>,
        /// Index mode: full, balanced, fast, or cross-project.
        #[arg(long, default_value = "full")]
        mode: String,
        /// Workspace directory to link already-indexed projects into (cross-project mode).
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<PathBuf>,
        /// Bootstrap from an exported index snapshot before indexing; `auto` uses the
        /// snapshot under .ok when one is present and falls back to a full index otherwise.
        #[arg(long = "from-snapshot", value_parser = ["auto"])]
        from_snapshot: Option<String>,
    },
    /// Export, import, or check a portable index snapshot for team and CI reuse.
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
    },
    /// Keep the local index current while repository files change.
    Watch {
        /// Repository to watch.
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    /// Report index readiness and quality for a repository.
    #[command(after_help = "Examples:
  ok status
  ok status --json
  ok status --markdown --write ok-status.md")]
    Status {
        /// Repository to report on.
        #[arg(default_value = ".")]
        repo: PathBuf,
        /// Render a portable Markdown status snapshot.
        #[arg(long, default_value_t = false)]
        markdown: bool,
        /// Write the Markdown status snapshot to a file.
        #[arg(long, value_name = "PATH")]
        write: Option<PathBuf>,
        /// Exit non-zero when readiness checks fail.
        #[arg(long, default_value_t = false)]
        exit_code: bool,
    },
    Doctor {
        #[arg(default_value = ".")]
        repo: PathBuf,
        #[arg(long, value_enum, default_value_t = DoctorFormat::Text)]
    /// Check index readiness, coverage, configuration, and integrations, with next steps.
        format: DoctorFormat,
        /// Repository to check.
    },
    Setup {
        /// Output format.
        #[command(subcommand)]
        command: SetupCommand,
    },
    /// Audit the install, or connect a coding-agent client to this repository's MCP server.
    Demo {
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
    /// Create a small sample repository to try Open Kioku on.
        force: bool,
        /// Where to create it; defaults to ./open-kioku-demo.
    },
    /// Search the indexed repository for code or graph matches.
        /// Replace an existing directory at that path.
    #[command(after_help = "Examples:
  ok search \"token refresh\"
  ok search Worker --kind graph --limit 5
  ok search \"fn issue_token\" --regex
  ok search issue_token --json")]
    Search {
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, value_enum, default_value_t = SearchKind::Code)]
        /// Search terms, identifiers, routes, or config keys; a regular expression with --regex.
        kind: SearchKind,
        /// Maximum results to return.
        #[arg(long, default_value_t = false)]
        explain_ranking: bool,
        /// Evidence to search: code (lexical BM25 over chunks and paths) or graph (indexed
        /// graph-node documents).
        /// Treat the query as a regular expression and return exact line matches
        /// instead of ranked candidates.
        /// Print the per-signal score breakdown behind each result.
        ///
        /// Exact matching and the ranked modes answer different questions, so
        /// asking for both is a mistake worth reporting rather than resolving
        /// by precedence.
        #[arg(long, default_value_t = false, conflicts_with_all = ["kind", "semantic", "hybrid"])]
        regex: bool,
        #[arg(long, default_value_t = false)]
        semantic: bool,
        #[arg(long, default_value_t = false)]
        hybrid: bool,
        /// Search the local semantic vector index instead of lexical BM25 (needs `ok semantic index`).
    },
    Semantic {
        /// Merge lexical and semantic candidates, deduplicated by path and re-sorted by combined score.
        #[command(subcommand)]
        command: SemanticCommand,
    },
    /// Build, inspect, or remove the optional local semantic vector index.
    Symbol {
        #[command(subcommand)]
        command: SymbolCommand,
    },
    /// Look up indexed symbols: find by name, definition, definition context, or references.
    Explain {
        #[command(subcommand)]
        command: ExplainCommand,
    },
    /// Explain an indexed file or symbol from stored evidence.
    Impact(ImpactArgs),
    Path {
        from: String,
        to: String,
    /// Analyze the blast radius of changing a file, a symbol, or a Git range.
    },
    /// Trace the shortest dependency path between two files or symbols in the persisted graph.
    Tests {
        /// Starting file path or symbol name.
        #[arg(long)]
        /// Target file path or symbol name.
        changed: PathBuf,
    },
    /// Rank the test files most relevant to a changed file, with the evidence behind each.
    Context {
        /// Repository-relative path of the changed file.
        task: String,
        #[arg(long, value_enum, default_value_t = ContextPackFormat::Json)]
        format: ContextPackFormat,
    /// Assemble a ranked, bounded context pack of files, symbols, tests, and history for a task.
    #[command(after_help = "Examples:
  ok context \"add rate limiting to the token endpoint\" --format markdown
  ok --json context \"add rate limiting to the token endpoint\"")]
        #[arg(long, default_value_t = false)]
        /// What you are about to do, in natural language.
        compressed: bool,
        /// Output format. Defaults to json; markdown carries the same evidence at a fraction
        /// of the size and is what an agent should read.
    },
    RetrieveContext {
        /// Store snippets under .ok and return handles that `ok retrieve-context` expands.
        handle: String,
    },
    Plan {
    /// Expand a handle from `ok context --compressed` into its original snippet.
        task: String,
        /// Handle id from a compressed context pack.
        #[arg(long, value_enum, default_value_t = PlanFormat::Text)]
        format: PlanFormat,
    /// Produce an evidence-backed pre-edit plan: files to edit, impact, edit boundaries, and tests.
    #[command(after_help = "Examples:
  ok plan \"change token expiration\"
  ok plan \"change token expiration\" --format json > plan.json
  ok plan \"change token expiration\" --since HEAD~1 --verify-evidence fail")]
        #[arg(long, default_value_t = 12)]
        /// The change to plan, in natural language.
        limit: usize,
        /// Output format. Save json to pass to `ok verify --plan`.
        #[arg(long, value_name = "REV")]
        since: Option<String>,
        /// Maximum context results the plan is built from.
        #[arg(long, value_enum, default_value_t = EvidenceVerifyMode::Off)]
        verify_evidence: EvidenceVerifyMode,
        /// Git revision or range whose changed files and line ranges (git diff --unified=0)
        /// are added to the planning context.
    },
    /// Return a concise evidence-backed decision before starting a multi-file edit.
        /// Check that every evidence reference in the plan resolves against the index:
        /// off, warn (report unresolved references), or fail (exit non-zero on any).
    Preflight {
        task: String,
        #[arg(long, value_enum, default_value_t = PreflightFormat::Text)]
        format: PreflightFormat,
        #[arg(long, default_value_t = 12)]
        /// The change to assess, in natural language.
        limit: usize,
        /// Output format.
        #[arg(long, value_name = "REV")]
        since: Option<String>,
        /// Maximum context results the decision is built from.
    },
    /// Verify changed files against a saved JSON plan boundary.
        /// Git revision or range whose changed files are added to the context.
    VerifyBoundary {
        #[arg(long, value_name = "PLAN_JSON")]
        plan: PathBuf,
        #[arg(long = "changed", required = true, value_name = "PATH")]
        changed: Vec<PathBuf>,
        /// Saved plan from `ok plan --format json`.
        #[arg(long = "evidence-ref", value_name = "REF")]
        evidence_refs: Vec<String>,
        /// Changed file path; repeat for each file.
    },
    /// Verify an actual diff against a saved JSON plan.
        /// Evidence reference id supporting the change; repeat for each.
    Verify {
        #[arg(long, value_name = "PLAN_JSON")]
        plan: PathBuf,
        #[arg(long, value_enum, default_value_t = VerifyReportFormat::Text)]
    #[command(after_help = "Examples:
  ok verify --plan plan.json --git
  ok verify --plan plan.json --since-plan HEAD~1 --check-api-surface
  ok verify --plan plan.json --diff change.patch --run-commands --write-attestation")]
        format: VerifyReportFormat,
        /// Saved plan from `ok plan --format json`.
        #[arg(long, value_name = "UNIFIED_DIFF")]
        diff: Option<PathBuf>,
        /// Output format.
        #[arg(long, default_value_t = false)]
        git: bool,
        /// Unified diff file to verify. Omit to derive the change with --git, --since-plan,
        /// or --changed.
        #[arg(long = "since-plan", value_name = "REV")]
        since_plan: Option<String>,
        /// Derive the diff from the working tree with git.
        #[arg(long = "changed", value_name = "PATH")]
        changed: Vec<PathBuf>,
        /// Git revision or range to diff (git diff --unified=0) for the changed files.
        #[arg(long = "evidence-ref", value_name = "REF")]
        evidence_refs: Vec<String>,
        /// Changed file path, when no diff is given; repeat for each file.
        #[arg(long, default_value_t = false)]
        traceability_strict: bool,
        /// Evidence reference id supporting the change; repeat for each.
        #[arg(long = "check-api-surface", default_value_t = false)]
        check_api_surface: bool,
        /// Reject evidence references that are not in the saved plan.
        #[arg(long = "check-deps", default_value_t = false)]
        check_deps: bool,
        /// Flag public API additions, removals, and signature changes as warnings.
        #[arg(long, default_value_t = false)]
        run_commands: bool,
        /// Flag dependency-graph changes and forbidden dependency additions under the
        /// architecture policy (a configured policy enables this anyway).
        #[arg(long = "write-attestation", default_value_t = false)]
        write_attestation: bool,
        /// Execute the plan's validation commands locally and record their exit codes.
    },
    Contract {
        /// With --run-commands, persist timestamped pass/fail attestation records under
        /// .ok/contracts/validation.
        #[command(subcommand)]
        command: ContractCommand,
    },
    /// Create, verify, explain, show, or export a versioned change contract.
    Bench(BenchArgs),
    WorkflowBench(WorkflowBenchArgs),
    RetrievalBench(RetrievalBenchArgs),
    RelationshipBench(RelationshipBenchArgs),
    /// Index a repository and report indexing and search timings, with optional quality cases.
    ContractBench(ContractBenchArgs),
    /// Run the frozen workflow benchmark corpus (context, tests, impact, verification).
    Eval(EvalArgs),
    /// Run the frozen retrieval benchmark corpus against the checked-in quality baseline.
    Prove(ProveArgs),
    /// Score relationship-resolution observations against a versioned conformance corpus.
    Adr {
    /// Run the contract benchmark corpus: generated boundaries and verification verdicts.
        #[command(subcommand)]
    /// Score search, context, and test selection against golden cases for a repository.
        command: AdrCommand,
    /// Index a repository and produce a shareable proof report of plan and verification quality.
    },
    /// List, add, link, or explain architecture decision records stored in the repository.
    Ui(UiArgs),

    Architecture {
        #[command(subcommand)]
    /// Render the local trust-workflow UI (plan, evidence, verification) as HTML, Markdown, or JSON.
        command: ArchitectureCommand,
    },
    /// Detect components and boundaries, check the architecture policy, and report drift.
    /// Experimental typed Git provenance lookup.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    Patch {
        #[command(subcommand)]
        command: PatchCommand,
    },
    /// Plan a patch from stored evidence; nothing is written.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Record and search durable repository-scoped facts in the local memory store.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    /// Print a client's MCP configuration, or serve the read-only stdio JSON-RPC MCP server.
    Scip {
        #[command(subcommand)]
        command: ScipCommand,
    },
    /// Check or set up the SCIP generators that provide exact references.
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },
    /// Print the evidence-graph schema or run a read-only graph query.
}

#[derive(Subcommand)]
enum GraphCommand {
    Schema {
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Print the versioned evidence-graph schema: node types, edge types, properties, capabilities.
    Query {
        /// Output format: json or markdown.
        #[arg(long)]
        dsl: String,
        #[arg(long, default_value = "50")]
    /// Run a read-only query in the constrained Cypher-like DSL.
        limit: usize,
        /// Query text, for example `MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s LIMIT 1`.
        #[arg(long, default_value = "3")]
        max_depth: usize,
        /// Maximum rows to return.
        #[arg(long, default_value = "5000")]
        timeout_ms: u64,
        /// Maximum traversal depth for path patterns.
        #[arg(long, default_value = "json")]
        format: String,
        /// Query timeout in milliseconds.
    },
}
        /// Output format: json or text.

#[derive(Subcommand)]
enum SnapshotCommand {
    Export {
        #[arg(long, value_enum, default_value_t = SnapshotQuality::Best)]
        quality: SnapshotQuality,
    },
    /// Export the current index as a compressed snapshot plus metadata under .ok.
    Import,
        /// Compression trade-off: best (smallest artifact) or fast.
    Doctor,
}

    /// Import the snapshot under .ok, replacing the current index and rebuilding search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
    /// Validate the snapshot artifact and metadata under .ok without importing them.
#[serde(rename_all = "snake_case")]
enum SnapshotQuality {
    Best,
    Fast,
}

impl SnapshotQuality {
    fn compression_level(self) -> i32 {
        match self {
            Self::Best => 9,
            Self::Fast => 1,
        }
    }
}

impl fmt::Display for SnapshotQuality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Best => "best",
            Self::Fast => "fast",
        })
    }
}

const SNAPSHOT_SCHEMA_VERSION: &str = "1.0.0";
const SNAPSHOT_ARTIFACT_KIND: &str = "index-snapshot";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SnapshotMetadata {
    schema_version: String,
    sqlite_user_version: i64,
    open_kioku_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    analysis_semantics: Option<open_kioku_core::AnalysisSemanticsState>,
    index_mode: String,
    repo_commit: String,
    indexed_at: String,
    file_count: usize,
    symbol_count: usize,
    chunk_count: usize,
    graph_node_count: usize,
    graph_edge_count: usize,
    original_size_bytes: u64,
    compressed_size_bytes: u64,
    compression_level: i32,
    source_root_hash: String,
    artifact_kind: String,
}

#[derive(Debug, Serialize)]
struct SnapshotExportReport {
    ok: bool,
    quality: SnapshotQuality,
    artifact_path: PathBuf,
    metadata_path: PathBuf,
    metadata: SnapshotMetadata,
}

#[derive(Debug, Serialize)]
struct SnapshotImportReport {
    ok: bool,
    imported: bool,
    rebuilt_search: bool,
    artifact_path: PathBuf,
    metadata_path: PathBuf,
    index_path: PathBuf,
    metadata: SnapshotMetadata,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SnapshotDoctorReport {
    ok: bool,
    artifact_path: PathBuf,
    metadata_path: PathBuf,
    metadata: Option<SnapshotMetadata>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

const WORKSPACE_LINK_CAP: usize = 1000;

#[derive(Debug, Clone, Deserialize)]
struct WorkspaceToml {
    workspace: WorkspaceConfig,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkspaceConfig {
    projects: Vec<WorkspaceProjectConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkspaceProjectConfig {
    name: String,
    repo: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
struct WorkspaceProjectReport {
    name: String,
    repo: PathBuf,
    index_path: PathBuf,
    graph_nodes: usize,
    graph_edges: usize,
}

#[derive(Debug, Clone, Serialize)]
struct WorkspaceLinkReport {
    ok: bool,
    workspace: PathBuf,
    config_path: PathBuf,
    graph_path: PathBuf,
    project_count: usize,
    projects: Vec<WorkspaceProjectReport>,
    links: Vec<WorkspaceLinkSummary>,
    link_count: usize,
    cap: usize,
    cap_hit: bool,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct WorkspaceLinkSummary {
    source_project: String,
    target_project: String,
    source_node: String,
    target_node: String,
    target: String,
    edge_type: GraphEdgeType,
    matching_strategy: String,
    confidence: Confidence,
    ambiguity: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct FleetArchitectureReport {
    ok: bool,
    workspace: PathBuf,
    graph_path: PathBuf,
    project_count: usize,
    link_count: usize,
    links: Vec<WorkspaceLinkSummary>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct WorkspaceProjectGraph {
    name: String,
    repo: PathBuf,
    index_path: PathBuf,
    graph_node_count: usize,
    graph_edge_count: usize,
    exposes: Vec<ProjectBoundaryEdge>,
    calls: Vec<ProjectBoundaryEdge>,
    publishes: Vec<ProjectBoundaryEdge>,
    consumes: Vec<ProjectBoundaryEdge>,
}

#[derive(Debug, Clone)]
struct ProjectBoundaryEdge {
    edge: GraphEdge,
    source: GraphNode,
    target: GraphNode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ConfidenceArg {
    Low,
    Medium,
    High,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EvidenceVerifyMode {
    Off,
    Warn,
    Fail,
}

impl From<ConfidenceArg> for Confidence {
    fn from(value: ConfidenceArg) -> Self {
        match value {
            ConfidenceArg::Low => Self::Low,
            ConfidenceArg::Medium => Self::Medium,
            ConfidenceArg::High => Self::High,
            ConfidenceArg::Exact => Self::Exact,
        }
    }
}

#[derive(Subcommand)]
enum MemoryCommand {
    Remember {
        text: String,
        #[arg(long, default_value = "cli")]
        source: String,
    /// Append a durable fact to the repository memory store.
        #[arg(long, value_enum, default_value_t = ConfidenceArg::Medium)]
        /// The fact to record.
        confidence: ConfidenceArg,
        /// Where the fact came from.
    },
    Search {
        /// Confidence to record with the fact.
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    /// Search stored facts by keyword, entity, or text.
    },
        /// Search text.
    Recent {
        /// Maximum facts to return.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// List the most recently stored facts.
}
        /// Maximum facts to return.

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SearchKind {
    Code,
    Graph,
}

#[derive(Subcommand)]
enum SemanticCommand {
    Status {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    /// Report the semantic index state, provider, model, vector counts, and rebuild requirements.
    Index {
        /// Repository whose semantic index to report on.
        #[arg(default_value = ".")]
        repo: PathBuf,
        /// Explicitly permit downloading a configured local neural model into .ok/models.
    /// Build or update the semantic vector index from the code index.
        #[arg(long, default_value_t = false)]
        /// Repository to embed.
        allow_model_download: bool,
    },
    Rebuild {
        #[arg(default_value = ".")]
        repo: PathBuf,
        /// Explicitly permit downloading a configured local neural model into .ok/models.
    /// Rebuild the semantic vector index from scratch.
        #[arg(long, default_value_t = false)]
        /// Repository to embed.
        allow_model_download: bool,
    },
    Clean {
        #[arg(default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = false)]
    /// Remove the semantic vector index.
        include_cache: bool,
        /// Repository whose semantic index to remove.
    },
}
        /// Also remove the embedding cache and downloaded model files.

#[derive(Subcommand)]
enum SetupCommand {
    /// Audit install readiness across index, security, MCP, and client surfaces.
    Audit {
        #[arg(default_value = ".")]
        repo: PathBuf,
        /// Render a portable Markdown setup report.
        #[arg(long, default_value_t = false)]
        /// Repository to audit.
        markdown: bool,
        /// Write the Markdown setup report to a file.
        #[arg(long, value_name = "PATH")]
        write: Option<PathBuf>,
        /// Exit non-zero when required setup checks fail.
        #[arg(long, default_value_t = false)]
        exit_code: bool,
    },
    /// Safely connect a repository-scoped coding-agent configuration to Open Kioku.
    Agent {
        #[command(flatten)]
        args: SetupAgentArgs,
    },
}

#[derive(Args)]
struct SetupAgentArgs {
    /// Coding-agent client to configure. Initial apply support is Claude and Cursor.
    client: McpClient,
    /// Repository to index and configure. Defaults to the current repository.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Apply the proposed configuration and index the repository. Without this flag, no files change.
    #[arg(long, default_value_t = false, conflicts_with_all = ["check", "uninstall"])]
    apply: bool,
    /// Check whether Open Kioku is already installed and the local MCP server is reachable.
    #[arg(long, default_value_t = false, conflicts_with_all = ["apply", "uninstall"])]
    check: bool,
    /// Remove only configuration and rule files that were created by Open Kioku.
    #[arg(long, default_value_t = false, conflicts_with_all = ["apply", "check"])]
    uninstall: bool,
}

#[derive(Args)]
struct BenchArgs {
    /// Repository to index and benchmark.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Search quality expectation as QUERY=EXPECTED_PATH_SUBSTRING.
    #[arg(long = "quality-case", value_name = "QUERY=EXPECTED_PATH")]
    quality_cases: Vec<String>,

    /// Number of search results considered for each quality case.
    #[arg(long, default_value_t = 10)]
    quality_limit: usize,

    /// Fail when quality precision@1 is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    quality_min_precision_at_1: f64,
}

#[derive(Args)]
struct WorkflowBenchArgs {
    /// Repository to index and benchmark.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// JSON file containing workflow benchmark cases.
    #[arg(long, default_value = "benchmarks/workflow-cases.json")]
    cases_file: PathBuf,

    /// Number of context/test/impact results considered for each case.
    #[arg(long, default_value_t = 10)]
    limit: usize,

    /// Use the existing .ok index instead of re-indexing before benchmarking.
    #[arg(long, default_value_t = false)]
    no_index: bool,

    /// Fail when context recall is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_context_recall: f64,

    /// Fail when verification verdict accuracy is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_verification_accuracy: f64,

    /// Fail unless at least this many cases are loaded.
    #[arg(long, default_value_t = 20)]
    min_cases: usize,
}

#[derive(Args)]
struct RelationshipBenchArgs {
    /// Versioned JSON relationship conformance corpus.
    #[arg(long, value_name = "CORPUS_JSON")]
    corpus: PathBuf,

    /// JSON observations produced by a resolver/index run.
    #[arg(long, value_name = "OBSERVATIONS_JSON")]
    observations: PathBuf,

    /// Optional path for the deterministic JSON score report.
    #[arg(long, value_name = "REPORT_JSON")]
    write: Option<PathBuf>,

    /// Versioned JSON release-gate policy. When supplied, gate results are included in the report.
    #[arg(long, value_name = "POLICY_JSON")]
    policy: Option<PathBuf>,

    /// Exit non-zero unless every configured release gate passes.
    #[arg(long, default_value_t = false)]
    enforce_gates: bool,
}

#[derive(Args)]
struct ContractBenchArgs {
    /// Repository fixture to index and benchmark.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// JSON file containing contract benchmark cases.
    #[arg(long, default_value = "benchmarks/contract-cases.json")]
    cases_file: PathBuf,

    /// Number of context/test/impact results considered while generating contracts.
    #[arg(long, default_value_t = 10)]
    limit: usize,

    /// Use the existing .ok index in each benchmark copy instead of re-indexing.
    #[arg(long, default_value_t = false)]
    no_index: bool,

    /// Fail unless at least this many cases are loaded.
    #[arg(long, default_value_t = 7)]
    min_cases: usize,

    /// Fail when exact contract-verification verdict accuracy is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_verdict_accuracy: f64,

    /// Fail when non-pass verification precision is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_verification_precision: f64,

    /// Fail when generated contract boundary precision is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_boundary_precision: f64,

    /// Fail when generated contract boundary recall is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_boundary_recall: f64,

    /// Fail when the smallest TOON byte reduction is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_toon_reduction: f64,
}

#[derive(Args)]
struct ArchitectureBenchArgs {
    /// Repository to index and evaluate.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// JSON file containing architecture policy benchmark cases.
    #[arg(long, default_value = "benchmarks/architecture-policy-cases.json")]
    cases_file: PathBuf,

    /// Use the existing .ok index instead of re-indexing before benchmarking.
    #[arg(long, default_value_t = false)]
    no_index: bool,

    /// Number of warmed policy-check iterations used for latency reporting.
    #[arg(long, default_value_t = 5)]
    iterations: usize,

    /// Fail when overall precision is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_precision: f64,

    /// Fail when overall recall is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_recall: f64,

    /// Fail when repo-wide policy-check p95 latency exceeds this value.
    #[arg(long)]
    max_p95_ms: Option<f64>,
}

#[derive(Args)]
struct ReviewerBenchArgs {
    /// JSON file containing reviewer suggestion benchmark cases.
    #[arg(long, default_value = "benchmarks/reviewer-cases.json")]
    cases_file: PathBuf,

    /// Fail when benchmark accuracy is below this threshold.
    #[arg(long, default_value_t = 0.80)]
    min_accuracy: f64,
}

#[derive(Args)]
struct SimilarHistoryBenchArgs {
    /// JSON file containing similar historical change benchmark cases.
    #[arg(long, default_value = "benchmarks/similar-history-cases.json")]
    cases_file: PathBuf,

    /// Fail when Top-5 recall is below this threshold.
    #[arg(long, default_value_t = 0.75)]
    min_recall_at_5: f64,
}

#[derive(Args)]
struct HistoryBenchArgs {
    /// JSON file containing the unified public history API benchmark corpus.
    #[arg(long, default_value = "benchmarks/history-cases.json")]
    cases_file: PathBuf,

    /// Fail when reviewer suggestion accuracy is below this threshold.
    #[arg(long, default_value_t = 0.80)]
    min_reviewer_accuracy: f64,

    /// Fail when similar-change Top-5 recall is below this threshold.
    #[arg(long, default_value_t = 0.75)]
    min_similar_recall_at_5: f64,

    /// Fail when p95 similar-change latency exceeds this value.
    #[arg(long, default_value_t = 700.0)]
    max_similar_p95_ms: f64,

    /// Fail when p95 ownership/churn lookup latency exceeds this value.
    #[arg(long, default_value_t = 200.0)]
    max_lookup_p95_ms: f64,
}

#[derive(Args)]
struct ProveArgs {
    /// Repository to index and evaluate.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Task to evaluate. Repeat to score multiple workflows.
    #[arg(long = "task", value_name = "TASK")]
    tasks: Vec<String>,

    /// Output format for the shareable proof report.
    #[arg(long, value_enum, default_value_t = ProveFormat::Markdown)]
    format: ProveFormat,

    /// Maximum context results considered by each plan.
    #[arg(long, default_value_t = 12)]
    limit: usize,

    /// Include repository-relative paths instead of redacted path shapes.
    #[arg(long, default_value_t = false)]
    reveal_paths: bool,

    /// Summarize a previously generated frozen retrieval benchmark report.
    ///
    /// These metrics remain explicitly separate from measurements of the repository passed to
    /// `ok prove`; the report is treated as benchmark evidence, not private-repository quality.
    #[arg(long, value_name = "RETRIEVAL_REPORT_JSON")]
    retrieval_report: Option<PathBuf>,

    /// Shorthand for --format html.
    #[arg(long, default_value_t = false)]
    html: bool,
}

#[derive(Args)]
struct UiArgs {
    /// Optional task shown at the start of the trust workflow.
    #[arg(long)]
    task: Option<String>,

    /// Output format for the local trust workflow UI.
    #[arg(long, value_enum, default_value_t = UiFormat::Html)]
    format: UiFormat,

    /// Write the rendered UI/report to a file instead of stdout.
    #[arg(long, value_name = "PATH")]
    output: Option<PathBuf>,
}

#[derive(Args)]
struct EvalArgs {
    /// Repository to index and evaluate.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Golden case as TASK=EXPECTED_PATH[,EXPECTED_PATH...].
    #[arg(long = "case", value_name = "TASK=EXPECTED_PATHS")]
    cases: Vec<String>,

    /// JSON file containing [{ "task": "...", "expected_paths": [...], "expected_tests": [...] }].
    #[arg(long)]
    cases_file: Option<PathBuf>,

    /// Number of search/context/test results considered for each case.
    #[arg(long, default_value_t = 10)]
    limit: usize,

    /// Fail when search recall@k is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_recall_at_k: f64,

    /// Fail when mean reciprocal rank is below this threshold.
    #[arg(long, default_value_t = 0.0)]
    min_mrr: f64,

    /// Use the existing .ok index instead of re-indexing before evaluation.
    #[arg(long, default_value_t = false)]
    no_index: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ProveFormat {
    Markdown,
    Json,
    Html,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum VerifyReportFormat {
    Text,
    Json,
    Html,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum UiFormat {
    Html,
    Markdown,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum AdrFormat {
    Text,
    Json,
    Markdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DoctorFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
enum SymbolCommand {
    /// Search indexed symbols by name substring.
    Find {
        /// Name or name fragment to match.
        name: String,
    },
    /// Print the definition record for a symbol.
    Definition {
        /// Exact or partial symbol name.
        name: String,
    },
    /// Print the definition body and surrounding indexed lines for a symbol.
    Context {
        /// Exact or partial symbol name.
        name: String,
    },
    /// List indexed references to a symbol.
    Refs {
        /// Exact or partial symbol name.
        name: String,
    },
}

#[derive(Subcommand)]
enum ExplainCommand {
    /// Explain what an indexed file defines, imports, and is depended on by.
    File {
        /// Repository-relative file path.
        path: PathBuf,
    },
    /// Explain a symbol from its definition, references, and callers.
    Symbol {
        /// Exact or partial symbol name.
        name: String,
    },
}

#[derive(Args)]
/// Analyze the blast radius of changing a file, a symbol, or a Git range.
#[command(after_help = "Examples:
  ok impact --file src/auth.rs
  ok impact --symbol issue_token --json
  ok impact --since HEAD~1")]
struct ImpactArgs {
    #[arg(long)]
    file: Option<PathBuf>,
    #[arg(long)]
    symbol: Option<String>,
    /// Repository-relative file to analyze.
    #[arg(long, value_name = "REV")]
    since: Option<String>,
    /// Symbol name to analyze.
}

    /// Git revision or range whose changed files are analyzed.
#[derive(Subcommand)]
enum ArchitectureCommand {
    Overview,
    Clusters,
    Hotspots,
    Drift,
    /// Architecture trust report: components, dependency edges, endpoints, hotspots, and caveats.
    Detect,
    /// The same trust report, read for its dependency clusters.
    Boundaries,
    /// The same trust report, read for its high-change and high-risk files.
    /// Components, configured policy, and the evaluated policy check in one report.
    /// The same trust report, read for departures from the detected components.
    Summary,
    /// Detect components and boundaries from the indexed dependency graph.
    Violations,
    /// The same trust report, read for component boundaries and the edges crossing them.
    Bench(ArchitectureBenchArgs),
    Fleet {
        #[arg(long, value_name = "WORKSPACE")]
    /// Architecture-policy violations with the evidence behind each; needs a configured policy.
        workspace: PathBuf,
    /// Run the architecture-policy benchmark corpus.
    },
    /// Report cross-project links across a linked workspace.
    /// Experimental repository-owned architecture policy commands.
        /// Workspace directory created by `ok index --mode cross-project --workspace`.
    Policy {
        #[command(subcommand)]
        command: ArchitecturePolicyCommand,
    },
}

#[derive(Subcommand)]
enum AdrCommand {
    List {
        #[arg(long, value_enum, default_value_t = AdrFormat::Text)]
        format: AdrFormat,
    },
    /// List the stored architecture decision records.
    Add {
        /// Output format.
        title: String,
        #[arg(long, default_value = "accepted")]
        status: String,
    /// Record a new decision with the components, boundaries, files, routes, contracts, and
    /// validation rules it governs.
        #[arg(long)]
        /// Decision title.
        decision: Option<String>,
        /// Decision status.
        #[arg(long = "component")]
        components: Vec<String>,
        /// Decision text.
        #[arg(long = "boundary")]
        boundaries: Vec<String>,
        /// Governed component; repeat for each.
        #[arg(long = "file")]
        files: Vec<PathBuf>,
        /// Governed boundary; repeat for each.
        #[arg(long = "route")]
        routes: Vec<String>,
        /// Governed file; repeat for each.
        #[arg(long = "contract")]
        contracts: Vec<String>,
        /// Governed route; repeat for each.
        #[arg(long = "validation-rule")]
        validation_rules: Vec<String>,
        /// Governed contract; repeat for each.
        #[arg(long, value_enum, default_value_t = AdrFormat::Text)]
        format: AdrFormat,
        /// Validation rule the decision imposes; repeat for each.
    },
    Link {
        /// Output format.
        #[arg(value_name = "ADR_ID")]
        id: Option<String>,
        #[arg(long = "component")]
    /// Attach governance facts to an existing decision record.
        components: Vec<String>,
        /// Decision record to link; defaults to the most recent.
        #[arg(long = "boundary")]
        boundaries: Vec<String>,
        /// Governed component; repeat for each.
        #[arg(long = "file")]
        files: Vec<PathBuf>,
        /// Governed boundary; repeat for each.
        #[arg(long = "route")]
        routes: Vec<String>,
        /// Governed file; repeat for each.
        #[arg(long = "contract")]
        contracts: Vec<String>,
        /// Governed route; repeat for each.
        #[arg(long = "validation-rule")]
        validation_rules: Vec<String>,
        /// Governed contract; repeat for each.
        #[arg(long, value_enum, default_value_t = AdrFormat::Text)]
        format: AdrFormat,
        /// Validation rule the decision imposes; repeat for each.
    },
    Explain {
        /// Output format.
        #[arg(long)]
        task: String,
        #[arg(long, value_enum, default_value_t = AdrFormat::Text)]
    /// Show which decision records govern a task.
        format: AdrFormat,
        /// Task text to match against decisions and their governance facts.
    },
}
        /// Output format.

#[derive(Subcommand)]
enum ArchitecturePolicyCommand {
    Validate {
        #[arg(long, value_name = "POLICY_TOML")]
        path: Option<PathBuf>,
        #[arg(long, value_enum)]
    /// Validate the repository's architecture policy file.
        format: Option<ArchitecturePolicyFormat>,
        /// Policy file to validate instead of the repository's own.
    },
    Print,
        /// Output format; defaults to text, or json under --json.
    Check {
        #[arg(long, value_enum)]
        format: Option<ArchitecturePolicyFormat>,
    /// Print the loaded architecture policy.
    },
    /// Evaluate the policy against the indexed dependency graph.
    Explain {
        /// Output format; defaults to text, or json under --json.
        #[arg(long, conflicts_with = "symbol")]
        file: Option<PathBuf>,
        #[arg(long, conflicts_with = "file")]
    /// Explain which policy components and rules apply to a file or symbol.
        symbol: Option<String>,
        /// Repository-relative file to explain.
        #[arg(long, value_enum)]
        format: Option<ArchitecturePolicyFormat>,
        /// Symbol name to explain.
    },
}
        /// Output format; defaults to text, or json under --json.

#[derive(Subcommand)]
enum ContractCommand {
    /// Create and optionally store a change contract from a task or saved plan.
    Create {
        #[arg(value_name = "TASK")]
        task: Option<String>,
        #[arg(long, value_name = "PLAN_JSON")]
        plan: Option<PathBuf>,
        /// The change to contract, in natural language; omit when --plan or --plan-json
        /// supplies a plan.
        #[arg(long = "plan-json", value_name = "JSON")]
        plan_json: Option<String>,
        /// Saved plan (from `ok plan --format json`) to build the contract from.
        #[arg(long, default_value_t = 12)]
        limit: usize,
        /// Inline JSON plan to build the contract from.
        #[arg(long = "no-store", default_value_t = false)]
        no_store: bool,
        /// Maximum context results the plan is built from.
        #[arg(long, value_enum, default_value_t = ContractFormat::Json)]
        format: ContractFormat,
        /// Return the contract without writing it under .ok/contracts.
    },
    /// Verify changes against a stored or inline change contract.
        /// Output format.
    Verify {
        #[arg(long, value_name = "CONTRACT_ID")]
        id: Option<String>,
        #[arg(long, value_name = "CONTRACT_JSON")]
        contract: Option<PathBuf>,
        /// Stored contract id under .ok/contracts; verification records are appended to it.
        #[arg(long = "contract-json", value_name = "JSON")]
        contract_json: Option<String>,
        /// Contract JSON file to verify against.
        #[arg(long, value_name = "UNIFIED_DIFF")]
        diff: Option<PathBuf>,
        /// Inline contract JSON to verify against.
        #[arg(long, default_value_t = false)]
        git: bool,
        /// Unified diff file to verify. Omit to derive the change with --git, --since-plan,
        /// or --changed.
        #[arg(long = "since-plan", value_name = "REV")]
        since_plan: Option<String>,
        /// Derive the diff from the working tree with git.
        #[arg(long = "changed", value_name = "PATH")]
        changed: Vec<PathBuf>,
        /// Git revision or range to diff (git diff --unified=0) for the changed files.
        #[arg(long = "evidence-ref", value_name = "REF")]
        evidence_refs: Vec<String>,
        /// Changed file path, when no diff is given; repeat for each file.
        #[arg(long, default_value_t = false)]
        traceability_strict: bool,
        /// Evidence reference id supporting the change; repeat for each.
        #[arg(long = "check-api-surface", default_value_t = false)]
        check_api_surface: bool,
        /// Reject evidence references that are not in the contract.
        #[arg(long = "check-deps", default_value_t = false)]
        check_deps: bool,
        /// Flag public API additions, removals, and signature changes as warnings.
        #[arg(long, default_value_t = false)]
        run_commands: bool,
        /// Flag dependency-graph changes and forbidden dependency additions under the
        /// architecture policy (a configured policy enables this anyway).
        #[arg(long = "write-attestation", default_value_t = false)]
        write_attestation: bool,
        /// Execute the contract's validation commands locally and record their exit codes.
        #[arg(long, value_enum, default_value_t = ContractFormat::Json)]
        format: ContractFormat,
        /// With --run-commands and a stored contract id, persist timestamped pass/fail
        /// attestation records under .ok/contracts/validation.
    },
    /// Explain the constraints, evidence, and traceability in a contract.
        /// Output format.
    Explain {
        #[arg(long, value_name = "CONTRACT_ID")]
        id: Option<String>,
        #[arg(long, value_name = "CONTRACT_JSON")]
        contract: Option<PathBuf>,
        /// Stored contract id under .ok/contracts.
        #[arg(long = "contract-json", value_name = "JSON")]
        contract_json: Option<String>,
        /// Contract JSON file to explain.
        #[arg(long, value_enum, default_value_t = ContractFormat::Markdown)]
        format: ContractFormat,
        /// Inline contract JSON to explain.
    },
    /// Show a stored contract by id.
        /// Output format.
    Show {
        id: String,
        #[arg(long, value_enum, default_value_t = ContractFormat::Json)]
        format: ContractFormat,
    },
        /// Stored contract id under .ok/contracts.
    /// Export a stored contract as JSON, Markdown, or TOON.
        /// Output format.
    Export {
        id: String,
        #[arg(long, value_enum, default_value_t = ContractFormat::Json)]
        format: ContractFormat,
    },
        /// Stored contract id under .ok/contracts.
}
        /// Output format.

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ContractFormat {
    Json,
    Markdown,
    Toon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ArchitecturePolicyFormat {
    Text,
    Markdown,
    Json,
}

#[derive(Subcommand)]
enum HistoryCommand {
    /// Retrieve similar historical commits or change groups.
    Similar {
        /// Natural-language task or change description.
        #[arg(long)]
        task: Option<String>,
        /// Repository-relative path to match. Repeat for multiple paths.
        #[arg(long = "path")]
        paths: Vec<PathBuf>,
        /// Symbol name, qualified name, or symbol ID to match. Repeat for multiple symbols.
        #[arg(long = "symbol")]
        symbols: Vec<String>,
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
    /// Show materialized churn and hotspot stats for a file, module, or symbol.
        /// Maximum matches to return.
    Churn {
        #[arg(long, conflicts_with_all = ["module", "symbol"])]
        path: Option<PathBuf>,
        #[arg(long, conflicts_with_all = ["path", "symbol"])]
        module: Option<PathBuf>,
        /// Repository-relative file path.
        /// Exact symbol name, qualified name, or symbol ID.
        #[arg(long, conflicts_with_all = ["path", "module"])]
        /// Repository-relative module directory.
        symbol: Option<String>,
    },
    /// Resolve path ownership from CODEOWNERS, local git history, and repo memory.
    Ownership {
        #[arg(long)]
        path: PathBuf,
    },
    /// Suggest reviewers from stored review evidence, ownership, and author history.
        /// Repository-relative path.
    Reviewers {
        #[arg(long)]
        path: PathBuf,
    },
    /// Run the deterministic reviewer suggestion benchmark corpus.
        /// Repository-relative path.
    ReviewersBench(ReviewerBenchArgs),
    /// Run the deterministic similar historical change benchmark corpus.
    SimilarBench(SimilarHistoryBenchArgs),
    /// Run the unified public history API benchmark corpus.
    Bench(HistoryBenchArgs),
    Provenance {
        #[arg(long, required_unless_present = "symbol", conflicts_with = "symbol")]
        path: Option<PathBuf>,
        /// Exact symbol name, qualified name, or symbol ID.
    /// Show the typed Git provenance records for a path or symbol.
        #[arg(long, required_unless_present = "path", conflicts_with = "path")]
        /// Repository-relative path.
        symbol: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}
        /// Maximum records to return.

#[derive(Subcommand)]
enum PatchCommand {
    Plan {
        task: String,
    },
}
    /// Plan a patch for a task from stored evidence; nothing is written.

        /// The change to plan, in natural language.
#[derive(Subcommand)]
enum McpCommand {
    Install {
        client: McpClient,
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    /// Print the MCP server entry for a client; `ok setup agent --apply` writes it for you.
    },
        /// Client to print configuration for.
    Serve {
        /// Repository the server entry will serve.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = true)]
    /// Serve the read-only stdio JSON-RPC MCP server for a repository.
    #[command(after_help = "Examples:
  ok mcp serve --repo .
  ok mcp serve --repo . --hide-experimental")]
        read_only: bool,
        /// Repository to serve.
        #[arg(long, default_value_t = true)]
        approval_required: bool,
        /// Refuse tool calls that write; pass --read-only=false to allow them.
        #[arg(long = "allow-command")]
        allow_command: Vec<String>,
        /// Require approval before any command executes; pass --approval-required=false
        /// to waive it.
        #[arg(long, default_value_t = true)]
        deny_network: bool,
        /// Command permitted to run during verification; repeat for each.
        #[arg(long, default_value_t = false)]
        hide_experimental: bool,
        /// Deny network access, failing closed; pass --deny-network=false to allow it.
    },
}
        /// Omit experimental tools from tools/list.

#[derive(Subcommand)]
enum ScipCommand {
    Doctor {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    /// Report which SCIP generators are installed and what the index would consume.
    Setup {
        /// Repository to check.
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    /// Show how to install the SCIP generators this repository's languages need; installs nothing.
}
        /// Repository to check.

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum McpClient {
    Claude,
    Cursor,
    Codex,
    Gemini,
    Opencode,
    Zed,
    Windsurf,
    Trae,
}

impl McpClient {
    fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Cursor => "cursor",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Opencode => "opencode",
            Self::Zed => "zed",
            Self::Windsurf => "windsurf",
            Self::Trae => "trae",
        }
    }

    fn config_format(self) -> &'static str {
        match self {
            Self::Codex => "toml",
            _ => "json",
        }
    }
}

#[derive(Debug, Clone)]
struct QualityCase {
    query: String,
    expected_path: String,
}

#[derive(Serialize)]
struct BenchReport {
    repo: PathBuf,
    index: IndexBenchReport,
    search: SearchBenchReport,
    quality: Option<QualityBenchReport>,
}

#[derive(Serialize)]
struct IndexBenchReport {
    file_count: usize,
    symbol_count: usize,
    chunk_count: usize,
    elapsed_ms: f64,
    files_per_second: f64,
}

#[derive(Serialize)]
struct SearchBenchReport {
    bm25_median_ms: f64,
    regex_median_ms: f64,
}

#[derive(Serialize)]
struct QualityBenchReport {
    case_count: usize,
    precision_at_1: f64,
    hit_rate_at_k: f64,
    mean_reciprocal_rank: f64,
    limit: usize,
    cases: Vec<QualityCaseReport>,
}

#[derive(Serialize)]
struct QualityCaseReport {
    query: String,
    expected_path: String,
    rank: Option<usize>,
    top_path: Option<PathBuf>,
    matched_path: Option<PathBuf>,
    result_count: usize,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkflowBenchCase {
    id: String,
    task: String,
    #[serde(default)]
    expected_primary_context: Vec<String>,
    #[serde(default)]
    expected_impact: Vec<String>,
    #[serde(default)]
    expected_tests: Vec<String>,
    #[serde(default)]
    expected_boundary: Vec<String>,
    #[serde(default)]
    forbidden_paths: Vec<String>,
    #[serde(default)]
    changed_files: Vec<PathBuf>,
    #[serde(default)]
    unified_diff: Option<String>,
    #[serde(default)]
    expected_verdict: Option<VerificationVerdict>,
    #[serde(default)]
    expected_confidence: Option<bool>,
}

#[derive(Serialize)]
struct WorkflowBenchReport {
    repo: PathBuf,
    cases_file: PathBuf,
    limit: usize,
    case_count: usize,
    baseline: WorkflowBenchSummary,
    workflow: WorkflowBenchSummary,
    deltas: WorkflowBenchDeltas,
    cases: Vec<WorkflowBenchCaseReport>,
}

#[derive(Serialize, Clone)]
struct WorkflowBenchSummary {
    context_recall_at_k: f64,
    impact_recall_at_k: f64,
    test_recall_at_k: f64,
    boundary_precision: f64,
    boundary_recall: f64,
    confidence_calibration_error: f64,
    verification_verdict_accuracy: f64,
}

#[derive(Serialize)]
struct WorkflowBenchDeltas {
    context_recall_at_k: f64,
    impact_recall_at_k: f64,
    test_recall_at_k: f64,
    boundary_precision: f64,
    boundary_recall: f64,
    confidence_calibration_error: f64,
    verification_verdict_accuracy: f64,
}

#[derive(Serialize)]
struct WorkflowBenchCaseReport {
    id: String,
    task: String,
    context_recall: f64,
    impact_recall: f64,
    test_recall: f64,
    boundary_precision: f64,
    boundary_recall: f64,
    confidence_expected_success: Option<bool>,
    confidence_probability: f64,
    confidence_calibration_error: Option<f64>,
    expected_verdict: Option<VerificationVerdict>,
    actual_verdict: Option<VerificationVerdict>,
    verification_correct: Option<bool>,
    baseline_context_recall: f64,
    baseline_impact_recall: f64,
    baseline_test_recall: f64,
    context_hits: Vec<String>,
    impact_hits: Vec<String>,
    test_hits: Vec<String>,
    boundary_hits: Vec<String>,
    forbidden_boundary_hits: Vec<String>,
    top_context_paths: Vec<PathBuf>,
    top_impact_paths: Vec<PathBuf>,
    top_tests: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewerBenchCase {
    id: String,
    path: PathBuf,
    #[serde(default)]
    review_evidence: Vec<ReviewerBenchReviewEvidence>,
    #[serde(default)]
    ownership: Vec<ReviewerBenchOwnershipEvidence>,
    #[serde(default)]
    author_touches: Vec<ReviewerBenchAuthorTouch>,
    expected_top_reviewer: String,
    expected_availability: ReviewerAvailability,
    #[serde(default)]
    expected_actual_review_evidence: Option<bool>,
    #[serde(default)]
    expected_inferred_from_authors: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewerBenchReviewEvidence {
    reviewer: String,
    role: ReviewerRole,
    #[serde(default = "default_reviewer_bench_confidence")]
    confidence: Confidence,
    #[serde(default)]
    days_ago: i64,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewerBenchOwnershipEvidence {
    owner: String,
    #[serde(default = "default_reviewer_bench_source_types")]
    source_types: Vec<OwnershipSourceType>,
    #[serde(default = "default_reviewer_bench_owner_score")]
    score: f32,
    #[serde(default)]
    days_ago: i64,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ReviewerBenchAuthorTouch {
    author: String,
    #[serde(default = "default_reviewer_bench_touch_count")]
    count: usize,
    #[serde(default)]
    days_ago: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct SimilarHistoryBenchCase {
    id: String,
    query: SimilarChangeQuery,
    snapshot: HistorySnapshot,
    expected_top_5: Vec<String>,
}

#[derive(Serialize)]
struct SimilarHistoryBenchReport {
    cases_file: PathBuf,
    case_count: usize,
    min_recall_at_5: f64,
    recall_at_5: f64,
    failures: Vec<String>,
    cases: Vec<SimilarHistoryBenchCaseReport>,
}

#[derive(Serialize)]
struct SimilarHistoryBenchCaseReport {
    id: String,
    expected_top_5: Vec<String>,
    actual_top_5: Vec<String>,
    matched: Vec<String>,
    recall_at_5: f64,
    passed: bool,
}

#[derive(Serialize)]
struct ReviewerBenchReport {
    cases_file: PathBuf,
    case_count: usize,
    min_accuracy: f64,
    accuracy: f64,
    failures: Vec<String>,
    cases: Vec<ReviewerBenchCaseReport>,
}

#[derive(Serialize)]
struct ReviewerBenchCaseReport {
    id: String,
    path: PathBuf,
    expected_top_reviewer: String,
    actual_top_reviewer: Option<String>,
    rank: Option<usize>,
    expected_availability: ReviewerAvailability,
    availability: ReviewerAvailability,
    availability_correct: bool,
    expected_actual_review_evidence: Option<bool>,
    actual_review_evidence: Option<bool>,
    actual_review_evidence_correct: bool,
    expected_inferred_from_authors: Option<bool>,
    inferred_from_authors: Option<bool>,
    inferred_from_authors_correct: bool,
    top_score: Option<f32>,
    passed: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryBenchCorpus {
    schema_version: u32,
    cases: Vec<HistoryBenchCase>,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryBenchCase {
    id: String,
    #[serde(default)]
    codeowners: Vec<String>,
    snapshot: HistorySnapshot,
    #[serde(default)]
    similar: Vec<HistoryBenchSimilarCase>,
    #[serde(default)]
    ownership: Vec<HistoryBenchOwnershipCase>,
    #[serde(default)]
    reviewers: Vec<HistoryBenchReviewerCase>,
    #[serde(default)]
    churn: Vec<HistoryBenchChurnCase>,
    #[serde(default)]
    provenance: Vec<HistoryBenchProvenanceCase>,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryBenchSimilarCase {
    id: String,
    query: SimilarChangeQuery,
    expected_top_5: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryBenchOwnershipCase {
    id: String,
    path: PathBuf,
    expected_owner: String,
    #[serde(default)]
    expected_source_types: Vec<OwnershipSourceType>,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryBenchReviewerCase {
    id: String,
    path: PathBuf,
    expected_top_reviewer: String,
    expected_availability: ReviewerAvailability,
    #[serde(default)]
    expected_actual_review_evidence: Option<bool>,
    #[serde(default)]
    expected_inferred_from_authors: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryBenchChurnCase {
    id: String,
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    module: Option<PathBuf>,
    #[serde(default)]
    symbol_id: Option<SymbolId>,
    #[serde(default)]
    min_touch_count: usize,
    #[serde(default)]
    min_hotspot_score: f32,
}

#[derive(Debug, Clone, Deserialize)]
struct HistoryBenchProvenanceCase {
    id: String,
    path: PathBuf,
    #[serde(default)]
    limit: Option<usize>,
    expected_first_seen: String,
    expected_last_touched: String,
    #[serde(default = "default_history_bench_min_recent_touches")]
    min_recent_touches: usize,
}

#[derive(Serialize)]
struct HistoryBenchReport {
    cases_file: PathBuf,
    schema_version: u32,
    case_count: usize,
    family_counts: HistoryBenchFamilyCounts,
    min_reviewer_accuracy: f64,
    reviewer_accuracy: f64,
    min_similar_recall_at_5: f64,
    similar_recall_at_5: f64,
    max_similar_p95_ms: f64,
    similar_p95_ms: f64,
    max_lookup_p95_ms: f64,
    ownership_churn_p95_ms: f64,
    family_p95_ms: BTreeMap<String, f64>,
    failures: Vec<String>,
    cases: Vec<HistoryBenchCaseReport>,
}

#[derive(Default, Serialize)]
struct HistoryBenchFamilyCounts {
    similar: usize,
    ownership: usize,
    reviewers: usize,
    churn: usize,
    provenance: usize,
}

#[derive(Serialize)]
struct HistoryBenchCaseReport {
    id: String,
    similar: Vec<HistoryBenchSimilarCaseReport>,
    ownership: Vec<HistoryBenchOwnershipCaseReport>,
    reviewers: Vec<HistoryBenchReviewerCaseReport>,
    churn: Vec<HistoryBenchChurnCaseReport>,
    provenance: Vec<HistoryBenchProvenanceCaseReport>,
    passed: bool,
}

#[derive(Serialize)]
struct HistoryBenchSimilarCaseReport {
    id: String,
    expected_top_5: Vec<String>,
    actual_top_5: Vec<String>,
    matched: Vec<String>,
    recall_at_5: f64,
    latency_ms: f64,
    passed: bool,
}

#[derive(Serialize)]
struct HistoryBenchOwnershipCaseReport {
    id: String,
    path: PathBuf,
    expected_owner: String,
    actual_owner: Option<String>,
    rank: Option<usize>,
    expected_source_types: Vec<OwnershipSourceType>,
    actual_source_types: Vec<OwnershipSourceType>,
    latency_ms: f64,
    passed: bool,
}

#[derive(Serialize)]
struct HistoryBenchReviewerCaseReport {
    id: String,
    path: PathBuf,
    expected_top_reviewer: String,
    actual_top_reviewer: Option<String>,
    rank: Option<usize>,
    expected_availability: ReviewerAvailability,
    availability: ReviewerAvailability,
    availability_correct: bool,
    expected_actual_review_evidence: Option<bool>,
    actual_review_evidence: Option<bool>,
    actual_review_evidence_correct: bool,
    expected_inferred_from_authors: Option<bool>,
    inferred_from_authors: Option<bool>,
    inferred_from_authors_correct: bool,
    latency_ms: f64,
    passed: bool,
}

#[derive(Serialize)]
struct HistoryBenchChurnCaseReport {
    id: String,
    target: String,
    touch_count: usize,
    hotspot_score: f32,
    min_touch_count: usize,
    min_hotspot_score: f32,
    confidence: Confidence,
    latency_ms: f64,
    passed: bool,
}

#[derive(Serialize)]
struct HistoryBenchProvenanceCaseReport {
    id: String,
    path: PathBuf,
    expected_first_seen: String,
    actual_first_seen: Option<String>,
    expected_last_touched: String,
    actual_last_touched: Option<String>,
    min_recent_touches: usize,
    recent_touch_count: usize,
    confidence: Confidence,
    latency_ms: f64,
    passed: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ContractBenchCase {
    id: String,
    rule_family: ContractBenchRuleFamily,
    task: String,
    expected_verdict: VerificationVerdict,
    #[serde(default)]
    expected_contract: ContractBenchExpectedContract,
    #[serde(default)]
    contract_overlay: ContractBenchContractOverlay,
    #[serde(default)]
    edits: Vec<ContractBenchEdit>,
    #[serde(default)]
    changed_files: Vec<PathBuf>,
    #[serde(default)]
    unified_diff: Option<String>,
    #[serde(default)]
    expected_findings: Vec<String>,
    #[serde(default)]
    explanation_terms: Vec<String>,
    #[serde(default)]
    check_api_surface: bool,
    #[serde(default)]
    check_dependency_delta: bool,
    #[serde(default)]
    traceability_strict: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ContractBenchExpectedContract {
    #[serde(default)]
    primary_files: Vec<String>,
    #[serde(default)]
    allowed_boundary: Vec<String>,
    #[serde(default)]
    forbidden_paths: Vec<String>,
    #[serde(default)]
    min_required_tests: usize,
    #[serde(default)]
    min_traceability: usize,
    #[serde(default)]
    min_architecture_constraints: usize,
    #[serde(default)]
    min_evidence_refs: usize,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ContractBenchContractOverlay {
    #[serde(default)]
    primary_files: Vec<ContractFile>,
    #[serde(default)]
    secondary_files: Vec<ContractFile>,
    #[serde(default)]
    forbidden_files: Vec<ContractFile>,
    #[serde(default)]
    api_surface_constraints: Vec<ApiSurfaceConstraint>,
    #[serde(default)]
    dependency_delta_constraints: Vec<DependencyDeltaConstraint>,
}

#[derive(Debug, Clone, Deserialize)]
struct ContractBenchEdit {
    path: PathBuf,
    content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContractBenchRuleFamily {
    AllowedEdit,
    ForbiddenEdit,
    MissingTests,
    ArchitectureViolation,
    DependencyDelta,
    ApiSurfaceDelta,
    ExplanationQuality,
}

#[derive(Serialize)]
struct ContractBenchReport {
    repo: PathBuf,
    cases_file: PathBuf,
    limit: usize,
    case_count: usize,
    summary: ContractBenchSummary,
    rule_families: Vec<ContractBenchFamilyReport>,
    failures: Vec<String>,
    cases: Vec<ContractBenchCaseReport>,
}

#[derive(Serialize, Default, Clone)]
struct ContractBenchSummary {
    verdict_accuracy: f64,
    verification_precision: f64,
    boundary_precision: f64,
    boundary_recall: f64,
    min_toon_reduction: f64,
    mean_toon_reduction: f64,
    mean_generation_ms: f64,
    mean_verification_ms: f64,
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
}

#[derive(Serialize)]
struct ContractBenchFamilyReport {
    rule_family: ContractBenchRuleFamily,
    case_count: usize,
    verdict_accuracy: f64,
    boundary_precision: f64,
    boundary_recall: f64,
}

#[derive(Serialize)]
struct ContractBenchCaseReport {
    id: String,
    rule_family: ContractBenchRuleFamily,
    task: String,
    contract_id: String,
    expected_verdict: VerificationVerdict,
    actual_verdict: VerificationVerdict,
    verdict_correct: bool,
    boundary_precision: f64,
    boundary_recall: f64,
    primary_file_hits: Vec<String>,
    boundary_hits: Vec<String>,
    forbidden_boundary_hits: Vec<String>,
    missing_contract_fields: Vec<String>,
    finding_hits: Vec<String>,
    missing_findings: Vec<String>,
    explanation_hits: Vec<String>,
    missing_explanation_terms: Vec<String>,
    pretty_json_bytes: usize,
    toon_bytes: usize,
    toon_reduction: f64,
    generation_ms: f64,
    verification_ms: f64,
    passed: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ArchitecturePolicyBenchCase {
    id: String,
    rule_family: ArchitecturePolicyRuleFamily,
    expected: ArchitecturePolicyBenchOutcome,
    #[serde(default)]
    rule_id: Option<String>,
    source_path: PathBuf,
    target_path: PathBuf,
    edge_type: EnforcedEdgeType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArchitecturePolicyRuleFamily {
    DependencyRule,
    PublicApiRule,
    InternalOnlyRule,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArchitecturePolicyBenchOutcome {
    Allowed,
    Violation,
    Exempted,
    Unknown,
}

#[derive(Debug, Clone)]
struct ArchitecturePolicyActualFinding {
    rule_family: ArchitecturePolicyRuleFamily,
    outcome: ArchitecturePolicyBenchOutcome,
    rule_id: Option<String>,
    source_path: PathBuf,
    target_path: PathBuf,
    edge_type: EnforcedEdgeType,
}

#[derive(Serialize)]
struct ArchitecturePolicyBenchReport {
    repo: PathBuf,
    cases_file: PathBuf,
    case_count: usize,
    iterations: usize,
    p95_policy_check_ms: f64,
    summary: ArchitecturePolicyBenchSummary,
    rule_families: Vec<ArchitecturePolicyBenchFamilyReport>,
    cases: Vec<ArchitecturePolicyBenchCaseReport>,
}

#[derive(Default, Clone, Serialize)]
struct ArchitecturePolicyBenchSummary {
    precision: f64,
    recall: f64,
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
    expected_positive_count: usize,
    actual_positive_count: usize,
}

#[derive(Default)]
struct ArchitecturePolicyBenchCounts {
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
    expected_positive_count: usize,
    actual_positive_count: usize,
}

#[derive(Serialize)]
struct ArchitecturePolicyBenchFamilyReport {
    rule_family: ArchitecturePolicyRuleFamily,
    precision: f64,
    recall: f64,
    true_positives: usize,
    false_positives: usize,
    false_negatives: usize,
    expected_positive_count: usize,
    actual_positive_count: usize,
}

#[derive(Serialize)]
struct ArchitecturePolicyBenchCaseReport {
    id: String,
    rule_family: ArchitecturePolicyRuleFamily,
    expected: ArchitecturePolicyBenchOutcome,
    actual: Vec<ArchitecturePolicyBenchOutcome>,
    rule_id: Option<String>,
    source_path: PathBuf,
    target_path: PathBuf,
    edge_type: EnforcedEdgeType,
    passed: bool,
    notes: Vec<String>,
}

#[derive(Serialize)]
struct ProofReport {
    repo: String,
    generated_by: &'static str,
    privacy: ProofPrivacy,
    summary: ProofSummary,
    retrieval_quality: ProofRetrievalQuality,
    languages: BTreeMap<String, usize>,
    tasks: Vec<ProofTaskReport>,
    reproduce: Vec<String>,
    notes: Vec<&'static str>,
}

#[derive(Serialize)]
struct ProofPrivacy {
    source_snippets_included: bool,
    local_root_included: bool,
    path_mode: &'static str,
}

#[derive(Serialize)]
struct ProofSummary {
    indexed_files: usize,
    indexed_symbols: usize,
    indexed_chunks: usize,
    tasks_scored: usize,
    average_score: f64,
    min_score: u32,
    max_score: u32,
    pass_rate_70: f64,
}

#[derive(Debug, Clone, Serialize)]
struct ProofRetrievalQuality {
    available: bool,
    scope: &'static str,
    applies_to_repository: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    corpus_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    corpus_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cases_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    report_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    strategy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    strategy_algorithm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    split: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recall_at_10: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mean_reciprocal_rank: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_f1_at_10: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    no_gold_false_positive_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    token_budget_gold_yield_2000: Option<f64>,
    caveats: Vec<String>,
}

#[derive(Serialize)]
struct ProofTaskReport {
    task: String,
    score: u32,
    checks: BTreeMap<&'static str, bool>,
    primary_context_count: usize,
    source_context_count: usize,
    impact_count: usize,
    validation_count: usize,
    tool_call_count: usize,
    risk_level: String,
    sample_paths: Vec<String>,
    top_search_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct EvalCase {
    task: String,
    #[serde(default)]
    expected_paths: Vec<String>,
    #[serde(default)]
    expected_tests: Vec<String>,
}

#[derive(Serialize)]
struct EvalReport {
    repo: PathBuf,
    limit: usize,
    case_count: usize,
    summary: EvalSummary,
    baseline: RankingEvalSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic: Option<RankingEvalSummary>,
    fusion: RankingEvalSummary,
    ablations: Vec<RankingAblationReport>,
    cases: Vec<EvalCaseReport>,
}

#[derive(Serialize)]
struct EvalSummary {
    search_recall_at_k: f64,
    search_mrr: f64,
    search_ndcg_at_k: f64,
    context_recall_at_k: f64,
    test_recall_at_k: f64,
    abstention_required: usize,
}

#[derive(Serialize, Clone)]
struct RankingEvalSummary {
    mode: String,
    search_recall_at_k: f64,
    search_mrr: f64,
    search_ndcg_at_k: f64,
}

#[derive(Serialize)]
struct RankingAblationReport {
    signal: String,
    search_recall_at_k: f64,
    search_mrr: f64,
    search_ndcg_at_k: f64,
    recall_delta_vs_fusion: f64,
    mrr_delta_vs_fusion: f64,
    ndcg_delta_vs_fusion: f64,
}

#[derive(Serialize)]
struct EvalCaseReport {
    task: String,
    expected_paths: Vec<String>,
    expected_tests: Vec<String>,
    search_ranks: Vec<Option<usize>>,
    context_hits: Vec<String>,
    test_hits: Vec<String>,
    top_search_paths: Vec<PathBuf>,
    top_context_paths: Vec<PathBuf>,
    top_search_signals: Vec<String>,
    confidence: &'static str,
    notes: Vec<String>,
}

#[derive(Serialize)]
struct DoctorReport {
    ok: bool,
    repo: PathBuf,
    checks: Vec<DoctorCheck>,
    /// Per-language discovered/indexed/skipped counts from the manifest; `None` when
    /// there is no index or it predates coverage recording.
    coverage: Option<IndexCoverage>,
    next_steps: Vec<String>,
}

#[derive(Serialize)]
struct SetupAuditReport {
    ok: bool,
    repo: PathBuf,
    generated_by: &'static str,
    checks: Vec<SetupAuditCheck>,
    providers: Vec<QualityProviderReport>,
    advanced_providers: Vec<QualityProviderReport>,
    clients: Vec<ClientInstallReport>,
    plugin_surfaces: Vec<PluginSurfaceReport>,
    next_steps: Vec<String>,
}

#[derive(Serialize)]
struct SetupAuditCheck {
    name: String,
    status: CheckStatus,
    message: String,
}

#[derive(Serialize)]
struct ClientInstallReport {
    client: &'static str,
    config_format: &'static str,
    install_command: String,
    verify: String,
    note: &'static str,
}

#[derive(Serialize)]
struct PluginSurfaceReport {
    name: &'static str,
    path: PathBuf,
    present: bool,
    note: &'static str,
}

#[derive(Serialize)]
struct QualityProviderReport {
    name: &'static str,
    status: CheckStatus,
    evidence: String,
    next_step: Option<String>,
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
struct ScipSetupReport {
    repo: PathBuf,
    mode: String,
    enabled: bool,
    allow_install: bool,
    timeout_seconds: u64,
    indexers: Vec<ScipIndexerReport>,
    configured_paths: Vec<PathBuf>,
}

#[derive(Serialize)]
struct ScipIndexerReport {
    language: &'static str,
    applicable: bool,
    installed: bool,
    command: String,
    output_path: PathBuf,
    note: String,
}

#[derive(Serialize)]
struct ArchitecturePolicyOutput {
    valid: bool,
    configured: bool,
    source: Option<PolicySource>,
    paths: Vec<PathBuf>,
    policy: Option<ArchitecturePolicy>,
    message: String,
}

#[derive(Serialize)]
struct ArchitecturePolicyExplainOutput {
    configured: bool,
    query_kind: String,
    query: String,
    file_path: Option<PathBuf>,
    symbol: Option<Symbol>,
    components: Vec<PolicyComponentMatch>,
    violations: Vec<PolicyViolation>,
    exemptions: Vec<PolicyExemptionEvidence>,
    uncertainty: Vec<String>,
    message: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

impl CheckStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }

    fn marker(self) -> &'static str {
        match self {
            Self::Pass => "[ok]",
            Self::Warn => "[warn]",
            Self::Fail => "[fail]",
        }
    }
}
