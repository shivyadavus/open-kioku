#[derive(Parser)]
#[command(name = "ok", version, about = "Open Kioku code-intelligence platform")]
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
        #[arg(long = "with-scip", value_parser = ["off", "consume", "auto", "required"])]
        with_scip: Option<String>,
        #[arg(long, default_value = "full")]
        mode: String,
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<PathBuf>,
        #[arg(long = "from-snapshot", value_parser = ["auto"])]
        from_snapshot: Option<String>,
    },
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
    },
    /// Keep the local index current while repository files change.
    Watch {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    Status {
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
        format: DoctorFormat,
    },
    Setup {
        #[command(subcommand)]
        command: SetupCommand,
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
        #[arg(long, value_enum, default_value_t = SearchKind::Code)]
        kind: SearchKind,
        #[arg(long, default_value_t = false)]
        explain_ranking: bool,
        #[arg(long, default_value_t = false)]
        semantic: bool,
        #[arg(long, default_value_t = false)]
        hybrid: bool,
    },
    Semantic {
        #[command(subcommand)]
        command: SemanticCommand,
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
        #[arg(long, default_value_t = false)]
        compressed: bool,
    },
    RetrieveContext {
        handle: String,
    },
    Plan {
        task: String,
        #[arg(long, value_enum, default_value_t = PlanFormat::Text)]
        format: PlanFormat,
        #[arg(long, default_value_t = 12)]
        limit: usize,
        #[arg(long, value_name = "REV")]
        since: Option<String>,
        #[arg(long, value_enum, default_value_t = EvidenceVerifyMode::Off)]
        verify_evidence: EvidenceVerifyMode,
    },
    /// Verify changed files against a saved JSON plan boundary.
    VerifyBoundary {
        #[arg(long, value_name = "PLAN_JSON")]
        plan: PathBuf,
        #[arg(long = "changed", required = true, value_name = "PATH")]
        changed: Vec<PathBuf>,
        #[arg(long = "evidence-ref", value_name = "REF")]
        evidence_refs: Vec<String>,
    },
    /// Verify an actual diff against a saved JSON plan.
    Verify {
        #[arg(long, value_name = "PLAN_JSON")]
        plan: PathBuf,
        #[arg(long, value_name = "UNIFIED_DIFF")]
        diff: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        git: bool,
        #[arg(long = "since-plan", value_name = "REV")]
        since_plan: Option<String>,
        #[arg(long = "changed", value_name = "PATH")]
        changed: Vec<PathBuf>,
        #[arg(long = "evidence-ref", value_name = "REF")]
        evidence_refs: Vec<String>,
        #[arg(long, default_value_t = false)]
        traceability_strict: bool,
        #[arg(long = "check-api-surface", default_value_t = false)]
        check_api_surface: bool,
        #[arg(long = "check-deps", default_value_t = false)]
        check_deps: bool,
        #[arg(long, default_value_t = false)]
        run_commands: bool,
        #[arg(long = "write-attestation", default_value_t = false)]
        write_attestation: bool,
    },
    Contract {
        #[command(subcommand)]
        command: ContractCommand,
    },
    Bench(BenchArgs),
    WorkflowBench(WorkflowBenchArgs),
    ContractBench(ContractBenchArgs),
    Eval(EvalArgs),
    Prove(ProveArgs),

    Architecture {
        #[command(subcommand)]
        command: ArchitectureCommand,
    },
    /// Experimental typed Git provenance lookup.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    Patch {
        #[command(subcommand)]
        command: PatchCommand,
    },
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
    Scip {
        #[command(subcommand)]
        command: ScipCommand,
    },
    Graph {
        #[command(subcommand)]
        command: GraphCommand,
    },
}

#[derive(Subcommand)]
enum GraphCommand {
    Schema {
        #[arg(long, default_value = "json")]
        format: String,
    },
    Query {
        #[arg(long)]
        dsl: String,
        #[arg(long, default_value = "50")]
        limit: usize,
        #[arg(long, default_value = "3")]
        max_depth: usize,
        #[arg(long, default_value = "5000")]
        timeout_ms: u64,
        #[arg(long, default_value = "json")]
        format: String,
    },
}

#[derive(Subcommand)]
enum SnapshotCommand {
    Export {
        #[arg(long, value_enum, default_value_t = SnapshotQuality::Best)]
        quality: SnapshotQuality,
    },
    Import,
    Doctor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
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
        #[arg(long, value_enum, default_value_t = ConfidenceArg::Medium)]
        confidence: ConfidenceArg,
    },
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    Recent {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

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
    Index {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    Rebuild {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    Clean {
        #[arg(default_value = ".")]
        repo: PathBuf,
        #[arg(long, default_value_t = false)]
        include_cache: bool,
    },
}

#[derive(Subcommand)]
enum SetupCommand {
    /// Audit install readiness across index, security, MCP, and client surfaces.
    Audit {
        #[arg(default_value = ".")]
        repo: PathBuf,
        /// Render a portable Markdown setup report.
        #[arg(long, default_value_t = false)]
        markdown: bool,
        /// Write the Markdown setup report to a file.
        #[arg(long, value_name = "PATH")]
        write: Option<PathBuf>,
        /// Exit non-zero when required setup checks fail.
        #[arg(long, default_value_t = false)]
        exit_code: bool,
    },
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DoctorFormat {
    Text,
    Json,
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
    #[arg(long, value_name = "REV")]
    since: Option<String>,
}

#[derive(Subcommand)]
enum ArchitectureCommand {
    Detect,
    Boundaries,
    Violations,
    Bench(ArchitectureBenchArgs),
    Fleet {
        #[arg(long, value_name = "WORKSPACE")]
        workspace: PathBuf,
    },
    /// Experimental repository-owned architecture policy commands.
    Policy {
        #[command(subcommand)]
        command: ArchitecturePolicyCommand,
    },
}

#[derive(Subcommand)]
enum ArchitecturePolicyCommand {
    Validate {
        #[arg(long, value_name = "POLICY_TOML")]
        path: Option<PathBuf>,
        #[arg(long, value_enum)]
        format: Option<ArchitecturePolicyFormat>,
    },
    Print,
    Check {
        #[arg(long, value_enum)]
        format: Option<ArchitecturePolicyFormat>,
    },
    Explain {
        #[arg(long, conflicts_with = "symbol")]
        file: Option<PathBuf>,
        #[arg(long, conflicts_with = "file")]
        symbol: Option<String>,
        #[arg(long, value_enum)]
        format: Option<ArchitecturePolicyFormat>,
    },
}

#[derive(Subcommand)]
enum ContractCommand {
    /// Create and optionally store a change contract from a task or saved plan.
    Create {
        #[arg(value_name = "TASK")]
        task: Option<String>,
        #[arg(long, value_name = "PLAN_JSON")]
        plan: Option<PathBuf>,
        #[arg(long = "plan-json", value_name = "JSON")]
        plan_json: Option<String>,
        #[arg(long, default_value_t = 12)]
        limit: usize,
        #[arg(long = "no-store", default_value_t = false)]
        no_store: bool,
        #[arg(long, value_enum, default_value_t = ContractFormat::Json)]
        format: ContractFormat,
    },
    /// Verify changes against a stored or inline change contract.
    Verify {
        #[arg(long, value_name = "CONTRACT_ID")]
        id: Option<String>,
        #[arg(long, value_name = "CONTRACT_JSON")]
        contract: Option<PathBuf>,
        #[arg(long = "contract-json", value_name = "JSON")]
        contract_json: Option<String>,
        #[arg(long, value_name = "UNIFIED_DIFF")]
        diff: Option<PathBuf>,
        #[arg(long, default_value_t = false)]
        git: bool,
        #[arg(long = "since-plan", value_name = "REV")]
        since_plan: Option<String>,
        #[arg(long = "changed", value_name = "PATH")]
        changed: Vec<PathBuf>,
        #[arg(long = "evidence-ref", value_name = "REF")]
        evidence_refs: Vec<String>,
        #[arg(long, default_value_t = false)]
        traceability_strict: bool,
        #[arg(long = "check-api-surface", default_value_t = false)]
        check_api_surface: bool,
        #[arg(long = "check-deps", default_value_t = false)]
        check_deps: bool,
        #[arg(long, default_value_t = false)]
        run_commands: bool,
        #[arg(long = "write-attestation", default_value_t = false)]
        write_attestation: bool,
        #[arg(long, value_enum, default_value_t = ContractFormat::Json)]
        format: ContractFormat,
    },
    /// Explain the constraints, evidence, and traceability in a contract.
    Explain {
        #[arg(long, value_name = "CONTRACT_ID")]
        id: Option<String>,
        #[arg(long, value_name = "CONTRACT_JSON")]
        contract: Option<PathBuf>,
        #[arg(long = "contract-json", value_name = "JSON")]
        contract_json: Option<String>,
        #[arg(long, value_enum, default_value_t = ContractFormat::Markdown)]
        format: ContractFormat,
    },
    /// Show a stored contract by id.
    Show {
        id: String,
        #[arg(long, value_enum, default_value_t = ContractFormat::Json)]
        format: ContractFormat,
    },
    /// Export a stored contract as JSON, Markdown, or TOON.
    Export {
        id: String,
        #[arg(long, value_enum, default_value_t = ContractFormat::Json)]
        format: ContractFormat,
    },
}

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
    Churn {
        #[arg(long, conflicts_with_all = ["module", "symbol"])]
        path: Option<PathBuf>,
        #[arg(long, conflicts_with_all = ["path", "symbol"])]
        module: Option<PathBuf>,
        /// Exact symbol name, qualified name, or symbol ID.
        #[arg(long, conflicts_with_all = ["path", "module"])]
        symbol: Option<String>,
    },
    /// Resolve path ownership from CODEOWNERS, local git history, and repo memory.
    Ownership {
        #[arg(long)]
        path: PathBuf,
    },
    /// Suggest reviewers from stored review evidence, ownership, and author history.
    Reviewers {
        #[arg(long)]
        path: PathBuf,
    },
    /// Run the deterministic reviewer suggestion benchmark corpus.
    ReviewersBench(ReviewerBenchArgs),
    /// Run the deterministic similar historical change benchmark corpus.
    SimilarBench(SimilarHistoryBenchArgs),
    /// Run the unified public history API benchmark corpus.
    Bench(HistoryBenchArgs),
    Provenance {
        #[arg(long, required_unless_present = "symbol", conflicts_with = "symbol")]
        path: Option<PathBuf>,
        /// Exact symbol name, qualified name, or symbol ID.
        #[arg(long, required_unless_present = "path", conflicts_with = "path")]
        symbol: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
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

#[derive(Subcommand)]
enum ScipCommand {
    Doctor {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    Setup {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
}

#[derive(Clone, Copy, ValueEnum)]
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
