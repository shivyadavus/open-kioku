use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use open_kioku_architecture::{
    evaluate_policy, evaluate_public_api_boundary, ArchitectureDetector, PolicyResolver,
};
use open_kioku_config::{
    load_architecture_policy, load_architecture_policy_from_path, ArchitecturePolicy, OkConfig,
    PolicySource, RankingConfig, ScipMode,
};
use open_kioku_context::{expanded_task_search_terms, ContextPackBuilder, ContextPackFormat};
use open_kioku_context_compress::ContextHandleStore;
use open_kioku_contract::{
    ApiSurfaceConstraint, ChangeContractV1, ContractFile, ContractId, ContractStore,
    DependencyDeltaConstraint, FsContractStore, StoredContractRecord,
};
use open_kioku_core::{
    ChurnSummary, Confidence, ContextHandleId, EdgeId, EnforcedEdgeType, Evidence, EvidenceId,
    EvidenceSourceType, FileProvenance, GitChangeKind, GitCochangeEdge, GitCommitId,
    GitCommitRecord, GraphEdge, GraphEdgeType, GraphNode, HistoryRecordId, HistorySnapshot,
    HistorySummary, IndexManifest, IndexMode, NodeId, Owner, OwnerSuggestion, OwnershipEvidence,
    OwnershipReport, OwnershipSourceType, PlanReport, PolicyCheckReport, PolicyComponentMatch,
    PolicyExemptionEvidence, PolicyViolation, ProvenanceTouch, ReviewerAvailability,
    ReviewerEvidence, ReviewerRole, ReviewerSuggestionReport, ScoreComponent, SearchResult,
    SimilarChangeQuery, SimilarChangeReport, Symbol, SymbolId, SymbolProvenance,
};
use open_kioku_graph::InMemoryGraph;
use open_kioku_impact::ImpactEngine;
use open_kioku_ingest::{IndexProgress, Indexer};
use open_kioku_memory::RepoMemoryStore;
use open_kioku_patch::{
    ChangeVerificationReport, ChangeVerifier, ContractVerificationReport, ContractVerifier,
    PatchPlanner, VerificationVerdict, VerifyChangeInput,
};
use open_kioku_plan::{ContractBuilder, PlanEngine, PlanFormat};
use open_kioku_ranking::{
    rerank_baseline, rerank_with_options, top_score_signals, RankingMode, RankingOptions,
    RankingSignal, RankingWeights,
};
use open_kioku_search_regex::search_chunks;
use open_kioku_search_tantivy::{
    default_index_dir, rebuild_disk_index_with_graph, TantivySearchIndex,
};
use open_kioku_semantic::SemanticIndexManager;
use open_kioku_storage::{
    GraphStore, HistoryStore, IndexData, MetadataStore, OkStore, SearchIndex,
};
use open_kioku_storage_sqlite::{SqliteStore, SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION};
use open_kioku_symbols::SymbolEngine;
use open_kioku_tests::TestSelector;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

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
                "Open Kioku is ready.\n\nNext:\n  ok index\n  ok doctor\n  ok mcp install cursor\n\nIf this is useful, star the repo:\nhttps://github.com/shivyadavus/open-kioku",
                &serde_json::json!({"status":"initialized"}),
            )?;
        }
        Command::Index {
            repo: command_repo,
            with_scip,
            mode,
            workspace,
            from_snapshot,
        } => {
            let repo = resolve_repo(&repo, command_repo);
            let mode = parse_index_mode(&mode)?;
            if mode == IndexMode::CrossProject {
                let workspace = workspace.ok_or_else(|| {
                    anyhow::anyhow!("--workspace is required for cross-project indexing")
                })?;
                let report = build_cross_project_workspace(&workspace)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_workspace_link_report(&report);
                }
                return Ok(());
            }
            if from_snapshot.as_deref() == Some("auto") {
                match snapshot_import(&repo) {
                    Ok(report) => {
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&report)?);
                        } else {
                            println!(
                                "Imported snapshot from {} and rebuilt search index",
                                report.artifact_path.display()
                            );
                            for warning in &report.warnings {
                                println!("warning: {warning}");
                            }
                        }
                        return Ok(());
                    }
                    Err(err) => {
                        eprintln!("snapshot import unavailable; falling back to full index: {err}");
                    }
                }
            }
            let snapshot = index_repo_with_scip_mode(&repo, with_scip.as_deref(), mode)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&snapshot.manifest)?);
            } else {
                println!(
                    "Indexed {} files, {} symbols, {} chunks in {} mode",
                    snapshot.manifest.file_count,
                    snapshot.manifest.symbol_count,
                    snapshot.manifest.chunk_count,
                    snapshot.manifest.index_mode
                );
                if let Some(scip) = &snapshot.scip {
                    println!(
                        "SCIP: mode {:?}, imported {} index(es), {} exact references",
                        scip.mode,
                        scip.imported_paths.len(),
                        scip.exact_references
                    );
                    for attempt in &scip.generator_attempts {
                        println!(
                            "SCIP {}: {:?} - {}",
                            attempt.language, attempt.status, attempt.message
                        );
                    }
                }
            }
        }
        Command::Snapshot { command } => match command {
            SnapshotCommand::Export { quality } => {
                let report = snapshot_export(&repo, quality)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!(
                        "Exported {} snapshot to {}",
                        report.quality,
                        report.artifact_path.display()
                    );
                    println!(
                        "Metadata: {} ({} -> {} bytes)",
                        report.metadata_path.display(),
                        report.metadata.original_size_bytes,
                        report.metadata.compressed_size_bytes
                    );
                }
            }
            SnapshotCommand::Import => {
                let report = snapshot_import(&repo)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!("Imported snapshot from {}", report.artifact_path.display());
                    if report.rebuilt_search {
                        println!("Rebuilt Tantivy search index from imported SQLite index");
                    }
                    for warning in &report.warnings {
                        println!("warning: {warning}");
                    }
                }
            }
            SnapshotCommand::Doctor => {
                let report = snapshot_doctor(&repo);
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_snapshot_doctor_report(&report);
                }
            }
        },
        Command::Watch { repo: command_repo } => {
            let repo = resolve_repo(&repo, command_repo);
            open_kioku_watch::watch_repo(&repo)?;
        }
        Command::Status {
            repo: command_repo,
            markdown,
            write,
            exit_code,
        } => {
            let repo = resolve_repo(&repo, command_repo);
            let manifest = load_index_manifest(&repo)?;
            let doctor = if markdown || write.is_some() || exit_code {
                Some(doctor_report(&repo))
            } else {
                None
            };
            if markdown || write.is_some() {
                let doctor_ref = doctor
                    .as_ref()
                    .expect("doctor report should be available for status snapshot");
                let rendered = render_status_markdown(&repo, manifest.as_ref(), doctor_ref);
                if let Some(path) = write {
                    fs::write(&path, rendered)?;
                    if cli.json {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "ok": doctor_ref.ok,
                                "path": path,
                            }))?
                        );
                    } else {
                        println!("Wrote Open Kioku status snapshot to {}", path.display());
                    }
                } else {
                    println!("{rendered}");
                }
            } else if cli.json {
                println!("{}", serde_json::to_string_pretty(&manifest)?);
            } else if let Some(manifest) = manifest {
                println!(
                    "Healthy index: {} files, {} symbols, {} skipped, mode {}, indexed at {}",
                    manifest.file_count,
                    manifest.symbol_count,
                    manifest.quality.skipped_paths.len(),
                    manifest.index_mode,
                    manifest.indexed_at
                );
            } else {
                println!("No index found. Run `ok index .`.");
            }
            if exit_code && !doctor.as_ref().map(|report| report.ok).unwrap_or(true) {
                anyhow::bail!("Open Kioku status has failing readiness checks");
            }
        }
        Command::Doctor {
            repo: command_repo,
            format,
        } => {
            let repo = resolve_repo(&repo, command_repo);
            let report = doctor_report(&repo);
            let ok = report.ok;
            if cli.json || format == DoctorFormat::Json {
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
        Command::Setup { command } => match command {
            SetupCommand::Audit {
                repo: command_repo,
                markdown,
                write,
                exit_code,
            } => {
                let repo = resolve_repo(&repo, command_repo);
                let report = setup_audit_report(&repo);
                if markdown || write.is_some() {
                    let rendered = render_setup_audit_markdown(&report);
                    if let Some(path) = write {
                        fs::write(&path, rendered)?;
                        if cli.json {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&serde_json::json!({
                                    "ok": report.ok,
                                    "path": path,
                                }))?
                            );
                        } else {
                            println!("Wrote Open Kioku setup audit to {}", path.display());
                        }
                    } else {
                        println!("{rendered}");
                    }
                } else if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_setup_audit_report(&report);
                }
                if exit_code && !report.ok {
                    anyhow::bail!("Open Kioku setup audit has failing checks");
                }
            }
        },
        Command::Graph { command } => match command {
            GraphCommand::Query {
                dsl,
                limit,
                max_depth,
                timeout_ms,
                format,
            } => {
                let store = open_store(&repo)?;
                let ast = open_kioku_graph::query::parse_graph_query(&dsl)?;
                let options = open_kioku_graph::query::GraphQueryOptions {
                    limit,
                    max_depth,
                    deadline_ms: timeout_ms,
                    ..Default::default()
                };
                let result = open_kioku_graph::query::execute_graph_query(
                    &store as &dyn open_kioku_storage::GraphStore,
                    &ast,
                    options,
                )?;
                if format.to_lowercase() == "json" {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("{:?}", result);
                }
            }
            GraphCommand::Schema { format } => {
                let store = open_store(&repo).ok();
                let manifest = store
                    .as_ref()
                    .and_then(|store| open_kioku_storage::MetadataStore::manifest(store).ok())
                    .flatten();
                let schema = open_kioku_graph::schema::current_schema_with_manifest(
                    store
                        .as_ref()
                        .map(|s| s as &dyn open_kioku_storage::GraphStore),
                    manifest.as_ref(),
                );
                if format.to_lowercase() == "markdown" {
                    let mut lines = vec![
                        format!("# Open Kioku Evidence Graph Schema v{}", schema.version),
                        "".to_string(),
                    ];

                    if !schema.feature_flags.is_empty() {
                        lines.push("## Supported Features".to_string());
                        for feature in &schema.feature_flags {
                            lines.push(format!("- `{}`", feature));
                        }
                        lines.push("".to_string());
                    }

                    if !schema.query_features.is_empty() {
                        lines.push("## Query Features".to_string());
                        for feature in &schema.query_features {
                            lines.push(format!("- `{}`", feature));
                        }
                        lines.push("".to_string());
                    }

                    if !schema.evidence_source_types.is_empty() {
                        lines.push("## Evidence Source Types".to_string());
                        for source_type in &schema.evidence_source_types {
                            lines.push(format!("- `{}`", source_type));
                        }
                        lines.push("".to_string());
                    }

                    if !schema.optional_evidence.is_empty() {
                        lines.push("## Optional Evidence Availability".to_string());
                        for evidence in &schema.optional_evidence {
                            lines.push(format!(
                                "- `{}`: {} (count: {})",
                                evidence.name, evidence.status, evidence.evidence_count
                            ));
                            for caveat in &evidence.caveats {
                                lines.push(format!("  - caveat: {}", caveat));
                            }
                        }
                        lines.push("".to_string());
                    }

                    lines.push("## Node Types".to_string());
                    for node in &schema.node_types {
                        let status = if node.stable {
                            "Stable"
                        } else {
                            "Experimental"
                        };
                        lines.push(format!("### {} ({})", node.name, status));
                        lines.push(node.description.clone());
                        if !node.required_fields.is_empty() {
                            lines.push(
                                "- **Required**: ".to_string() + &node.required_fields.join(", "),
                            );
                        }
                        if !node.optional_fields.is_empty() {
                            lines.push(
                                "- **Optional**: ".to_string() + &node.optional_fields.join(", "),
                            );
                        }
                        lines.push("".to_string());
                    }

                    lines.push("## Edge Types".to_string());
                    for edge in &schema.edge_types {
                        let status = if edge.stable {
                            "Stable"
                        } else {
                            "Experimental"
                        };
                        lines.push(format!("### {} ({})", edge.name, status));
                        lines.push(edge.description.clone());
                        lines.push(format!("- **Sources**: {}", edge.source_types.join(", ")));
                        lines.push(format!("- **Targets**: {}", edge.target_types.join(", ")));
                        if !edge.required_evidence.is_empty() {
                            lines.push(format!(
                                "- **Evidence**: {}",
                                edge.required_evidence.join(", ")
                            ));
                        }
                        lines.push("".to_string());
                    }

                    println!("{}", lines.join("\n"));
                } else {
                    println!("{}", serde_json::to_string_pretty(&schema)?);
                }
            }
        },
        Command::Demo { path, force } => {
            let repo = demo_repo_path(path.clone())?;
            let report = build_demo_repo(&repo, force)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                let rel_path = if let Some(ref p) = path {
                    p.display().to_string()
                } else {
                    "./open-kioku-demo".to_string()
                };
                println!("Open Kioku is ready.\n");
                println!("Next:");
                println!("  ok demo --force");
                println!("  ok --repo {} plan token --format markdown\n", rel_path);
                println!("If this is useful, star the repo:");
                println!("https://github.com/shivyadavus/open-kioku");
            }
        }
        Command::Search {
            query,
            limit,
            kind,
            explain_ranking,
            semantic,
            hybrid,
        } => {
            let store = open_store(&repo)?;
            let results = if matches!(kind, SearchKind::Graph) {
                graph_search(&repo, &query, limit)?
            } else if semantic {
                semantic_search(&repo, &store, &query, limit)?
            } else if hybrid {
                hybrid_search(&repo, &store, &query, limit)?
            } else {
                search(&repo, &store, &query, limit)?
            };
            output(cli.json, &results, || {
                for result in &results {
                    println!(
                        "{}:{}  {:.2}  {}",
                        result.path.display(),
                        result.line_range.as_ref().map(|r| r.start).unwrap_or(0),
                        result.score,
                        result.snippet
                    );
                    if explain_ranking {
                        let signals = top_score_signals(result, 3);
                        if signals.is_empty() {
                            println!("  ranking: no dominant signals");
                        } else {
                            println!("  ranking: {}", signals.join(", "));
                        }
                    }
                }
            })?;
        }
        Command::Semantic { command } => match command {
            SemanticCommand::Status { repo: command_repo } => {
                let repo = absolutize(&resolve_repo(&repo, command_repo))?;
                let store = open_store(&repo)?;
                let config = OkConfig::load_from_repo(&repo)?;
                let manager = SemanticIndexManager::new(&repo, &store, &config.semantic);
                let status = manager.status();
                output(cli.json, &status, || print_semantic_status(&status))?;
            }
            SemanticCommand::Index { repo: command_repo } => {
                let repo = absolutize(&resolve_repo(&repo, command_repo))?;
                let store = open_store(&repo)?;
                let mut config = OkConfig::load_from_repo(&repo)?;
                config.semantic.enabled = true;
                let manager = SemanticIndexManager::new(&repo, &store, &config.semantic);
                let report = manager.index()?;
                output(cli.json, &report, || {
                    println!(
                        "Semantic index ready: {} vectors, {} reused, {} embedded",
                        report.status.vector_count, report.reused_embeddings, report.embedded_count
                    );
                })?;
            }
            SemanticCommand::Rebuild { repo: command_repo } => {
                let repo = absolutize(&resolve_repo(&repo, command_repo))?;
                let store = open_store(&repo)?;
                let mut config = OkConfig::load_from_repo(&repo)?;
                config.semantic.enabled = true;
                let manager = SemanticIndexManager::new(&repo, &store, &config.semantic);
                let report = manager.rebuild()?;
                output(cli.json, &report, || {
                    println!(
                        "Semantic index rebuilt: {} vectors, {} embedded",
                        report.status.vector_count, report.embedded_count
                    );
                })?;
            }
            SemanticCommand::Clean {
                repo: command_repo,
                include_cache,
            } => {
                let repo = absolutize(&resolve_repo(&repo, command_repo))?;
                let store = open_store(&repo)?;
                let config = OkConfig::load_from_repo(&repo)?;
                let manager = SemanticIndexManager::new(&repo, &store, &config.semantic);
                manager.clean(include_cache)?;
                println!("Semantic artifacts removed.");
            }
        },
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
                    let symbols = if let Some(file) = &file {
                        store.symbols_for_file(&file.id)?
                    } else {
                        Vec::new()
                    };
                    output(
                        cli.json,
                        &serde_json::json!({"file": file, "chunks": chunks, "symbols": symbols}),
                        || {
                            if let Some(f) = &file {
                                println!(
                                    "{} ({:?}, {} bytes)",
                                    path.display(),
                                    f.language,
                                    f.size_bytes
                                );
                            }
                            println!("{} chunks, {} symbols indexed", chunks.len(), symbols.len());
                            for symbol in &symbols {
                                let range = symbol
                                    .range
                                    .as_ref()
                                    .map(|r| format!(":{}–{}", r.start, r.end))
                                    .unwrap_or_default();
                                println!("  {:?} {}{}", symbol.kind, symbol.name, range);
                            }
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
            let index_dir = default_index_dir(&repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(&index_dir)?)
            } else {
                None
            };
            let engine = ImpactEngine::new(&store)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_history_store(Some(&store));
            let architecture_policy = configured_architecture_policy_report(&repo, &store)?;

            if let Some(since) = args.since.as_deref() {
                let changed = changed_ranges_since(&repo, since)?;
                let mut reports = Vec::new();
                for file in changed.iter().filter_map(|change| change.new_path.as_ref()) {
                    let mut report = engine.for_file(file)?;
                    report.architecture_policy = architecture_policy.clone();
                    reports.push(report);
                }
                if cli.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "since": since,
                            "changed_files": changed,
                            "impact_reports": reports,
                        }))?
                    );
                } else {
                    println!("Changed files since {since}:");
                    for change in &changed {
                        println!("  {}", render_changed_range(change));
                    }
                    for report in &reports {
                        println!("\nImpact target: {}", report.target);
                        println!(
                            "Risk: {} ({:.2})",
                            report.risk_report.level, report.risk_report.score
                        );
                    }
                }
                return Ok(());
            }

            let mut report = if let Some(path) = args.file {
                let normalized = normalize_to_repo_relative(&repo, &path);
                engine.for_file(&normalized)?
            } else if let Some(symbol) = args.symbol {
                let definition = SymbolEngine::new(&store).definition(&symbol)?;
                let files = store.list_files(usize::MAX, 0)?;
                let file = files.iter().find(|file| file.id == definition.file_id);
                let path_to_use = file
                    .map(|file| file.path.as_path())
                    .unwrap_or(Path::new(&symbol));
                let normalized = normalize_to_repo_relative(&repo, path_to_use);
                engine.for_file(&normalized)?
            } else {
                anyhow::bail!("provide --file or --symbol");
            };
            report.architecture_policy = architecture_policy;
            output(cli.json, &report, || {
                println!("Impact target: {}", report.target);
                println!(
                    "Risk: {} ({:.2})",
                    report.risk_report.level, report.risk_report.score
                );
                println!("\nDirect impacts ({}):", report.direct_impacts.len());
                for result in &report.direct_impacts {
                    println!(
                        "  {}:{} ({:.2})",
                        result.path.display(),
                        result.line_range.as_ref().map(|r| r.start).unwrap_or(0),
                        result.score
                    );
                }
                if !report.indirect_impacts.is_empty() {
                    println!("\nIndirect impacts ({}):", report.indirect_impacts.len());
                    for result in report.indirect_impacts.iter().take(5) {
                        println!(
                            "  {}:{} ({:.2})",
                            result.path.display(),
                            result.line_range.as_ref().map(|r| r.start).unwrap_or(0),
                            result.score
                        );
                    }
                }
            })?;
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
                &TestSelector::new(&store).for_changed_path_with_evidence(&changed, 20)?,
                || {},
            )?;
        }
        Command::Context {
            task,
            format,
            compressed,
        } => {
            let store = open_store(&repo)?;
            let pack = build_context_pack(&repo, &store, &task, 20)?;
            if compressed {
                let compressed = ContextHandleStore::open_repo(&repo)?.compress_pack(&pack)?;
                if cli.json || format == ContextPackFormat::Json {
                    println!("{}", serde_json::to_string_pretty(&compressed)?);
                } else if format == ContextPackFormat::Toon {
                    println!(
                        "{}",
                        open_kioku_format::render_compressed_context_toon(&compressed)
                    );
                } else {
                    println!("{}", serde_json::to_string_pretty(&compressed)?);
                }
            } else {
                let rendered = format.render(&pack)?;
                println!("{}", rendered);
            }
        }
        Command::RetrieveContext { handle } => {
            let retrieved =
                ContextHandleStore::open_repo(&repo)?.retrieve(&ContextHandleId::new(handle))?;
            output(cli.json, &retrieved, || {
                if let Some(retrieved) = &retrieved {
                    println!("{}", retrieved.original);
                } else {
                    println!("No context handle found.");
                }
            })?;
        }
        Command::Plan {
            task,
            format,
            limit,
            since,
            verify_evidence,
        } => {
            let store = open_store(&repo)?;
            let task = if let Some(since) = since.as_deref() {
                task_with_changed_ranges(&repo, &task, since)?
            } else {
                task
            };
            let context = build_context_pack(&repo, &store, &task, limit)?;
            let index_dir = default_index_dir(&repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(&index_dir)?)
            } else {
                None
            };
            let report = PlanEngine::new(&store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_history_store(Some(&store))
                .with_memory_facts(RepoMemoryStore::open_repo(&repo)?.search(&task, 8)?)
                .plan_from_context(&task, limit, context)?;
            let format = if cli.json { PlanFormat::Json } else { format };
            println!("{}", format.render(&report)?);
            verify_plan_evidence(&report, verify_evidence)?;
        }
        Command::VerifyBoundary {
            plan,
            changed,
            evidence_refs,
        } => {
            let report = load_saved_plan(&plan)?;
            let outcome = verify_saved_plan_boundary(&report, &changed, &evidence_refs)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&outcome)?);
            } else {
                println!(
                    "Boundary verification passed for {} changed file(s)",
                    outcome.changed_files.len()
                );
                for warning in &outcome.warnings {
                    eprintln!("{warning}");
                }
            }
        }
        Command::Verify {
            plan,
            diff,
            git,
            since_plan,
            changed,
            evidence_refs,
            traceability_strict,
            check_api_surface,
            check_deps,
            run_commands,
            write_attestation,
        } => {
            let store = open_store(&repo)?;
            let report = load_saved_plan(&plan)?;
            let mut changed = changed;
            let unified_diff = if let Some(since) = since_plan.as_deref() {
                for change in changed_ranges_since(&repo, since)? {
                    if let Some(path) = change.new_path.or(change.old_path) {
                        changed.push(path);
                    }
                }
                verify_diff_since(&repo, diff.as_deref(), since)?
            } else {
                verify_diff_input(&repo, diff.as_deref(), git)?
            };
            let index_dir = default_index_dir(&repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(&index_dir)?)
            } else {
                None
            };
            let architecture_policy = load_architecture_policy(&repo)?;
            let check_dependency_delta = check_deps || architecture_policy.is_some();
            let contract_store =
                write_attestation.then(|| FsContractStore::new(repo.join(".ok/contracts")));
            let verification = ChangeVerifier::new(&store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_contract_store(
                    contract_store
                        .as_ref()
                        .map(|store| store as &dyn ContractStore),
                )
                .verify(
                    &repo,
                    &report,
                    VerifyChangeInput {
                        changed_files: changed,
                        unified_diff,
                        evidence_refs,
                        run_commands,
                        write_attestation,
                        validation_attestations: Vec::new(),
                        traceability_strict,
                        check_api_surface,
                        check_dependency_delta,
                        architecture_policy,
                    },
                )?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&verification)?);
            } else {
                print_verify_report(&verification);
            }
            if matches!(
                verification.verdict,
                open_kioku_patch::VerificationVerdict::Fail
            ) {
                anyhow::bail!("change verification failed");
            }
        }
        Command::Contract { command } => {
            handle_contract_command(cli.json, &repo, command)?;
        }
        Command::Bench(args) => {
            let min_precision = args.quality_min_precision_at_1;
            let report = run_bench(args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_bench_report(&report);
            }
            if let Some(quality) = &report.quality {
                if quality.precision_at_1 < min_precision {
                    anyhow::bail!(
                        "quality precision@1 {:.3} is below required {:.3}",
                        quality.precision_at_1,
                        min_precision
                    );
                }
            }
        }
        Command::WorkflowBench(args) => {
            let min_context_recall = args.min_context_recall;
            let min_verification_accuracy = args.min_verification_accuracy;
            let min_cases = args.min_cases;
            let report = run_workflow_bench(args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_workflow_bench_report(&report);
            }
            if report.case_count < min_cases {
                anyhow::bail!(
                    "workflow benchmark loaded {} cases, below required {}",
                    report.case_count,
                    min_cases
                );
            }
            if report.workflow.context_recall_at_k < min_context_recall {
                anyhow::bail!(
                    "workflow context recall@{} {:.3} is below required {:.3}",
                    report.limit,
                    report.workflow.context_recall_at_k,
                    min_context_recall
                );
            }
            if report.workflow.verification_verdict_accuracy < min_verification_accuracy {
                anyhow::bail!(
                    "workflow verification accuracy {:.3} is below required {:.3}",
                    report.workflow.verification_verdict_accuracy,
                    min_verification_accuracy
                );
            }
        }
        Command::ContractBench(args) => {
            let min_cases = args.min_cases;
            let min_verdict_accuracy = args.min_verdict_accuracy;
            let min_verification_precision = args.min_verification_precision;
            let min_boundary_precision = args.min_boundary_precision;
            let min_boundary_recall = args.min_boundary_recall;
            let min_toon_reduction = args.min_toon_reduction;
            let report = run_contract_bench(args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_contract_bench_report(&report);
            }
            if report.case_count < min_cases {
                anyhow::bail!(
                    "contract benchmark loaded {} cases, below required {}",
                    report.case_count,
                    min_cases
                );
            }
            if !report.failures.is_empty() {
                anyhow::bail!(
                    "contract benchmark failed {} case expectation(s): {}",
                    report.failures.len(),
                    report.failures.join(", ")
                );
            }
            if report.summary.verdict_accuracy < min_verdict_accuracy {
                anyhow::bail!(
                    "contract verdict accuracy {:.3} is below required {:.3}",
                    report.summary.verdict_accuracy,
                    min_verdict_accuracy
                );
            }
            if report.summary.verification_precision < min_verification_precision {
                anyhow::bail!(
                    "contract verification precision {:.3} is below required {:.3}",
                    report.summary.verification_precision,
                    min_verification_precision
                );
            }
            if report.summary.boundary_precision < min_boundary_precision {
                anyhow::bail!(
                    "contract boundary precision {:.3} is below required {:.3}",
                    report.summary.boundary_precision,
                    min_boundary_precision
                );
            }
            if report.summary.boundary_recall < min_boundary_recall {
                anyhow::bail!(
                    "contract boundary recall {:.3} is below required {:.3}",
                    report.summary.boundary_recall,
                    min_boundary_recall
                );
            }
            if report.summary.min_toon_reduction < min_toon_reduction {
                anyhow::bail!(
                    "contract TOON reduction {:.3} is below required {:.3}",
                    report.summary.min_toon_reduction,
                    min_toon_reduction
                );
            }
        }
        Command::Eval(args) => {
            let min_recall = args.min_recall_at_k;
            let min_mrr = args.min_mrr;
            let report = run_eval(args)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_eval_report(&report);
            }
            if report.summary.search_recall_at_k < min_recall {
                anyhow::bail!(
                    "eval search recall@{} {:.3} is below required {:.3}",
                    report.limit,
                    report.summary.search_recall_at_k,
                    min_recall
                );
            }
            if report.summary.search_mrr < min_mrr {
                anyhow::bail!(
                    "eval MRR {:.3} is below required {:.3}",
                    report.summary.search_mrr,
                    min_mrr
                );
            }
        }
        Command::Prove(args) => {
            let format = if cli.json {
                ProveFormat::Json
            } else {
                args.format
            };
            let report = run_proof(args)?;
            if matches!(format, ProveFormat::Json) {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", render_proof_markdown(&report));
                println!("\nShareable proof generated.");
                println!("Repo: https://github.com/shivyadavus/open-kioku");
            }
        }
        Command::Architecture { command } => match command {
            ArchitectureCommand::Policy { command } => {
                handle_architecture_policy_command(cli.json, &repo, command)?;
            }
            ArchitectureCommand::Detect => {
                let store = open_store(&repo)?;
                let summary = ArchitectureDetector::new(&store, None).detect()?;
                output(cli.json, &summary, || {})?;
            }
            ArchitectureCommand::Boundaries => {
                let store = open_store(&repo)?;
                let summary = ArchitectureDetector::new(&store, None).detect()?;
                output(cli.json, &summary.components, || {})?;
            }
            ArchitectureCommand::Violations => {
                let store = open_store(&repo)?;
                let summary = ArchitectureDetector::new(&store, None).detect()?;
                output(cli.json, &summary.violations, || {})?;
            }
            ArchitectureCommand::Bench(args) => {
                let min_precision = args.min_precision;
                let min_recall = args.min_recall;
                let max_p95_ms = args.max_p95_ms;
                let report = run_architecture_policy_bench(args)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_architecture_policy_bench_report(&report);
                }
                if report.summary.precision < min_precision {
                    anyhow::bail!(
                        "architecture policy benchmark precision {:.3} is below required {:.3}",
                        report.summary.precision,
                        min_precision
                    );
                }
                if report.summary.recall < min_recall {
                    anyhow::bail!(
                        "architecture policy benchmark recall {:.3} is below required {:.3}",
                        report.summary.recall,
                        min_recall
                    );
                }
                if let Some(max_p95_ms) = max_p95_ms {
                    if report.p95_policy_check_ms > max_p95_ms {
                        anyhow::bail!(
                            "architecture policy p95 {:.2}ms exceeds required {:.2}ms",
                            report.p95_policy_check_ms,
                            max_p95_ms
                        );
                    }
                }
            }
            ArchitectureCommand::Fleet { workspace } => {
                let report = load_fleet_architecture_report(&workspace)?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print_fleet_architecture_report(&report);
                }
            }
        },
        Command::History { command } => {
            let store = open_store(&repo)?;
            match command {
                HistoryCommand::Similar {
                    task,
                    paths,
                    symbols,
                    limit,
                } => {
                    let query = SimilarChangeQuery {
                        task,
                        paths,
                        symbols,
                    };
                    let report = store.similar_changes(&query, limit)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_similar_change_report(&report);
                    }
                }
                HistoryCommand::Churn {
                    path,
                    module,
                    symbol,
                } => {
                    let provided = usize::from(path.is_some())
                        + usize::from(module.is_some())
                        + usize::from(symbol.is_some());
                    if provided != 1 {
                        anyhow::bail!("provide exactly one of --path, --module, or --symbol");
                    }
                    let summary = if let Some(path) = path {
                        store.churn_for_file(&path)?
                    } else if let Some(module) = module {
                        store.churn_for_module(&module)?
                    } else if let Some(query) = symbol {
                        let symbol = resolve_provenance_symbol(&store, &query)?;
                        store.churn_for_symbol(&symbol.id)?
                    } else {
                        unreachable!("exactly one churn target was checked above");
                    };
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&summary)?);
                    } else {
                        print_churn_summary(&summary);
                    }
                }
                HistoryCommand::Ownership { path } => {
                    let components = ownership_components(&repo, &store, &path)?;
                    let memory_facts = ownership_memory_facts(&repo, &path, &components)?;
                    let report =
                        open_kioku_git::ownership_for_path(open_kioku_git::OwnershipInput {
                            repo: &repo,
                            path: &path,
                            history: &store,
                            memory_facts: &memory_facts,
                            components,
                        })?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_ownership_report(&report);
                    }
                }
                HistoryCommand::Reviewers { path } => {
                    let components = ownership_components(&repo, &store, &path)?;
                    let memory_facts = ownership_memory_facts(&repo, &path, &components)?;
                    let ownership =
                        open_kioku_git::ownership_for_path(open_kioku_git::OwnershipInput {
                            repo: &repo,
                            path: &path,
                            history: &store,
                            memory_facts: &memory_facts,
                            components,
                        })?;
                    let report = open_kioku_git::suggest_reviewers(
                        open_kioku_git::ReviewerSuggestionInput {
                            path: &path,
                            history: &store,
                            ownership: Some(&ownership),
                        },
                    )?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_reviewer_suggestion_report(&report);
                    }
                }
                HistoryCommand::ReviewersBench(args) => {
                    let min_accuracy = args.min_accuracy;
                    let report = run_reviewer_bench(&repo, args)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_reviewer_bench_report(&report);
                    }
                    if report.accuracy < min_accuracy {
                        anyhow::bail!(
                            "reviewer benchmark accuracy {:.3} is below required {:.3}",
                            report.accuracy,
                            min_accuracy
                        );
                    }
                }
                HistoryCommand::SimilarBench(args) => {
                    let min_recall_at_5 = args.min_recall_at_5;
                    let report = run_similar_history_bench(&repo, args)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_similar_history_bench_report(&report);
                    }
                    if report.recall_at_5 < min_recall_at_5 {
                        anyhow::bail!(
                            "similar-history benchmark Top-5 recall {:.3} is below required {:.3}",
                            report.recall_at_5,
                            min_recall_at_5
                        );
                    }
                }
                HistoryCommand::Bench(args) => {
                    let min_reviewer_accuracy = args.min_reviewer_accuracy;
                    let min_similar_recall_at_5 = args.min_similar_recall_at_5;
                    let max_similar_p95_ms = args.max_similar_p95_ms;
                    let max_lookup_p95_ms = args.max_lookup_p95_ms;
                    let report = run_history_bench(&repo, args)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    } else {
                        print_history_bench_report(&report);
                    }
                    if report.reviewer_accuracy < min_reviewer_accuracy {
                        anyhow::bail!(
                            "history benchmark reviewer accuracy {:.3} is below required {:.3}",
                            report.reviewer_accuracy,
                            min_reviewer_accuracy
                        );
                    }
                    if report.similar_recall_at_5 < min_similar_recall_at_5 {
                        anyhow::bail!(
                            "history benchmark similar-change Top-5 recall {:.3} is below required {:.3}",
                            report.similar_recall_at_5,
                            min_similar_recall_at_5
                        );
                    }
                    if report.similar_p95_ms > max_similar_p95_ms {
                        anyhow::bail!(
                            "history benchmark similar-change p95 latency {:.3} ms exceeds {:.3} ms",
                            report.similar_p95_ms,
                            max_similar_p95_ms
                        );
                    }
                    if report.ownership_churn_p95_ms > max_lookup_p95_ms {
                        anyhow::bail!(
                            "history benchmark ownership/churn p95 latency {:.3} ms exceeds {:.3} ms",
                            report.ownership_churn_p95_ms,
                            max_lookup_p95_ms
                        );
                    }
                    if !report.failures.is_empty() {
                        anyhow::bail!(
                            "history benchmark had {} failing public API case(s)",
                            report.failures.len()
                        );
                    }
                }
                HistoryCommand::Provenance {
                    path,
                    symbol,
                    limit,
                } => {
                    if let Some(path) = path {
                        let provenance = store.provenance_for_path(&path, limit)?;
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&provenance)?);
                        } else {
                            print_file_provenance(&provenance);
                        }
                    } else if let Some(query) = symbol {
                        let symbol = resolve_provenance_symbol(&store, &query)?;
                        let provenance = store.provenance_for_symbol(&symbol.id, limit)?;
                        if cli.json {
                            println!("{}", serde_json::to_string_pretty(&provenance)?);
                        } else {
                            print_symbol_provenance(&provenance);
                        }
                    }
                }
            }
        }
        Command::Patch { command } => {
            let config = OkConfig::load_from_repo(&repo)?;
            let store = open_store(&repo)?;
            let planner = PatchPlanner::new(&config, &store as &dyn OkStore);
            match command {
                PatchCommand::Plan { task } => output(cli.json, &planner.plan(&task)?, || {})?,
                PatchCommand::Review { id } => {
                    let response = serde_json::json!({
                        "id": id,
                        "status": "requires_stored_patch_plan",
                        "message": "patch review requires a stored patch plan"
                    });
                    print_text_or_json(
                        cli.json,
                        &format!("patch review requires stored patch plan id={id}"),
                        &response,
                    )?;
                }
                PatchCommand::Apply { id, approved } => {
                    anyhow::bail!("patch apply is policy gated and requires a stored diff; id={id} approved={approved}");
                }
            }
        }
        Command::Memory { command } => {
            let memory = RepoMemoryStore::open_repo(&repo)?;
            match command {
                MemoryCommand::Remember {
                    text,
                    source,
                    confidence,
                } => {
                    let fact = memory.remember(&text, &source, confidence.into())?;
                    output(cli.json, &fact, || {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&fact).unwrap_or_default()
                        );
                    })?;
                }
                MemoryCommand::Search { query, limit } => {
                    let results = memory.search(&query, limit)?;
                    output(cli.json, &results, || {
                        if results.is_empty() {
                            println!("No repo memory matched.");
                        } else {
                            for result in &results {
                                println!(
                                    "{:.2} {} [{}]",
                                    result.score, result.fact.text, result.fact.source
                                );
                            }
                        }
                    })?;
                }
                MemoryCommand::Recent { limit } => {
                    let facts = memory.recent(limit)?;
                    output(cli.json, &facts, || {
                        for fact in &facts {
                            println!("{} [{}]", fact.text, fact.source);
                        }
                    })?;
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
                    if let Some(config_text) = snippet["config_text"].as_str() {
                        println!("{config_text}");
                    } else if let Ok(config) = serde_json::to_string_pretty(&snippet["config"]) {
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
        Command::Scip { command } => match command {
            ScipCommand::Doctor { repo: command_repo } => {
                let repo = resolve_repo(&repo, command_repo);
                let config = OkConfig::load_from_repo(&repo)?;
                let snapshot = scip_setup_report(&repo, &config);
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&snapshot)?);
                } else {
                    print_scip_setup_report(&snapshot);
                }
            }
            ScipCommand::Setup { repo: command_repo } => {
                let repo = resolve_repo(&repo, command_repo);
                let config = OkConfig::load_from_repo(&repo)?;
                let snapshot = scip_setup_report(&repo, &config);
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&snapshot)?);
                } else {
                    print_scip_setup_report(&snapshot);
                    println!("\nTo generate where installed:");
                    println!(
                        "  ok index {} --with-scip auto",
                        shell_quote(&repo.display().to_string())
                    );
                    println!("\nOpen Kioku will never install SCIP indexers unless a future explicit install flag enables it.");
                }
            }
        },
    }
    Ok(())
}

fn handle_architecture_policy_command(
    json: bool,
    repo: &Path,
    command: ArchitecturePolicyCommand,
) -> anyhow::Result<()> {
    match command {
        ArchitecturePolicyCommand::Validate { path, format } => {
            let (policy, paths) = if let Some(path) = path {
                let path = if path.is_absolute() {
                    path
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
            let output = architecture_policy_output(policy, paths);
            match architecture_policy_format(json, format) {
                ArchitecturePolicyFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                ArchitecturePolicyFormat::Markdown => {
                    print!("{}", render_architecture_policy_validate_markdown(&output));
                }
                ArchitecturePolicyFormat::Text => {
                    println!("{}", output.message);
                }
            }
        }
        ArchitecturePolicyCommand::Print => {
            let policy = load_architecture_policy(repo)?;
            let paths = policy
                .as_ref()
                .map(|policy| policy.source_paths(repo))
                .unwrap_or_default();
            let output = architecture_policy_output(policy, paths);
            if json {
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if let Some(policy) = &output.policy {
                println!("# source: {}", output.source.unwrap_or_default());
                for path in &output.paths {
                    println!("# path: {}", path.display());
                }
                print!("{}", policy.to_toml()?);
            } else {
                println!("{}", output.message);
            }
        }
        ArchitecturePolicyCommand::Check { format } => {
            let format = architecture_policy_format(json, format);
            let Some(policy) = load_architecture_policy(repo)? else {
                let report = open_kioku_core::PolicyCheckReport {
                    configured: false,
                    uncertainty: vec![
                        "no architecture policy configured; dependency edges were not evaluated"
                            .into(),
                    ],
                    ..Default::default()
                };
                match format {
                    ArchitecturePolicyFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&report)?);
                    }
                    ArchitecturePolicyFormat::Markdown => {
                        print!("{}", render_policy_check_markdown(&report));
                    }
                    ArchitecturePolicyFormat::Text => {
                        println!(
                            "No architecture policy configured; dependency edges were not evaluated."
                        );
                    }
                }
                return Ok(());
            };
            let store = open_store(repo)?;
            let resolver = PolicyResolver::new(&policy)?;
            let report = evaluate_policy(&store, &resolver, &policy)?;
            match format {
                ArchitecturePolicyFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                }
                ArchitecturePolicyFormat::Markdown => {
                    print!("{}", render_policy_check_markdown(&report));
                }
                ArchitecturePolicyFormat::Text => {
                    println!(
                        "Evaluated {} dependency edge(s): {} allowed, {} violation(s), {} unknown.",
                        report.evaluated_edge_count,
                        report.allowed_edges,
                        report.violation_count,
                        report.unknown_edge_count
                    );
                    for violation in &report.violations {
                        println!(
                            "{} {} -> {} via {:?}: {}",
                            violation.severity,
                            violation.source_path.display(),
                            violation.target_path.display(),
                            violation.edge_type,
                            violation.rule_id
                        );
                    }
                    for note in &report.uncertainty {
                        println!("uncertainty: {note}");
                    }
                }
            }
        }
        ArchitecturePolicyCommand::Explain {
            file,
            symbol,
            format,
        } => {
            let format = architecture_policy_format(json, format);
            let Some(policy) = load_architecture_policy(repo)? else {
                let output = ArchitecturePolicyExplainOutput {
                    configured: false,
                    query_kind: if symbol.is_some() {
                        "symbol".into()
                    } else if file.is_some() {
                        "file".into()
                    } else {
                        "repo".into()
                    },
                    query: symbol
                        .clone()
                        .unwrap_or_else(|| file.unwrap_or_else(|| repo.to_path_buf()).display().to_string()),
                    file_path: None,
                    symbol: None,
                    components: Vec::new(),
                    violations: Vec::new(),
                    exemptions: Vec::new(),
                    uncertainty: vec![
                        "no architecture policy configured; public API boundaries were not evaluated"
                            .into(),
                    ],
                    message: "No architecture policy configured; public API boundaries were not evaluated."
                        .into(),
                };
                match format {
                    ArchitecturePolicyFormat::Json => {
                        println!("{}", serde_json::to_string_pretty(&output)?);
                    }
                    ArchitecturePolicyFormat::Markdown => {
                        print!("{}", render_policy_explain_markdown(&output));
                    }
                    ArchitecturePolicyFormat::Text => {
                        println!("{}", output.message);
                    }
                }
                return Ok(());
            };
            let store = open_store(repo)?;
            let resolver = PolicyResolver::new(&policy)?;
            let output =
                architecture_policy_explain_output(repo, &store, &resolver, &policy, file, symbol)?;
            match format {
                ArchitecturePolicyFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&output)?);
                }
                ArchitecturePolicyFormat::Markdown => {
                    print!("{}", render_policy_explain_markdown(&output));
                }
                ArchitecturePolicyFormat::Text => {
                    println!("{}", output.message);
                    for component in &output.components {
                        println!(
                            "component: {} via {}",
                            component.component_id, component.matched_glob
                        );
                    }
                    for violation in &output.violations {
                        println!(
                            "{} {} -> {} via {:?}: {}",
                            violation.severity,
                            violation.source_path.display(),
                            violation.target_path.display(),
                            violation.edge_type,
                            violation.rule_id
                        );
                    }
                    for exemption in &output.exemptions {
                        println!(
                            "exempted {} by {} ({}): {} -> {}",
                            exemption.rule_id,
                            exemption.exemption_id,
                            exemption.scope,
                            exemption.source_path.display(),
                            exemption.target_path.display()
                        );
                    }
                    for note in &output.uncertainty {
                        println!("uncertainty: {note}");
                    }
                }
            }
        }
    }
    Ok(())
}

fn architecture_policy_output(
    policy: Option<ArchitecturePolicy>,
    paths: Vec<PathBuf>,
) -> ArchitecturePolicyOutput {
    let source = policy.as_ref().map(|policy| policy.source);
    let configured = policy.is_some();
    let message = if let Some(source) = source {
        format!("Architecture policy is valid ({source}).")
    } else {
        "No architecture policy configured. Heuristic architecture detection remains active.".into()
    };
    ArchitecturePolicyOutput {
        valid: true,
        configured,
        source,
        paths,
        policy,
        message,
    }
}

fn architecture_policy_format(
    global_json: bool,
    requested: Option<ArchitecturePolicyFormat>,
) -> ArchitecturePolicyFormat {
    if global_json {
        ArchitecturePolicyFormat::Json
    } else {
        requested.unwrap_or(ArchitecturePolicyFormat::Text)
    }
}

fn render_architecture_policy_validate_markdown(output: &ArchitecturePolicyOutput) -> String {
    let mut out = String::new();
    out.push_str("# Architecture Policy Validation\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    out.push_str(&format!("| Valid | `{}` |\n", output.valid));
    out.push_str(&format!("| Configured | `{}` |\n", output.configured));
    out.push_str(&format!(
        "| Source | `{}` |\n",
        output.source.unwrap_or_default()
    ));
    out.push_str(&format!("| Paths | `{}` |\n", output.paths.len()));
    out.push('\n');
    out.push_str(&output.message);
    out.push('\n');
    if !output.paths.is_empty() {
        out.push_str("\n## Paths\n\n");
        for path in &output.paths {
            out.push_str(&format!("- `{}`\n", path.display()));
        }
    }
    if let Some(policy) = &output.policy {
        out.push_str("\n## Policy Summary\n\n");
        out.push_str(&format!("- Layers: `{}`\n", policy.layers.len()));
        out.push_str(&format!(
            "- Dependency rules: `{}`\n",
            policy.dependency_rules.len()
        ));
        out.push_str(&format!(
            "- Public API rules: `{}`\n",
            policy.public_api_rules.len()
        ));
        out.push_str(&format!(
            "- Internal-only rules: `{}`\n",
            policy.internal_only_rules.len()
        ));
        out.push_str(&format!("- Exemptions: `{}`\n", policy.exemptions.len()));
    }
    out
}

fn render_policy_check_markdown(report: &open_kioku_core::PolicyCheckReport) -> String {
    let mut out = String::new();
    out.push_str("# Architecture Policy Check\n\n");
    out.push_str("| Metric | Value |\n| --- | ---: |\n");
    out.push_str(&format!("| Configured | `{}` |\n", report.configured));
    out.push_str(&format!(
        "| Evaluated edges | {} |\n",
        report.evaluated_edge_count
    ));
    out.push_str(&format!("| Allowed edges | {} |\n", report.allowed_edges));
    out.push_str(&format!("| Violations | {} |\n", report.violation_count));
    out.push_str(&format!(
        "| Public API violations | {} |\n",
        report.public_api_violation_count
    ));
    out.push_str(&format!(
        "| Unknown edges | {} |\n",
        report.unknown_edge_count
    ));
    if !report.violations.is_empty() {
        out.push_str("\n## Violations\n\n");
        for violation in &report.violations {
            out.push_str(&format!(
                "- `{}` `{}` -> `{}` via `{:?}` ({})\n",
                violation.rule_id,
                violation.source_path.display(),
                violation.target_path.display(),
                violation.edge_type,
                violation.severity
            ));
        }
    }
    if !report.exemptions.is_empty() {
        out.push_str("\n## Exemptions\n\n");
        for exemption in &report.exemptions {
            out.push_str(&format!(
                "- `{}` exempted by `{}` for `{}` -> `{}`\n",
                exemption.rule_id,
                exemption.exemption_id,
                exemption.source_path.display(),
                exemption.target_path.display()
            ));
        }
    }
    if !report.uncertainty.is_empty() {
        out.push_str("\n## Uncertainty\n\n");
        for note in &report.uncertainty {
            out.push_str(&format!("- {}\n", note));
        }
    }
    out
}

fn render_policy_explain_markdown(output: &ArchitecturePolicyExplainOutput) -> String {
    let mut out = String::new();
    out.push_str("# Architecture Policy Explanation\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    out.push_str(&format!("| Configured | `{}` |\n", output.configured));
    out.push_str(&format!("| Query kind | `{}` |\n", output.query_kind));
    out.push_str(&format!("| Query | `{}` |\n", output.query));
    out.push_str(&format!("| Components | `{}` |\n", output.components.len()));
    out.push_str(&format!("| Violations | `{}` |\n", output.violations.len()));
    out.push_str(&format!("| Exemptions | `{}` |\n", output.exemptions.len()));
    out.push('\n');
    out.push_str(&output.message);
    out.push('\n');
    if !output.components.is_empty() {
        out.push_str("\n## Components\n\n");
        for component in &output.components {
            out.push_str(&format!(
                "- `{}` via `{}`\n",
                component.component_id, component.matched_glob
            ));
        }
    }
    if !output.violations.is_empty() {
        out.push_str("\n## Violations\n\n");
        for violation in &output.violations {
            out.push_str(&format!(
                "- `{}` `{}` -> `{}` via `{:?}` ({})\n",
                violation.rule_id,
                violation.source_path.display(),
                violation.target_path.display(),
                violation.edge_type,
                violation.severity
            ));
        }
    }
    if !output.exemptions.is_empty() {
        out.push_str("\n## Exemptions\n\n");
        for exemption in &output.exemptions {
            out.push_str(&format!(
                "- `{}` exempted by `{}` for `{}` -> `{}`\n",
                exemption.rule_id,
                exemption.exemption_id,
                exemption.source_path.display(),
                exemption.target_path.display()
            ));
        }
    }
    if !output.uncertainty.is_empty() {
        out.push_str("\n## Uncertainty\n\n");
        for note in &output.uncertainty {
            out.push_str(&format!("- {}\n", note));
        }
    }
    out
}

fn architecture_policy_explain_output<S>(
    repo: &Path,
    store: &S,
    resolver: &PolicyResolver,
    policy: &ArchitecturePolicy,
    file: Option<PathBuf>,
    symbol: Option<String>,
) -> anyhow::Result<ArchitecturePolicyExplainOutput>
where
    S: MetadataStore + GraphStore,
{
    let (query_kind, query, file_path, symbol) = if let Some(symbol_query) = symbol {
        let symbol = SymbolEngine::new(store).definition(&symbol_query)?;
        let file_path = file_path_for_symbol(store, &symbol)?;
        ("symbol".into(), symbol_query, Some(file_path), Some(symbol))
    } else if let Some(path) = file {
        let path = repo_relative_path(repo, &path);
        ("file".into(), path.display().to_string(), Some(path), None)
    } else {
        ("repo".into(), repo.display().to_string(), None, None)
    };

    let mut uncertainty = Vec::new();
    let components = if let Some(file_path) = &file_path {
        match resolver.resolve_node(file_path, symbol.as_ref().map(|symbol| symbol.id.clone())) {
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
    let message = format!(
        "Architecture policy explanation for {query_kind} `{query}`: {} component match(es), {} violation(s), {} exemption(s).",
        components.len(),
        violations.len(),
        exemptions.len()
    );
    Ok(ArchitecturePolicyExplainOutput {
        configured: true,
        query_kind,
        query,
        file_path,
        symbol,
        components,
        violations,
        exemptions,
        uncertainty,
        message,
    })
}

fn file_path_for_symbol(store: &dyn MetadataStore, symbol: &Symbol) -> anyhow::Result<PathBuf> {
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

fn load_index_manifest(repo: &Path) -> anyhow::Result<Option<IndexManifest>> {
    let index_path = repo.join(".ok/index.sqlite");
    if !index_path.exists() {
        return Ok(None);
    }
    Ok(SqliteStore::open(&index_path)?.manifest()?)
}

fn render_status_markdown(
    repo: &Path,
    manifest: Option<&IndexManifest>,
    doctor: &DoctorReport,
) -> String {
    let mut out = String::new();
    out.push_str("# Open Kioku Status\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    out.push_str(&format!("| Repo | `{}` |\n", repo.display()));
    out.push_str(&format!("| Ready | `{}` |\n", doctor.ok));
    out.push_str("| Generated by | `ok status --markdown` |\n");

    out.push_str("\n## Index\n\n");
    if let Some(manifest) = manifest {
        out.push_str("| Metric | Value |\n| --- | ---: |\n");
        out.push_str(&format!("| Mode | `{}` |\n", manifest.index_mode));
        out.push_str(&format!("| Files | {} |\n", manifest.file_count));
        out.push_str(&format!("| Symbols | {} |\n", manifest.symbol_count));
        out.push_str(&format!("| Chunks | {} |\n", manifest.chunk_count));
        out.push_str(&format!(
            "| Skipped paths | {} |\n",
            manifest.quality.skipped_paths.len()
        ));
        out.push_str(&format!("| Tests | {} |\n", manifest.quality.test_count));
        out.push_str(&format!(
            "| Imports | {} |\n",
            manifest.quality.import_count
        ));
        out.push_str(&format!(
            "| SCIP indexes imported | {} |\n",
            manifest.quality.scip_indexes_imported
        ));
        out.push_str(&format!(
            "| SCIP exact references | {} |\n",
            manifest.quality.scip_exact_references
        ));
        out.push_str(&format!(
            "| Static analysis facts | {} |\n",
            manifest.quality.static_analysis_facts
        ));
        if manifest.quality.runtime_analysis_facts > 0 {
            out.push_str(&format!(
                "| Runtime analysis facts | {} |\n",
                manifest.quality.runtime_analysis_facts
            ));
        }
        if manifest.quality.git_history_facts > 0 {
            out.push_str(&format!(
                "| Git history facts | {} |\n",
                manifest.quality.git_history_facts
            ));
        }
        if manifest.quality.codeql_databases > 0 {
            out.push_str(&format!(
                "| CodeQL databases | {} |\n",
                manifest.quality.codeql_databases
            ));
        }
        if manifest.quality.coverage_reports > 0 {
            out.push_str(&format!(
                "| Coverage reports | {} |\n",
                manifest.quality.coverage_reports
            ));
        }
        if manifest.quality.junit_reports > 0 {
            out.push_str(&format!(
                "| JUnit reports | {} |\n",
                manifest.quality.junit_reports
            ));
        }
        out.push_str(&format!("\nIndexed at `{}`.\n", manifest.indexed_at));
        if !manifest.quality.build_systems.is_empty() {
            out.push_str(&format!(
                "\nBuild systems: `{}`.\n",
                manifest.quality.build_systems.join(", ")
            ));
        }
        if !manifest.quality.semantic_provider_notes.is_empty() {
            out.push_str("\nLocal signal notes:\n");
            for note in &manifest.quality.semantic_provider_notes {
                out.push_str(&format!("- {}\n", note));
            }
        }
        if !manifest.quality.quality_notes.is_empty() {
            out.push_str("\nQuality notes:\n");
            for note in &manifest.quality.quality_notes {
                out.push_str(&format!("- {}\n", note));
            }
        }
    } else {
        out.push_str(
            "No index manifest was found. Run `ok index .` before handing this repo to an agent.\n",
        );
    }

    out.push_str("\n## Readiness Checks\n\n");
    out.push_str("| Status | Check | Evidence |\n| --- | --- | --- |\n");
    for check in &doctor.checks {
        out.push_str(&format!(
            "| `{}` | `{}` | {} |\n",
            check.status.label(),
            check.name,
            markdown_cell(&check.message)
        ));
    }

    out.push_str("\n## Next Steps\n\n");
    if doctor.next_steps.is_empty() {
        out.push_str("- No required next steps.\n");
    } else {
        for step in &doctor.next_steps {
            out.push_str(&format!("- {step}\n"));
        }
    }

    out.push_str("\n## Handoff Commands\n\n");
    out.push_str(&format!(
        "- `ok setup audit --repo {}`\n",
        shell_quote(&repo.display().to_string())
    ));
    out.push_str(&format!(
        "- `ok prove {}`\n",
        shell_quote(&repo.display().to_string())
    ));
    out
}

fn setup_audit_report(repo: &Path) -> SetupAuditReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let doctor = doctor_report(&repo);
    let manifest = load_index_manifest(&repo).ok().flatten();
    let config = OkConfig::load_from_repo(&repo).ok();
    let providers = quality_provider_report(&repo, manifest.as_ref());
    let advanced_providers = advanced_quality_provider_report(&repo, manifest.as_ref());
    let mut checks = Vec::new();
    let mut next_steps = doctor.next_steps.clone();

    if let Some(manifest) = &manifest {
        checks.push(SetupAuditCheck {
            name: "index".into(),
            status: CheckStatus::Pass,
            message: format!(
                "{} files, {} symbols, {} chunks",
                manifest.file_count, manifest.symbol_count, manifest.chunk_count
            ),
        });
        if manifest.quality.scip_exact_references > 0 {
            checks.push(SetupAuditCheck {
                name: "scip".into(),
                status: CheckStatus::Pass,
                message: format!(
                    "{} exact references imported",
                    manifest.quality.scip_exact_references
                ),
            });
        } else {
            checks.push(SetupAuditCheck {
                name: "scip".into(),
                status: CheckStatus::Warn,
                message:
                    "SCIP exact references are unavailable; impact and plan quality are reduced"
                        .into(),
            });
        }
    } else {
        checks.push(SetupAuditCheck {
            name: "index".into(),
            status: CheckStatus::Fail,
            message: "missing .ok/index.sqlite manifest".into(),
        });
        next_steps.push("Run `ok index .` before relying on Open Kioku in an agent.".into());
    }

    match config {
        Some(config) => {
            if !config.security.allow_write
                && config.security.deny_network
                && config.security.approval_required
            {
                checks.push(SetupAuditCheck {
                    name: "security".into(),
                    status: CheckStatus::Pass,
                    message: "read-only source access, network denied, approvals required".into(),
                });
            } else {
                checks.push(SetupAuditCheck {
                    name: "security".into(),
                    status: CheckStatus::Warn,
                    message: format!(
                        "allow_write={}, deny_network={}, approval_required={}",
                        config.security.allow_write,
                        config.security.deny_network,
                        config.security.approval_required
                    ),
                });
                next_steps.push(
                    "Review ok.toml before exposing write, command, or network access to agents."
                        .into(),
                );
            }
        }
        None => {
            checks.push(SetupAuditCheck {
                name: "config".into(),
                status: CheckStatus::Warn,
                message: "ok.toml is missing or invalid; defaults will be used".into(),
            });
            next_steps.push("Run `ok init .` to create an explicit ok.toml.".into());
        }
    }

    let mcp_check = doctor.checks.iter().find(|check| check.name == "mcp");
    checks.push(SetupAuditCheck {
        name: "mcp".into(),
        status: mcp_check
            .map(|check| check.status)
            .unwrap_or(CheckStatus::Warn),
        message: mcp_check
            .map(|check| check.message.clone())
            .unwrap_or_else(|| "MCP server check was not available".into()),
    });

    let plugin_surfaces = plugin_surfaces(&repo);

    next_steps.sort();
    next_steps.dedup();
    let ok = checks
        .iter()
        .all(|check| !matches!(check.status, CheckStatus::Fail));
    SetupAuditReport {
        ok,
        repo: repo.clone(),
        generated_by: "ok setup audit",
        checks,
        providers,
        advanced_providers,
        clients: all_mcp_clients()
            .into_iter()
            .map(|client| ClientInstallReport {
                client: client.as_str(),
                config_format: client.config_format(),
                install_command: format!(
                    "ok mcp install {} --repo {}",
                    client.as_str(),
                    shell_quote(&repo.display().to_string())
                ),
                verify: client_verify_command(client),
                note: client_install_note(client),
            })
            .collect(),
        plugin_surfaces,
        next_steps,
    }
}

fn print_setup_audit_report(report: &SetupAuditReport) {
    println!("Open Kioku setup audit for {}", report.repo.display());
    for check in &report.checks {
        println!(
            "{:<6} {:<12} {}",
            check.status.marker(),
            check.name,
            check.message
        );
    }
    println!("\nMCP clients:");
    for client in &report.clients {
        println!(
            "- {:<8} {:<5} {}",
            client.client, client.config_format, client.install_command
        );
    }
    println!("\nQuality signals:");
    for provider in &report.providers {
        println!(
            "{:<6} {:<12} {}",
            provider.status.marker(),
            provider.name,
            provider.evidence
        );
    }
    println!("\nAdvanced providers (optional):");
    if report.advanced_providers.is_empty() {
        println!("- none detected; not required for default SCIP/indexed-facts workflow");
    } else {
        for provider in &report.advanced_providers {
            println!(
                "{:<6} {:<12} {}",
                provider.status.marker(),
                provider.name,
                provider.evidence
            );
        }
    }
    println!("\nSource checkout surfaces (optional):");
    for surface in &report.plugin_surfaces {
        let status = if surface.present {
            "present"
        } else {
            "missing"
        };
        println!(
            "- {:<14} {:<7} {}",
            surface.name,
            status,
            surface.path.display()
        );
    }
    if !report.next_steps.is_empty() {
        println!("\nNext steps:");
        for step in &report.next_steps {
            println!("- {step}");
        }
    }
}

fn render_setup_audit_markdown(report: &SetupAuditReport) -> String {
    let mut out = String::new();
    out.push_str("# Open Kioku Setup Audit\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    out.push_str(&format!("| Repo | `{}` |\n", report.repo.display()));
    out.push_str(&format!("| Ready | `{}` |\n", report.ok));
    out.push_str(&format!("| Generated by | `{}` |\n", report.generated_by));

    out.push_str("\n## Checks\n\n");
    out.push_str("| Status | Check | Evidence |\n| --- | --- | --- |\n");
    for check in &report.checks {
        out.push_str(&format!(
            "| `{}` | `{}` | {} |\n",
            check.status.label(),
            check.name,
            markdown_cell(&check.message)
        ));
    }

    out.push_str("\n## MCP Client Matrix\n\n");
    out.push_str("| Client | Config | Install command | Verify |\n| --- | --- | --- | --- |\n");
    for client in &report.clients {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | `{}` |\n",
            client.client, client.config_format, client.install_command, client.verify
        ));
    }

    out.push_str("\n## Quality Signals\n\n");
    out.push_str("| Status | Signal | Evidence | Next step |\n| --- | --- | --- | --- |\n");
    for provider in &report.providers {
        out.push_str(&format!(
            "| `{}` | `{}` | {} | {} |\n",
            provider.status.label(),
            provider.name,
            markdown_cell(&provider.evidence),
            markdown_cell(provider.next_step.as_deref().unwrap_or("None"))
        ));
    }

    out.push_str("\n## Advanced Providers\n\n");
    out.push_str("Optional only. Missing entries do not reduce default readiness; Open Kioku's primary precision path is local indexed facts plus SCIP when available.\n\n");
    if report.advanced_providers.is_empty() {
        out.push_str("No advanced provider artifacts detected.\n");
    } else {
        out.push_str("| Status | Provider | Evidence | Next step |\n| --- | --- | --- | --- |\n");
        for provider in &report.advanced_providers {
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {} |\n",
                provider.status.label(),
                provider.name,
                markdown_cell(&provider.evidence),
                markdown_cell(provider.next_step.as_deref().unwrap_or("None"))
            ));
        }
    }

    out.push_str("\n## Source Checkout Surfaces\n\n");
    out.push_str("These are expected only when auditing the Open Kioku source checkout, not every indexed target repository.\n\n");
    out.push_str("| Surface | Status | Path | Note |\n| --- | --- | --- | --- |\n");
    for surface in &report.plugin_surfaces {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | {} |\n",
            surface.name,
            if surface.present {
                "present"
            } else {
                "missing"
            },
            surface.path.display(),
            surface.note
        ));
    }

    out.push_str("\n## Next Steps\n\n");
    if report.next_steps.is_empty() {
        out.push_str("- No required next steps.\n");
    } else {
        for step in &report.next_steps {
            out.push_str(&format!("- {step}\n"));
        }
    }
    out
}

fn markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn all_mcp_clients() -> [McpClient; 8] {
    [
        McpClient::Claude,
        McpClient::Cursor,
        McpClient::Codex,
        McpClient::Gemini,
        McpClient::Opencode,
        McpClient::Zed,
        McpClient::Windsurf,
        McpClient::Trae,
    ]
}

fn client_verify_command(client: McpClient) -> String {
    match client {
        McpClient::Claude => "restart Claude, then inspect MCP server logs".into(),
        McpClient::Cursor => "open Cursor MCP settings and confirm open-kioku is enabled".into(),
        McpClient::Codex => "run /mcp in Codex and confirm open-kioku is listed".into(),
        McpClient::Gemini => "run gemini /mcp and confirm open-kioku is connected".into(),
        McpClient::Opencode => "run opencode and ask it to use the open-kioku MCP tools".into(),
        McpClient::Zed => "open Agent Panel settings and confirm the server is active".into(),
        McpClient::Windsurf => {
            "open Windsurf, click Cascade MCPs icon, and confirm open-kioku is connected".into()
        }
        McpClient::Trae => "open Trae Settings -> MCP, and confirm open-kioku is active".into(),
    }
}

fn client_install_note(client: McpClient) -> &'static str {
    match client {
        McpClient::Claude => "Claude-style mcpServers JSON.",
        McpClient::Cursor => "Cursor MCP JSON entry.",
        McpClient::Codex => "Codex config.toml mcp_servers entry.",
        McpClient::Gemini => "Gemini CLI settings.json mcpServers entry.",
        McpClient::Opencode => "OpenCode opencode.json mcp local server entry.",
        McpClient::Zed => "Zed settings.json context_servers entry.",
        McpClient::Windsurf => "Windsurf mcp_config.json entry.",
        McpClient::Trae => "Trae mcp.json entry.",
    }
}

fn plugin_surfaces(repo: &Path) -> Vec<PluginSurfaceReport> {
    vec![
        PluginSurfaceReport {
            name: "cursor-plugin",
            path: repo.join(".cursor-plugin/plugin.json"),
            present: repo.join(".cursor-plugin/plugin.json").exists(),
            note: "Cursor plugin manifest.",
        },
        PluginSurfaceReport {
            name: "claude-plugin",
            path: repo.join(".claude-plugin/plugin.json"),
            present: repo.join(".claude-plugin/plugin.json").exists(),
            note: "Claude plugin manifest.",
        },
        PluginSurfaceReport {
            name: "codex-plugin",
            path: repo.join(".codex-plugin/plugin.json"),
            present: repo.join(".codex-plugin/plugin.json").exists(),
            note: "Codex plugin manifest.",
        },
        PluginSurfaceReport {
            name: "github-workflows",
            path: repo.join(".github/workflows"),
            present: repo.join(".github/workflows").is_dir(),
            note: "CI and release automation.",
        },
    ]
}

fn quality_provider_report(
    repo: &Path,
    manifest: Option<&IndexManifest>,
) -> Vec<QualityProviderReport> {
    let build_systems = detect_build_systems(repo);
    let test_count = manifest
        .map(|manifest| manifest.quality.test_count)
        .unwrap_or(0);
    let import_count = manifest
        .map(|manifest| manifest.quality.import_count)
        .unwrap_or(0);
    let static_analysis_facts = manifest
        .map(|manifest| manifest.quality.static_analysis_facts)
        .unwrap_or(0);
    let runtime_analysis_facts = manifest
        .map(|manifest| manifest.quality.runtime_analysis_facts)
        .unwrap_or(0);
    let git_history_facts = manifest
        .map(|manifest| manifest.quality.git_history_facts)
        .unwrap_or(0);
    let mut providers = Vec::new();

    providers.push(QualityProviderReport {
        name: "build",
        status: if build_systems.is_empty() {
            CheckStatus::Warn
        } else {
            CheckStatus::Pass
        },
        evidence: if build_systems.is_empty() {
            "no build system files detected".into()
        } else {
            format!("detected {}", build_systems.join(", "))
        },
        next_step: if build_systems.is_empty() {
            Some("Run from the repository root or add ok.toml with the intended root.".into())
        } else {
            None
        },
    });
    providers.push(QualityProviderReport {
        name: "tests",
        status: if test_count > 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence: format!("{test_count} indexed test target(s)"),
        next_step: if test_count == 0 {
            Some("Index test files before relying on validation recommendations.".into())
        } else {
            None
        },
    });
    providers.push(QualityProviderReport {
        name: "imports",
        status: if import_count > 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence: format!("{import_count} indexed import edge(s)"),
        next_step: if import_count == 0 {
            Some("Index source files with imports to improve local dependency evidence.".into())
        } else {
            None
        },
    });
    providers.push(QualityProviderReport {
        name: "static",
        status: if manifest.is_some() {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence: format!("{static_analysis_facts} language-specific static analysis fact(s)"),
        next_step: if manifest.is_none() {
            Some("Run `ok index .` to collect language-specific static analysis facts.".into())
        } else {
            None
        },
    });
    if runtime_analysis_facts > 0 {
        providers.push(QualityProviderReport {
            name: "runtime",
            status: CheckStatus::Pass,
            evidence: format!(
                "{runtime_analysis_facts} runtime analysis fact(s) from local artifacts"
            ),
            next_step: None,
        });
    }
    providers.push(QualityProviderReport {
        name: "git-history",
        status: if git_history_facts > 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence: format!("{git_history_facts} git co-change fact(s) from local history"),
        next_step: if git_history_facts == 0 {
            Some("Keep repository history available and enable `[history].enabled = true`, then rerun `ok index .`.".into())
        } else {
            None
        },
    });
    providers.push(QualityProviderReport {
        name: "validation",
        status: if test_count > 0 {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        evidence: if build_systems.iter().any(|system| system == "gradle") && test_count > 0 {
            "Gradle-scoped validation commands enabled for indexed Java test paths".into()
        } else if test_count > 0 {
            "indexed validation candidates available".into()
        } else {
            "no indexed validation candidates".into()
        },
        next_step: if test_count == 0 {
            Some("Add or index tests so plans can return concrete validation commands.".into())
        } else {
            None
        },
    });
    providers
}

fn advanced_quality_provider_report(
    repo: &Path,
    manifest: Option<&IndexManifest>,
) -> Vec<QualityProviderReport> {
    let codeql_dbs = detect_codeql_databases(repo);
    let bsp_descriptors = count_named_artifacts(&[repo.join(".bsp")], &[".json"], 2);
    let coverage_reports = manifest
        .map(|manifest| manifest.quality.coverage_reports)
        .unwrap_or_else(|| {
            count_named_artifacts(&analysis_roots(repo), &["jacoco.xml", "coverage.xml"], 5)
        });
    let junit_reports = manifest
        .map(|manifest| manifest.quality.junit_reports)
        .unwrap_or_else(|| count_named_artifacts(&analysis_roots(repo), &["test-", "junit"], 5));
    let lsp = relevant_lsp_servers(repo);
    let mut providers = Vec::new();

    if bsp_descriptors > 0 {
        providers.push(QualityProviderReport {
            name: "bsp",
            status: CheckStatus::Pass,
            evidence: format!("{bsp_descriptors} BSP descriptor(s) under .bsp"),
            next_step: None,
        });
    }
    if codeql_dbs > 0 {
        providers.push(QualityProviderReport {
            name: "codeql",
            status: CheckStatus::Pass,
            evidence: format!("{codeql_dbs} local CodeQL database artifact(s) detected"),
            next_step: None,
        });
    }
    if !lsp.present.is_empty() && lsp.missing.is_empty() {
        providers.push(QualityProviderReport {
            name: "lsp",
            status: CheckStatus::Pass,
            evidence: format!(
                "detected matching language servers: {}",
                lsp.present.join(", ")
            ),
            next_step: None,
        });
    }
    if coverage_reports > 0 {
        providers.push(QualityProviderReport {
            name: "coverage",
            status: CheckStatus::Pass,
            evidence: format!("{coverage_reports} coverage report artifact(s) detected"),
            next_step: None,
        });
    }
    if junit_reports > 0 {
        providers.push(QualityProviderReport {
            name: "junit",
            status: CheckStatus::Pass,
            evidence: format!("{junit_reports} JUnit-style report artifact(s) detected"),
            next_step: None,
        });
    }
    providers
}

fn detect_build_systems(repo: &Path) -> Vec<String> {
    let mut systems = Vec::new();
    for (name, paths) in [
        (
            "gradle",
            &[
                "settings.gradle",
                "settings.gradle.kts",
                "build.gradle",
                "build.gradle.kts",
            ][..],
        ),
        ("maven", &["pom.xml"][..]),
        (
            "bazel",
            &["WORKSPACE", "WORKSPACE.bazel", "MODULE.bazel"][..],
        ),
        ("cargo", &["Cargo.toml"][..]),
        ("npm", &["package.json"][..]),
        ("go", &["go.mod"][..]),
    ] {
        if paths.iter().any(|path| repo.join(path).exists()) {
            systems.push(name.to_string());
        }
    }
    systems
}

fn detect_codeql_databases(repo: &Path) -> usize {
    [
        ".ok/codeql",
        "codeql-db",
        "codeql-database",
        ".codeql/database",
    ]
    .iter()
    .filter(|path| {
        let path = repo.join(path);
        path.is_dir()
            && (path.join("db-java").exists()
                || path.join("codeql-database.yml").exists()
                || path.join("log").exists())
    })
    .count()
}

fn analysis_roots(repo: &Path) -> Vec<PathBuf> {
    vec![
        repo.join(".ok/analysis"),
        repo.join("build/reports"),
        repo.join("target/site"),
        repo.join("coverage"),
    ]
}

fn count_named_artifacts(roots: &[PathBuf], needles: &[&str], max_depth: usize) -> usize {
    let mut count = 0;
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(root)
            .max_depth(max_depth)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path().to_string_lossy().to_ascii_lowercase();
            let file_name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if needles
                .iter()
                .any(|needle| file_name.contains(needle) || path.ends_with(needle))
            {
                count += 1;
            }
        }
    }
    count
}

struct LspProviderInventory {
    present: Vec<String>,
    missing: Vec<String>,
}

fn relevant_lsp_servers(repo: &Path) -> LspProviderInventory {
    let languages = sample_lsp_languages(repo);
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for language in &languages {
        let candidates: &[&str] = match language.as_str() {
            "java" => &["jdtls", "java-language-server"],
            "rust" => &["rust-analyzer"],
            "go" => &["gopls"],
            "python" => &["pyright-langserver", "pylsp"],
            "typescript" | "javascript" => &["typescript-language-server"],
            _ => &[],
        };
        if candidates.is_empty() {
            continue;
        }
        if let Some(found) = candidates.iter().find(|binary| command_exists(binary)) {
            present.push(format!("{language}:{found}"));
        } else {
            missing.push(format!("{} ({})", language, candidates.join(" or ")));
        }
    }
    present.sort();
    present.dedup();
    missing.sort();
    missing.dedup();
    LspProviderInventory { present, missing }
}

fn sample_lsp_languages(repo: &Path) -> Vec<String> {
    let mut languages = Vec::new();
    for entry in walkdir::WalkDir::new(repo)
        .max_depth(6)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            name != ".git"
                && name != ".ok"
                && name != "build"
                && name != "target"
                && name != "node_modules"
        })
        .filter_map(|entry| entry.ok())
        .take(5000)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let Some(ext) = entry.path().extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        let language = match ext {
            "java" => "java",
            "rs" => "rust",
            "go" => "go",
            "py" => "python",
            "ts" | "tsx" => "typescript",
            "js" | "jsx" => "javascript",
            _ => continue,
        };
        if !languages.iter().any(|existing| existing == language) {
            languages.push(language.to_string());
        }
    }
    languages.sort();
    languages
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

    if index_path.exists() {
        if let Ok(store) = SqliteStore::open(&index_path) {
            if let Ok(Some(manifest)) = store.manifest() {
                let quality = &manifest.quality;
                if quality.scip_indexes_imported > 0 && quality.scip_exact_references > 0 {
                    checks.push(DoctorCheck {
                        name: "quality",
                        status: CheckStatus::Pass,
                        message: format!(
                            "SCIP imported {} index(es), {} exact references, {} tests",
                            quality.scip_indexes_imported,
                            quality.scip_exact_references,
                            quality.test_count
                        ),
                    });
                } else {
                    checks.push(DoctorCheck {
                        name: "quality",
                        status: CheckStatus::Warn,
                        message: format!(
                            "SCIP exact references unavailable; {} tests, {} imports indexed",
                            quality.test_count, quality.import_count
                        ),
                    });
                    next_steps.push(
                    "For better references, impact, tests, and planning: run `ok scip setup .`, then `ok index . --with-scip auto`.".into(),
                );
                }
            }
        }
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

fn scip_setup_report(repo: &Path, config: &OkConfig) -> ScipSetupReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let ts_args = if repo.join("tsconfig.json").exists() || repo.join("jsconfig.json").exists() {
        "scip-typescript index --output .ok/indexes/typescript.scip"
    } else {
        "scip-typescript index --output .ok/indexes/typescript.scip --infer-tsconfig"
    };
    let indexers = vec![
        ScipIndexerReport {
            language: "typescript/javascript",
            applicable: repo.join("package.json").exists(),
            installed: command_exists("scip-typescript"),
            command: ts_args.into(),
            output_path: ".ok/indexes/typescript.scip".into(),
            note: "best for TypeScript and JavaScript symbol references".into(),
        },
        ScipIndexerReport {
            language: "go",
            applicable: repo.join("go.mod").exists(),
            installed: command_exists("scip-go"),
            command: "scip-go".into(),
            output_path: "index.scip".into(),
            note: "best for Go definition/reference precision".into(),
        },
        ScipIndexerReport {
            language: "java",
            applicable: repo.join("pom.xml").exists()
                || repo.join("build.gradle").exists()
                || repo.join("build.gradle.kts").exists(),
            installed: command_exists("scip-java"),
            command: "scip-java index --output .ok/indexes/java.scip".into(),
            output_path: ".ok/indexes/java.scip".into(),
            note: "may run build-tool analysis and can take time".into(),
        },
        ScipIndexerReport {
            language: "python",
            applicable: repo.join("pyproject.toml").exists() || repo.join("setup.py").exists(),
            installed: command_exists("scip-python"),
            command: "scip-python index . --project-name <repo> --project-version _ --output .ok/indexes/python.scip".into(),
            output_path: ".ok/indexes/python.scip".into(),
            note: "requires project metadata for best external reference stability".into(),
        },
    ];
    ScipSetupReport {
        repo,
        mode: format!("{:?}", config.scip.mode).to_ascii_lowercase(),
        enabled: config.scip.enabled,
        allow_install: config.scip.allow_install,
        timeout_seconds: config.scip.timeout_seconds,
        indexers,
        configured_paths: config.scip.paths.clone(),
    }
}

fn print_scip_setup_report(report: &ScipSetupReport) {
    println!("SCIP setup for {}", report.repo.display());
    println!(
        "mode={}, enabled={}, timeout={}s",
        report.mode, report.enabled, report.timeout_seconds
    );
    println!("\nConfigured SCIP paths:");
    for path in &report.configured_paths {
        println!("- {}", path.display());
    }
    println!("\nIndexers:");
    for indexer in &report.indexers {
        let applicability = if indexer.applicable {
            "applicable"
        } else {
            "not detected"
        };
        let installed = if indexer.installed {
            "installed"
        } else {
            "missing"
        };
        println!(
            "- {}: {}, {}; {}",
            indexer.language, applicability, installed, indexer.note
        );
        if indexer.applicable {
            println!("  {}", indexer.command);
        }
    }
}

fn command_exists(binary: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(binary))
                .any(|path| path.is_file())
        })
        .unwrap_or(false)
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
            format!("ok --repo {repo_display} context token --format markdown"),
            format!("ok --repo {repo_display} plan token --format markdown"),
            format!("ok prove {repo_display} --task token"),
            format!("ok mcp install claude --repo {repo_display}"),
        ],
    })
}

fn run_bench(args: BenchArgs) -> anyhow::Result<BenchReport> {
    let path = args.path;
    let quality_cases = parse_quality_cases(&args.quality_cases)?;

    let start = Instant::now();
    let snapshot = index_repo(&path)?;
    let index_duration = start.elapsed();

    let index = TantivySearchIndex::open_or_create(default_index_dir(&path))?;
    let bm25_median = median_duration(time_searches(10, || index.search("fn", 10).map(|_| ()))?);

    let store = open_store(&path)?;
    let files = store.list_files(usize::MAX, 0)?;
    let chunks = store.all_chunks()?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    let regex_median = median_duration(time_searches(10, || {
        search_chunks(&chunks, &files, &symbols, "fn", 10).map(|_| ())
    })?);

    let quality = if quality_cases.is_empty() {
        None
    } else {
        Some(evaluate_quality_cases(
            &index,
            &quality_cases,
            args.quality_limit,
        )?)
    };
    let manifest = snapshot.manifest;
    let elapsed_seconds = index_duration.as_secs_f64();

    Ok(BenchReport {
        repo: path,
        index: IndexBenchReport {
            file_count: manifest.file_count,
            symbol_count: manifest.symbol_count,
            chunk_count: manifest.chunk_count,
            elapsed_ms: duration_ms(index_duration),
            files_per_second: if elapsed_seconds > 0.0 {
                manifest.file_count as f64 / elapsed_seconds
            } else {
                0.0
            },
        },
        search: SearchBenchReport {
            bm25_median_ms: duration_ms(bm25_median),
            regex_median_ms: duration_ms(regex_median),
        },
        quality,
    })
}

fn parse_quality_cases(values: &[String]) -> anyhow::Result<Vec<QualityCase>> {
    values
        .iter()
        .map(|value| {
            let (query, expected_path) = value.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("quality case must use QUERY=EXPECTED_PATH_SUBSTRING: {value}")
            })?;
            let query = query.trim();
            let expected_path = expected_path.trim();
            if query.is_empty() || expected_path.is_empty() {
                anyhow::bail!("quality case query and expected path must be non-empty: {value}");
            }
            Ok(QualityCase {
                query: query.to_string(),
                expected_path: expected_path.to_string(),
            })
        })
        .collect()
}

fn evaluate_quality_cases(
    index: &TantivySearchIndex,
    cases: &[QualityCase],
    limit: usize,
) -> anyhow::Result<QualityBenchReport> {
    let limit = limit.max(1);
    let mut reports = Vec::with_capacity(cases.len());
    let mut top_hits = 0usize;
    let mut any_hits = 0usize;
    let mut reciprocal_rank = 0.0;

    for case in cases {
        let results = index.search(&case.query, limit)?;
        let expected = normalize_path_fragment(&case.expected_path);
        let rank = results.iter().position(|result| {
            normalize_path_fragment(&result.path.to_string_lossy()).contains(&expected)
        });
        let rank = rank.map(|value| value + 1);
        if rank == Some(1) {
            top_hits += 1;
        }
        if let Some(rank) = rank {
            any_hits += 1;
            reciprocal_rank += 1.0 / rank as f64;
        }
        reports.push(QualityCaseReport {
            query: case.query.clone(),
            expected_path: case.expected_path.clone(),
            rank,
            top_path: results.first().map(|result| result.path.clone()),
            matched_path: rank
                .and_then(|rank| results.get(rank - 1).map(|result| result.path.clone())),
            result_count: results.len(),
        });
    }

    let total = cases.len() as f64;
    Ok(QualityBenchReport {
        case_count: cases.len(),
        precision_at_1: top_hits as f64 / total,
        hit_rate_at_k: any_hits as f64 / total,
        mean_reciprocal_rank: reciprocal_rank / total,
        limit,
        cases: reports,
    })
}

fn print_bench_report(report: &BenchReport) {
    println!(
        "Indexed {} files, {} symbols, and {} chunks in {:.2}ms",
        report.index.file_count,
        report.index.symbol_count,
        report.index.chunk_count,
        report.index.elapsed_ms
    );
    println!("{:.2} files/sec", report.index.files_per_second);
    println!("BM25 search: {:.2}ms median", report.search.bm25_median_ms);
    println!(
        "Regex search: {:.2}ms median",
        report.search.regex_median_ms
    );

    if let Some(quality) = &report.quality {
        println!(
            "Quality: precision@1 {:.3}, hit-rate@{} {:.3}, MRR {:.3}",
            quality.precision_at_1,
            quality.limit,
            quality.hit_rate_at_k,
            quality.mean_reciprocal_rank
        );
        for case in &quality.cases {
            let status = match case.rank {
                Some(1) => "pass",
                Some(_) => "hit",
                None => "miss",
            };
            let rank = case
                .rank
                .map(|rank| rank.to_string())
                .unwrap_or_else(|| "-".to_string());
            let top_path = case
                .top_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "-".to_string());
            println!(
                "  {status}: query {:?}, expected {:?}, rank {}, top {}",
                case.query, case.expected_path, rank, top_path
            );
        }
    }
}

fn run_architecture_policy_bench(
    args: ArchitectureBenchArgs,
) -> anyhow::Result<ArchitecturePolicyBenchReport> {
    let repo = absolutize(&args.path)?;
    let cases_file = absolutize(&args.cases_file)?;
    let cases = load_architecture_policy_bench_cases(&cases_file)?;
    if cases.is_empty() {
        anyhow::bail!(
            "architecture policy benchmark cases file is empty: {}",
            cases_file.display()
        );
    }
    if !args.no_index {
        index_repo(&repo)?;
    }
    let Some(policy) = load_architecture_policy(&repo)? else {
        anyhow::bail!(
            "architecture policy benchmark requires a configured policy in {}",
            repo.display()
        );
    };
    let store = open_store(&repo)?;
    let resolver = PolicyResolver::new(&policy)?;
    let iterations = args.iterations.max(1);
    let mut durations = Vec::with_capacity(iterations);
    let mut report = None;
    for _ in 0..iterations {
        let started = Instant::now();
        let check = evaluate_policy(&store, &resolver, &policy)?;
        durations.push(started.elapsed());
        report = Some(check);
    }
    let report = report.expect("at least one architecture policy benchmark iteration");
    let actual_findings = architecture_policy_actual_findings(&policy, &report);
    let (summary, families, case_reports) =
        score_architecture_policy_cases(&policy, &cases, &actual_findings);

    Ok(ArchitecturePolicyBenchReport {
        repo,
        cases_file,
        case_count: cases.len(),
        iterations,
        p95_policy_check_ms: percentile_duration_ms(&durations, 0.95),
        summary,
        rule_families: families,
        cases: case_reports,
    })
}

fn load_architecture_policy_bench_cases(
    path: &Path,
) -> anyhow::Result<Vec<ArchitecturePolicyBenchCase>> {
    let raw = fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read architecture policy cases {}",
            path.display()
        )
    })?;
    let cases: Vec<ArchitecturePolicyBenchCase> =
        serde_json::from_str(&raw).with_context(|| {
            format!(
                "failed to parse architecture policy cases {}",
                path.display()
            )
        })?;
    let mut seen = BTreeMap::new();
    for case in &cases {
        if case.id.trim().is_empty() {
            anyhow::bail!("architecture policy benchmark case id must be non-empty");
        }
        if let Some(previous) = seen.insert(case.id.clone(), true) {
            if previous {
                anyhow::bail!(
                    "duplicate architecture policy benchmark case id `{}`",
                    case.id
                );
            }
        }
        if matches!(
            case.expected,
            ArchitecturePolicyBenchOutcome::Violation | ArchitecturePolicyBenchOutcome::Exempted
        ) && case.rule_id.as_deref().unwrap_or_default().is_empty()
        {
            anyhow::bail!(
                "architecture policy benchmark case `{}` requires rule_id for {:?}",
                case.id,
                case.expected
            );
        }
    }
    Ok(cases)
}

fn architecture_policy_actual_findings(
    policy: &ArchitecturePolicy,
    report: &open_kioku_core::PolicyCheckReport,
) -> Vec<ArchitecturePolicyActualFinding> {
    let mut findings = Vec::new();
    for violation in &report.violations {
        findings.push(ArchitecturePolicyActualFinding {
            rule_family: architecture_policy_rule_family(policy, &violation.rule_id),
            outcome: ArchitecturePolicyBenchOutcome::Violation,
            rule_id: Some(violation.rule_id.clone()),
            source_path: violation.source_path.clone(),
            target_path: violation.target_path.clone(),
            edge_type: violation.edge_type,
        });
    }
    for exemption in &report.exemptions {
        findings.push(ArchitecturePolicyActualFinding {
            rule_family: architecture_policy_rule_family(policy, &exemption.rule_id),
            outcome: ArchitecturePolicyBenchOutcome::Exempted,
            rule_id: Some(exemption.rule_id.clone()),
            source_path: exemption.source_path.clone(),
            target_path: exemption.target_path.clone(),
            edge_type: exemption.evidence.edge_type,
        });
    }
    for unknown in &report.unknown_edges {
        findings.push(ArchitecturePolicyActualFinding {
            rule_family: ArchitecturePolicyRuleFamily::Unknown,
            outcome: ArchitecturePolicyBenchOutcome::Unknown,
            rule_id: None,
            source_path: unknown.evidence.source_path.clone(),
            target_path: unknown.evidence.target_path.clone(),
            edge_type: unknown.evidence.edge_type,
        });
    }
    findings.sort_by(|left, right| {
        left.rule_family
            .cmp(&right.rule_family)
            .then_with(|| left.rule_id.cmp(&right.rule_id))
            .then_with(|| left.source_path.cmp(&right.source_path))
            .then_with(|| left.target_path.cmp(&right.target_path))
            .then_with(|| left.edge_type.cmp(&right.edge_type))
            .then_with(|| format!("{:?}", left.outcome).cmp(&format!("{:?}", right.outcome)))
    });
    findings.dedup_by(|left, right| {
        left.rule_family == right.rule_family
            && left.outcome == right.outcome
            && left.rule_id == right.rule_id
            && same_architecture_bench_path(&left.source_path, &right.source_path)
            && same_architecture_bench_path(&left.target_path, &right.target_path)
            && left.edge_type == right.edge_type
    });
    findings
}

fn score_architecture_policy_cases(
    _policy: &ArchitecturePolicy,
    cases: &[ArchitecturePolicyBenchCase],
    actual_findings: &[ArchitecturePolicyActualFinding],
) -> (
    ArchitecturePolicyBenchSummary,
    Vec<ArchitecturePolicyBenchFamilyReport>,
    Vec<ArchitecturePolicyBenchCaseReport>,
) {
    let mut overall = ArchitecturePolicyBenchCounts::default();
    let mut families: BTreeMap<ArchitecturePolicyRuleFamily, ArchitecturePolicyBenchCounts> =
        BTreeMap::new();
    let mut matched_positive_cases = vec![false; cases.len()];
    let mut case_reports = Vec::with_capacity(cases.len());

    for (case_index, case) in cases.iter().enumerate() {
        let matching = actual_findings
            .iter()
            .filter(|finding| architecture_policy_case_selector_matches(case, finding))
            .collect::<Vec<_>>();
        let actual = matching
            .iter()
            .map(|finding| finding.outcome)
            .collect::<Vec<_>>();
        let matched = matching
            .iter()
            .any(|finding| architecture_policy_case_exact_match(case, finding));
        let passed = if case.expected == ArchitecturePolicyBenchOutcome::Allowed {
            matching.is_empty()
        } else {
            matched
        };
        if matched && case.expected != ArchitecturePolicyBenchOutcome::Allowed {
            matched_positive_cases[case_index] = true;
        }
        let mut notes = Vec::new();
        if !passed {
            if case.expected == ArchitecturePolicyBenchOutcome::Allowed {
                notes.push("expected no policy finding, but at least one finding matched".into());
            } else if matching.is_empty() {
                notes.push("expected policy finding was not reported".into());
            } else {
                notes.push(
                    "reported policy finding did not match expected outcome, family, or rule"
                        .into(),
                );
            }
        }
        case_reports.push(ArchitecturePolicyBenchCaseReport {
            id: case.id.clone(),
            rule_family: case.rule_family,
            expected: case.expected,
            actual,
            rule_id: case.rule_id.clone(),
            source_path: case.source_path.clone(),
            target_path: case.target_path.clone(),
            edge_type: case.edge_type,
            passed,
            notes,
        });
    }

    for case in cases {
        if case.expected != ArchitecturePolicyBenchOutcome::Allowed {
            overall.expected_positive_count += 1;
            families
                .entry(case.rule_family)
                .or_default()
                .expected_positive_count += 1;
        }
    }

    for finding in actual_findings {
        let Some((case_index, case)) = cases
            .iter()
            .enumerate()
            .find(|(_, case)| architecture_policy_case_selector_matches(case, finding))
        else {
            continue;
        };
        overall.actual_positive_count += 1;
        families
            .entry(finding.rule_family)
            .or_default()
            .actual_positive_count += 1;
        if architecture_policy_case_exact_match(case, finding) {
            overall.true_positives += 1;
            families
                .entry(finding.rule_family)
                .or_default()
                .true_positives += 1;
            matched_positive_cases[case_index] = true;
        } else {
            overall.false_positives += 1;
            families
                .entry(finding.rule_family)
                .or_default()
                .false_positives += 1;
        }
    }

    for (case_index, case) in cases.iter().enumerate() {
        if case.expected != ArchitecturePolicyBenchOutcome::Allowed
            && !matched_positive_cases[case_index]
        {
            overall.false_negatives += 1;
            families
                .entry(case.rule_family)
                .or_default()
                .false_negatives += 1;
        }
    }

    let summary = architecture_policy_counts_summary(overall);
    let family_reports = families
        .into_iter()
        .map(|(rule_family, counts)| architecture_policy_family_report(rule_family, counts))
        .collect::<Vec<_>>();

    (summary, family_reports, case_reports)
}

fn architecture_policy_case_selector_matches(
    case: &ArchitecturePolicyBenchCase,
    finding: &ArchitecturePolicyActualFinding,
) -> bool {
    same_architecture_bench_path(&case.source_path, &finding.source_path)
        && same_architecture_bench_path(&case.target_path, &finding.target_path)
        && case.edge_type == finding.edge_type
        && case
            .rule_id
            .as_ref()
            .map(|rule_id| finding.rule_id.as_ref() == Some(rule_id))
            .unwrap_or(true)
}

fn architecture_policy_case_exact_match(
    case: &ArchitecturePolicyBenchCase,
    finding: &ArchitecturePolicyActualFinding,
) -> bool {
    architecture_policy_case_selector_matches(case, finding)
        && case.expected == finding.outcome
        && case.rule_family == finding.rule_family
}

fn same_architecture_bench_path(left: &Path, right: &Path) -> bool {
    normalize_path_fragment(&left.display().to_string())
        == normalize_path_fragment(&right.display().to_string())
}

fn architecture_policy_rule_family(
    policy: &ArchitecturePolicy,
    rule_id: &str,
) -> ArchitecturePolicyRuleFamily {
    if policy
        .dependency_rules
        .iter()
        .any(|rule| rule.id == rule_id)
    {
        ArchitecturePolicyRuleFamily::DependencyRule
    } else if policy
        .public_api_rules
        .iter()
        .any(|rule| rule.id == rule_id)
    {
        ArchitecturePolicyRuleFamily::PublicApiRule
    } else if policy
        .internal_only_rules
        .iter()
        .any(|rule| rule.id == rule_id)
    {
        ArchitecturePolicyRuleFamily::InternalOnlyRule
    } else {
        ArchitecturePolicyRuleFamily::Unknown
    }
}

fn architecture_policy_counts_summary(
    counts: ArchitecturePolicyBenchCounts,
) -> ArchitecturePolicyBenchSummary {
    ArchitecturePolicyBenchSummary {
        precision: ratio(counts.true_positives, counts.actual_positive_count),
        recall: ratio(counts.true_positives, counts.expected_positive_count),
        true_positives: counts.true_positives,
        false_positives: counts.false_positives,
        false_negatives: counts.false_negatives,
        expected_positive_count: counts.expected_positive_count,
        actual_positive_count: counts.actual_positive_count,
    }
}

fn architecture_policy_family_report(
    rule_family: ArchitecturePolicyRuleFamily,
    counts: ArchitecturePolicyBenchCounts,
) -> ArchitecturePolicyBenchFamilyReport {
    ArchitecturePolicyBenchFamilyReport {
        rule_family,
        precision: ratio(counts.true_positives, counts.actual_positive_count),
        recall: ratio(counts.true_positives, counts.expected_positive_count),
        true_positives: counts.true_positives,
        false_positives: counts.false_positives,
        false_negatives: counts.false_negatives,
        expected_positive_count: counts.expected_positive_count,
        actual_positive_count: counts.actual_positive_count,
    }
}

fn percentile_duration_ms(durations: &[Duration], percentile: f64) -> f64 {
    if durations.is_empty() {
        return 0.0;
    }
    let mut values = durations
        .iter()
        .map(|duration| duration_ms(*duration))
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((values.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    values[rank]
}

fn print_architecture_policy_bench_report(report: &ArchitecturePolicyBenchReport) {
    println!(
        "Architecture policy benchmark: {} case(s), p95 {:.2}ms",
        report.case_count, report.p95_policy_check_ms
    );
    println!(
        "Overall: precision {:.3}, recall {:.3}, TP {}, FP {}, FN {}",
        report.summary.precision,
        report.summary.recall,
        report.summary.true_positives,
        report.summary.false_positives,
        report.summary.false_negatives
    );
    for family in &report.rule_families {
        println!(
            "  {:?}: precision {:.3}, recall {:.3}, TP {}, FP {}, FN {}",
            family.rule_family,
            family.precision,
            family.recall,
            family.true_positives,
            family.false_positives,
            family.false_negatives
        );
    }
    for case in &report.cases {
        let status = if case.passed { "pass" } else { "fail" };
        println!(
            "  {status}: {} {:?} {:?} {} -> {} via {:?}",
            case.id,
            case.rule_family,
            case.expected,
            case.source_path.display(),
            case.target_path.display(),
            case.edge_type
        );
        for note in &case.notes {
            println!("    note: {note}");
        }
    }
}

fn run_history_bench(repo: &Path, args: HistoryBenchArgs) -> anyhow::Result<HistoryBenchReport> {
    let cases_file = if args.cases_file.is_absolute() {
        args.cases_file.clone()
    } else {
        repo.join(&args.cases_file)
    };
    let corpus = load_history_bench_corpus(&cases_file)?;
    if corpus.cases.is_empty() {
        anyhow::bail!(
            "history benchmark cases file is empty: {}",
            cases_file.display()
        );
    }

    let mut scores = HistoryBenchScoring::default();
    let mut cases = Vec::with_capacity(corpus.cases.len());
    let mut failures = Vec::new();
    for case in &corpus.cases {
        let report = score_history_bench_case(case, &mut scores)?;
        failures.extend(history_bench_failures(&report));
        cases.push(report);
    }

    let family_p95_ms = scores
        .family_latencies_ms
        .iter()
        .map(|(family, values)| (family.clone(), p95_ms(values)))
        .collect::<BTreeMap<_, _>>();

    Ok(HistoryBenchReport {
        cases_file,
        schema_version: corpus.schema_version,
        case_count: cases.len(),
        family_counts: scores.family_counts,
        min_reviewer_accuracy: args.min_reviewer_accuracy,
        reviewer_accuracy: ratio(scores.reviewer_passed, scores.reviewer_total),
        min_similar_recall_at_5: args.min_similar_recall_at_5,
        similar_recall_at_5: ratio(scores.similar_matched_total, scores.similar_expected_total),
        max_similar_p95_ms: args.max_similar_p95_ms,
        similar_p95_ms: p95_ms(&scores.similar_latencies_ms),
        max_lookup_p95_ms: args.max_lookup_p95_ms,
        ownership_churn_p95_ms: p95_ms(&scores.ownership_churn_latencies_ms),
        family_p95_ms,
        failures,
        cases,
    })
}

#[derive(Default)]
struct HistoryBenchScoring {
    family_counts: HistoryBenchFamilyCounts,
    similar_expected_total: usize,
    similar_matched_total: usize,
    reviewer_total: usize,
    reviewer_passed: usize,
    similar_latencies_ms: Vec<f64>,
    ownership_churn_latencies_ms: Vec<f64>,
    family_latencies_ms: BTreeMap<String, Vec<f64>>,
}

fn load_history_bench_corpus(path: &Path) -> anyhow::Result<HistoryBenchCorpus> {
    let raw = fs::read_to_string(path)?;
    let corpus: HistoryBenchCorpus = serde_json::from_str(&raw)?;
    if corpus.schema_version != 1 {
        anyhow::bail!(
            "unsupported history benchmark schema_version {}; expected 1",
            corpus.schema_version
        );
    }

    let mut top_ids = BTreeSet::new();
    let mut child_ids = BTreeSet::new();
    for case in &corpus.cases {
        if case.id.trim().is_empty() {
            anyhow::bail!("history benchmark cases require non-empty id");
        }
        if !top_ids.insert(case.id.clone()) {
            anyhow::bail!("duplicate history benchmark case id `{}`", case.id);
        }
        if case.similar.is_empty()
            && case.ownership.is_empty()
            && case.reviewers.is_empty()
            && case.churn.is_empty()
            && case.provenance.is_empty()
        {
            anyhow::bail!(
                "history benchmark case `{}` must include at least one public API family",
                case.id
            );
        }
        for child in &case.similar {
            validate_history_bench_child_id(&case.id, "similar", &child.id, &mut child_ids)?;
            if child.expected_top_5.is_empty() {
                anyhow::bail!(
                    "history benchmark similar case `{}::{}` requires expected_top_5",
                    case.id,
                    child.id
                );
            }
        }
        for child in &case.ownership {
            validate_history_bench_child_id(&case.id, "ownership", &child.id, &mut child_ids)?;
            if child.path.as_os_str().is_empty() || child.expected_owner.trim().is_empty() {
                anyhow::bail!(
                    "history benchmark ownership case `{}::{}` requires path and expected_owner",
                    case.id,
                    child.id
                );
            }
        }
        for child in &case.reviewers {
            validate_history_bench_child_id(&case.id, "reviewers", &child.id, &mut child_ids)?;
            if child.path.as_os_str().is_empty() || child.expected_top_reviewer.trim().is_empty() {
                anyhow::bail!(
                    "history benchmark reviewer case `{}::{}` requires path and expected_top_reviewer",
                    case.id,
                    child.id
                );
            }
        }
        for child in &case.churn {
            validate_history_bench_child_id(&case.id, "churn", &child.id, &mut child_ids)?;
            let provided = usize::from(child.path.is_some())
                + usize::from(child.module.is_some())
                + usize::from(child.symbol_id.is_some());
            if provided != 1 {
                anyhow::bail!(
                    "history benchmark churn case `{}::{}` must provide exactly one of path, module, or symbol_id",
                    case.id,
                    child.id
                );
            }
        }
        for child in &case.provenance {
            validate_history_bench_child_id(&case.id, "provenance", &child.id, &mut child_ids)?;
            if child.path.as_os_str().is_empty()
                || child.expected_first_seen.trim().is_empty()
                || child.expected_last_touched.trim().is_empty()
            {
                anyhow::bail!(
                    "history benchmark provenance case `{}::{}` requires path, expected_first_seen, and expected_last_touched",
                    case.id,
                    child.id
                );
            }
        }
    }
    Ok(corpus)
}

fn validate_history_bench_child_id(
    case_id: &str,
    family: &str,
    child_id: &str,
    seen: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    if child_id.trim().is_empty() {
        anyhow::bail!("history benchmark {family} cases require non-empty id");
    }
    let key = format!("{case_id}::{family}::{child_id}");
    if !seen.insert(key.clone()) {
        anyhow::bail!("duplicate history benchmark child case id `{key}`");
    }
    Ok(())
}

fn score_history_bench_case(
    case: &HistoryBenchCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchCaseReport> {
    let store = SqliteStore::open(":memory:")?;
    store.put_history_snapshot(&case.snapshot)?;
    let fixture_repo = prepare_history_bench_repo(&case.id, &case.codeowners)?;

    let similar = case
        .similar
        .iter()
        .map(|child| score_history_bench_similar(&store, child, scores))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let ownership = case
        .ownership
        .iter()
        .map(|child| score_history_bench_ownership(&fixture_repo.path, &store, child, scores))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let reviewers = case
        .reviewers
        .iter()
        .map(|child| score_history_bench_reviewer(&fixture_repo.path, &store, child, scores))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let churn = case
        .churn
        .iter()
        .map(|child| score_history_bench_churn(&store, child, scores))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let provenance = case
        .provenance
        .iter()
        .map(|child| score_history_bench_provenance(&store, child, scores))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let passed = similar.iter().all(|report| report.passed)
        && ownership.iter().all(|report| report.passed)
        && reviewers.iter().all(|report| report.passed)
        && churn.iter().all(|report| report.passed)
        && provenance.iter().all(|report| report.passed);

    Ok(HistoryBenchCaseReport {
        id: case.id.clone(),
        similar,
        ownership,
        reviewers,
        churn,
        provenance,
        passed,
    })
}

struct HistoryBenchTempRepo {
    path: PathBuf,
}

impl Drop for HistoryBenchTempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn prepare_history_bench_repo(
    case_id: &str,
    codeowners: &[String],
) -> anyhow::Result<HistoryBenchTempRepo> {
    let stamp = chrono::Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_else(|| chrono::Utc::now().timestamp_micros());
    let path = std::env::temp_dir().join(format!(
        "open-kioku-history-bench-{}-{}-{}",
        std::process::id(),
        stamp,
        sanitize_temp_path_fragment(case_id)
    ));
    fs::create_dir_all(&path)?;
    if !codeowners.is_empty() {
        let codeowners_dir = path.join(".github");
        fs::create_dir_all(&codeowners_dir)?;
        fs::write(codeowners_dir.join("CODEOWNERS"), codeowners.join("\n"))?;
    }
    Ok(HistoryBenchTempRepo { path })
}

fn score_history_bench_similar(
    store: &SqliteStore,
    case: &HistoryBenchSimilarCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchSimilarCaseReport> {
    let started_at = Instant::now();
    let report = store.similar_changes(&case.query, 5)?;
    let latency_ms = elapsed_ms(started_at);
    record_history_bench_latency(scores, "similar", latency_ms);
    scores.family_counts.similar += 1;

    let actual_top_5 = report
        .hits
        .iter()
        .map(|hit| hit.change.commit.id.0.clone())
        .collect::<Vec<_>>();
    let expected = case.expected_top_5.iter().cloned().collect::<BTreeSet<_>>();
    let actual = actual_top_5.iter().cloned().collect::<BTreeSet<_>>();
    let matched = expected.intersection(&actual).cloned().collect::<Vec<_>>();
    scores.similar_expected_total += expected.len();
    scores.similar_matched_total += matched.len();
    let recall_at_5 = ratio(matched.len(), expected.len());

    Ok(HistoryBenchSimilarCaseReport {
        id: case.id.clone(),
        expected_top_5: case.expected_top_5.clone(),
        actual_top_5,
        matched,
        recall_at_5,
        latency_ms,
        passed: recall_at_5 >= 1.0,
    })
}

fn score_history_bench_ownership(
    repo: &Path,
    store: &SqliteStore,
    case: &HistoryBenchOwnershipCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchOwnershipCaseReport> {
    let started_at = Instant::now();
    let report = open_kioku_git::ownership_for_path(open_kioku_git::OwnershipInput {
        repo,
        path: &case.path,
        history: store,
        memory_facts: &[],
        components: Vec::new(),
    })?;
    let latency_ms = elapsed_ms(started_at);
    record_history_bench_latency(scores, "ownership", latency_ms);
    scores.family_counts.ownership += 1;

    let rank = report
        .owners
        .iter()
        .position(|suggestion| owner_matches_expected(&suggestion.owner, &case.expected_owner))
        .map(|index| index + 1);
    let top = report.owners.first();
    let actual_owner = top.map(|suggestion| owner_display(&suggestion.owner));
    let actual_source_types = top
        .map(|suggestion| suggestion.source_types.clone())
        .unwrap_or_default();
    let source_types_match = case
        .expected_source_types
        .iter()
        .all(|expected| actual_source_types.contains(expected));

    Ok(HistoryBenchOwnershipCaseReport {
        id: case.id.clone(),
        path: case.path.clone(),
        expected_owner: case.expected_owner.clone(),
        actual_owner,
        rank,
        expected_source_types: case.expected_source_types.clone(),
        actual_source_types,
        latency_ms,
        passed: rank == Some(1) && source_types_match,
    })
}

fn score_history_bench_reviewer(
    repo: &Path,
    store: &SqliteStore,
    case: &HistoryBenchReviewerCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchReviewerCaseReport> {
    let started_at = Instant::now();
    let ownership = open_kioku_git::ownership_for_path(open_kioku_git::OwnershipInput {
        repo,
        path: &case.path,
        history: store,
        memory_facts: &[],
        components: Vec::new(),
    })?;
    let report = open_kioku_git::suggest_reviewers(open_kioku_git::ReviewerSuggestionInput {
        path: &case.path,
        history: store,
        ownership: Some(&ownership),
    })?;
    let latency_ms = elapsed_ms(started_at);
    record_history_bench_latency(scores, "reviewers", latency_ms);
    scores.family_counts.reviewers += 1;
    scores.reviewer_total += 1;

    let rank = report
        .suggestions
        .iter()
        .position(|suggestion| {
            owner_matches_expected(&suggestion.reviewer, &case.expected_top_reviewer)
        })
        .map(|index| index + 1);
    let top = report.suggestions.first();
    let actual_top_reviewer = top.map(|suggestion| owner_display(&suggestion.reviewer));
    let actual_review_evidence = top.map(|suggestion| suggestion.actual_review_evidence);
    let inferred_from_authors = top.map(|suggestion| suggestion.inferred_from_authors);
    let availability_correct = report.availability == case.expected_availability;
    let actual_review_evidence_correct = case
        .expected_actual_review_evidence
        .zip(actual_review_evidence)
        .map(|(expected, actual)| expected == actual)
        .unwrap_or(true);
    let inferred_from_authors_correct = case
        .expected_inferred_from_authors
        .zip(inferred_from_authors)
        .map(|(expected, actual)| expected == actual)
        .unwrap_or(true);
    let passed = rank == Some(1)
        && availability_correct
        && actual_review_evidence_correct
        && inferred_from_authors_correct;
    if passed {
        scores.reviewer_passed += 1;
    }

    Ok(HistoryBenchReviewerCaseReport {
        id: case.id.clone(),
        path: case.path.clone(),
        expected_top_reviewer: case.expected_top_reviewer.clone(),
        actual_top_reviewer,
        rank,
        expected_availability: case.expected_availability,
        availability: report.availability,
        availability_correct,
        expected_actual_review_evidence: case.expected_actual_review_evidence,
        actual_review_evidence,
        actual_review_evidence_correct,
        expected_inferred_from_authors: case.expected_inferred_from_authors,
        inferred_from_authors,
        inferred_from_authors_correct,
        latency_ms,
        passed,
    })
}

fn score_history_bench_churn(
    store: &SqliteStore,
    case: &HistoryBenchChurnCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchChurnCaseReport> {
    let target = history_bench_churn_target(case);
    let started_at = Instant::now();
    let summary = if let Some(path) = &case.path {
        store.churn_for_file(path)?
    } else if let Some(module) = &case.module {
        store.churn_for_module(module)?
    } else if let Some(symbol_id) = &case.symbol_id {
        store.churn_for_symbol(symbol_id)?
    } else {
        unreachable!("history benchmark churn targets are validated before scoring");
    };
    let latency_ms = elapsed_ms(started_at);
    record_history_bench_latency(scores, "churn", latency_ms);
    scores.family_counts.churn += 1;

    let passed = summary.stats.touch_count >= case.min_touch_count
        && summary.stats.hotspot_score >= case.min_hotspot_score;
    Ok(HistoryBenchChurnCaseReport {
        id: case.id.clone(),
        target,
        touch_count: summary.stats.touch_count,
        hotspot_score: summary.stats.hotspot_score,
        min_touch_count: case.min_touch_count,
        min_hotspot_score: case.min_hotspot_score,
        confidence: summary.confidence,
        latency_ms,
        passed,
    })
}

fn score_history_bench_provenance(
    store: &SqliteStore,
    case: &HistoryBenchProvenanceCase,
    scores: &mut HistoryBenchScoring,
) -> anyhow::Result<HistoryBenchProvenanceCaseReport> {
    let started_at = Instant::now();
    let report = store.provenance_for_path(&case.path, case.limit.unwrap_or(20))?;
    let latency_ms = elapsed_ms(started_at);
    record_history_bench_latency(scores, "provenance", latency_ms);
    scores.family_counts.provenance += 1;

    let actual_first_seen = report
        .first_seen
        .as_ref()
        .map(|touch| touch.commit.id.0.clone());
    let actual_last_touched = report
        .last_touched
        .as_ref()
        .map(|touch| touch.commit.id.0.clone());
    let passed = actual_first_seen.as_deref() == Some(case.expected_first_seen.as_str())
        && actual_last_touched.as_deref() == Some(case.expected_last_touched.as_str())
        && report.recent_touches.len() >= case.min_recent_touches;

    Ok(HistoryBenchProvenanceCaseReport {
        id: case.id.clone(),
        path: case.path.clone(),
        expected_first_seen: case.expected_first_seen.clone(),
        actual_first_seen,
        expected_last_touched: case.expected_last_touched.clone(),
        actual_last_touched,
        min_recent_touches: case.min_recent_touches,
        recent_touch_count: report.recent_touches.len(),
        confidence: report.confidence,
        latency_ms,
        passed,
    })
}

fn history_bench_churn_target(case: &HistoryBenchChurnCase) -> String {
    if let Some(path) = &case.path {
        format!("file:{}", path.display())
    } else if let Some(module) = &case.module {
        format!("module:{}", module.display())
    } else if let Some(symbol_id) = &case.symbol_id {
        format!("symbol:{}", symbol_id.0)
    } else {
        "unknown".into()
    }
}

fn history_bench_failures(report: &HistoryBenchCaseReport) -> Vec<String> {
    let mut failures = Vec::new();
    for case in &report.similar {
        if !case.passed {
            failures.push(format!(
                "{}::similar::{} expected Top-5 {:?}, got {:?}",
                report.id, case.id, case.expected_top_5, case.actual_top_5
            ));
        }
    }
    for case in &report.ownership {
        if !case.passed {
            failures.push(format!(
                "{}::ownership::{} expected owner `{}` at rank 1, got {:?} at rank {:?}",
                report.id, case.id, case.expected_owner, case.actual_owner, case.rank
            ));
        }
    }
    for case in &report.reviewers {
        if !case.passed {
            failures.push(format!(
                "{}::reviewers::{} expected reviewer `{}` at rank 1 with {:?}, got {:?} at rank {:?} with {:?}",
                report.id,
                case.id,
                case.expected_top_reviewer,
                case.expected_availability,
                case.actual_top_reviewer,
                case.rank,
                case.availability
            ));
        }
    }
    for case in &report.churn {
        if !case.passed {
            failures.push(format!(
                "{}::churn::{} expected touch_count >= {} and hotspot_score >= {:.3}, got {} and {:.3}",
                report.id,
                case.id,
                case.min_touch_count,
                case.min_hotspot_score,
                case.touch_count,
                case.hotspot_score
            ));
        }
    }
    for case in &report.provenance {
        if !case.passed {
            failures.push(format!(
                "{}::provenance::{} expected first/last {}/{}, got {:?}/{:?}",
                report.id,
                case.id,
                case.expected_first_seen,
                case.expected_last_touched,
                case.actual_first_seen,
                case.actual_last_touched
            ));
        }
    }
    failures
}

fn record_history_bench_latency(scores: &mut HistoryBenchScoring, family: &str, latency_ms: f64) {
    if family == "similar" {
        scores.similar_latencies_ms.push(latency_ms);
    }
    if matches!(family, "ownership" | "churn") {
        scores.ownership_churn_latencies_ms.push(latency_ms);
    }
    scores
        .family_latencies_ms
        .entry(family.to_string())
        .or_default()
        .push(latency_ms);
}

fn elapsed_ms(started_at: Instant) -> f64 {
    started_at.elapsed().as_secs_f64() * 1000.0
}

fn p95_ms(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.total_cmp(right));
    let index = ((sorted.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

fn default_history_bench_min_recent_touches() -> usize {
    1
}

fn print_history_bench_report(report: &HistoryBenchReport) {
    println!(
        "History API benchmark: {} case set(s); reviewer accuracy {:.3} (min {:.3}); similar Top-5 recall {:.3} (min {:.3})",
        report.case_count,
        report.reviewer_accuracy,
        report.min_reviewer_accuracy,
        report.similar_recall_at_5,
        report.min_similar_recall_at_5
    );
    println!(
        "Latency p95: similar {:.2}ms (max {:.2}ms); ownership/churn {:.2}ms (max {:.2}ms)",
        report.similar_p95_ms,
        report.max_similar_p95_ms,
        report.ownership_churn_p95_ms,
        report.max_lookup_p95_ms
    );
    println!(
        "Families: similar {}, ownership {}, reviewers {}, churn {}, provenance {}",
        report.family_counts.similar,
        report.family_counts.ownership,
        report.family_counts.reviewers,
        report.family_counts.churn,
        report.family_counts.provenance
    );
    for (family, latency_ms) in &report.family_p95_ms {
        println!("  {family}: p95 {latency_ms:.2}ms");
    }
    for case in &report.cases {
        let status = if case.passed { "pass" } else { "fail" };
        println!("  {status}: {}", case.id);
    }
    if !report.failures.is_empty() {
        println!("Failures:");
        for failure in &report.failures {
            println!("- {failure}");
        }
    }
}

fn run_reviewer_bench(repo: &Path, args: ReviewerBenchArgs) -> anyhow::Result<ReviewerBenchReport> {
    let cases_file = if args.cases_file.is_absolute() {
        args.cases_file.clone()
    } else {
        repo.join(&args.cases_file)
    };
    let cases = load_reviewer_bench_cases(&cases_file)?;
    if cases.is_empty() {
        anyhow::bail!(
            "reviewer benchmark cases file is empty: {}",
            cases_file.display()
        );
    }

    let mut reports = Vec::with_capacity(cases.len());
    let mut failures = Vec::new();
    for case in &cases {
        let report = score_reviewer_bench_case(case)?;
        if !report.passed {
            failures.push(format!(
                "{} expected top reviewer `{}` at rank 1, got {:?} at rank {:?}",
                case.id, case.expected_top_reviewer, report.actual_top_reviewer, report.rank
            ));
        }
        reports.push(report);
    }

    let passed = reports.iter().filter(|case| case.passed).count();
    Ok(ReviewerBenchReport {
        cases_file,
        case_count: reports.len(),
        min_accuracy: args.min_accuracy,
        accuracy: ratio(passed, reports.len()),
        failures,
        cases: reports,
    })
}

fn run_similar_history_bench(
    repo: &Path,
    args: SimilarHistoryBenchArgs,
) -> anyhow::Result<SimilarHistoryBenchReport> {
    let cases_file = if args.cases_file.is_absolute() {
        args.cases_file.clone()
    } else {
        repo.join(&args.cases_file)
    };
    let cases = load_similar_history_bench_cases(&cases_file)?;
    if cases.is_empty() {
        anyhow::bail!(
            "similar history benchmark cases file is empty: {}",
            cases_file.display()
        );
    }

    let mut reports = Vec::with_capacity(cases.len());
    let mut failures = Vec::new();
    let mut expected_total = 0_usize;
    let mut matched_total = 0_usize;
    for case in &cases {
        let report = score_similar_history_bench_case(case)?;
        expected_total += report.expected_top_5.len();
        matched_total += report.matched.len();
        if !report.passed {
            failures.push(format!(
                "{} expected Top-5 commit(s) {:?}, got {:?}",
                case.id, report.expected_top_5, report.actual_top_5
            ));
        }
        reports.push(report);
    }

    Ok(SimilarHistoryBenchReport {
        cases_file,
        case_count: reports.len(),
        min_recall_at_5: args.min_recall_at_5,
        recall_at_5: ratio(matched_total, expected_total),
        failures,
        cases: reports,
    })
}

fn load_reviewer_bench_cases(path: &Path) -> anyhow::Result<Vec<ReviewerBenchCase>> {
    let raw = fs::read_to_string(path)?;
    let cases: Vec<ReviewerBenchCase> = serde_json::from_str(&raw)?;
    let mut ids = BTreeSet::new();
    for case in &cases {
        if case.id.trim().is_empty() || case.expected_top_reviewer.trim().is_empty() {
            anyhow::bail!(
                "reviewer benchmark cases require non-empty id and expected_top_reviewer"
            );
        }
        if case.path.as_os_str().is_empty() {
            anyhow::bail!("reviewer benchmark case `{}` requires a path", case.id);
        }
        if !ids.insert(case.id.clone()) {
            anyhow::bail!("duplicate reviewer benchmark case id `{}`", case.id);
        }
    }
    Ok(cases)
}

fn load_similar_history_bench_cases(path: &Path) -> anyhow::Result<Vec<SimilarHistoryBenchCase>> {
    let raw = fs::read_to_string(path)?;
    let cases: Vec<SimilarHistoryBenchCase> = serde_json::from_str(&raw)?;
    let mut ids = BTreeSet::new();
    for case in &cases {
        if case.id.trim().is_empty() || case.expected_top_5.is_empty() {
            anyhow::bail!(
                "similar history benchmark cases require non-empty id and expected_top_5"
            );
        }
        if !ids.insert(case.id.clone()) {
            anyhow::bail!("duplicate similar history benchmark case id `{}`", case.id);
        }
    }
    Ok(cases)
}

fn score_similar_history_bench_case(
    case: &SimilarHistoryBenchCase,
) -> anyhow::Result<SimilarHistoryBenchCaseReport> {
    let store = SqliteStore::open(":memory:")?;
    store.put_history_snapshot(&case.snapshot)?;
    let report = store.similar_changes(&case.query, 5)?;
    let actual_top_5 = report
        .hits
        .iter()
        .map(|hit| hit.change.commit.id.0.clone())
        .collect::<Vec<_>>();
    let expected = case.expected_top_5.iter().cloned().collect::<BTreeSet<_>>();
    let actual = actual_top_5.iter().cloned().collect::<BTreeSet<_>>();
    let matched = expected.intersection(&actual).cloned().collect::<Vec<_>>();
    let recall_at_5 = ratio(matched.len(), expected.len());

    Ok(SimilarHistoryBenchCaseReport {
        id: case.id.clone(),
        expected_top_5: case.expected_top_5.clone(),
        actual_top_5,
        matched,
        recall_at_5,
        passed: recall_at_5 >= 1.0,
    })
}

fn score_reviewer_bench_case(case: &ReviewerBenchCase) -> anyhow::Result<ReviewerBenchCaseReport> {
    let history = ReviewerBenchHistoryStore::from_case(case);
    let ownership = reviewer_bench_ownership_report(case);
    let report = open_kioku_git::suggest_reviewers(open_kioku_git::ReviewerSuggestionInput {
        path: &case.path,
        history: &history,
        ownership: Some(&ownership),
    })?;

    let rank = report
        .suggestions
        .iter()
        .position(|suggestion| {
            owner_matches_expected(&suggestion.reviewer, &case.expected_top_reviewer)
        })
        .map(|index| index + 1);
    let top = report.suggestions.first();
    let actual_top_reviewer = top.map(|suggestion| owner_display(&suggestion.reviewer));
    let actual_review_evidence = top.map(|suggestion| suggestion.actual_review_evidence);
    let inferred_from_authors = top.map(|suggestion| suggestion.inferred_from_authors);
    let top_score = top.map(|suggestion| suggestion.score);

    let availability_correct = report.availability == case.expected_availability;
    let actual_review_evidence_correct = case
        .expected_actual_review_evidence
        .zip(actual_review_evidence)
        .map(|(expected, actual)| expected == actual)
        .unwrap_or(true);
    let inferred_from_authors_correct = case
        .expected_inferred_from_authors
        .zip(inferred_from_authors)
        .map(|(expected, actual)| expected == actual)
        .unwrap_or(true);
    let passed = rank == Some(1)
        && availability_correct
        && actual_review_evidence_correct
        && inferred_from_authors_correct;

    Ok(ReviewerBenchCaseReport {
        id: case.id.clone(),
        path: case.path.clone(),
        expected_top_reviewer: case.expected_top_reviewer.clone(),
        actual_top_reviewer,
        rank,
        expected_availability: case.expected_availability,
        availability: report.availability,
        availability_correct,
        expected_actual_review_evidence: case.expected_actual_review_evidence,
        actual_review_evidence,
        actual_review_evidence_correct,
        expected_inferred_from_authors: case.expected_inferred_from_authors,
        inferred_from_authors,
        inferred_from_authors_correct,
        top_score,
        passed,
    })
}

#[derive(Clone)]
struct ReviewerBenchHistoryStore {
    history: HistorySummary,
    provenance: FileProvenance,
}

impl ReviewerBenchHistoryStore {
    fn from_case(case: &ReviewerBenchCase) -> Self {
        let mut reviewer_evidence = Vec::with_capacity(case.review_evidence.len());
        for (index, evidence) in case.review_evidence.iter().enumerate() {
            reviewer_evidence.push(ReviewerEvidence {
                id: HistoryRecordId::new(format!("reviewer-bench:{}:{index}", case.id)),
                commit_id: Some(GitCommitId::new(format!(
                    "reviewer-bench-{}-{index}",
                    case.id
                ))),
                path: Some(case.path.clone()),
                reviewer: owner_from_token(&evidence.reviewer),
                role: evidence.role,
                observed_at: reviewer_bench_time(evidence.days_ago),
                source: evidence
                    .source
                    .clone()
                    .unwrap_or_else(|| format!("reviewer-bench:{}", case.id)),
                confidence: evidence.confidence,
            });
        }

        let mut touches = Vec::new();
        let mut touch_index = 0usize;
        for touch in &case.author_touches {
            for offset in 0..touch.count.max(1) {
                touches.push(reviewer_bench_touch(case, touch, touch_index, offset));
                touch_index += 1;
            }
        }
        touches.sort_by(|left, right| {
            right
                .commit
                .committed_at
                .cmp(&left.commit.committed_at)
                .then_with(|| left.commit.id.0.cmp(&right.commit.id.0))
        });

        Self {
            history: HistorySummary {
                path: case.path.clone(),
                recent_commits: Vec::new(),
                file_touches: Vec::new(),
                symbol_touches: Vec::new(),
                cochange_neighbors: Vec::new(),
                reviewer_evidence,
                truncated: false,
                uncertainty: Vec::new(),
            },
            provenance: FileProvenance {
                path: case.path.clone(),
                first_seen: touches.last().cloned(),
                last_touched: touches.first().cloned(),
                recent_touches: touches,
                confidence: Confidence::High,
                truncated: false,
                uncertainty: Vec::new(),
            },
        }
    }
}

impl HistoryStore for ReviewerBenchHistoryStore {
    fn put_history_snapshot(&self, _snapshot: &HistorySnapshot) -> open_kioku_errors::Result<()> {
        Ok(())
    }

    fn history_for_file(
        &self,
        path: &Path,
        _limit: usize,
    ) -> open_kioku_errors::Result<HistorySummary> {
        if path == self.history.path {
            Ok(self.history.clone())
        } else {
            Ok(HistorySummary::empty(path))
        }
    }

    fn provenance_for_path(
        &self,
        path: &Path,
        _limit: usize,
    ) -> open_kioku_errors::Result<FileProvenance> {
        if path == self.provenance.path {
            Ok(self.provenance.clone())
        } else {
            Ok(FileProvenance {
                path: path.to_path_buf(),
                first_seen: None,
                last_touched: None,
                recent_touches: Vec::new(),
                confidence: Confidence::Low,
                truncated: false,
                uncertainty: vec!["reviewer benchmark provenance unavailable for this path".into()],
            })
        }
    }

    fn provenance_for_symbol(
        &self,
        symbol_id: &SymbolId,
        _limit: usize,
    ) -> open_kioku_errors::Result<SymbolProvenance> {
        Ok(SymbolProvenance {
            symbol_id: symbol_id.clone(),
            qualified_name: "reviewer_bench::unknown".into(),
            file_path: self.provenance.path.clone(),
            range: None,
            first_seen: None,
            last_touched: None,
            recent_touches: Vec::new(),
            confidence: Confidence::Low,
            truncated: false,
            uncertainty: vec!["reviewer benchmark symbol provenance unavailable".into()],
        })
    }

    fn cochange_neighbors(
        &self,
        _path: &Path,
        _limit: usize,
    ) -> open_kioku_errors::Result<Vec<GitCochangeEdge>> {
        Ok(Vec::new())
    }

    fn recent_commits(&self, _limit: usize) -> open_kioku_errors::Result<Vec<GitCommitRecord>> {
        Ok(Vec::new())
    }
}

fn reviewer_bench_ownership_report(case: &ReviewerBenchCase) -> OwnershipReport {
    let generated_at = chrono::Utc::now();
    let owners = case
        .ownership
        .iter()
        .enumerate()
        .map(|(index, evidence)| {
            let owner = owner_from_token(&evidence.owner);
            let source_types = if evidence.source_types.is_empty() {
                default_reviewer_bench_source_types()
            } else {
                evidence.source_types.clone()
            };
            let observed_at = reviewer_bench_time(evidence.days_ago);
            let stale = reviewer_bench_is_stale(evidence.days_ago);
            let confidence = Confidence::from_score(evidence.score);
            let source = evidence
                .source
                .clone()
                .unwrap_or_else(|| format!("reviewer-bench:{}:{index}", case.id));
            let ownership_evidence = source_types
                .iter()
                .map(|source_type| OwnershipEvidence {
                    source_type: *source_type,
                    owner: owner.clone(),
                    source: source.clone(),
                    message: "reviewer benchmark ownership signal".into(),
                    confidence,
                    observed_at: Some(observed_at),
                    stale,
                })
                .collect::<Vec<_>>();
            OwnerSuggestion {
                owner,
                rationale: "reviewer benchmark ownership signal".into(),
                confidence,
                score: evidence.score,
                source_types: source_types.clone(),
                stale,
                evidence: ownership_evidence,
                confidence_breakdown: open_kioku_core::OwnershipConfidenceBreakdown {
                    codeowners: if source_types.contains(&OwnershipSourceType::Codeowners) {
                        evidence.score
                    } else {
                        0.0
                    },
                    git_history: if source_types.contains(&OwnershipSourceType::GitHistory) {
                        evidence.score
                    } else {
                        0.0
                    },
                    memory: if source_types.contains(&OwnershipSourceType::RepoMemory) {
                        evidence.score
                    } else {
                        0.0
                    },
                    freshness: if stale { 0.0 } else { 0.05 },
                    ambiguity_penalty: 0.0,
                    final_score: evidence.score,
                },
            }
        })
        .collect();

    OwnershipReport {
        path: case.path.clone(),
        components: Vec::new(),
        generated_at,
        owners,
        uncertainty: Vec::new(),
    }
}

fn reviewer_bench_touch(
    case: &ReviewerBenchCase,
    touch: &ReviewerBenchAuthorTouch,
    index: usize,
    offset: usize,
) -> ProvenanceTouch {
    let author = owner_from_token(&touch.author);
    let observed_at = reviewer_bench_time(touch.days_ago + offset as i64);
    let commit_id = GitCommitId::new(format!("reviewer-bench-{}-touch-{index}", case.id));
    ProvenanceTouch {
        commit: GitCommitRecord {
            id: commit_id,
            parent_ids: Vec::new(),
            author: author.clone(),
            committer: None,
            authored_at: observed_at,
            committed_at: observed_at,
            summary: format!("reviewer benchmark touch by {}", owner_display(&author)),
            message: format!("reviewer benchmark touch by {}", owner_display(&author)),
            file_count: 1,
        },
        path: case.path.clone(),
        previous_path: None,
        symbol_id: None,
        qualified_name: None,
        change_kind: GitChangeKind::Modified,
        line_ranges: Vec::new(),
        confidence: Confidence::High,
        uncertainty: Vec::new(),
    }
}

fn default_reviewer_bench_confidence() -> Confidence {
    Confidence::High
}

fn default_reviewer_bench_source_types() -> Vec<OwnershipSourceType> {
    vec![OwnershipSourceType::Codeowners]
}

fn default_reviewer_bench_owner_score() -> f32 {
    0.90
}

fn default_reviewer_bench_touch_count() -> usize {
    1
}

fn reviewer_bench_time(days_ago: i64) -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() - chrono::Duration::days(days_ago.max(0))
}

fn reviewer_bench_is_stale(days_ago: i64) -> bool {
    days_ago > 365
}

fn owner_from_token(value: &str) -> Owner {
    let trimmed = value.trim();
    if let (Some(start), Some(end)) = (trimmed.rfind('<'), trimmed.rfind('>')) {
        if start < end {
            let name = trimmed[..start].trim();
            let email = trimmed[start + 1..end].trim();
            return Owner {
                name: if name.is_empty() {
                    owner_name_from_email(email)
                } else {
                    name.to_string()
                },
                email: (!email.is_empty()).then(|| email.to_string()),
            };
        }
    }
    if trimmed.contains('@') {
        Owner {
            name: owner_name_from_email(trimmed),
            email: Some(trimmed.to_string()),
        }
    } else {
        Owner {
            name: trimmed.to_string(),
            email: None,
        }
    }
}

fn owner_name_from_email(email: &str) -> String {
    email.split('@').next().unwrap_or(email).to_string()
}

fn owner_matches_expected(owner: &Owner, expected: &str) -> bool {
    let expected = expected.trim().to_ascii_lowercase();
    owner
        .email
        .as_deref()
        .is_some_and(|email| email.eq_ignore_ascii_case(&expected))
        || owner.name.eq_ignore_ascii_case(&expected)
}

fn owner_display(owner: &Owner) -> String {
    owner.email.clone().unwrap_or_else(|| owner.name.clone())
}

fn print_reviewer_bench_report(report: &ReviewerBenchReport) {
    println!(
        "Reviewer benchmark: {} case(s), accuracy {:.3}, min {:.3}",
        report.case_count, report.accuracy, report.min_accuracy
    );
    for case in &report.cases {
        println!(
            "  {}: rank={:?} top={:?} availability={:?} score={:?} passed={}",
            case.id,
            case.rank,
            case.actual_top_reviewer,
            case.availability,
            case.top_score,
            case.passed
        );
    }
    if !report.failures.is_empty() {
        println!("Failures:");
        for failure in &report.failures {
            println!("- {failure}");
        }
    }
}

fn print_similar_history_bench_report(report: &SimilarHistoryBenchReport) {
    println!(
        "Similar history benchmark: {} case(s), Top-5 recall {:.3}, min {:.3}",
        report.case_count, report.recall_at_5, report.min_recall_at_5
    );
    for case in &report.cases {
        println!(
            "  {}: recall_at_5={:.3} expected={:?} actual={:?} passed={}",
            case.id, case.recall_at_5, case.expected_top_5, case.actual_top_5, case.passed
        );
    }
    if !report.failures.is_empty() {
        println!("Failures:");
        for failure in &report.failures {
            println!("- {failure}");
        }
    }
}

fn run_workflow_bench(args: WorkflowBenchArgs) -> anyhow::Result<WorkflowBenchReport> {
    let repo = absolutize(&args.path)?;
    let cases_file = if args.cases_file.is_absolute() {
        args.cases_file.clone()
    } else {
        repo.join(&args.cases_file)
    };
    let cases = load_workflow_bench_cases(&cases_file)?;
    if cases.is_empty() {
        anyhow::bail!(
            "workflow benchmark cases file is empty: {}",
            cases_file.display()
        );
    }
    if !args.no_index {
        index_repo(&repo)?;
    }
    let store = open_store(&repo)?;
    let index_dir = default_index_dir(&repo);
    let search_index = if TantivySearchIndex::exists(&index_dir) {
        Some(TantivySearchIndex::open_or_create(&index_dir)?)
    } else {
        None
    };
    let planner = PlanEngine::new(&store as &dyn OkStore)
        .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
        .with_history_store(Some(&store));
    let verifier = ChangeVerifier::new(&store as &dyn OkStore)
        .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex));
    let limit = args.limit.clamp(1, 100);
    let mut reports = Vec::with_capacity(cases.len());
    for case in cases {
        let baseline_paths = baseline_context_paths(&repo, &store, &case.task, limit, &cases_file)?;
        let plan = workflow_plan(&repo, &store, &planner, &case.task, limit, &cases_file)?;
        reports.push(score_workflow_case(
            &repo,
            &verifier,
            &case,
            &plan,
            &baseline_paths,
            limit,
        )?);
    }
    let workflow = summarize_workflow_cases(&reports, false);
    let baseline = summarize_workflow_cases(&reports, true);
    let deltas = WorkflowBenchDeltas {
        context_recall_at_k: workflow.context_recall_at_k - baseline.context_recall_at_k,
        impact_recall_at_k: workflow.impact_recall_at_k - baseline.impact_recall_at_k,
        test_recall_at_k: workflow.test_recall_at_k - baseline.test_recall_at_k,
        boundary_precision: workflow.boundary_precision - baseline.boundary_precision,
        boundary_recall: workflow.boundary_recall - baseline.boundary_recall,
        confidence_calibration_error: baseline.confidence_calibration_error
            - workflow.confidence_calibration_error,
        verification_verdict_accuracy: workflow.verification_verdict_accuracy
            - baseline.verification_verdict_accuracy,
    };
    Ok(WorkflowBenchReport {
        repo,
        cases_file,
        limit,
        case_count: reports.len(),
        baseline,
        workflow,
        deltas,
        cases: reports,
    })
}

fn load_workflow_bench_cases(path: &Path) -> anyhow::Result<Vec<WorkflowBenchCase>> {
    let raw = fs::read_to_string(path)?;
    let cases: Vec<WorkflowBenchCase> = serde_json::from_str(&raw)?;
    for case in &cases {
        if case.id.trim().is_empty() || case.task.trim().is_empty() {
            anyhow::bail!("workflow benchmark cases require non-empty id and task");
        }
    }
    Ok(cases)
}

fn baseline_context_paths(
    repo: &Path,
    store: &dyn MetadataStore,
    task: &str,
    limit: usize,
    cases_file: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut raw = search_raw(repo, store, task, ranking_candidate_limit(limit))?;
    filter_workflow_benchmark_artifacts(&mut raw, repo, cases_file);
    Ok(top_unique_paths(rerank_baseline(raw), limit)
        .into_iter()
        .map(|result| result.path)
        .collect())
}

fn workflow_plan(
    repo: &Path,
    store: &SqliteStore,
    planner: &PlanEngine,
    task: &str,
    limit: usize,
    cases_file: &Path,
) -> anyhow::Result<PlanReport> {
    let mut context = build_context_pack(repo, store, task, limit)?;
    context
        .primary_files
        .retain(|result| !is_workflow_benchmark_artifact(&result.path, repo, cases_file));
    context
        .supporting_files
        .retain(|result| !is_workflow_benchmark_artifact(&result.path, repo, cases_file));
    planner
        .plan_from_context(task, limit, context)
        .map_err(Into::into)
}

fn filter_workflow_benchmark_artifacts(
    results: &mut Vec<open_kioku_core::SearchResult>,
    repo: &Path,
    cases_file: &Path,
) {
    results.retain(|result| !is_workflow_benchmark_artifact(&result.path, repo, cases_file));
}

fn is_workflow_benchmark_artifact(path: &Path, repo: &Path, cases_file: &Path) -> bool {
    let normalized = normalize_path_fragment(&path.to_string_lossy());
    let cases = cases_file
        .strip_prefix(repo)
        .unwrap_or(cases_file)
        .to_string_lossy();
    normalized == normalize_path_fragment(&cases) || normalized.starts_with("benchmarks/")
}

fn score_workflow_case(
    repo: &Path,
    verifier: &ChangeVerifier,
    case: &WorkflowBenchCase,
    plan: &PlanReport,
    baseline_paths: &[PathBuf],
    limit: usize,
) -> anyhow::Result<WorkflowBenchCaseReport> {
    let context_paths = plan
        .primary_context
        .iter()
        .take(limit)
        .map(|result| result.path.clone())
        .collect::<Vec<_>>();
    let impact_paths = plan
        .impact
        .direct_impacts
        .iter()
        .chain(plan.impact.indirect_impacts.iter())
        .take(limit)
        .map(|result| result.path.clone())
        .collect::<Vec<_>>();
    let test_names = plan
        .validation
        .iter()
        .take(limit)
        .map(|test| test.name.clone())
        .collect::<Vec<_>>();
    let boundary_paths = plan
        .recommended_change_boundary
        .allowed_files
        .iter()
        .chain(plan.recommended_change_boundary.caution_files.iter())
        .cloned()
        .collect::<Vec<_>>();

    let context_hits = matching_expected_values(&case.expected_primary_context, &context_paths);
    let impact_hits = matching_expected_values(&case.expected_impact, &impact_paths);
    let test_hits = matching_expected_strings(&case.expected_tests, &test_names);
    let boundary_hits = matching_expected_values(&case.expected_boundary, &boundary_paths);
    let forbidden_boundary_hits = matching_expected_values(&case.forbidden_paths, &boundary_paths);
    let baseline_context_hits =
        matching_expected_values(&case.expected_primary_context, baseline_paths);

    let expected_success = case.expected_confidence.unwrap_or_else(|| {
        !case
            .expected_verdict
            .is_some_and(|verdict| verdict == VerificationVerdict::Fail)
    });
    let confidence_probability = plan_success_probability(plan);
    let confidence_calibration_error =
        Some((confidence_probability - if expected_success { 1.0 } else { 0.0 }).abs());

    let verification = if case.expected_verdict.is_some()
        && (!case.changed_files.is_empty() || case.unified_diff.is_some())
    {
        Some(verifier.verify(
            repo,
            plan,
            VerifyChangeInput {
                changed_files: case.changed_files.clone(),
                unified_diff: case.unified_diff.clone(),
                evidence_refs: Vec::new(),
                run_commands: false,
                write_attestation: false,
                validation_attestations: Vec::new(),
                traceability_strict: false,
                check_api_surface: false,
                check_dependency_delta: false,
                architecture_policy: None,
            },
        )?)
    } else {
        None
    };
    let actual_verdict = verification.as_ref().map(|report| report.verdict);
    let verification_correct = case
        .expected_verdict
        .zip(actual_verdict)
        .map(|(expected, actual)| expected == actual);

    Ok(WorkflowBenchCaseReport {
        id: case.id.clone(),
        task: case.task.clone(),
        context_recall: ratio(context_hits.len(), case.expected_primary_context.len()),
        impact_recall: ratio(impact_hits.len(), case.expected_impact.len()),
        test_recall: ratio(test_hits.len(), case.expected_tests.len()),
        boundary_precision: boundary_precision(&boundary_paths, &case.forbidden_paths),
        boundary_recall: ratio(boundary_hits.len(), case.expected_boundary.len()),
        confidence_expected_success: Some(expected_success),
        confidence_probability,
        confidence_calibration_error,
        expected_verdict: case.expected_verdict,
        actual_verdict,
        verification_correct,
        baseline_context_recall: ratio(
            baseline_context_hits.len(),
            case.expected_primary_context.len(),
        ),
        baseline_impact_recall: 0.0,
        baseline_test_recall: 0.0,
        context_hits,
        impact_hits,
        test_hits,
        boundary_hits,
        forbidden_boundary_hits,
        top_context_paths: context_paths,
        top_impact_paths: impact_paths,
        top_tests: test_names,
    })
}

fn summarize_workflow_cases(
    reports: &[WorkflowBenchCaseReport],
    baseline: bool,
) -> WorkflowBenchSummary {
    let count = reports.len() as f64;
    let verification = reports
        .iter()
        .filter_map(|case| case.verification_correct)
        .collect::<Vec<_>>();
    let calibration = reports
        .iter()
        .filter_map(|case| case.confidence_calibration_error)
        .collect::<Vec<_>>();
    WorkflowBenchSummary {
        context_recall_at_k: mean(
            reports
                .iter()
                .map(|case| {
                    if baseline {
                        case.baseline_context_recall
                    } else {
                        case.context_recall
                    }
                })
                .sum::<f64>(),
            count,
        ),
        impact_recall_at_k: mean(
            reports
                .iter()
                .map(|case| {
                    if baseline {
                        case.baseline_impact_recall
                    } else {
                        case.impact_recall
                    }
                })
                .sum::<f64>(),
            count,
        ),
        test_recall_at_k: mean(
            reports
                .iter()
                .map(|case| {
                    if baseline {
                        case.baseline_test_recall
                    } else {
                        case.test_recall
                    }
                })
                .sum::<f64>(),
            count,
        ),
        boundary_precision: if baseline {
            0.0
        } else {
            mean(
                reports
                    .iter()
                    .map(|case| case.boundary_precision)
                    .sum::<f64>(),
                count,
            )
        },
        boundary_recall: if baseline {
            0.0
        } else {
            mean(
                reports.iter().map(|case| case.boundary_recall).sum::<f64>(),
                count,
            )
        },
        confidence_calibration_error: if baseline {
            1.0
        } else {
            mean(calibration.iter().sum::<f64>(), calibration.len() as f64)
        },
        verification_verdict_accuracy: if baseline {
            0.0
        } else {
            mean(
                verification.iter().filter(|correct| **correct).count() as f64,
                verification.len() as f64,
            )
        },
    }
}

fn boundary_precision(selected: &[PathBuf], forbidden: &[String]) -> f64 {
    if selected.is_empty() {
        return 1.0;
    }
    let forbidden_hits = matching_expected_values(forbidden, selected).len();
    (selected.len().saturating_sub(forbidden_hits)) as f64 / selected.len() as f64
}

fn plan_success_probability(plan: &PlanReport) -> f64 {
    match plan.risk.level.as_str() {
        "low" => 0.85,
        "medium" => 0.6,
        "high" => 0.3,
        "critical" => 0.1,
        _ => 0.5,
    }
}

fn mean(sum: f64, count: f64) -> f64 {
    if count == 0.0 {
        1.0
    } else {
        sum / count
    }
}

fn print_workflow_bench_report(report: &WorkflowBenchReport) {
    println!(
        "Workflow benchmark: {} case(s), limit {}",
        report.case_count, report.limit
    );
    println!(
        "Workflow: context recall {:.3}, impact recall {:.3}, test recall {:.3}, boundary precision {:.3}, boundary recall {:.3}, calibration error {:.3}, verification accuracy {:.3}",
        report.workflow.context_recall_at_k,
        report.workflow.impact_recall_at_k,
        report.workflow.test_recall_at_k,
        report.workflow.boundary_precision,
        report.workflow.boundary_recall,
        report.workflow.confidence_calibration_error,
        report.workflow.verification_verdict_accuracy
    );
    println!(
        "Deltas vs baseline: context {:+.3}, impact {:+.3}, test {:+.3}, boundary precision {:+.3}, boundary recall {:+.3}, calibration {:+.3}, verification {:+.3}",
        report.deltas.context_recall_at_k,
        report.deltas.impact_recall_at_k,
        report.deltas.test_recall_at_k,
        report.deltas.boundary_precision,
        report.deltas.boundary_recall,
        report.deltas.confidence_calibration_error,
        report.deltas.verification_verdict_accuracy
    );
    for case in &report.cases {
        let verdict = case
            .actual_verdict
            .map(|verdict| format!("{verdict:?}"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {}: context {:.3}, impact {:.3}, tests {:.3}, boundary {:.3}/{:.3}, verdict {}",
            case.id,
            case.context_recall,
            case.impact_recall,
            case.test_recall,
            case.boundary_precision,
            case.boundary_recall,
            verdict
        );
    }
}

const REQUIRED_CONTRACT_BENCH_RULE_FAMILIES: [ContractBenchRuleFamily; 7] = [
    ContractBenchRuleFamily::AllowedEdit,
    ContractBenchRuleFamily::ForbiddenEdit,
    ContractBenchRuleFamily::MissingTests,
    ContractBenchRuleFamily::ArchitectureViolation,
    ContractBenchRuleFamily::DependencyDelta,
    ContractBenchRuleFamily::ApiSurfaceDelta,
    ContractBenchRuleFamily::ExplanationQuality,
];

fn run_contract_bench(args: ContractBenchArgs) -> anyhow::Result<ContractBenchReport> {
    let repo = absolutize(&args.path)?;
    let cases_file = resolve_contract_bench_cases_file(&repo, &args.cases_file)?;
    let cases = load_contract_bench_cases(&cases_file)?;
    validate_contract_bench_coverage(&cases)?;
    let limit = args.limit.clamp(1, 100);
    let mut reports = Vec::with_capacity(cases.len());

    for case in cases {
        let temp_repo = prepare_contract_bench_repo(&repo, &case.id)?;
        if !args.no_index {
            index_repo(&temp_repo.path)?;
        }
        let store = open_store(&temp_repo.path)?;
        let index_dir = default_index_dir(&temp_repo.path);
        let search_index = if TantivySearchIndex::exists(&index_dir) {
            Some(TantivySearchIndex::open_or_create(&index_dir)?)
        } else {
            None
        };
        let planner = PlanEngine::new(&store as &dyn OkStore)
            .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
            .with_history_store(Some(&store));
        let generation_started = Instant::now();
        let plan = workflow_plan(
            &temp_repo.path,
            &store,
            &planner,
            &case.task,
            limit,
            &cases_file,
        )?;
        let mut contract = ContractBuilder::from_plan(&plan)?;
        apply_contract_bench_overlay(&mut contract, &case.contract_overlay)?;
        let generation_ms = duration_ms(generation_started.elapsed());
        apply_contract_bench_edits(&temp_repo.path, &case)?;
        let verifier = ContractVerifier::new(&store as &dyn OkStore)
            .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex));
        let verification_started = Instant::now();
        let verification = verifier.verify(
            &temp_repo.path,
            &contract,
            VerifyChangeInput {
                changed_files: contract_bench_changed_files(&case),
                unified_diff: case.unified_diff.clone(),
                evidence_refs: Vec::new(),
                run_commands: false,
                write_attestation: false,
                validation_attestations: Vec::new(),
                traceability_strict: case.traceability_strict,
                check_api_surface: case.check_api_surface,
                check_dependency_delta: case.check_dependency_delta,
                architecture_policy: load_architecture_policy(&temp_repo.path)?,
            },
        )?;
        let verification_ms = duration_ms(verification_started.elapsed());
        reports.push(score_contract_bench_case(
            &case,
            &contract,
            &verification,
            generation_ms,
            verification_ms,
        )?);
    }

    let summary = summarize_contract_bench_cases(&reports);
    let rule_families = summarize_contract_bench_families(&reports);
    let failures = reports
        .iter()
        .filter(|case| !case.passed)
        .map(|case| case.id.clone())
        .collect::<Vec<_>>();
    Ok(ContractBenchReport {
        repo,
        cases_file,
        limit,
        case_count: reports.len(),
        summary,
        rule_families,
        failures,
        cases: reports,
    })
}

fn resolve_contract_bench_cases_file(repo: &Path, cases_file: &Path) -> anyhow::Result<PathBuf> {
    if cases_file.is_absolute() {
        return Ok(cases_file.to_path_buf());
    }
    let repo_relative = repo.join(cases_file);
    if repo_relative.exists() {
        return Ok(repo_relative);
    }
    absolutize(cases_file)
}

fn load_contract_bench_cases(path: &Path) -> anyhow::Result<Vec<ContractBenchCase>> {
    let raw = fs::read_to_string(path)?;
    let cases: Vec<ContractBenchCase> = serde_json::from_str(&raw)?;
    let mut seen = BTreeSet::new();
    for case in &cases {
        if case.id.trim().is_empty() || case.task.trim().is_empty() {
            anyhow::bail!("contract benchmark cases require non-empty id and task");
        }
        if !seen.insert(case.id.clone()) {
            anyhow::bail!("duplicate contract benchmark case id `{}`", case.id);
        }
        if case.changed_files.is_empty() && case.unified_diff.is_none() && case.edits.is_empty() {
            anyhow::bail!(
                "contract benchmark case `{}` requires changed_files, unified_diff, or edits",
                case.id
            );
        }
    }
    Ok(cases)
}

fn validate_contract_bench_coverage(cases: &[ContractBenchCase]) -> anyhow::Result<()> {
    let covered = cases
        .iter()
        .map(|case| case.rule_family)
        .collect::<BTreeSet<_>>();
    let missing = REQUIRED_CONTRACT_BENCH_RULE_FAMILIES
        .iter()
        .copied()
        .filter(|family| !covered.contains(family))
        .map(|family| format!("{family:?}"))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        anyhow::bail!(
            "contract benchmark cases missing required rule family coverage: {}",
            missing.join(", ")
        );
    }
    Ok(())
}

struct ContractBenchTempRepo {
    path: PathBuf,
}

impl Drop for ContractBenchTempRepo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn prepare_contract_bench_repo(
    repo: &Path,
    case_id: &str,
) -> anyhow::Result<ContractBenchTempRepo> {
    let stamp = chrono::Utc::now()
        .timestamp_nanos_opt()
        .unwrap_or_else(|| chrono::Utc::now().timestamp_micros());
    let path = std::env::temp_dir().join(format!(
        "open-kioku-contract-bench-{}-{}-{}",
        std::process::id(),
        stamp,
        sanitize_temp_path_fragment(case_id)
    ));
    copy_contract_bench_repo(repo, &path)?;
    Ok(ContractBenchTempRepo { path })
}

fn sanitize_temp_path_fragment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn copy_contract_bench_repo(source: &Path, destination: &Path) -> anyhow::Result<()> {
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    fs::create_dir_all(destination)?;
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(source)?;
        if relative.as_os_str().is_empty() || should_skip_contract_bench_copy(relative) {
            continue;
        }
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn should_skip_contract_bench_copy(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        matches!(name.as_ref(), ".git" | ".ok" | "target")
    })
}

fn apply_contract_bench_overlay(
    contract: &mut ChangeContractV1,
    overlay: &ContractBenchContractOverlay,
) -> anyhow::Result<()> {
    merge_contract_files(&mut contract.primary_files, &overlay.primary_files);
    merge_contract_files(&mut contract.secondary_files, &overlay.secondary_files);
    merge_contract_files(&mut contract.forbidden_files, &overlay.forbidden_files);
    remove_contract_files(&mut contract.primary_files, &overlay.forbidden_files);
    remove_contract_files(&mut contract.secondary_files, &overlay.forbidden_files);
    contract
        .api_surface_constraints
        .extend(overlay.api_surface_constraints.clone());
    contract
        .dependency_delta_constraints
        .extend(overlay.dependency_delta_constraints.clone());
    contract.validate().map_err(|err| {
        anyhow::anyhow!("contract benchmark overlay produced invalid contract: {err}")
    })
}

fn merge_contract_files(target: &mut Vec<ContractFile>, additions: &[ContractFile]) {
    for addition in additions {
        if !target
            .iter()
            .any(|current| same_normalized_contract_file(current, addition))
        {
            target.push(addition.clone());
        }
    }
}

fn remove_contract_files(target: &mut Vec<ContractFile>, removals: &[ContractFile]) {
    target.retain(|candidate| {
        !removals
            .iter()
            .any(|removal| same_normalized_contract_file(candidate, removal))
    });
}

fn same_normalized_contract_file(left: &ContractFile, right: &ContractFile) -> bool {
    normalize_path_fragment(left.as_str()) == normalize_path_fragment(right.as_str())
}

fn apply_contract_bench_edits(repo: &Path, case: &ContractBenchCase) -> anyhow::Result<()> {
    for edit in &case.edits {
        let path = repo.join(&edit.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &edit.content)?;
    }
    Ok(())
}

fn contract_bench_changed_files(case: &ContractBenchCase) -> Vec<PathBuf> {
    if !case.changed_files.is_empty() {
        return case.changed_files.clone();
    }
    case.edits.iter().map(|edit| edit.path.clone()).collect()
}

fn score_contract_bench_case(
    case: &ContractBenchCase,
    contract: &ChangeContractV1,
    verification: &ContractVerificationReport,
    generation_ms: f64,
    verification_ms: f64,
) -> anyhow::Result<ContractBenchCaseReport> {
    let contract_primary = contract_pathbufs(&contract.primary_files);
    let contract_allowed_boundary = contract_allowed_boundary_paths(contract);
    let contract_forbidden = contract_pathbufs(&contract.forbidden_files);
    let primary_file_hits =
        matching_expected_values(&case.expected_contract.primary_files, &contract_primary);
    let boundary_hits = matching_expected_values(
        &case.expected_contract.allowed_boundary,
        &contract_allowed_boundary,
    );
    let forbidden_boundary_hits = matching_expected_values(
        &case.expected_contract.forbidden_paths,
        &contract_allowed_boundary,
    );
    let forbidden_contract_hits =
        matching_expected_values(&case.expected_contract.forbidden_paths, &contract_forbidden);
    let mut missing_contract_fields = Vec::new();
    push_missing_expected_values(
        &mut missing_contract_fields,
        "primary_files",
        &case.expected_contract.primary_files,
        &primary_file_hits,
    );
    push_missing_expected_values(
        &mut missing_contract_fields,
        "allowed_boundary",
        &case.expected_contract.allowed_boundary,
        &boundary_hits,
    );
    push_missing_expected_values(
        &mut missing_contract_fields,
        "forbidden_files",
        &case.expected_contract.forbidden_paths,
        &forbidden_contract_hits,
    );
    if contract.required_tests.len() < case.expected_contract.min_required_tests {
        missing_contract_fields.push(format!(
            "required_tests: expected at least {}, got {}",
            case.expected_contract.min_required_tests,
            contract.required_tests.len()
        ));
    }
    if contract.traceability.len() < case.expected_contract.min_traceability {
        missing_contract_fields.push(format!(
            "traceability: expected at least {}, got {}",
            case.expected_contract.min_traceability,
            contract.traceability.len()
        ));
    }
    if contract.architecture_constraints.len() < case.expected_contract.min_architecture_constraints
    {
        missing_contract_fields.push(format!(
            "architecture_constraints: expected at least {}, got {}",
            case.expected_contract.min_architecture_constraints,
            contract.architecture_constraints.len()
        ));
    }
    if contract.evidence_refs.len() < case.expected_contract.min_evidence_refs {
        missing_contract_fields.push(format!(
            "evidence_refs: expected at least {}, got {}",
            case.expected_contract.min_evidence_refs,
            contract.evidence_refs.len()
        ));
    }

    let actual_finding_keys = contract_bench_finding_keys(verification);
    let finding_hits = matching_expected_strings(&case.expected_findings, &actual_finding_keys);
    let mut missing_findings = Vec::new();
    push_missing_expected_values(
        &mut missing_findings,
        "findings",
        &case.expected_findings,
        &finding_hits,
    );
    let explanation = render_contract_explain_markdown(&explain_contract(contract));
    let explanation_lower = explanation.to_ascii_lowercase();
    let explanation_hits = case
        .explanation_terms
        .iter()
        .filter(|term| explanation_lower.contains(&term.to_ascii_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    let mut missing_explanation_terms = Vec::new();
    push_missing_expected_values(
        &mut missing_explanation_terms,
        "explanation_terms",
        &case.explanation_terms,
        &explanation_hits,
    );
    let pretty_json = serde_json::to_string_pretty(contract)?;
    let toon = render_contract_toon(contract);
    let pretty_json_bytes = pretty_json.len();
    let toon_bytes = toon.len();
    let toon_reduction = if pretty_json_bytes == 0 {
        0.0
    } else {
        1.0 - (toon_bytes as f64 / pretty_json_bytes as f64)
    };
    let actual_verdict = verification.change_report.verdict;
    let verdict_correct = actual_verdict == case.expected_verdict;
    let boundary_precision = boundary_precision(
        &contract_allowed_boundary,
        &case.expected_contract.forbidden_paths,
    );
    let boundary_recall = ratio(
        boundary_hits.len(),
        case.expected_contract.allowed_boundary.len(),
    );
    let passed = verdict_correct
        && missing_contract_fields.is_empty()
        && missing_findings.is_empty()
        && missing_explanation_terms.is_empty()
        && forbidden_boundary_hits.is_empty();
    Ok(ContractBenchCaseReport {
        id: case.id.clone(),
        rule_family: case.rule_family,
        task: case.task.clone(),
        contract_id: contract.id.0.clone(),
        expected_verdict: case.expected_verdict,
        actual_verdict,
        verdict_correct,
        boundary_precision,
        boundary_recall,
        primary_file_hits,
        boundary_hits,
        forbidden_boundary_hits,
        missing_contract_fields,
        finding_hits,
        missing_findings,
        explanation_hits,
        missing_explanation_terms,
        pretty_json_bytes,
        toon_bytes,
        toon_reduction,
        generation_ms,
        verification_ms,
        passed,
    })
}

fn push_missing_expected_values(
    target: &mut Vec<String>,
    field: &str,
    expected: &[String],
    hits: &[String],
) {
    for value in expected {
        if !hits.iter().any(|hit| hit == value) {
            target.push(format!("{field}: missing `{value}`"));
        }
    }
}

fn contract_pathbufs(files: &[ContractFile]) -> Vec<PathBuf> {
    files
        .iter()
        .map(|file| PathBuf::from(file.as_str()))
        .collect()
}

fn contract_allowed_boundary_paths(contract: &ChangeContractV1) -> Vec<PathBuf> {
    contract
        .primary_files
        .iter()
        .chain(contract.secondary_files.iter())
        .map(|file| PathBuf::from(file.as_str()))
        .collect()
}

fn contract_bench_finding_keys(report: &ContractVerificationReport) -> Vec<String> {
    let mut keys = Vec::new();
    keys.extend(
        report
            .change_report
            .boundary_violations
            .iter()
            .map(|finding| finding.kind.clone()),
    );
    keys.extend(
        report
            .change_report
            .warnings
            .iter()
            .map(|finding| finding.kind.clone()),
    );
    keys.extend(
        report
            .change_report
            .missing_tests
            .iter()
            .map(|finding| finding.kind.clone()),
    );
    keys.extend(
        report
            .change_report
            .changed_impact
            .iter()
            .map(|finding| finding.kind.clone()),
    );
    keys.extend(
        report
            .change_report
            .api_surface_deltas
            .iter()
            .map(|finding| finding.kind.clone()),
    );
    keys.extend(
        report
            .change_report
            .dependency_deltas
            .iter()
            .map(|finding| {
                format!("dependency_delta:{:?}", finding.classification).to_ascii_lowercase()
            }),
    );
    keys.sort();
    keys.dedup();
    keys
}

fn summarize_contract_bench_cases(reports: &[ContractBenchCaseReport]) -> ContractBenchSummary {
    let count = reports.len() as f64;
    let verdict_correct = reports.iter().filter(|case| case.verdict_correct).count() as f64;
    let mut true_positives = 0;
    let mut false_positives = 0;
    let mut false_negatives = 0;
    for case in reports {
        let expected_positive = case.expected_verdict != VerificationVerdict::Pass;
        let actual_positive = case.actual_verdict != VerificationVerdict::Pass;
        match (expected_positive, actual_positive) {
            (true, true) => true_positives += 1,
            (false, true) => false_positives += 1,
            (true, false) => false_negatives += 1,
            (false, false) => {}
        }
    }
    let min_toon_reduction = reports
        .iter()
        .map(|case| case.toon_reduction)
        .fold(1.0, f64::min);
    ContractBenchSummary {
        verdict_accuracy: mean(verdict_correct, count),
        verification_precision: mean(
            true_positives as f64,
            (true_positives + false_positives) as f64,
        ),
        boundary_precision: mean(
            reports
                .iter()
                .map(|case| case.boundary_precision)
                .sum::<f64>(),
            count,
        ),
        boundary_recall: mean(
            reports.iter().map(|case| case.boundary_recall).sum::<f64>(),
            count,
        ),
        min_toon_reduction,
        mean_toon_reduction: mean(
            reports.iter().map(|case| case.toon_reduction).sum::<f64>(),
            count,
        ),
        mean_generation_ms: mean(
            reports.iter().map(|case| case.generation_ms).sum::<f64>(),
            count,
        ),
        mean_verification_ms: mean(
            reports.iter().map(|case| case.verification_ms).sum::<f64>(),
            count,
        ),
        true_positives,
        false_positives,
        false_negatives,
    }
}

fn summarize_contract_bench_families(
    reports: &[ContractBenchCaseReport],
) -> Vec<ContractBenchFamilyReport> {
    let mut grouped = BTreeMap::<ContractBenchRuleFamily, Vec<&ContractBenchCaseReport>>::new();
    for report in reports {
        grouped.entry(report.rule_family).or_default().push(report);
    }
    grouped
        .into_iter()
        .map(|(rule_family, cases)| {
            let count = cases.len() as f64;
            ContractBenchFamilyReport {
                rule_family,
                case_count: cases.len(),
                verdict_accuracy: mean(
                    cases.iter().filter(|case| case.verdict_correct).count() as f64,
                    count,
                ),
                boundary_precision: mean(
                    cases
                        .iter()
                        .map(|case| case.boundary_precision)
                        .sum::<f64>(),
                    count,
                ),
                boundary_recall: mean(
                    cases.iter().map(|case| case.boundary_recall).sum::<f64>(),
                    count,
                ),
            }
        })
        .collect()
}

fn print_contract_bench_report(report: &ContractBenchReport) {
    println!(
        "Contract benchmark: {} case(s), limit {}",
        report.case_count, report.limit
    );
    println!(
        "Summary: verdict accuracy {:.3}, verification precision {:.3}, boundary precision {:.3}, boundary recall {:.3}, min TOON reduction {:.3}, mean TOON reduction {:.3}",
        report.summary.verdict_accuracy,
        report.summary.verification_precision,
        report.summary.boundary_precision,
        report.summary.boundary_recall,
        report.summary.min_toon_reduction,
        report.summary.mean_toon_reduction
    );
    for family in &report.rule_families {
        println!(
            "  {:?}: {} case(s), verdict {:.3}, boundary {:.3}/{:.3}",
            family.rule_family,
            family.case_count,
            family.verdict_accuracy,
            family.boundary_precision,
            family.boundary_recall
        );
    }
    for case in &report.cases {
        println!(
            "  {}: {:?}, verdict {:?}/{:?}, boundary {:.3}/{:.3}, TOON {:+.1}%, {}",
            case.id,
            case.rule_family,
            case.actual_verdict,
            case.expected_verdict,
            case.boundary_precision,
            case.boundary_recall,
            case.toon_reduction * 100.0,
            if case.passed { "pass" } else { "fail" }
        );
    }
}

fn run_eval(args: EvalArgs) -> anyhow::Result<EvalReport> {
    let repo = absolutize(&args.path)?;
    let limit = args.limit.clamp(1, 100);
    let cases = load_eval_cases(&args.cases, args.cases_file.as_ref())?;
    if cases.is_empty() {
        anyhow::bail!("no eval cases provided; pass --case TASK=EXPECTED_PATH or --cases-file");
    }

    if !args.no_index {
        index_repo(&repo)?;
    }
    let store = open_store(&repo)?;
    let ranking_options = ranking_options_for_repo(&repo)?;
    let mut semantic_config = OkConfig::load_from_repo(&repo)?.semantic;
    semantic_config.enabled = true;
    let semantic_manager = SemanticIndexManager::new(&repo, &store, &semantic_config);
    let semantic_ready = semantic_manager.status().ready;
    let mut case_reports = Vec::with_capacity(cases.len());
    let mut recall_sum = 0.0;
    let mut mrr_sum = 0.0;
    let mut ndcg_sum = 0.0;
    let mut semantic_recall_sum = 0.0;
    let mut semantic_mrr_sum = 0.0;
    let mut semantic_ndcg_sum = 0.0;
    let mut baseline_recall_sum = 0.0;
    let mut baseline_mrr_sum = 0.0;
    let mut baseline_ndcg_sum = 0.0;
    let signals = ranking_ablation_signals();
    let mut ablation_sums = signals
        .iter()
        .map(|signal| (*signal, 0.0, 0.0, 0.0))
        .collect::<Vec<_>>();
    let mut context_recall_sum = 0.0;
    let mut test_recall_sum = 0.0;
    let mut abstention_required = 0usize;

    for case in cases {
        let mut raw_candidates =
            search_raw(&repo, &store, &case.task, ranking_candidate_limit(limit))?;
        let baseline_results = top_unique_paths(rerank_baseline(raw_candidates.clone()), limit);
        annotate_candidates_with_git_history(&store, &mut raw_candidates)?;
        let semantic_results = if semantic_ready {
            semantic_manager.search(&case.task, ranking_candidate_limit(limit))?
        } else {
            Vec::new()
        };
        if semantic_ready {
            raw_candidates.extend(semantic_results.clone());
        }
        let mut case_ranking_options = ranking_options.clone();
        case_ranking_options.query = Some(case.task.clone());
        let search_results = top_unique_paths_merging(
            rerank_with_options(raw_candidates.clone(), &case_ranking_options),
            limit,
        );
        let context = build_context_pack(&repo, &store, &case.task, limit)?;
        let search_paths = search_results
            .iter()
            .map(|result| result.path.clone())
            .collect::<Vec<_>>();
        let baseline_paths = baseline_results
            .iter()
            .map(|result| result.path.clone())
            .collect::<Vec<_>>();
        let context_paths = context
            .primary_files
            .iter()
            .chain(context.supporting_files.iter())
            .map(|result| result.path.clone())
            .collect::<Vec<_>>();
        let selected_tests = context
            .test_candidates
            .iter()
            .map(|test| test.name.clone())
            .collect::<Vec<_>>();

        let baseline_ranks = expected_path_ranks(&case.expected_paths, &baseline_paths);
        baseline_recall_sum += ratio(
            baseline_ranks.iter().filter(|rank| rank.is_some()).count(),
            case.expected_paths.len(),
        );
        baseline_mrr_sum += reciprocal_rank(&baseline_ranks);
        baseline_ndcg_sum += ndcg(&baseline_ranks, limit);

        if semantic_ready {
            let semantic_paths = semantic_results
                .iter()
                .map(|result| result.path.clone())
                .collect::<Vec<_>>();
            let semantic_ranks = expected_path_ranks(&case.expected_paths, &semantic_paths);
            semantic_recall_sum += ratio(
                semantic_ranks.iter().filter(|rank| rank.is_some()).count(),
                case.expected_paths.len(),
            );
            semantic_mrr_sum += reciprocal_rank(&semantic_ranks);
            semantic_ndcg_sum += ndcg(&semantic_ranks, limit);
        }

        let search_ranks = expected_path_ranks(&case.expected_paths, &search_paths);
        let search_hits = search_ranks.iter().filter(|rank| rank.is_some()).count();
        let search_recall = ratio(search_hits, case.expected_paths.len());
        recall_sum += search_recall;
        mrr_sum += reciprocal_rank(&search_ranks);
        ndcg_sum += ndcg(&search_ranks, limit);
        for (signal, recall, mrr, ndcg_value) in &mut ablation_sums {
            let mut ablation_options = case_ranking_options.clone();
            ablation_options.mode = RankingMode::WithoutSignal(*signal);
            let candidates = if *signal == RankingSignal::GitCochange {
                without_git_history_candidates(raw_candidates.clone())
            } else {
                raw_candidates.clone()
            };
            let ablated =
                top_unique_paths(rerank_with_options(candidates, &ablation_options), limit);
            let ablated_paths = ablated
                .iter()
                .map(|result| result.path.clone())
                .collect::<Vec<_>>();
            let ablated_ranks = expected_path_ranks(&case.expected_paths, &ablated_paths);
            *recall += ratio(
                ablated_ranks.iter().filter(|rank| rank.is_some()).count(),
                case.expected_paths.len(),
            );
            *mrr += reciprocal_rank(&ablated_ranks);
            *ndcg_value += ndcg(&ablated_ranks, limit);
        }

        let context_hits = matching_expected_values(&case.expected_paths, &context_paths);
        let context_recall = ratio(context_hits.len(), case.expected_paths.len());
        context_recall_sum += context_recall;

        let test_hits = matching_expected_strings(&case.expected_tests, &selected_tests);
        let test_recall = ratio(test_hits.len(), case.expected_tests.len());
        test_recall_sum += test_recall;

        let mut notes = Vec::new();
        if search_recall == 0.0 {
            notes.push("expected files were not found in top search results".into());
        }
        if context_recall == 0.0 {
            notes.push("expected files were not grounded in context pack".into());
        }
        if !case.expected_tests.is_empty() && test_recall == 0.0 {
            notes.push("expected tests were not selected".into());
        }
        let confidence = if search_recall > 0.0 && context_recall > 0.0 {
            "grounded"
        } else if search_results.is_empty() || context.primary_files.is_empty() {
            abstention_required += 1;
            "abstain"
        } else {
            abstention_required += 1;
            "weak"
        };

        case_reports.push(EvalCaseReport {
            task: case.task,
            expected_paths: case.expected_paths,
            expected_tests: case.expected_tests,
            search_ranks,
            context_hits,
            test_hits,
            top_search_paths: search_paths.into_iter().take(limit).collect(),
            top_context_paths: context_paths.into_iter().take(limit).collect(),
            top_search_signals: search_results
                .first()
                .map(|result| top_score_signals(result, 3))
                .unwrap_or_default(),
            confidence,
            notes,
        });
    }

    let count = case_reports.len() as f64;
    let fusion = RankingEvalSummary {
        mode: "fusion".into(),
        search_recall_at_k: recall_sum / count,
        search_mrr: mrr_sum / count,
        search_ndcg_at_k: ndcg_sum / count,
    };
    let baseline = RankingEvalSummary {
        mode: "baseline".into(),
        search_recall_at_k: baseline_recall_sum / count,
        search_mrr: baseline_mrr_sum / count,
        search_ndcg_at_k: baseline_ndcg_sum / count,
    };
    let semantic = semantic_ready.then(|| RankingEvalSummary {
        mode: "semantic".into(),
        search_recall_at_k: semantic_recall_sum / count,
        search_mrr: semantic_mrr_sum / count,
        search_ndcg_at_k: semantic_ndcg_sum / count,
    });
    let ablations = ablation_sums
        .into_iter()
        .map(|(signal, recall, mrr, ndcg_value)| {
            let recall_at_k = recall / count;
            let search_mrr = mrr / count;
            let ndcg_at_k = ndcg_value / count;
            RankingAblationReport {
                signal: ranking_signal_name(signal).into(),
                search_recall_at_k: recall_at_k,
                search_mrr,
                search_ndcg_at_k: ndcg_at_k,
                recall_delta_vs_fusion: fusion.search_recall_at_k - recall_at_k,
                mrr_delta_vs_fusion: fusion.search_mrr - search_mrr,
                ndcg_delta_vs_fusion: fusion.search_ndcg_at_k - ndcg_at_k,
            }
        })
        .collect::<Vec<_>>();
    Ok(EvalReport {
        repo,
        limit,
        case_count: case_reports.len(),
        summary: EvalSummary {
            search_recall_at_k: fusion.search_recall_at_k,
            search_mrr: fusion.search_mrr,
            search_ndcg_at_k: fusion.search_ndcg_at_k,
            context_recall_at_k: context_recall_sum / count,
            test_recall_at_k: test_recall_sum / count,
            abstention_required,
        },
        baseline,
        semantic,
        fusion,
        ablations,
        cases: case_reports,
    })
}

fn load_eval_cases(
    values: &[String],
    cases_file: Option<&PathBuf>,
) -> anyhow::Result<Vec<EvalCase>> {
    let mut cases = values
        .iter()
        .map(|value| {
            let (task, expected) = value.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("eval case must use TASK=EXPECTED_PATH[,EXPECTED_PATH]: {value}")
            })?;
            let expected_paths = expected
                .split(',')
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if task.trim().is_empty() || expected_paths.is_empty() {
                anyhow::bail!("eval task and expected paths must be non-empty: {value}");
            }
            Ok(EvalCase {
                task: task.trim().to_string(),
                expected_paths,
                expected_tests: Vec::new(),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if let Some(path) = cases_file {
        let raw = fs::read_to_string(path)?;
        let mut from_file: Vec<EvalCase> = serde_json::from_str(&raw)?;
        cases.append(&mut from_file);
    }
    Ok(cases)
}

fn expected_path_ranks(expected_paths: &[String], actual_paths: &[PathBuf]) -> Vec<Option<usize>> {
    expected_paths
        .iter()
        .map(|expected| {
            let expected = normalize_path_fragment(expected);
            actual_paths
                .iter()
                .position(|path| {
                    normalize_path_fragment(&path.to_string_lossy()).contains(&expected)
                })
                .map(|rank| rank + 1)
        })
        .collect()
}

fn matching_expected_values(expected: &[String], actual: &[PathBuf]) -> Vec<String> {
    expected
        .iter()
        .filter(|expected| {
            let expected = normalize_path_fragment(expected);
            actual
                .iter()
                .any(|path| normalize_path_fragment(&path.to_string_lossy()).contains(&expected))
        })
        .cloned()
        .collect()
}

fn matching_expected_strings(expected: &[String], actual: &[String]) -> Vec<String> {
    expected
        .iter()
        .filter(|expected| {
            let expected = expected.to_ascii_lowercase();
            actual
                .iter()
                .any(|value| value.to_ascii_lowercase().contains(&expected))
        })
        .cloned()
        .collect()
}

fn reciprocal_rank(ranks: &[Option<usize>]) -> f64 {
    ranks
        .iter()
        .flatten()
        .min()
        .map(|rank| 1.0 / *rank as f64)
        .unwrap_or(0.0)
}

fn ndcg(ranks: &[Option<usize>], limit: usize) -> f64 {
    if ranks.is_empty() {
        return 1.0;
    }
    let dcg = ranks
        .iter()
        .flatten()
        .filter(|rank| **rank <= limit)
        .map(|rank| 1.0 / ((*rank as f64) + 1.0).log2())
        .sum::<f64>();
    let ideal = (1..=ranks.len().min(limit))
        .map(|rank| 1.0 / ((rank as f64) + 1.0).log2())
        .sum::<f64>();
    if ideal == 0.0 {
        0.0
    } else {
        dcg / ideal
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn print_eval_report(report: &EvalReport) {
    println!("Open Kioku eval for {}", report.repo.display());
    println!(
        "Search recall@{} {:.3}, MRR {:.3}, nDCG@{} {:.3}",
        report.limit,
        report.summary.search_recall_at_k,
        report.summary.search_mrr,
        report.limit,
        report.summary.search_ndcg_at_k
    );
    println!(
        "Ranking baseline: recall@{} {:.3}, MRR {:.3}, nDCG@{} {:.3}",
        report.limit,
        report.baseline.search_recall_at_k,
        report.baseline.search_mrr,
        report.limit,
        report.baseline.search_ndcg_at_k
    );
    println!(
        "Ranking fusion: recall@{} {:.3}, MRR {:.3}, nDCG@{} {:.3}",
        report.limit,
        report.fusion.search_recall_at_k,
        report.fusion.search_mrr,
        report.limit,
        report.fusion.search_ndcg_at_k
    );
    if let Some(semantic) = &report.semantic {
        println!(
            "Ranking semantic-only: recall@{} {:.3}, MRR {:.3}, nDCG@{} {:.3}",
            report.limit,
            semantic.search_recall_at_k,
            semantic.search_mrr,
            report.limit,
            semantic.search_ndcg_at_k
        );
    }
    if !report.ablations.is_empty() {
        println!("Ranking ablations:");
        for ablation in &report.ablations {
            println!(
                "  - without {}: recall@{} {:.3} (delta {:+.3}), MRR {:.3} (delta {:+.3}), nDCG {:.3} (delta {:+.3})",
                ablation.signal,
                report.limit,
                ablation.search_recall_at_k,
                ablation.recall_delta_vs_fusion,
                ablation.search_mrr,
                ablation.mrr_delta_vs_fusion,
                ablation.search_ndcg_at_k,
                ablation.ndcg_delta_vs_fusion
            );
        }
    }
    println!(
        "Context recall@{} {:.3}, test recall@{} {:.3}, weak/abstain {}",
        report.limit,
        report.summary.context_recall_at_k,
        report.limit,
        report.summary.test_recall_at_k,
        report.summary.abstention_required
    );
    for case in &report.cases {
        println!("\n- {} [{}]", case.task, case.confidence);
        println!("  expected paths: {}", case.expected_paths.join(", "));
        println!("  ranks: {:?}", case.search_ranks);
        if !case.top_search_signals.is_empty() {
            println!(
                "  top ranking signals: {}",
                case.top_search_signals.join(", ")
            );
        }
        if !case.test_hits.is_empty() {
            println!("  test hits: {}", case.test_hits.join(", "));
        }
        for note in &case.notes {
            println!("  note: {note}");
        }
    }
}

fn verify_plan_evidence(report: &PlanReport, mode: EvidenceVerifyMode) -> anyhow::Result<()> {
    if mode == EvidenceVerifyMode::Off {
        return Ok(());
    }
    let missing = report
        .negative_evidence
        .iter()
        .filter(|item| item.confidence >= 0.70)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    for item in &missing {
        let next_probe = item
            .suggested_next_probe
            .as_deref()
            .unwrap_or("collect stronger evidence before editing");
        eprintln!(
            "negative evidence [{}]: {} (confidence {:.2}); next probe: {}",
            item.scope, item.reason, item.confidence, next_probe
        );
    }
    if mode == EvidenceVerifyMode::Fail {
        anyhow::bail!(
            "plan evidence verification failed: {} required evidence signal(s) missing",
            missing.len()
        );
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct BoundaryVerificationOutcome {
    changed_files: Vec<String>,
    warnings: Vec<String>,
    evidence_refs: Vec<String>,
}

fn load_saved_plan(path: &Path) -> anyhow::Result<PlanReport> {
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug, Serialize)]
struct ContractCreateOutput {
    contract_id: String,
    stored: bool,
    store_path: Option<PathBuf>,
    contract: ChangeContractV1,
}

#[derive(Debug, Serialize)]
struct ContractExplainOutput {
    contract_id: String,
    task: String,
    primary_files: Vec<String>,
    secondary_files: Vec<String>,
    forbidden_files: Vec<String>,
    architecture_constraints: Vec<String>,
    api_surface_constraints: Vec<String>,
    dependency_delta_constraints: Vec<String>,
    required_tests: Vec<String>,
    validation_commands: Vec<String>,
    traceability: Vec<String>,
    evidence_ref_count: usize,
}

fn handle_contract_command(
    json: bool,
    repo: &Path,
    command: ContractCommand,
) -> anyhow::Result<()> {
    match command {
        ContractCommand::Create {
            task,
            plan,
            plan_json,
            limit,
            no_store,
            format,
        } => {
            let store = open_store(repo)?;
            let plan = contract_plan_from_input(repo, &store, task, plan, plan_json, limit)?;
            let contract = ContractBuilder::from_plan(&plan)?;
            let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
            let stored = !no_store;
            if stored {
                contract_store.save(&contract)?;
            }
            let output = ContractCreateOutput {
                contract_id: contract.id.0.clone(),
                stored,
                store_path: stored.then(|| {
                    repo.join(".ok/contracts")
                        .join(format!("{}.json", contract.id.0))
                }),
                contract,
            };
            print_contract_create_output(&output, effective_contract_format(json, format))?;
        }
        ContractCommand::Verify {
            id,
            contract,
            contract_json,
            diff,
            git,
            since_plan,
            mut changed,
            evidence_refs,
            traceability_strict,
            check_api_surface,
            check_deps,
            run_commands,
            write_attestation,
            format,
        } => {
            let store = open_store(repo)?;
            let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
            let (contract, stored) =
                load_contract_input(&contract_store, id, contract, contract_json)?;
            if write_attestation && !stored {
                anyhow::bail!("--write-attestation requires a stored contract --id");
            }
            let unified_diff = if let Some(since) = since_plan.as_deref() {
                for change in changed_ranges_since(repo, since)? {
                    if let Some(path) = change.new_path.or(change.old_path) {
                        changed.push(path);
                    }
                }
                verify_diff_since(repo, diff.as_deref(), since)?
            } else {
                verify_diff_input(repo, diff.as_deref(), git)?
            };
            let index_dir = default_index_dir(repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(&index_dir)?)
            } else {
                None
            };
            let architecture_policy = load_architecture_policy(repo)?;
            let check_dependency_delta = check_deps || architecture_policy.is_some();
            let verification = ContractVerifier::new(&store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_contract_store(stored.then_some(&contract_store as &dyn ContractStore))
                .verify(
                    repo,
                    &contract,
                    VerifyChangeInput {
                        changed_files: changed,
                        unified_diff,
                        evidence_refs,
                        run_commands,
                        write_attestation,
                        validation_attestations: Vec::new(),
                        traceability_strict,
                        check_api_surface,
                        check_dependency_delta,
                        architecture_policy,
                    },
                )?;
            let failed = matches!(
                verification.decision,
                open_kioku_patch::VerificationDecision::Fail
            );
            print_contract_verification_output(
                &verification,
                effective_contract_format(json, format),
            )?;
            if failed {
                anyhow::bail!("contract verification failed");
            }
        }
        ContractCommand::Explain {
            id,
            contract,
            contract_json,
            format,
        } => {
            let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
            let (contract, _) = load_contract_input(&contract_store, id, contract, contract_json)?;
            let explanation = explain_contract(&contract);
            print_contract_explain_output(&explanation, effective_contract_format(json, format))?;
        }
        ContractCommand::Show { id, format } | ContractCommand::Export { id, format } => {
            let contract_store = FsContractStore::new(repo.join(".ok/contracts"));
            let contract = contract_store.load(&ContractId::new(id))?;
            print_contract_output(&contract, effective_contract_format(json, format))?;
        }
    }
    Ok(())
}

fn effective_contract_format(json: bool, format: ContractFormat) -> ContractFormat {
    if json {
        ContractFormat::Json
    } else {
        format
    }
}

fn contract_plan_from_input(
    repo: &Path,
    store: &SqliteStore,
    task: Option<String>,
    plan: Option<PathBuf>,
    plan_json: Option<String>,
    limit: usize,
) -> anyhow::Result<PlanReport> {
    match (task, plan, plan_json) {
        (None, Some(path), None) => load_saved_plan(&path),
        (None, None, Some(json)) => Ok(serde_json::from_str(&json)?),
        (Some(task), None, None) => {
            let context = build_context_pack(repo, store, &task, limit)?;
            let index_dir = default_index_dir(repo);
            let search_index = if TantivySearchIndex::exists(&index_dir) {
                Some(TantivySearchIndex::open_or_create(&index_dir)?)
            } else {
                None
            };
            Ok(PlanEngine::new(store as &dyn OkStore)
                .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
                .with_history_store(Some(store))
                .with_memory_facts(RepoMemoryStore::open_repo(repo)?.search(&task, 8)?)
                .plan_from_context(&task, limit, context)?)
        }
        _ => anyhow::bail!("provide exactly one of TASK, --plan, or --plan-json"),
    }
}

fn load_contract_input(
    store: &FsContractStore,
    id: Option<String>,
    contract: Option<PathBuf>,
    contract_json: Option<String>,
) -> anyhow::Result<(ChangeContractV1, bool)> {
    match (id, contract, contract_json) {
        (Some(id), None, None) => Ok((store.load(&ContractId::new(id))?, true)),
        (None, Some(path), None) => Ok((parse_contract_json(&fs::read_to_string(path)?)?, false)),
        (None, None, Some(json)) => Ok((parse_contract_json(&json)?, false)),
        _ => anyhow::bail!("provide exactly one of --id, --contract, or --contract-json"),
    }
}

fn parse_contract_json(json: &str) -> anyhow::Result<ChangeContractV1> {
    if let Ok(contract) = serde_json::from_str::<ChangeContractV1>(json) {
        return Ok(contract);
    }
    let record: StoredContractRecord = serde_json::from_str(json)?;
    Ok(record.contract)
}

fn explain_contract(contract: &ChangeContractV1) -> ContractExplainOutput {
    ContractExplainOutput {
        contract_id: contract.id.0.clone(),
        task: contract.task.clone(),
        primary_files: contract_files(&contract.primary_files),
        secondary_files: contract_files(&contract.secondary_files),
        forbidden_files: contract_files(&contract.forbidden_files),
        architecture_constraints: contract
            .architecture_constraints
            .iter()
            .map(|constraint| format!("{} ({:?})", constraint.rule, constraint.severity))
            .collect(),
        api_surface_constraints: contract
            .api_surface_constraints
            .iter()
            .map(|constraint| {
                format!(
                    "{} {:?} ({:?})",
                    constraint.scope, constraint.allowed_changes, constraint.severity
                )
            })
            .collect(),
        dependency_delta_constraints: contract
            .dependency_delta_constraints
            .iter()
            .map(|constraint| {
                format!(
                    "{} -> {} {:?} ({:?})",
                    constraint.source, constraint.target, constraint.action, constraint.severity
                )
            })
            .collect(),
        required_tests: contract
            .required_tests
            .iter()
            .map(|test| format!("{}: {}", test.target, test.reason))
            .collect(),
        validation_commands: contract
            .validation_commands
            .iter()
            .map(|command| format!("{}: {}", command.command, command.reason))
            .collect(),
        traceability: contract
            .traceability
            .iter()
            .map(|trace| format!("{}: {}", trace.field, trace.rationale))
            .collect(),
        evidence_ref_count: contract.evidence_refs.len(),
    }
}

fn contract_files(files: &[open_kioku_contract::ContractFile]) -> Vec<String> {
    files.iter().map(|file| file.as_str().to_string()).collect()
}

fn print_contract_create_output(
    output: &ContractCreateOutput,
    format: ContractFormat,
) -> anyhow::Result<()> {
    match format {
        ContractFormat::Json => println!("{}", serde_json::to_string_pretty(output)?),
        ContractFormat::Markdown => print!("{}", render_contract_create_markdown(output)),
        ContractFormat::Toon => print!("{}", render_contract_create_toon(output)),
    }
    Ok(())
}

fn print_contract_output(
    contract: &ChangeContractV1,
    format: ContractFormat,
) -> anyhow::Result<()> {
    match format {
        ContractFormat::Json => println!("{}", serde_json::to_string_pretty(contract)?),
        ContractFormat::Markdown => print!("{}", render_contract_markdown(contract)),
        ContractFormat::Toon => print!("{}", render_contract_toon(contract)),
    }
    Ok(())
}

fn print_contract_explain_output(
    explanation: &ContractExplainOutput,
    format: ContractFormat,
) -> anyhow::Result<()> {
    match format {
        ContractFormat::Json => println!("{}", serde_json::to_string_pretty(explanation)?),
        ContractFormat::Markdown => print!("{}", render_contract_explain_markdown(explanation)),
        ContractFormat::Toon => print!("{}", render_contract_explain_toon(explanation)),
    }
    Ok(())
}

fn print_contract_verification_output(
    report: &ContractVerificationReport,
    format: ContractFormat,
) -> anyhow::Result<()> {
    match format {
        ContractFormat::Json => println!("{}", serde_json::to_string_pretty(report)?),
        ContractFormat::Markdown => print!("{}", render_contract_verification_markdown(report)),
        ContractFormat::Toon => print!("{}", render_contract_verification_toon(report)),
    }
    Ok(())
}

fn render_contract_create_markdown(output: &ContractCreateOutput) -> String {
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

fn render_contract_create_toon(output: &ContractCreateOutput) -> String {
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
    out
}

fn render_contract_explain_markdown(explanation: &ContractExplainOutput) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# Contract Explanation `{}`\n\nTask: {}\n\n",
        explanation.contract_id, explanation.task
    ));
    push_markdown_list(&mut out, "Primary Files", &explanation.primary_files);
    push_markdown_list(
        &mut out,
        "Architecture Constraints",
        &explanation.architecture_constraints,
    );
    push_markdown_list(
        &mut out,
        "API Surface Constraints",
        &explanation.api_surface_constraints,
    );
    push_markdown_list(
        &mut out,
        "Dependency Delta Constraints",
        &explanation.dependency_delta_constraints,
    );
    push_markdown_list(&mut out, "Required Tests", &explanation.required_tests);
    push_markdown_list(
        &mut out,
        "Validation Commands",
        &explanation.validation_commands,
    );
    push_markdown_list(&mut out, "Traceability", &explanation.traceability);
    out.push_str(&format!(
        "\nEvidence refs: `{}`\n",
        explanation.evidence_ref_count
    ));
    out
}

fn render_contract_explain_toon(explanation: &ContractExplainOutput) -> String {
    let mut out = format!(
        "type: contract_explanation\nid: {}\ntask: {}\nevidence_ref_count: {}\n",
        explanation.contract_id, explanation.task, explanation.evidence_ref_count
    );
    push_toon_list(&mut out, "primary_files", &explanation.primary_files);
    push_toon_list(
        &mut out,
        "architecture_constraints",
        &explanation.architecture_constraints,
    );
    push_toon_list(
        &mut out,
        "api_surface_constraints",
        &explanation.api_surface_constraints,
    );
    push_toon_list(
        &mut out,
        "dependency_delta_constraints",
        &explanation.dependency_delta_constraints,
    );
    push_toon_list(&mut out, "traceability", &explanation.traceability);
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
        &finding_summaries(&report.change_report.boundary_violations),
    );
    push_markdown_list(
        &mut out,
        "Warnings",
        &finding_summaries(&report.change_report.warnings),
    );
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

fn finding_summaries(findings: &[open_kioku_patch::VerificationFinding]) -> Vec<String> {
    findings
        .iter()
        .map(|finding| format!("{}: {}", finding.kind, finding.reason))
        .collect()
}

fn verify_diff_input(
    repo: &Path,
    diff_path: Option<&Path>,
    include_git_diff: bool,
) -> anyhow::Result<Option<String>> {
    let mut diffs = Vec::new();
    if let Some(path) = diff_path {
        diffs.push(fs::read_to_string(path)?);
    }
    if include_git_diff {
        let output = ProcessCommand::new("git")
            .arg("-C")
            .arg(repo)
            .args(["diff", "--unified=0", "--no-ext-diff", "--relative", "HEAD"])
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "git diff failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        diffs.push(String::from_utf8(output.stdout)?);
    }
    if diffs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(diffs.join("\n")))
    }
}

fn verify_diff_since(
    repo: &Path,
    diff_path: Option<&Path>,
    since: &str,
) -> anyhow::Result<Option<String>> {
    let mut diffs = Vec::new();
    if let Some(path) = diff_path {
        diffs.push(fs::read_to_string(path)?);
    }
    let output = ProcessCommand::new("git")
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
    diffs.push(String::from_utf8(output.stdout)?);
    if diffs.is_empty() {
        Ok(None)
    } else {
        Ok(Some(diffs.join("\n")))
    }
}

fn changed_ranges_since(repo: &Path, since: &str) -> anyhow::Result<Vec<open_kioku_git::DiffFile>> {
    let changes = open_kioku_git::diff_unified_zero_since(repo, since)?;
    Ok(changes
        .into_iter()
        .filter(|change| change.old_path.is_some() || change.new_path.is_some())
        .collect())
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
    let score = change
        .rename_score
        .map(|score| format!(" rename_score={score}"))
        .unwrap_or_default();
    if ranges.is_empty() {
        format!("{:?} {}{}", change.status, path, score)
    } else {
        format!(
            "{:?} {} lines {}{}",
            change.status,
            path,
            ranges.join(","),
            score
        )
    }
}

fn print_verify_report(report: &ChangeVerificationReport) {
    println!("Verification: {:?}", report.verdict);
    println!("Changed files: {}", report.changed_files.len());
    for path in &report.changed_files {
        println!("  - {}", path.display());
    }
    if !report.changed_symbols.is_empty() {
        println!("Changed symbols:");
        for symbol in &report.changed_symbols {
            println!("  - {symbol}");
        }
    }
    if !report.traceability.is_empty() {
        println!("Traceability:");
        for trace in &report.traceability {
            let evidence = if trace.evidence_refs.is_empty() {
                "no direct evidence refs".into()
            } else {
                trace.evidence_refs.join(", ")
            };
            println!("  - {}: {} ({})", trace.field, trace.rationale, evidence);
        }
    }
    print_findings("Boundary failures", &report.boundary_violations);
    print_findings("Warnings", &report.warnings);
    print_findings("Missing tests", &report.missing_tests);
    print_findings("Changed impact", &report.changed_impact);
    print_findings("API surface deltas", &report.api_surface_deltas);
    if !report.dependency_deltas.is_empty() {
        println!("Dependency deltas:");
        for finding in &report.dependency_deltas {
            let source_path = finding
                .source_path
                .as_ref()
                .map(|path| path.as_str())
                .unwrap_or("<unknown>");
            let target_path = finding
                .target_path
                .as_ref()
                .map(|path| path.as_str())
                .unwrap_or(&finding.target);
            let evidence = if finding.evidence_refs.is_empty() {
                "no direct evidence refs".into()
            } else {
                finding
                    .evidence_refs
                    .iter()
                    .map(|reference| reference.0.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            println!(
                "  - {:?}: {} -> {} via {} ({}) [{}]",
                finding.classification,
                source_path,
                target_path,
                finding.edge_type,
                finding.reason,
                evidence
            );
        }
    }
    if !report.recommended_tests.is_empty() {
        println!("Recommended tests:");
        for test in &report.recommended_tests {
            let command = test.command.as_deref().unwrap_or("manual validation");
            println!("  - {} via {}", test.name, command);
        }
    }
    if !report.command_results.is_empty() {
        println!("Command results:");
        for result in &report.command_results {
            let attestation = result
                .attestation_id
                .as_deref()
                .map(|id| format!(" attestation={id}"))
                .unwrap_or_default();
            println!(
                "  - {}: {} ({:?}){}",
                result.command, result.status, result.exit_code, attestation
            );
        }
    }
    if let Some(path) = &report.validation_ledger_path {
        println!("Validation ledger: {}", path.display());
    }
}

fn print_findings(label: &str, findings: &[open_kioku_patch::VerificationFinding]) {
    if findings.is_empty() {
        return;
    }
    println!("{label}:");
    for finding in findings {
        let path = finding
            .path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "-".into());
        println!("  - [{}] {}: {}", finding.kind, path, finding.reason);
    }
}

fn verify_saved_plan_boundary(
    report: &PlanReport,
    changed: &[PathBuf],
    evidence_refs: &[String],
) -> anyhow::Result<BoundaryVerificationOutcome> {
    let boundary = &report.recommended_change_boundary;
    let allowed = boundary
        .allowed_files
        .iter()
        .map(|path| normalize_boundary_path(path))
        .collect::<std::collections::BTreeSet<_>>();
    let caution = boundary
        .caution_files
        .iter()
        .map(|path| normalize_boundary_path(path))
        .collect::<std::collections::BTreeSet<_>>();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let changed_files = changed
        .iter()
        .map(|path| normalize_boundary_path(path))
        .collect::<Vec<_>>();

    for path in &changed_files {
        if let Some(rule) = boundary
            .forbidden_rules
            .iter()
            .find(|rule| boundary_pattern_matches(&rule.pattern, path))
        {
            errors.push(format!(
                "forbidden boundary edit: {path} matches `{}` ({})",
                rule.pattern, rule.reason
            ));
            continue;
        }
        if allowed.contains(path) {
            continue;
        }
        if let Some(rule) = boundary
            .caution_rules
            .iter()
            .find(|rule| normalize_boundary_path(&rule.path) == *path)
        {
            warnings.push(format!(
                "caution boundary edit: {path} ({}) evidence: {}",
                rule.reason,
                rule.evidence_refs.join(", ")
            ));
            continue;
        }
        if caution.contains(path) {
            warnings.push(format!("caution boundary edit: {path}"));
            continue;
        }
        if evidence_refs.is_empty() {
            errors.push(format!(
                "out of saved plan boundary: {path}; boundary expansion requires explicit evidence via --evidence-ref"
            ));
        } else {
            warnings.push(format!(
                "expanded boundary for {path} with explicit evidence refs: {}",
                evidence_refs.join(", ")
            ));
        }
    }

    if !errors.is_empty() {
        anyhow::bail!("boundary verification failed:\n{}", errors.join("\n"));
    }

    Ok(BoundaryVerificationOutcome {
        changed_files,
        warnings,
        evidence_refs: evidence_refs.to_vec(),
    })
}

fn normalize_boundary_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    raw.trim_start_matches("./").to_string()
}

fn boundary_pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./").replace('\\', "/");
    if pattern == path {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        if let Some(middle) = prefix.strip_prefix("**/") {
            return path == middle
                || path.starts_with(&format!("{middle}/"))
                || path.contains(&format!("/{middle}/"));
        }
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if pattern.contains('*') {
        let mut remainder = path;
        for part in pattern.split('*').filter(|part| !part.is_empty()) {
            if let Some(index) = remainder.find(part) {
                remainder = &remainder[index + part.len()..];
            } else {
                return false;
            }
        }
        return true;
    }
    false
}

fn ranking_ablation_signals() -> Vec<RankingSignal> {
    vec![
        RankingSignal::TextRelevance,
        RankingSignal::ExactReference,
        RankingSignal::GraphProximity,
        RankingSignal::BoundaryFit,
        RankingSignal::RuntimeCorroboration,
        RankingSignal::GitCochange,
        RankingSignal::ValidationProximity,
        RankingSignal::MemorySignal,
        RankingSignal::SemanticSimilarity,
        RankingSignal::PathQuality,
    ]
}

fn ranking_signal_name(signal: RankingSignal) -> &'static str {
    match signal {
        RankingSignal::TextRelevance => "text_relevance",
        RankingSignal::ExactReference => "exact_reference",
        RankingSignal::GraphProximity => "graph_proximity",
        RankingSignal::BoundaryFit => "boundary_fit",
        RankingSignal::RuntimeCorroboration => "runtime_corroboration",
        RankingSignal::GitCochange => "git_cochange",
        RankingSignal::ValidationProximity => "validation_proximity",
        RankingSignal::MemorySignal => "memory_signal",
        RankingSignal::SemanticSimilarity => "semantic_similarity",
        RankingSignal::PathQuality => "path_quality",
    }
}

const DEFAULT_PROOF_TASKS: &[&str] = &[
    "authentication",
    "configuration",
    "tests",
    "security",
    "database",
    "api",
    "mcp",
    "context pack",
    "impact analysis",
    "search code",
    "symbol lookup",
    "release workflow",
    "npm package",
    "policy",
    "validation",
];

fn run_proof(args: ProveArgs) -> anyhow::Result<ProofReport> {
    let repo = absolutize(&args.path)?;
    let limit = args.limit.clamp(1, 100);
    let snapshot = index_repo(&repo)?;
    let store = open_store(&repo)?;
    let files = store.list_files(usize::MAX, 0)?;
    let languages = language_counts(&files);
    let tasks = if args.tasks.is_empty() {
        choose_proof_tasks(&repo, &store, 3)?
    } else {
        args.tasks
            .iter()
            .map(|task| task.trim())
            .filter(|task| !task.is_empty())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    if tasks.is_empty() {
        anyhow::bail!("no proof tasks were provided or discovered");
    }

    let index_dir = default_index_dir(&repo);
    let search_index = if TantivySearchIndex::exists(&index_dir) {
        Some(TantivySearchIndex::open_or_create(&index_dir)?)
    } else {
        None
    };
    let planner = PlanEngine::new(&store as &dyn OkStore)
        .with_search_index(search_index.as_ref().map(|idx| idx as &dyn SearchIndex))
        .with_history_store(Some(&store));
    let mut task_reports = Vec::with_capacity(tasks.len());
    for task in &tasks {
        let plan = planner.plan(task, limit)?;
        let top_results = search(&repo, &store, task, 5)?;
        task_reports.push(score_proof_task(
            &repo,
            task,
            &plan,
            &top_results,
            args.reveal_paths,
        ));
    }

    let scores = task_reports
        .iter()
        .map(|task| task.score)
        .collect::<Vec<_>>();
    let total = scores.iter().sum::<u32>();
    let tasks_scored = task_reports.len();
    let average_score = if tasks_scored > 0 {
        total as f64 / tasks_scored as f64
    } else {
        0.0
    };
    let pass_rate_70 = if tasks_scored > 0 {
        100.0 * scores.iter().filter(|score| **score >= 70).count() as f64 / tasks_scored as f64
    } else {
        0.0
    };

    Ok(ProofReport {
        repo: if args.reveal_paths {
            repo.display().to_string()
        } else {
            "local repository".into()
        },
        generated_by: "ok prove",
        privacy: ProofPrivacy {
            source_snippets_included: false,
            local_root_included: args.reveal_paths,
            path_mode: if args.reveal_paths {
                "repository_relative"
            } else {
                "redacted_shapes"
            },
        },
        summary: ProofSummary {
            indexed_files: snapshot.manifest.file_count,
            indexed_symbols: snapshot.manifest.symbol_count,
            indexed_chunks: snapshot.manifest.chunk_count,
            tasks_scored,
            average_score: round1(average_score),
            min_score: scores.iter().min().copied().unwrap_or(0),
            max_score: scores.iter().max().copied().unwrap_or(0),
            pass_rate_70: round1(pass_rate_70),
        },
        languages,
        tasks: task_reports,
        reproduce: reproduce_commands(&repo, &tasks, limit, args.reveal_paths),
        notes: vec![
            "The report includes metrics and path shapes only; it does not include source snippets.",
            "Scores measure whether Open Kioku returned grounded planning context, impact, validation, risk, and agent tool calls.",
            "Use --task to evaluate product-specific workflows and --reveal-paths when repository-relative paths are safe to share.",
        ],
    })
}

fn choose_proof_tasks(
    repo: &Path,
    store: &dyn MetadataStore,
    max_tasks: usize,
) -> anyhow::Result<Vec<String>> {
    let mut tasks = Vec::new();
    for candidate in DEFAULT_PROOF_TASKS {
        if !search(repo, store, candidate, 1)?.is_empty() {
            tasks.push((*candidate).to_string());
        }
        if tasks.len() >= max_tasks {
            return Ok(tasks);
        }
    }

    for symbol in store.list_symbols(None, max_tasks, 0)? {
        if !symbol.name.trim().is_empty() {
            tasks.push(symbol.name);
        }
        if tasks.len() >= max_tasks {
            break;
        }
    }
    tasks.sort();
    tasks.dedup();
    Ok(tasks)
}

fn score_proof_task(
    repo: &Path,
    task: &str,
    plan: &open_kioku_core::PlanReport,
    top_results: &[open_kioku_core::SearchResult],
    reveal_paths: bool,
) -> ProofTaskReport {
    let primary_paths = plan
        .primary_context
        .iter()
        .map(|result| result.path.as_path())
        .collect::<Vec<_>>();
    let existing_paths = primary_paths
        .iter()
        .filter(|path| repo.join(path).exists())
        .count();
    let source_context_count = primary_paths
        .iter()
        .filter(|path| is_source_path(path))
        .count();
    let impact_count = plan.impact.direct_impacts.len() + plan.impact.indirect_impacts.len();

    let mut checks = BTreeMap::new();
    checks.insert("primary_context", !plan.primary_context.is_empty());
    checks.insert(
        "paths_exist",
        !primary_paths.is_empty() && existing_paths == primary_paths.len(),
    );
    checks.insert("source_context", source_context_count > 0);
    checks.insert("impact_candidates", impact_count > 0);
    checks.insert("validation_candidates", !plan.validation.is_empty());
    checks.insert("agent_tool_calls", plan.tool_calls.len() >= 3);
    checks.insert("known_risk", plan.risk.level != "unknown");

    let mut score = 0;
    for (name, weight) in [
        ("primary_context", 25),
        ("paths_exist", 15),
        ("source_context", 15),
        ("impact_candidates", 15),
        ("validation_candidates", 15),
        ("agent_tool_calls", 10),
        ("known_risk", 5),
    ] {
        if checks.get(name).copied().unwrap_or(false) {
            score += weight;
        }
    }

    ProofTaskReport {
        task: task.into(),
        score,
        checks,
        primary_context_count: plan.primary_context.len(),
        source_context_count,
        impact_count,
        validation_count: plan.validation.len(),
        tool_call_count: plan.tool_calls.len(),
        risk_level: plan.risk.level.clone(),
        sample_paths: redact_paths(primary_paths, reveal_paths),
        top_search_paths: redact_paths(
            top_results
                .iter()
                .map(|result| result.path.as_path())
                .collect::<Vec<_>>(),
            reveal_paths,
        ),
    }
}

fn language_counts(files: &[open_kioku_core::File]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for file in files {
        *counts
            .entry(format!("{:?}", file.language).to_ascii_lowercase())
            .or_insert(0) += 1;
    }
    counts
}

fn is_source_path(path: &Path) -> bool {
    !is_doc_path(path) && !is_test_path(path)
}

fn is_doc_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("md" | "mdx" | "txt" | "rst")
    ) || path
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        == Some("docs")
}

fn is_test_path(path: &Path) -> bool {
    let value = normalize_path_fragment(&path.to_string_lossy());
    value.contains("/test")
        || value.contains("test/")
        || value.contains("/spec")
        || value.ends_with("_test.go")
        || value.ends_with(".test.ts")
        || value.ends_with(".spec.ts")
}

fn redact_paths(paths: Vec<&Path>, reveal_paths: bool) -> Vec<String> {
    let mut values = paths
        .into_iter()
        .map(|path| proof_path(path, reveal_paths))
        .collect::<Vec<_>>();
    values.sort();
    values.dedup();
    values.truncate(5);
    values
}

fn proof_path(path: &Path, reveal_paths: bool) -> String {
    if reveal_paths {
        return path.display().to_string();
    }
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("file");
    if path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .is_none()
    {
        return format!("**/*.{ext}");
    }
    let top = path
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("repo");
    format!("{top}/**/*.{ext}")
}

fn reproduce_commands(
    repo: &Path,
    tasks: &[String],
    limit: usize,
    reveal_paths: bool,
) -> Vec<String> {
    let repo_arg = if reveal_paths {
        repo.display().to_string()
    } else {
        "/path/to/repo".into()
    };
    let mut command = format!("ok prove {repo_arg} --limit {limit}");
    for task in tasks {
        command.push_str(" --task ");
        command.push_str(&shell_quote(task));
    }
    vec![
        format!("ok init {repo_arg}"),
        format!("ok index {repo_arg}"),
        command,
        format!("ok mcp install cursor --repo {repo_arg}"),
        format!("ok mcp install claude --repo {repo_arg}"),
    ]
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/' | '.'))
    {
        value.into()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn render_proof_markdown(report: &ProofReport) -> String {
    let mut out = String::new();
    out.push_str("# Open Kioku Proof\n\n");
    out.push_str("Generated by `ok prove` against a real local repository.\n\n");
    out.push_str("This report is designed to be shared: it records metrics and path shapes, not source snippets.\n\n");

    out.push_str("## Summary\n\n");
    out.push_str("| Metric | Value |\n");
    out.push_str("| --- | ---: |\n");
    out.push_str(&format!(
        "| Indexed files | {} |\n",
        report.summary.indexed_files
    ));
    out.push_str(&format!(
        "| Indexed symbols | {} |\n",
        report.summary.indexed_symbols
    ));
    out.push_str(&format!(
        "| Indexed chunks | {} |\n",
        report.summary.indexed_chunks
    ));
    out.push_str(&format!(
        "| Tasks scored | {} |\n",
        report.summary.tasks_scored
    ));
    out.push_str(&format!(
        "| Average proof score | {:.1}/100 |\n",
        report.summary.average_score
    ));
    out.push_str(&format!(
        "| Pass rate at 70+ | {:.1}% |\n",
        report.summary.pass_rate_70
    ));

    out.push_str("\n## Languages\n\n");
    for (language, count) in &report.languages {
        out.push_str(&format!("- `{language}`: {count}\n"));
    }

    out.push_str("\n## Task Scores\n\n");
    out.push_str("| Task | Score | Context | Impact | Validation | Risk | Sample paths |\n");
    out.push_str("| --- | ---: | ---: | ---: | ---: | --- | --- |\n");
    for task in &report.tasks {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            escape_table_cell(&task.task),
            task.score,
            task.primary_context_count,
            task.impact_count,
            task.validation_count,
            escape_table_cell(&task.risk_level),
            escape_table_cell(&task.sample_paths.join(", "))
        ));
    }

    out.push_str("\n## What Was Checked\n\n");
    out.push_str("- Primary context exists for each task.\n");
    out.push_str("- Returned paths exist in the indexed repository.\n");
    out.push_str("- At least one source file appears in context when available.\n");
    out.push_str(
        "- Impact candidates, validation candidates, risk, and agent tool calls are produced.\n",
    );

    out.push_str("\n## Reproduce\n\n");
    out.push_str("```sh\n");
    for command in &report.reproduce {
        out.push_str(command);
        out.push('\n');
    }
    out.push_str("```\n");

    out.push_str("\n## Privacy\n\n");
    out.push_str(&format!(
        "- Source snippets included: `{}`\n",
        report.privacy.source_snippets_included
    ));
    out.push_str(&format!(
        "- Local root included: `{}`\n",
        report.privacy.local_root_included
    ));
    out.push_str(&format!("- Path mode: `{}`\n", report.privacy.path_mode));
    out.push_str("\n---\n\nIf Open Kioku helps your AI coding workflow, please consider starring the repository:\nhttps://github.com/shivyadavus/open-kioku\n");
    out
}

fn escape_table_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn time_searches(
    iterations: usize,
    mut run: impl FnMut() -> open_kioku_errors::Result<()>,
) -> anyhow::Result<Vec<Duration>> {
    let mut times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let started = Instant::now();
        run()?;
        times.push(started.elapsed());
    }
    Ok(times)
}

fn median_duration(mut values: Vec<Duration>) -> Duration {
    values.sort();
    values[values.len() / 2]
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn normalize_path_fragment(value: &str) -> String {
    value.replace('\\', "/").to_ascii_lowercase()
}

fn build_context_pack(
    repo: &Path,
    store: &SqliteStore,
    task: &str,
    limit: usize,
) -> anyhow::Result<open_kioku_core::ContextPack> {
    let search_dir = default_index_dir(repo);
    let mut ranking_options = ranking_options_for_repo(repo)?;
    ranking_options.query = Some(task.into());
    let builder = ContextPackBuilder::new(store as &dyn OkStore)
        .with_history_store(Some(store))
        .with_ranking_options(ranking_options);
    let mut pack = if TantivySearchIndex::exists(&search_dir) {
        let index = TantivySearchIndex::open_or_create(&search_dir)?;
        let primary = search_context_candidates(&index, task, ranking_candidate_limit(limit))?;
        builder.build_from_primary(task, limit, primary)?
    } else {
        builder.build(task, limit)?
    };
    pack.architecture_policy = configured_architecture_policy_report(repo, store)?;
    Ok(pack)
}

fn search_context_candidates(
    index: &TantivySearchIndex,
    task: &str,
    limit: usize,
) -> anyhow::Result<Vec<SearchResult>> {
    let mut merged = std::collections::BTreeMap::<String, SearchResult>::new();
    for term in expanded_task_search_terms(task) {
        let mut results = index.search(&term, limit)?;
        for result in &mut results {
            if term != task {
                let evidence = format!("expanded task query `{term}` matched");
                if !result.evidence.contains(&evidence) {
                    result.evidence.push(evidence);
                }
                if !result.match_reason.contains(&term) {
                    result.match_reason =
                        format!("{}; expanded task query `{term}`", result.match_reason);
                }
            }
        }
        for result in results {
            merge_context_candidate(&mut merged, result);
        }
    }
    Ok(merged.into_values().collect())
}

fn merge_context_candidate(
    merged: &mut std::collections::BTreeMap<String, SearchResult>,
    result: SearchResult,
) {
    let key = search_result_key(&result);
    match merged.get_mut(&key) {
        Some(existing) => {
            if result.score > existing.score {
                existing.score = result.score;
                existing.snippet = result.snippet.clone();
                existing.line_range = result.line_range.clone();
                existing.symbol = result.symbol.clone();
            }
            for evidence in result.evidence {
                if !existing.evidence.contains(&evidence) {
                    existing.evidence.push(evidence);
                }
            }
            for evidence_ref in result.evidence_refs {
                if !existing.evidence_refs.contains(&evidence_ref) {
                    existing.evidence_refs.push(evidence_ref);
                }
            }
            if !existing.match_reason.contains(&result.match_reason) {
                existing.match_reason =
                    format!("{}; {}", existing.match_reason, result.match_reason);
            }
            for component in result.score_breakdown {
                existing.score_breakdown.push(component);
            }
            existing.reconcile_score_breakdown();
        }
        None => {
            merged.insert(key, result);
        }
    }
}

fn search_result_key(result: &SearchResult) -> String {
    format!(
        "{}:{}-{}",
        normalize_path_fragment(&result.path.to_string_lossy()),
        result
            .line_range
            .as_ref()
            .map(|range| range.start)
            .unwrap_or_default(),
        result
            .line_range
            .as_ref()
            .map(|range| range.end)
            .unwrap_or_default()
    )
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

fn ownership_components<S>(
    repo: &Path,
    store: &S,
    path: &Path,
) -> anyhow::Result<Vec<PolicyComponentMatch>>
where
    S: MetadataStore,
{
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

fn snapshot_export(repo: &Path, quality: SnapshotQuality) -> anyhow::Result<SnapshotExportReport> {
    let repo = absolutize(repo)?;
    let index_path = index_sqlite_path(&repo);
    if !index_path.exists() {
        anyhow::bail!(
            "index database is missing at {}; run `ok index` first",
            index_path.display()
        );
    }

    let artifact_dir = snapshot_artifact_dir(&repo);
    fs::create_dir_all(&artifact_dir)?;
    ensure_snapshot_gitattributes(&artifact_dir)?;
    checkpoint_sqlite(&index_path);

    let temp_db = unique_temp_path(&artifact_dir, "index.snapshot", "sqlite.tmp");
    match quality {
        SnapshotQuality::Best => {
            let conn = Connection::open_with_flags(&index_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| format!("opening {} read-only", index_path.display()))?;
            let temp_db_string = temp_db.to_string_lossy().to_string();
            conn.execute("VACUUM INTO ?1", params![temp_db_string])
                .with_context(|| {
                    format!("compacting snapshot database to {}", temp_db.display())
                })?;
        }
        SnapshotQuality::Fast => {
            fs::copy(&index_path, &temp_db).with_context(|| {
                format!(
                    "copying index database from {} to {}",
                    index_path.display(),
                    temp_db.display()
                )
            })?;
        }
    }

    integrity_check_sqlite(&temp_db)?;
    ensure_required_snapshot_tables(&temp_db)?;

    let manifest = read_manifest_from_sqlite(&temp_db)?
        .ok_or_else(|| anyhow::anyhow!("snapshot source database has no index manifest"))?;
    let graph_counts = read_graph_counts_from_sqlite(&temp_db)?;
    let sqlite_user_version = sqlite_user_version(&temp_db)?;
    let original_size_bytes = fs::metadata(&temp_db)?.len();

    let artifact_path = snapshot_artifact_path(&repo);
    let artifact_tmp = unique_temp_path(&artifact_dir, "index.snapshot", "zst.tmp");
    compress_file(&temp_db, &artifact_tmp, quality.compression_level())?;
    fs::rename(&artifact_tmp, &artifact_path).with_context(|| {
        format!(
            "promoting snapshot artifact {} to {}",
            artifact_tmp.display(),
            artifact_path.display()
        )
    })?;
    let compressed_size_bytes = fs::metadata(&artifact_path)?.len();

    let metadata = SnapshotMetadata {
        schema_version: SNAPSHOT_SCHEMA_VERSION.to_string(),
        sqlite_user_version,
        open_kioku_version: env!("CARGO_PKG_VERSION").to_string(),
        index_mode: manifest.index_mode.to_string(),
        repo_commit: manifest
            .repository
            .commit
            .clone()
            .or_else(|| open_kioku_git::commit(&repo))
            .unwrap_or_else(|| "unknown".to_string()),
        indexed_at: manifest.indexed_at.to_rfc3339(),
        file_count: manifest.file_count,
        symbol_count: manifest.symbol_count,
        chunk_count: manifest.chunk_count,
        graph_node_count: graph_counts.0,
        graph_edge_count: graph_counts.1,
        original_size_bytes,
        compressed_size_bytes,
        compression_level: quality.compression_level(),
        source_root_hash: source_root_hash(&repo),
        artifact_kind: SNAPSHOT_ARTIFACT_KIND.to_string(),
    };
    atomic_write_json(&snapshot_metadata_path(&repo), &metadata)?;
    let _ = fs::remove_file(&temp_db);

    Ok(SnapshotExportReport {
        ok: true,
        quality,
        artifact_path,
        metadata_path: snapshot_metadata_path(&repo),
        metadata,
    })
}

fn snapshot_import(repo: &Path) -> anyhow::Result<SnapshotImportReport> {
    let repo = absolutize(repo)?;
    let artifact_path = snapshot_artifact_path(&repo);
    let metadata_path = snapshot_metadata_path(&repo);
    let metadata = read_snapshot_metadata(&metadata_path)?;
    let warnings = validate_snapshot_metadata(&metadata)?;
    if !artifact_path.exists() {
        anyhow::bail!("snapshot artifact is missing: {}", artifact_path.display());
    }
    validate_compressed_snapshot_size(&artifact_path, &metadata)?;

    let artifact_dir = snapshot_artifact_dir(&repo);
    fs::create_dir_all(&artifact_dir)?;
    let temp_db = unique_temp_path(&artifact_dir, "index.snapshot.import", "sqlite.tmp");
    decompress_file(&artifact_path, &temp_db)?;
    validate_decompressed_snapshot_size(&temp_db, &metadata)?;
    integrity_check_sqlite(&temp_db)?;
    ensure_required_snapshot_tables(&temp_db)?;
    let temp_user_version = sqlite_user_version(&temp_db)?;
    if temp_user_version != metadata.sqlite_user_version {
        let _ = fs::remove_file(&temp_db);
        anyhow::bail!(
            "snapshot metadata user_version {} does not match database user_version {}",
            metadata.sqlite_user_version,
            temp_user_version
        );
    }
    let temp_manifest = read_manifest_from_sqlite(&temp_db)?;
    if temp_manifest.is_none() {
        let _ = fs::remove_file(&temp_db);
        anyhow::bail!("snapshot database has no index manifest");
    }

    let index_path = index_sqlite_path(&repo);
    promote_snapshot_db(&repo, &temp_db)?;
    let store = open_store(&repo)?;
    rebuild_search_from_store(&repo, &store)?;

    Ok(SnapshotImportReport {
        ok: true,
        imported: true,
        rebuilt_search: true,
        artifact_path,
        metadata_path,
        index_path,
        metadata,
        warnings,
    })
}

fn snapshot_doctor(repo: &Path) -> SnapshotDoctorReport {
    let repo = absolutize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let artifact_path = snapshot_artifact_path(&repo);
    let metadata_path = snapshot_metadata_path(&repo);
    let mut warnings = Vec::new();
    let mut errors = Vec::new();

    let metadata = match read_snapshot_metadata(&metadata_path) {
        Ok(metadata) => {
            match validate_snapshot_metadata(&metadata) {
                Ok(metadata_warnings) => warnings.extend(metadata_warnings),
                Err(err) => errors.push(err.to_string()),
            }
            Some(metadata)
        }
        Err(err) => {
            errors.push(err.to_string());
            None
        }
    };

    if !artifact_path.exists() {
        errors.push(format!(
            "snapshot artifact is missing: {}",
            artifact_path.display()
        ));
    } else {
        if let Some(metadata) = &metadata {
            if let Err(err) = validate_compressed_snapshot_size(&artifact_path, metadata) {
                errors.push(err.to_string());
            }
        }
        let artifact_dir = snapshot_artifact_dir(&repo);
        if let Err(err) = fs::create_dir_all(&artifact_dir) {
            errors.push(format!(
                "cannot create artifact temp directory {}: {err}",
                artifact_dir.display()
            ));
        } else {
            let temp_db = unique_temp_path(&artifact_dir, "index.snapshot.doctor", "sqlite.tmp");
            match decompress_file(&artifact_path, &temp_db)
                .and_then(|_| {
                    if let Some(metadata) = &metadata {
                        validate_decompressed_snapshot_size(&temp_db, metadata)?;
                    }
                    Ok(())
                })
                .and_then(|_| integrity_check_sqlite(&temp_db))
                .and_then(|_| ensure_required_snapshot_tables(&temp_db))
            {
                Ok(()) => {
                    if let Some(metadata) = &metadata {
                        match sqlite_user_version(&temp_db) {
                            Ok(user_version) if user_version != metadata.sqlite_user_version => {
                                errors.push(format!(
                                    "metadata user_version {} does not match artifact user_version {}",
                                    metadata.sqlite_user_version, user_version
                                ));
                            }
                            Ok(_) => {}
                            Err(err) => errors.push(err.to_string()),
                        }
                    }
                }
                Err(err) => errors.push(err.to_string()),
            }
            let _ = fs::remove_file(&temp_db);
        }
    }

    SnapshotDoctorReport {
        ok: errors.is_empty(),
        artifact_path,
        metadata_path,
        metadata,
        warnings,
        errors,
    }
}

fn print_snapshot_doctor_report(report: &SnapshotDoctorReport) {
    println!("Open Kioku snapshot doctor");
    println!(
        "{} metadata: {}",
        if report.metadata.is_some() {
            "[ok]  "
        } else {
            "[fail]"
        },
        report.metadata_path.display()
    );
    println!(
        "{} artifact: {}",
        if report.artifact_path.exists() {
            "[ok]  "
        } else {
            "[fail]"
        },
        report.artifact_path.display()
    );
    for warning in &report.warnings {
        println!("[warn] {warning}");
    }
    for error in &report.errors {
        println!("[fail] {error}");
    }
    if report.ok {
        println!("[ok]   snapshot is importable");
    }
}

fn snapshot_artifact_dir(repo: &Path) -> PathBuf {
    repo.join(".ok/artifacts")
}

fn snapshot_artifact_path(repo: &Path) -> PathBuf {
    snapshot_artifact_dir(repo).join("index.snapshot.zst")
}

fn snapshot_metadata_path(repo: &Path) -> PathBuf {
    snapshot_artifact_dir(repo).join("index.snapshot.json")
}

fn index_sqlite_path(repo: &Path) -> PathBuf {
    repo.join(".ok/index.sqlite")
}

fn workspace_graph_path(workspace: &Path) -> PathBuf {
    workspace.join(".ok/workspace.sqlite")
}

fn build_cross_project_workspace(workspace: &Path) -> anyhow::Result<WorkspaceLinkReport> {
    let (workspace_root, config_path, config) = load_workspace_config(workspace)?;
    let mut warnings = Vec::new();
    let mut projects = Vec::new();
    for project in &config.projects {
        projects.push(load_workspace_project(&workspace_root, project)?);
    }

    let mut workspace_nodes = HashMap::<String, GraphNode>::new();
    let mut workspace_edges = Vec::<GraphEdge>::new();
    let mut links = Vec::<WorkspaceLinkSummary>::new();
    let mut cap_hit = false;

    for source in &projects {
        for target in &projects {
            if source.name == target.name {
                continue;
            }
            link_boundary_edges(
                source,
                target,
                &source.calls,
                &target.exposes,
                GraphEdgeType::CallsEndpoint,
                "endpoint_path_protocol",
                &mut workspace_nodes,
                &mut workspace_edges,
                &mut links,
                &mut warnings,
                &mut cap_hit,
            );
            link_boundary_edges(
                source,
                target,
                &source.publishes,
                &target.consumes,
                GraphEdgeType::PublishesEvent,
                "topic_name",
                &mut workspace_nodes,
                &mut workspace_edges,
                &mut links,
                &mut warnings,
                &mut cap_hit,
            );
        }
    }

    let graph_path = workspace_graph_path(&workspace_root);
    let store = SqliteStore::open(&graph_path)?;
    let mut nodes = workspace_nodes.into_values().collect::<Vec<_>>();
    nodes.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    workspace_edges.sort_by(|a, b| a.id.0.cmp(&b.id.0));
    store.replace_graph(&nodes, &workspace_edges)?;

    let project_reports = projects
        .into_iter()
        .map(|project| WorkspaceProjectReport {
            name: project.name,
            repo: project.repo,
            index_path: project.index_path,
            graph_nodes: project.graph_node_count,
            graph_edges: project.graph_edge_count,
        })
        .collect::<Vec<_>>();

    Ok(WorkspaceLinkReport {
        ok: !cap_hit,
        workspace: workspace_root,
        config_path,
        graph_path,
        project_count: project_reports.len(),
        projects: project_reports,
        link_count: links.len(),
        links,
        cap: WORKSPACE_LINK_CAP,
        cap_hit,
        warnings,
    })
}

fn load_fleet_architecture_report(workspace: &Path) -> anyhow::Result<FleetArchitectureReport> {
    let (workspace_root, _config_path, config) = load_workspace_config(workspace)?;
    let graph_path = workspace_graph_path(&workspace_root);
    if !graph_path.exists() {
        anyhow::bail!(
            "workspace graph is missing at {}; run `ok index --mode cross-project --workspace {}`",
            graph_path.display(),
            workspace_root.display()
        );
    }
    let conn = Connection::open_with_flags(
        &graph_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening workspace graph {}", graph_path.display()))?;
    conn.execute_batch("PRAGMA query_only = ON;")?;
    let links = load_workspace_link_summaries(&conn)?;
    Ok(FleetArchitectureReport {
        ok: true,
        workspace: workspace_root,
        graph_path,
        project_count: config.projects.len(),
        link_count: links.len(),
        links,
        warnings: Vec::new(),
    })
}

fn load_workspace_config(workspace: &Path) -> anyhow::Result<(PathBuf, PathBuf, WorkspaceConfig)> {
    let workspace = absolutize(workspace)?;
    let (workspace_root, config_path) = if workspace.is_file() {
        let root = workspace
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        (root, workspace)
    } else {
        let candidates = [
            workspace.join("ok-workspace.toml"),
            workspace.join("workspace.toml"),
            workspace.join("ok.toml"),
        ];
        let config_path = candidates
            .into_iter()
            .find(|path| path.exists())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "workspace config not found in {}; expected ok-workspace.toml, workspace.toml, or ok.toml",
                    workspace.display()
                )
            })?;
        (workspace, config_path)
    };
    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("reading workspace config {}", config_path.display()))?;
    let parsed: WorkspaceToml = toml::from_str(&raw)
        .with_context(|| format!("parsing workspace config {}", config_path.display()))?;
    if parsed.workspace.projects.is_empty() {
        anyhow::bail!("workspace config {} has no projects", config_path.display());
    }
    let mut names = std::collections::HashSet::new();
    for project in &parsed.workspace.projects {
        if project.name.trim().is_empty() {
            anyhow::bail!("workspace project names must not be empty");
        }
        if !names.insert(project.name.clone()) {
            anyhow::bail!("duplicate workspace project name `{}`", project.name);
        }
    }
    Ok((workspace_root, config_path, parsed.workspace))
}

fn load_workspace_project(
    workspace_root: &Path,
    project: &WorkspaceProjectConfig,
) -> anyhow::Result<WorkspaceProjectGraph> {
    let repo = if project.repo.is_absolute() {
        project.repo.clone()
    } else {
        workspace_root.join(&project.repo)
    };
    let repo = absolutize(&repo)?;
    let index_path = index_sqlite_path(&repo);
    if !index_path.exists() {
        anyhow::bail!(
            "missing project index for `{}` at {}; run `ok index` in that project first",
            project.name,
            index_path.display()
        );
    }
    let conn = Connection::open_with_flags(
        &index_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening project index {}", index_path.display()))?;
    conn.execute_batch("PRAGMA query_only = ON;")?;

    let nodes = load_graph_nodes_by_id(&conn)?;
    let graph_node_count = nodes.len();
    let graph_edge_count = graph_row_count(&conn, "graph_edges")?;
    let exposes = load_boundary_edges(&conn, &nodes, GraphEdgeType::ExposesEndpoint)?;
    let calls = load_boundary_edges(&conn, &nodes, GraphEdgeType::CallsEndpoint)?;
    let publishes = load_boundary_edges(&conn, &nodes, GraphEdgeType::PublishesEvent)?;
    let consumes = load_boundary_edges(&conn, &nodes, GraphEdgeType::ConsumesEvent)?;

    Ok(WorkspaceProjectGraph {
        name: project.name.clone(),
        repo,
        index_path,
        graph_node_count,
        graph_edge_count,
        exposes,
        calls,
        publishes,
        consumes,
    })
}

fn load_graph_nodes_by_id(conn: &Connection) -> anyhow::Result<HashMap<String, GraphNode>> {
    let mut stmt = conn.prepare("SELECT json FROM graph_nodes ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut nodes = HashMap::new();
    while let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        let node: GraphNode = serde_json::from_str(&raw)?;
        nodes.insert(node.id.0.clone(), node);
    }
    Ok(nodes)
}

fn load_boundary_edges(
    conn: &Connection,
    nodes: &HashMap<String, GraphNode>,
    edge_type: GraphEdgeType,
) -> anyhow::Result<Vec<ProjectBoundaryEdge>> {
    let mut stmt = conn.prepare("SELECT json FROM graph_edges WHERE edge_type = ?1 ORDER BY id")?;
    let mut rows = stmt.query(params![format!("{:?}", edge_type)])?;
    let mut edges = Vec::new();
    while let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        let edge: GraphEdge = serde_json::from_str(&raw)?;
        let Some(source) = nodes.get(&edge.from.0).cloned() else {
            continue;
        };
        let Some(target) = nodes.get(&edge.to.0).cloned() else {
            continue;
        };
        edges.push(ProjectBoundaryEdge {
            edge,
            source,
            target,
        });
    }
    Ok(edges)
}

fn graph_row_count(conn: &Connection, table: &str) -> anyhow::Result<usize> {
    let sql = match table {
        "graph_edges" => "SELECT COUNT(*) FROM graph_edges",
        "graph_nodes" => "SELECT COUNT(*) FROM graph_nodes",
        _ => anyhow::bail!("unsupported graph count table {table}"),
    };
    let count: i64 = conn.query_row(sql, [], |row| row.get(0))?;
    Ok(count as usize)
}

#[allow(clippy::too_many_arguments)]
fn link_boundary_edges(
    source_project: &WorkspaceProjectGraph,
    target_project: &WorkspaceProjectGraph,
    sources: &[ProjectBoundaryEdge],
    targets: &[ProjectBoundaryEdge],
    edge_type: GraphEdgeType,
    strategy: &str,
    workspace_nodes: &mut HashMap<String, GraphNode>,
    workspace_edges: &mut Vec<GraphEdge>,
    links: &mut Vec<WorkspaceLinkSummary>,
    warnings: &mut Vec<String>,
    cap_hit: &mut bool,
) {
    for source in sources {
        if workspace_edges.len() >= WORKSPACE_LINK_CAP {
            *cap_hit = true;
            warnings.push(format!(
                "workspace link cap {} reached; additional cross-project edges were skipped",
                WORKSPACE_LINK_CAP
            ));
            return;
        }
        let matches = targets
            .iter()
            .filter(|candidate| boundary_targets_match(source, candidate, &edge_type))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            continue;
        }
        for candidate in matches
            .iter()
            .take(WORKSPACE_LINK_CAP - workspace_edges.len())
        {
            let ambiguity = workspace_ambiguity(source, matches.len());
            let confidence = if ambiguity.is_empty() {
                Confidence::High
            } else {
                Confidence::Medium
            };
            let source_node = upsert_workspace_node(
                workspace_nodes,
                &source_project.name,
                &source.source,
                &source_project.repo,
            );
            let target_node = upsert_workspace_node(
                workspace_nodes,
                &target_project.name,
                &candidate.source,
                &target_project.repo,
            );
            let target_label = boundary_target_label(source);
            let edge_id = workspace_edge_id(
                &edge_type,
                &source_project.name,
                &target_project.name,
                &source.edge.id,
                &candidate.edge.id,
                &source_node,
                &target_node,
                &target_label,
                strategy,
            );
            let mut properties = BTreeMap::new();
            properties.insert(
                "source_project".into(),
                serde_json::json!(source_project.name),
            );
            properties.insert(
                "target_project".into(),
                serde_json::json!(target_project.name),
            );
            properties.insert("target".into(), serde_json::json!(target_label.clone()));
            properties.insert("source_node".into(), serde_json::json!(source.edge.from.0));
            properties.insert(
                "target_node".into(),
                serde_json::json!(candidate.edge.from.0),
            );
            properties.insert(
                "source_endpoint_node".into(),
                serde_json::json!(source.edge.to.0),
            );
            properties.insert(
                "target_endpoint_node".into(),
                serde_json::json!(candidate.edge.to.0),
            );
            properties.insert("matching_strategy".into(), serde_json::json!(strategy));
            properties.insert(
                "confidence".into(),
                serde_json::json!(format!("{:?}", confidence)),
            );
            if let Some(file_id) = &candidate.source.file_id {
                properties.insert("target_file".into(), serde_json::json!(file_id.0));
            }
            if let Some(symbol_id) = &candidate.source.symbol_id {
                properties.insert("target_symbol".into(), serde_json::json!(symbol_id.0));
            }
            workspace_edges.push(GraphEdge {
                id: edge_id.clone(),
                from: source_node.clone(),
                to: target_node.clone(),
                edge_type: edge_type.clone(),
                properties,
                source_pass: Some("workspace_linker".into()),
                index_mode: Some(IndexMode::CrossProject.to_string()),
                ambiguity: ambiguity.clone(),
                evidence: Evidence {
                    id: EvidenceId::new(open_kioku_core::identity::stable_hash(&format!(
                        "workspace-link-evidence:{}",
                        edge_id.0
                    ))),
                    source: "open-kioku-workspace".into(),
                    source_type: EvidenceSourceType::StaticAnalysis,
                    file_range: source.edge.evidence.file_range.clone(),
                    symbol_id: source.edge.evidence.symbol_id.clone(),
                    confidence,
                    message: format!(
                        "{} links {} to {} via {}",
                        source_project.name, target_label, target_project.name, strategy
                    ),
                    indexed_at: chrono::Utc::now(),
                    confidence_score: None,
                    confidence_reason: Some(if ambiguity.is_empty() {
                        "single cross-project boundary match".into()
                    } else {
                        "ambiguous cross-project boundary match".into()
                    }),
                    freshness: None,
                },
                ..Default::default()
            });
            links.push(WorkspaceLinkSummary {
                source_project: source_project.name.clone(),
                target_project: target_project.name.clone(),
                source_node: source_node.0,
                target_node: target_node.0,
                target: target_label,
                edge_type: edge_type.clone(),
                matching_strategy: strategy.into(),
                confidence,
                ambiguity,
            });
        }
    }
}

fn boundary_targets_match(
    source: &ProjectBoundaryEdge,
    target: &ProjectBoundaryEdge,
    edge_type: &GraphEdgeType,
) -> bool {
    match edge_type {
        GraphEdgeType::CallsEndpoint => {
            let source_path = normalized_boundary_value(&source.target);
            let target_path = normalized_boundary_value(&target.target);
            if source_path.is_empty() || source_path != target_path {
                return false;
            }
            let source_protocol = string_property(&source.target, "protocol").unwrap_or("http");
            let target_protocol = string_property(&target.target, "protocol").unwrap_or("http");
            if source_protocol != target_protocol {
                return false;
            }
            let source_method = string_property(&source.target, "method");
            let target_method = string_property(&target.target, "method");
            source_method.is_none() || target_method.is_none() || source_method == target_method
        }
        GraphEdgeType::PublishesEvent => {
            let source_topic = normalized_boundary_value(&source.target);
            !source_topic.is_empty() && source_topic == normalized_boundary_value(&target.target)
        }
        _ => false,
    }
}

fn workspace_ambiguity(source: &ProjectBoundaryEdge, match_count: usize) -> Vec<String> {
    let mut ambiguity = source.edge.ambiguity.clone();
    if match_count > 1 {
        ambiguity.push(format!(
            "{match_count} candidate cross-project targets matched this boundary"
        ));
    }
    ambiguity.sort();
    ambiguity.dedup();
    ambiguity
}

fn upsert_workspace_node(
    nodes: &mut HashMap<String, GraphNode>,
    project: &str,
    node: &GraphNode,
    repo: &Path,
) -> NodeId {
    let workspace_id = workspace_node_id(project, &node.id);
    nodes.entry(workspace_id.0.clone()).or_insert_with(|| {
        let mut cloned = node.clone();
        cloned.id = workspace_id.clone();
        cloned
            .properties
            .insert("project".into(), serde_json::json!(project));
        cloned
            .properties
            .insert("repo".into(), serde_json::json!(repo.display().to_string()));
        cloned.index_mode = Some(IndexMode::CrossProject.to_string());
        cloned
    });
    workspace_id
}

fn workspace_node_id(project: &str, node_id: &NodeId) -> NodeId {
    NodeId::new(format!(
        "workspace:{}:{}",
        open_kioku_core::identity::stable_hash(project),
        node_id.0
    ))
}

#[allow(clippy::too_many_arguments)]
fn workspace_edge_id(
    edge_type: &GraphEdgeType,
    source_project: &str,
    target_project: &str,
    source_edge: &EdgeId,
    target_edge: &EdgeId,
    source_node: &NodeId,
    target_node: &NodeId,
    target: &str,
    strategy: &str,
) -> EdgeId {
    EdgeId::new(format!(
        "workspace-link:{}",
        open_kioku_core::identity::stable_hash(&format!(
            "{edge_type:?}:{source_project}:{target_project}:{}:{}:{}:{}:{target}:{strategy}",
            source_edge.0, target_edge.0, source_node.0, target_node.0
        ))
    ))
}

fn boundary_target_label(edge: &ProjectBoundaryEdge) -> String {
    let normalized = normalized_boundary_value(&edge.target);
    if normalized.is_empty() {
        edge.target.label.clone()
    } else {
        normalized
    }
}

fn normalized_boundary_value(node: &GraphNode) -> String {
    string_property(node, "normalized_path")
        .unwrap_or(node.label.as_str())
        .trim()
        .to_string()
}

fn string_property<'a>(node: &'a GraphNode, key: &str) -> Option<&'a str> {
    node.properties.get(key).and_then(|value| value.as_str())
}

fn load_workspace_link_summaries(conn: &Connection) -> anyhow::Result<Vec<WorkspaceLinkSummary>> {
    let mut stmt = conn
        .prepare("SELECT json FROM graph_edges WHERE source_type = 'StaticAnalysis' ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut links = Vec::new();
    while let Some(row) = rows.next()? {
        let raw: String = row.get(0)?;
        let edge: GraphEdge = serde_json::from_str(&raw)?;
        if edge.source_pass.as_deref() != Some("workspace_linker") {
            continue;
        }
        links.push(WorkspaceLinkSummary {
            source_project: edge
                .properties
                .get("source_project")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
                .to_string(),
            target_project: edge
                .properties
                .get("target_project")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
                .to_string(),
            source_node: edge.from.0,
            target_node: edge.to.0,
            target: edge
                .properties
                .get("target")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            edge_type: edge.edge_type,
            matching_strategy: edge
                .properties
                .get("matching_strategy")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
                .to_string(),
            confidence: edge.evidence.confidence,
            ambiguity: edge.ambiguity,
        });
    }
    Ok(links)
}

fn print_workspace_link_report(report: &WorkspaceLinkReport) {
    println!(
        "Linked {} cross-project boundary edge(s) across {} project(s)",
        report.link_count, report.project_count
    );
    println!("Workspace graph: {}", report.graph_path.display());
    for project in &report.projects {
        println!(
            "- {}: {} nodes, {} edges ({})",
            project.name,
            project.graph_nodes,
            project.graph_edges,
            project.repo.display()
        );
    }
    for link in &report.links {
        println!(
            "- {} -> {} {} via {} ({:?})",
            link.source_project,
            link.target_project,
            link.target,
            link.matching_strategy,
            link.confidence
        );
        for ambiguity in &link.ambiguity {
            println!("  caveat: {ambiguity}");
        }
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
}

fn print_fleet_architecture_report(report: &FleetArchitectureReport) {
    println!(
        "Fleet graph: {} cross-project link(s) across {} project(s)",
        report.link_count, report.project_count
    );
    println!("Workspace graph: {}", report.graph_path.display());
    for link in &report.links {
        println!(
            "- {} -> {} {} via {} ({:?})",
            link.source_project,
            link.target_project,
            link.target,
            link.matching_strategy,
            link.confidence
        );
        for ambiguity in &link.ambiguity {
            println!("  caveat: {ambiguity}");
        }
    }
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
}

fn unique_temp_path(dir: &Path, stem: &str, suffix: &str) -> PathBuf {
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    dir.join(format!(
        ".{stem}.{}.{}.{}",
        std::process::id(),
        nanos,
        suffix
    ))
}

fn ensure_snapshot_gitattributes(artifact_dir: &Path) -> anyhow::Result<()> {
    let path = artifact_dir.join(".gitattributes");
    if path.exists() {
        return Ok(());
    }
    fs::write(
        &path,
        "*.snapshot.zst binary -merge\n*.snapshot.json text\n",
    )
    .with_context(|| format!("writing {}", path.display()))
}

fn checkpoint_sqlite(path: &Path) {
    if !path.exists() {
        return;
    }
    if let Ok(conn) = Connection::open(path) {
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
    }
}

fn read_snapshot_metadata(path: &Path) -> anyhow::Result<SnapshotMetadata> {
    let file = File::open(path)
        .with_context(|| format!("snapshot metadata is missing: {}", path.display()))?;
    serde_json::from_reader(file)
        .with_context(|| format!("reading snapshot metadata {}", path.display()))
}

fn validate_snapshot_metadata(metadata: &SnapshotMetadata) -> anyhow::Result<Vec<String>> {
    if metadata.artifact_kind != SNAPSHOT_ARTIFACT_KIND {
        anyhow::bail!(
            "unsupported snapshot artifact kind {}; expected {}",
            metadata.artifact_kind,
            SNAPSHOT_ARTIFACT_KIND
        );
    }
    if metadata.schema_version != SNAPSHOT_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported snapshot schema version {}; expected {}",
            metadata.schema_version,
            SNAPSHOT_SCHEMA_VERSION
        );
    }
    if metadata.sqlite_user_version > SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION {
        anyhow::bail!(
            "snapshot sqlite user_version {} is newer than supported version {}",
            metadata.sqlite_user_version,
            SQLITE_SUPPORTED_INDEX_SCHEMA_VERSION
        );
    }
    let mut warnings = Vec::new();
    if metadata.open_kioku_version != env!("CARGO_PKG_VERSION") {
        warnings.push(format!(
            "snapshot was exported by Open Kioku {}; current binary is {}",
            metadata.open_kioku_version,
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(warnings)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)?;
    let tmp = unique_temp_path(parent, "index.snapshot.metadata", "json.tmp");
    {
        let mut file = File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        serde_json::to_writer_pretty(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("promoting metadata {} to {}", tmp.display(), path.display()))
}

fn compress_file(input: &Path, output: &Path, level: i32) -> anyhow::Result<()> {
    let mut reader = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let writer = File::create(output).with_context(|| format!("creating {}", output.display()))?;
    zstd::stream::copy_encode(&mut reader, writer, level)
        .with_context(|| format!("compressing {}", input.display()))?;
    Ok(())
}

fn decompress_file(input: &Path, output: &Path) -> anyhow::Result<()> {
    let mut reader = File::open(input).with_context(|| format!("opening {}", input.display()))?;
    let writer = File::create(output).with_context(|| format!("creating {}", output.display()))?;
    zstd::stream::copy_decode(&mut reader, writer)
        .with_context(|| format!("decompressing {}", input.display()))?;
    Ok(())
}

fn validate_compressed_snapshot_size(
    artifact_path: &Path,
    metadata: &SnapshotMetadata,
) -> anyhow::Result<()> {
    let actual = fs::metadata(artifact_path)
        .with_context(|| format!("reading metadata for {}", artifact_path.display()))?
        .len();
    if actual != metadata.compressed_size_bytes {
        anyhow::bail!(
            "snapshot compressed size mismatch: metadata says {} bytes, artifact is {} bytes",
            metadata.compressed_size_bytes,
            actual
        );
    }
    Ok(())
}

fn validate_decompressed_snapshot_size(
    db_path: &Path,
    metadata: &SnapshotMetadata,
) -> anyhow::Result<()> {
    let actual = fs::metadata(db_path)
        .with_context(|| format!("reading metadata for {}", db_path.display()))?
        .len();
    if actual != metadata.original_size_bytes {
        anyhow::bail!(
            "snapshot original size mismatch: metadata says {} bytes, artifact expands to {} bytes",
            metadata.original_size_bytes,
            actual
        );
    }
    Ok(())
}

fn integrity_check_sqlite(path: &Path) -> anyhow::Result<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for integrity check", path.display()))?;
    let result: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .with_context(|| format!("running integrity_check on {}", path.display()))?;
    if result != "ok" {
        anyhow::bail!(
            "sqlite integrity_check failed for {}: {result}",
            path.display()
        );
    }
    Ok(())
}

fn sqlite_user_version(path: &Path) -> anyhow::Result<i64> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for user_version", path.display()))?;
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .with_context(|| format!("reading sqlite user_version from {}", path.display()))
}

fn read_manifest_from_sqlite(path: &Path) -> anyhow::Result<Option<IndexManifest>> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for manifest read", path.display()))?;
    let raw: Option<String> = conn
        .query_row("SELECT json FROM manifests WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()
        .with_context(|| format!("reading index manifest from {}", path.display()))?;
    raw.map(|json| serde_json::from_str(&json).map_err(Into::into))
        .transpose()
}

fn read_graph_counts_from_sqlite(path: &Path) -> anyhow::Result<(usize, usize)> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for graph counts", path.display()))?;
    let nodes: usize = conn
        .query_row("SELECT COUNT(*) FROM graph_nodes", [], |row| row.get(0))
        .with_context(|| format!("counting graph nodes in {}", path.display()))?;
    let edges: usize = conn
        .query_row("SELECT COUNT(*) FROM graph_edges", [], |row| row.get(0))
        .with_context(|| format!("counting graph edges in {}", path.display()))?;
    Ok((nodes, edges))
}

fn ensure_required_snapshot_tables(path: &Path) -> anyhow::Result<()> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("opening {} for schema check", path.display()))?;
    let required = [
        "manifests",
        "files",
        "symbols",
        "chunks",
        "tests",
        "imports",
        "occurrences",
        "analysis_facts",
        "graph_nodes",
        "graph_edges",
    ];
    for table in required {
        let exists: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![table],
                |row| row.get(0),
            )
            .optional()
            .with_context(|| format!("checking for required table {table}"))?;
        if exists.is_none() {
            anyhow::bail!("snapshot database is missing required table `{table}`");
        }
    }
    Ok(())
}

fn promote_snapshot_db(repo: &Path, temp_db: &Path) -> anyhow::Result<()> {
    let ok_dir = repo.join(".ok");
    fs::create_dir_all(&ok_dir)?;
    let index_path = index_sqlite_path(repo);
    checkpoint_sqlite(&index_path);
    let backup_path = unique_temp_path(&ok_dir, "index.sqlite", "backup");
    let had_existing = index_path.exists();
    if had_existing {
        fs::rename(&index_path, &backup_path).with_context(|| {
            format!(
                "moving existing index {} to rollback backup {}",
                index_path.display(),
                backup_path.display()
            )
        })?;
    }

    if let Err(err) = fs::rename(temp_db, &index_path) {
        if had_existing {
            let _ = fs::rename(&backup_path, &index_path);
        }
        return Err(err).with_context(|| {
            format!(
                "promoting imported snapshot {} to {}",
                temp_db.display(),
                index_path.display()
            )
        });
    }
    remove_sqlite_sidecars(&index_path);

    match SqliteStore::open(&index_path) {
        Ok(_) => {
            if had_existing {
                let _ = fs::remove_file(&backup_path);
            }
            Ok(())
        }
        Err(err) => {
            let _ = fs::remove_file(&index_path);
            if had_existing {
                let _ = fs::rename(&backup_path, &index_path);
            }
            remove_sqlite_sidecars(&index_path);
            Err(anyhow::Error::from(err)).context("imported snapshot failed SQLite open")
        }
    }
}

fn remove_sqlite_sidecars(index_path: &Path) {
    let wal = index_path.with_extension("sqlite-wal");
    let shm = index_path.with_extension("sqlite-shm");
    let _ = fs::remove_file(wal);
    let _ = fs::remove_file(shm);
}

fn rebuild_search_from_store(repo: &Path, store: &SqliteStore) -> anyhow::Result<()> {
    let chunks = store.all_chunks()?;
    let files = store.list_files(usize::MAX, 0)?;
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    let graph_nodes = store.all_graph_nodes()?;
    rebuild_disk_index_with_graph(
        default_index_dir(repo),
        &chunks,
        &files,
        &symbols,
        &graph_nodes,
    )?;
    Ok(())
}

fn source_root_hash(repo: &Path) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"open-kioku-source-root-v1\0");
    let root = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    hasher.update(root.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    if let Some(commit) = open_kioku_git::commit(repo) {
        hasher.update(commit.as_bytes());
    }
    format!("{:x}", hasher.finalize())
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
    search_with_ranking_mode(repo, store, query, limit, RankingMode::Fusion)
}

fn graph_search(
    repo: impl AsRef<Path>,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let index_dir = default_index_dir(repo);
    if !TantivySearchIndex::exists(&index_dir) {
        anyhow::bail!("graph search index is missing; run `ok index .` first");
    }
    Ok(TantivySearchIndex::open_or_create(index_dir)?.search_graph(query, limit)?)
}

fn semantic_search(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let repo = repo.as_ref();
    let mut config = OkConfig::load_from_repo(repo)?;
    config.semantic.enabled = true;
    let manager = SemanticIndexManager::new(repo, store, &config.semantic);
    let mut results = manager.search(query, limit)?;
    let mut options = ranking_options_for_repo(repo)?;
    options.query = Some(query.into());
    Ok(top_unique_paths(
        rerank_with_options(results.split_off(0), &options),
        limit,
    ))
}

fn hybrid_search(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let repo = repo.as_ref();
    let candidate_limit = ranking_candidate_limit(limit);
    let mut raw = search_raw(repo, store, query, candidate_limit)?;
    annotate_candidates_with_git_history(store, &mut raw)?;

    let mut config = OkConfig::load_from_repo(repo)?;
    config.semantic.enabled = true;
    let manager = SemanticIndexManager::new(repo, store, &config.semantic);
    if manager.status().ready {
        raw.extend(manager.search(query, candidate_limit)?);
    }

    let mut options = ranking_options_for_repo(repo)?;
    options.query = Some(query.into());
    Ok(top_unique_paths_merging(
        rerank_with_options(raw, &options),
        limit,
    ))
}

fn search_with_ranking_mode(
    repo: impl AsRef<Path>,
    store: &dyn MetadataStore,
    query: &str,
    limit: usize,
    mode: RankingMode,
) -> anyhow::Result<Vec<open_kioku_core::SearchResult>> {
    let repo = repo.as_ref();
    let candidate_limit = ranking_candidate_limit(limit);
    let mut raw = search_raw(repo, store, query, candidate_limit)?;
    annotate_candidates_with_git_history(store, &mut raw)?;
    let mut options = ranking_options_for_repo(repo)?;
    options.mode = mode;
    options.query = Some(query.into());
    Ok(top_unique_paths(rerank_with_options(raw, &options), limit))
}

fn annotate_candidates_with_git_history(
    store: &dyn MetadataStore,
    results: &mut Vec<open_kioku_core::SearchResult>,
) -> anyhow::Result<()> {
    if results.is_empty() {
        return Ok(());
    }
    let facts = store.analysis_facts(Some(EvidenceSourceType::GitHistory), 10_000)?;
    if facts.is_empty() {
        return Ok(());
    }
    let files = store.list_files(usize::MAX, 0)?;
    let files_by_path = files
        .into_iter()
        .map(|file| (normalize_path_fragment(&file.path.to_string_lossy()), file))
        .collect::<std::collections::HashMap<_, _>>();
    let mut existing_paths = results
        .iter()
        .map(|result| normalize_path_fragment(&result.path.to_string_lossy()))
        .collect::<std::collections::HashSet<_>>();
    let mut additions = Vec::new();
    for result in &mut *results {
        let Some(file) =
            files_by_path.get(&normalize_path_fragment(&result.path.to_string_lossy()))
        else {
            continue;
        };
        let matched = facts
            .iter()
            .filter(|fact| fact.file_id == file.id)
            .take(32)
            .collect::<Vec<_>>();
        if matched.is_empty() {
            continue;
        }
        let displayed = matched.iter().copied().take(3).collect::<Vec<_>>();
        let evidence_ids = displayed
            .iter()
            .map(|fact| fact.id.clone())
            .collect::<Vec<_>>();
        let labels = displayed
            .iter()
            .map(|fact| fact.target.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        for fact in &displayed {
            let evidence = format!(
                "git co-change from local history: `{}` ({})",
                fact.target, fact.message
            );
            if !result.evidence.contains(&evidence) {
                result.evidence.push(evidence);
            }
        }
        for id in &evidence_ids {
            if !result.evidence_refs.contains(id) {
                result.evidence_refs.push(id.clone());
            }
        }
        result.score_breakdown.push(ScoreComponent::adjustment(
            "similar_change_overlap",
            (0.12 * matched.len() as f32).min(0.18),
            evidence_ids,
            format!("bounded local git history says this result co-changed with: {labels}"),
        ));
        for fact in matched {
            let target_path = normalize_path_fragment(&fact.target);
            if !existing_paths.insert(target_path.clone()) {
                continue;
            }
            let Some(target_file) = files_by_path.get(&target_path) else {
                continue;
            };
            let snippet = store
                .chunks_for_file(&target_file.id)?
                .first()
                .map(|chunk| chunk.text.clone())
                .unwrap_or_else(|| target_file.path.display().to_string());
            additions.push(open_kioku_core::SearchResult {
                path: target_file.path.clone(),
                line_range: None,
                snippet,
                symbol: None,
                score: 0.18 + (fact.confidence.score() * 0.05).min(0.05),
                match_reason: "historical git co-change candidate".into(),
                evidence: vec![format!(
                    "git co-change from local history: `{}` ({})",
                    fact.target, fact.message
                )],
                evidence_refs: vec![fact.id.clone()],
                confidence: fact.confidence.score(),
                score_breakdown: vec![ScoreComponent::single(
                    "similar_change_overlap",
                    0.18,
                    vec![fact.id.clone()],
                    "candidate added from bounded historical similar-change evidence",
                )],
            });
        }
    }
    results.extend(additions);
    Ok(())
}

fn without_git_history_candidates(
    results: Vec<open_kioku_core::SearchResult>,
) -> Vec<open_kioku_core::SearchResult> {
    results
        .into_iter()
        .filter(|result| result.match_reason != "historical git co-change candidate")
        .collect()
}

fn top_unique_paths(
    results: Vec<open_kioku_core::SearchResult>,
    limit: usize,
) -> Vec<open_kioku_core::SearchResult> {
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::with_capacity(limit);
    for result in results {
        let path = normalize_path_fragment(&result.path.to_string_lossy());
        if !seen.insert(path) {
            continue;
        }
        unique.push(result);
        if unique.len() == limit {
            break;
        }
    }
    unique
}

fn top_unique_paths_merging(
    results: Vec<open_kioku_core::SearchResult>,
    limit: usize,
) -> Vec<open_kioku_core::SearchResult> {
    let mut indexes = std::collections::HashMap::<String, usize>::new();
    let mut unique = Vec::<open_kioku_core::SearchResult>::with_capacity(limit);
    for result in results {
        let path = normalize_path_fragment(&result.path.to_string_lossy());
        if let Some(index) = indexes.get(&path).copied() {
            if !has_semantic_signal(&result) {
                continue;
            }
            let existing = &mut unique[index];
            for evidence in result.evidence {
                if !existing.evidence.contains(&evidence) {
                    existing.evidence.push(evidence);
                }
            }
            for evidence_ref in result.evidence_refs {
                if !existing.evidence_refs.contains(&evidence_ref) {
                    existing.evidence_refs.push(evidence_ref);
                }
            }
            for component in result.score_breakdown {
                if !existing
                    .score_breakdown
                    .iter()
                    .any(|existing| existing.signal == component.signal)
                {
                    existing.score_breakdown.push(component);
                }
            }
            existing.reconcile_score_breakdown();
            continue;
        }
        if unique.len() == limit {
            continue;
        }
        indexes.insert(path, unique.len());
        unique.push(result);
    }
    unique
}

fn has_semantic_signal(result: &open_kioku_core::SearchResult) -> bool {
    result
        .score_breakdown
        .iter()
        .any(|component| component.signal == "semantic_similarity")
}

fn search_raw(
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

fn ranking_candidate_limit(limit: usize) -> usize {
    limit.clamp(1, 100).saturating_mul(4).clamp(100, 200)
}

fn ranking_options_for_repo(repo: &Path) -> anyhow::Result<RankingOptions> {
    let config = OkConfig::load_from_repo(repo)?;
    Ok(RankingOptions {
        weights: ranking_weights_from_config(&config.ranking),
        mode: RankingMode::Fusion,
        query: None,
    })
}

fn ranking_weights_from_config(config: &RankingConfig) -> RankingWeights {
    RankingWeights {
        text_relevance: config.text_relevance,
        exact_reference: config.exact_reference,
        graph_proximity: config.graph_proximity,
        boundary_fit: config.boundary_fit,
        runtime_corroboration: config.runtime_corroboration,
        git_cochange: config.git_cochange,
        validation_proximity: config.validation_proximity,
        memory_signal: config.memory_signal,
        path_quality: config.path_quality,
        semantic_similarity: config.semantic_similarity,
    }
}

fn print_semantic_status(status: &open_kioku_semantic::SemanticStatus) {
    println!("# Open Kioku Semantic Status");
    println!("state: {}", status.state);
    println!("backend: {}", status.backend);
    println!("provider: {}", status.provider);
    println!("model: {}", status.model);
    println!("dimensions: {}", status.dimensions);
    println!("vectors: {}", status.vector_count);
    println!("indexed: {}", status.indexed_count);
    println!("stale: {}", status.stale_count);
    println!("failed: {}", status.failed_count);
    println!("disk_bytes: {}", status.disk_usage_bytes);
    if !status.notes.is_empty() {
        println!("notes:");
        for note in &status.notes {
            println!("- {note}");
        }
    }
}

fn resolve_provenance_symbol(store: &dyn MetadataStore, query: &str) -> anyhow::Result<Symbol> {
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
        [] if candidates.is_empty() => Err(anyhow::anyhow!("symbol not found: {query}")),
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
            Err(anyhow::anyhow!(
                "symbol query `{query}` is ambiguous; use a qualified name or symbol ID: {names}"
            ))
        }
    }
}

fn print_file_provenance(provenance: &FileProvenance) {
    println!("File provenance: {}", provenance.path.display());
    print_provenance_summary(
        provenance.first_seen.as_ref(),
        provenance.last_touched.as_ref(),
        &provenance.recent_touches,
        provenance.confidence,
        provenance.truncated,
        &provenance.uncertainty,
    );
}

fn print_symbol_provenance(provenance: &SymbolProvenance) {
    println!("Symbol provenance: {}", provenance.qualified_name);
    println!("File: {}", provenance.file_path.display());
    if let Some(range) = &provenance.range {
        println!("Current range: {}-{}", range.start, range.end);
    } else {
        println!("Current range: unavailable");
    }
    print_provenance_summary(
        provenance.first_seen.as_ref(),
        provenance.last_touched.as_ref(),
        &provenance.recent_touches,
        provenance.confidence,
        provenance.truncated,
        &provenance.uncertainty,
    );
}

fn print_similar_change_report(report: &SimilarChangeReport) {
    println!("Similar historical changes");
    println!("Generated at: {}", report.generated_at);
    if let Some(task) = &report.query.task {
        println!("Task: {task}");
    }
    if !report.query.paths.is_empty() {
        println!(
            "Paths: {}",
            report
                .query
                .paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !report.query.symbols.is_empty() {
        println!("Symbols: {}", report.query.symbols.join(", "));
    }
    if report.hits.is_empty() {
        println!("Hits: none");
    } else {
        println!("Hits:");
        for hit in &report.hits {
            println!(
                "- {} score={:.3} confidence={:?} {}",
                hit.change.commit.id, hit.score, hit.confidence, hit.change.commit.summary
            );
            if !hit.change.touched_paths.is_empty() {
                println!(
                    "  paths: {}",
                    hit.change
                        .touched_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !hit.change.touched_symbols.is_empty() {
                println!("  symbols: {}", hit.change.touched_symbols.join(", "));
            }
            for evidence in &hit.evidence {
                println!(
                    "  - {:?} +{:.3}: {}",
                    evidence.source_type, evidence.score, evidence.message
                );
            }
            for note in &hit.uncertainty {
                println!("  ! {note}");
            }
        }
    }
    if !report.uncertainty.is_empty() {
        println!("Uncertainty:");
        for note in &report.uncertainty {
            println!("- {note}");
        }
    }
}

fn print_churn_summary(summary: &ChurnSummary) {
    println!("Churn target: {:?} {}", summary.entity_kind, summary.key);
    if let Some(path) = &summary.path {
        println!("Path: {}", path.display());
    }
    if let Some(name) = &summary.qualified_name {
        println!("Symbol: {name}");
    }
    if let Some(symbol_id) = &summary.symbol_id {
        println!("Symbol ID: {symbol_id}");
    }
    println!("Generated at: {}", summary.generated_at);
    println!("Confidence: {:?}", summary.confidence);
    println!("Touches: {}", summary.stats.touch_count);
    println!("All time: {}", summary.stats.all_time);
    println!("Last 30d: {}", summary.stats.last_30d);
    println!("Last 90d: {}", summary.stats.last_90d);
    println!("Recency weighted: {:.3}", summary.stats.recency_weighted);
    println!("Hotspot score: {:.3}", summary.stats.hotspot_score);
    if !summary.uncertainty.is_empty() {
        println!("Uncertainty:");
        for note in &summary.uncertainty {
            println!("- {note}");
        }
    }
}

fn print_ownership_report(report: &OwnershipReport) {
    println!("Ownership target: {}", report.path.display());
    if !report.components.is_empty() {
        println!("Components:");
        for component in &report.components {
            println!(
                "- {} via {}",
                component.component_id, component.matched_glob
            );
        }
    }
    println!("Generated at: {}", report.generated_at);
    if report.owners.is_empty() {
        println!("Owners: none");
    } else {
        println!("Owners:");
        for suggestion in &report.owners {
            let email = suggestion
                .owner
                .email
                .as_deref()
                .map(|email| format!(" <{email}>"))
                .unwrap_or_default();
            let sources = suggestion
                .source_types
                .iter()
                .map(|source| format!("{source:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "- {}{} confidence={:?} score={:.3} stale={} sources=[{}]",
                suggestion.owner.name,
                email,
                suggestion.confidence,
                suggestion.score,
                suggestion.stale,
                sources
            );
            println!("  rationale: {}", suggestion.rationale);
            for evidence in &suggestion.evidence {
                println!(
                    "  - {:?} {} confidence={:?} stale={}",
                    evidence.source_type, evidence.source, evidence.confidence, evidence.stale
                );
            }
        }
    }
    if !report.uncertainty.is_empty() {
        println!("Uncertainty:");
        for note in &report.uncertainty {
            println!("- {note}");
        }
    }
}

fn print_reviewer_suggestion_report(report: &ReviewerSuggestionReport) {
    println!("Reviewer target: {}", report.path.display());
    println!("Generated at: {}", report.generated_at);
    println!("Availability: {:?}", report.availability);
    if report.suggestions.is_empty() {
        println!("Reviewer suggestions: none");
    } else {
        println!("Reviewer suggestions:");
        for suggestion in &report.suggestions {
            let email = suggestion
                .reviewer
                .email
                .as_deref()
                .map(|email| format!(" <{email}>"))
                .unwrap_or_default();
            let sources = suggestion
                .source_types
                .iter()
                .map(|source| format!("{source:?}"))
                .collect::<Vec<_>>()
                .join(", ");
            println!(
                "- {}{} confidence={:?} score={:.3} availability={:?} actual_review={} inferred_from_authors={} stale={} sources=[{}]",
                suggestion.reviewer.name,
                email,
                suggestion.confidence,
                suggestion.score,
                suggestion.availability,
                suggestion.actual_review_evidence,
                suggestion.inferred_from_authors,
                suggestion.stale,
                sources
            );
            println!("  rationale: {}", suggestion.rationale);
            for signal in &suggestion.signals {
                println!(
                    "  - {:?} {} confidence={:?} actual_review={} stale={}",
                    signal.source_type,
                    signal.source,
                    signal.confidence,
                    signal.actual_review_evidence,
                    signal.stale
                );
            }
        }
    }
    if !report.uncertainty.is_empty() {
        println!("Uncertainty:");
        for note in &report.uncertainty {
            println!("- {note}");
        }
    }
}

fn print_provenance_summary(
    first_seen: Option<&ProvenanceTouch>,
    last_touched: Option<&ProvenanceTouch>,
    recent_touches: &[ProvenanceTouch],
    confidence: Confidence,
    truncated: bool,
    uncertainty: &[String],
) {
    println!("Confidence: {confidence:?}");
    match first_seen {
        Some(touch) => println!("First seen: {}", format_provenance_touch(touch)),
        None => println!("First seen: unavailable"),
    }
    match last_touched {
        Some(touch) => println!("Last touched: {}", format_provenance_touch(touch)),
        None => println!("Last touched: unavailable"),
    }
    println!("Recent touches:");
    for touch in recent_touches {
        println!("- {}", format_provenance_touch(touch));
    }
    if recent_touches.is_empty() {
        println!("- none");
    }
    if truncated {
        println!("Results are truncated.");
    }
    if !uncertainty.is_empty() {
        println!("Uncertainty:");
        for note in uncertainty {
            println!("- {note}");
        }
    }
}

fn format_provenance_touch(touch: &ProvenanceTouch) -> String {
    let ranges = if touch.line_ranges.is_empty() {
        String::new()
    } else {
        format!(
            " lines {}",
            touch
                .line_ranges
                .iter()
                .map(|range| format!("{}-{}", range.start, range.end))
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    format!(
        "{} {} {} <{}> {:?}{} - {}",
        touch.commit.id,
        touch.commit.authored_at,
        touch.commit.author.name,
        touch.commit.author.email.as_deref().unwrap_or("unknown"),
        touch.change_kind,
        ranges,
        touch.commit.summary
    )
}

fn resolve_repo(global: &Path, command: PathBuf) -> PathBuf {
    if command == Path::new(".") {
        global.to_path_buf()
    } else {
        command
    }
}

fn normalize_to_repo_relative(repo_root: &Path, path: &Path) -> PathBuf {
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };

    let absolute_repo = std::fs::canonicalize(repo_root)
        .or_else(|_| absolutize(repo_root))
        .unwrap_or_else(|_| repo_root.to_path_buf());

    let absolute_path_canonical = std::fs::canonicalize(&absolute_path)
        .or_else(|_| absolutize(&absolute_path))
        .unwrap_or(absolute_path);

    if let Ok(rel) = absolute_path_canonical.strip_prefix(&absolute_repo) {
        rel.to_path_buf()
    } else if let Ok(rel) = absolute_path_canonical.strip_prefix(repo_root) {
        rel.to_path_buf()
    } else {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            let mut components = path.components();
            if let Some(std::path::Component::CurDir) = components.next() {
                components.as_path().to_path_buf()
            } else {
                path.to_path_buf()
            }
        }
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
