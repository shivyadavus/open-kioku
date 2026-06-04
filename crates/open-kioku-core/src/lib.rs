use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
        )]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_type!(RepositoryId);
id_type!(FileId);
id_type!(FileVersionId);
id_type!(SymbolId);
id_type!(NodeId);
id_type!(EdgeId);
id_type!(PatchId);
id_type!(EvidenceId);
id_type!(MemoryFactId);
id_type!(ContextHandleId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
    Exact,
}

impl Confidence {
    pub fn score(self) -> f32 {
        match self {
            Self::Low => 0.35,
            Self::Medium => 0.6,
            Self::High => 0.85,
            Self::Exact => 1.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    pub fn single(line: u32) -> Self {
        Self {
            start: line,
            end: line,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileRange {
    pub path: PathBuf,
    pub line_range: Option<LineRange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSourceType {
    TreeSitter,
    Scip,
    Lsp,
    Regex,
    Lexical,
    Semantic,
    Runtime,
    ExternalIntegration,
    Heuristic,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub id: EvidenceId,
    pub source: String,
    pub source_type: EvidenceSourceType,
    pub file_range: Option<FileRange>,
    pub symbol_id: Option<SymbolId>,
    pub confidence: Confidence,
    pub message: String,
    pub indexed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Repository {
    pub id: RepositoryId,
    pub name: String,
    pub root: PathBuf,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub indexed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Commit {
    pub sha: String,
    pub message: Option<String>,
    pub authored_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Branch {
    pub name: String,
    pub head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Rust,
    Java,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Yaml,
    Json,
    Toml,
    Sql,
    Markdown,
    Text,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct File {
    pub id: FileId,
    pub repository_id: RepositoryId,
    pub path: PathBuf,
    pub language: Language,
    pub size_bytes: u64,
    pub content_hash: String,
    pub is_generated: bool,
    pub is_vendor: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileVersion {
    pub id: FileVersionId,
    pub file_id: FileId,
    pub commit: Option<String>,
    pub content_hash: String,
    pub indexed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Module,
    Package,
    Class,
    Trait,
    Interface,
    Function,
    Method,
    Field,
    Variable,
    Constant,
    Endpoint,
    DatabaseTable,
    Test,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub qualified_name: String,
    pub kind: SymbolKind,
    pub file_id: FileId,
    pub range: Option<LineRange>,
    pub language: Language,
    pub confidence: Confidence,
    pub provenance: EvidenceSourceType,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SymbolOccurrence {
    pub symbol_id: SymbolId,
    pub file_id: FileId,
    pub range: Option<LineRange>,
    pub is_definition: bool,
    pub confidence: Confidence,
    pub provenance: EvidenceSourceType,
}

pub type Reference = SymbolOccurrence;
pub type Definition = SymbolOccurrence;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Import {
    pub file_id: FileId,
    pub imported: String,
    pub range: Option<LineRange>,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CodeChunk {
    pub id: String,
    pub file_id: FileId,
    pub range: LineRange,
    pub language: Language,
    pub text: String,
    pub symbol_id: Option<SymbolId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    pub severity: String,
    pub message: String,
    pub file_range: Option<FileRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TestTarget {
    pub id: String,
    pub name: String,
    pub file_id: FileId,
    pub range: Option<LineRange>,
    pub command: Option<String>,
    pub confidence: Confidence,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BuildTarget {
    pub id: String,
    pub name: String,
    pub command: String,
    pub files: Vec<FileId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeSignal {
    pub id: String,
    pub kind: String,
    pub message: String,
    pub file_range: Option<FileRange>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Owner {
    pub name: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArchitectureComponent {
    pub id: String,
    pub name: String,
    pub paths: Vec<String>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndexManifest {
    pub repository: Repository,
    pub file_count: usize,
    pub symbol_count: usize,
    pub chunk_count: usize,
    pub indexed_at: DateTime<Utc>,
    pub schema_version: u32,
    #[serde(default)]
    pub quality: IndexQuality,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct IndexQuality {
    pub scip_enabled: bool,
    pub scip_mode: String,
    pub scip_indexes_imported: usize,
    pub scip_symbols: usize,
    pub scip_occurrences: usize,
    pub scip_exact_references: usize,
    pub test_count: usize,
    pub import_count: usize,
    pub quality_notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeType {
    File,
    Directory,
    Module,
    Package,
    Class,
    Trait,
    Interface,
    Function,
    Method,
    Field,
    Endpoint,
    DatabaseTable,
    Collection,
    Queue,
    Topic,
    ConfigKey,
    Test,
    BuildTarget,
    RuntimeError,
    Ticket,
    PullRequest,
    ArchitectureComponent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GraphEdgeType {
    Contains,
    Defines,
    References,
    Calls,
    Implements,
    Extends,
    Imports,
    DependsOn,
    ExposesEndpoint,
    CallsEndpoint,
    ReadsConfig,
    WritesConfig,
    ReadsTable,
    WritesTable,
    PublishesEvent,
    ConsumesEvent,
    Tests,
    OwnedBy,
    ChangedBy,
    FailedIn,
    MentionedIn,
    RelatedToTicket,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphNode {
    pub id: NodeId,
    pub node_type: GraphNodeType,
    pub label: String,
    pub file_id: Option<FileId>,
    pub symbol_id: Option<SymbolId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphEdge {
    pub id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub edge_type: GraphEdgeType,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    pub path: PathBuf,
    pub line_range: Option<LineRange>,
    pub snippet: String,
    pub symbol: Option<Symbol>,
    pub score: f32,
    pub match_reason: String,
    pub evidence: Vec<String>,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityLink {
    pub kind: String,
    pub value: String,
    pub file_range: Option<FileRange>,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryFact {
    pub id: MemoryFactId,
    pub text: String,
    pub source: String,
    pub confidence: Confidence,
    pub entities: Vec<EntityLink>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemorySearchResult {
    pub fact: MemoryFact,
    pub score: f32,
    pub match_reason: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextHandle {
    pub id: ContextHandleId,
    pub kind: String,
    pub summary: String,
    pub file_range: Option<FileRange>,
    pub entities: Vec<EntityLink>,
    pub original_tokens_estimate: usize,
    pub compressed_tokens_estimate: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CompressedContextPack {
    pub task: String,
    pub summary: String,
    pub handles: Vec<ContextHandle>,
    pub original_tokens_estimate: usize,
    pub compressed_tokens_estimate: usize,
    pub compression_ratio: f32,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RiskReport {
    pub level: String,
    pub score: f32,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChangeBoundary {
    pub allowed_files: Vec<PathBuf>,
    pub caution_files: Vec<PathBuf>,
    pub forbidden_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ValidationPlan {
    pub commands: Vec<String>,
    pub tests: Vec<TestTarget>,
    pub requires_approval: bool,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImpactReport {
    pub target: String,
    pub direct_impacts: Vec<SearchResult>,
    pub indirect_impacts: Vec<SearchResult>,
    pub risk_report: RiskReport,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextPack {
    pub task: String,
    pub intent: String,
    pub primary_files: Vec<SearchResult>,
    pub primary_symbols: Vec<Symbol>,
    pub supporting_files: Vec<SearchResult>,
    pub dependency_edges: Vec<GraphEdge>,
    pub runtime_signals: Vec<RuntimeSignal>,
    pub test_candidates: Vec<TestTarget>,
    pub risk_report: RiskReport,
    pub recommended_change_boundary: ChangeBoundary,
    pub validation_plan: ValidationPlan,
    pub evidence: Vec<Evidence>,
    pub confidence_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallRecommendation {
    pub tool: String,
    pub purpose: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanReport {
    pub task: String,
    pub summary: String,
    pub primary_context: Vec<SearchResult>,
    pub relevant_symbols: Vec<Symbol>,
    pub impact: ImpactReport,
    pub validation: Vec<TestTarget>,
    pub risk: RiskReport,
    pub recommended_change_boundary: ChangeBoundary,
    pub recommended_next_steps: Vec<String>,
    pub tool_calls: Vec<ToolCallRecommendation>,
    pub memory_facts: Vec<MemorySearchResult>,
    pub evidence: Vec<Evidence>,
    pub confidence_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PatchPlan {
    pub id: PatchId,
    pub task: String,
    pub allowed_files: Vec<PathBuf>,
    pub caution_files: Vec<PathBuf>,
    pub forbidden_files: Vec<PathBuf>,
    pub change_steps: Vec<String>,
    pub risks: Vec<String>,
    pub assumptions: Vec<String>,
    pub tests: Vec<TestTarget>,
    pub rollback_notes: Vec<String>,
    pub unified_diff: Option<String>,
    pub requires_approval: bool,
    pub evidence: Vec<Evidence>,
}
