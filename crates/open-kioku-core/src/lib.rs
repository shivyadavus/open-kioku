use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

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
id_type!(GitCommitId);
id_type!(HistoryRecordId);

pub const HISTORY_SCHEMA_VERSION: u32 = 1;

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

    pub fn from_score(score: f32) -> Self {
        if score >= 0.95 {
            Self::Exact
        } else if score >= 0.75 {
            Self::High
        } else if score >= 0.55 {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ConfidenceBreakdown {
    pub overall_enum: Confidence,
    pub overall_score: f32,
    pub components: Vec<ScoreComponent>,
    pub blockers: Vec<String>,
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NegativeEvidence {
    pub query: String,
    pub scope: String,
    pub inspected_sources: Vec<String>,
    pub reason: String,
    pub confidence: f32,
    pub suggested_next_probe: Option<String>,
}

impl Default for ConfidenceBreakdown {
    fn default() -> Self {
        Self {
            overall_enum: Confidence::Low,
            overall_score: 0.0,
            components: Vec::new(),
            blockers: Vec::new(),
            caveats: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ConfidenceSignalInput {
    pub primary_file_count: usize,
    pub evidence_count: usize,
    pub exact_reference_count: usize,
    pub validation_count: usize,
    pub validation_with_command_count: usize,
    pub negative_evidence_count: usize,
    pub allowed_file_count: usize,
    pub runtime_signal_count: usize,
}

impl ConfidenceBreakdown {
    pub fn from_signals(input: ConfidenceSignalInput) -> Self {
        let mut blockers = Vec::new();
        let mut caveats = Vec::new();

        if input.primary_file_count == 0 {
            blockers.push("no primary context matched the task".into());
        }
        if input.negative_evidence_count > 0 {
            blockers.push(format!(
                "{} negative evidence signal(s) lowered confidence",
                input.negative_evidence_count
            ));
        }
        if input.exact_reference_count == 0 {
            caveats.push("exact symbol/reference evidence is absent".into());
        }
        if input.validation_count == 0 {
            caveats.push("no validation target was selected".into());
        } else if input.validation_with_command_count == 0 {
            caveats.push("validation targets require manual commands".into());
        }
        if input.runtime_signal_count == 0 {
            caveats.push("runtime corroboration is absent".into());
        }
        if input.allowed_file_count == 0 {
            caveats.push("change boundary has no allowed files".into());
        } else if input.allowed_file_count > 8 {
            caveats.push("change boundary is broad".into());
        }

        let evidence_target = input.primary_file_count.max(1) * 2;
        let evidence_density = if input.primary_file_count == 0 {
            0.0
        } else {
            (input.evidence_count as f32 / evidence_target.max(4) as f32).min(1.0)
        };
        if evidence_density < 0.5 {
            caveats.push("evidence density is thin".into());
        }

        let exact_reference = if input.exact_reference_count > 0 {
            1.0
        } else {
            0.25
        };
        let validation_availability = if input.validation_count > 0 { 1.0 } else { 0.2 };
        let negative_evidence = if input.negative_evidence_count == 0 {
            1.0
        } else if input.negative_evidence_count <= 2 {
            0.3
        } else {
            0.1
        };
        let boundary_tightness = if input.primary_file_count == 0 {
            0.0
        } else if input.allowed_file_count == 0 {
            0.3
        } else if input.allowed_file_count <= 3 {
            1.0
        } else if input.allowed_file_count <= 8
            && input.allowed_file_count <= input.primary_file_count.max(1) * 2
        {
            0.85
        } else {
            0.45
        };
        let runtime_corroboration = if input.runtime_signal_count > 0 {
            1.0
        } else {
            0.25
        };
        let test_coverage = if input.validation_count == 0 {
            0.2
        } else if input.validation_with_command_count > 0 {
            1.0
        } else {
            0.6
        };

        let mut components = vec![
            confidence_component(
                "evidence_density",
                evidence_density,
                0.20,
                "amount of independent indexed evidence near the selected context",
            ),
            confidence_component(
                "exact_references",
                exact_reference,
                0.20,
                "explicit exact symbol references or SCIP signals",
            ),
            confidence_component(
                "validation_availability",
                validation_availability,
                0.15,
                "presence of validation targets for the likely change",
            ),
            confidence_component(
                "negative_evidence",
                negative_evidence,
                0.15,
                "absence of low-confidence, missing-anchor, or no-match evidence",
            ),
            confidence_component(
                "boundary_tightness",
                boundary_tightness,
                0.15,
                "how narrowly allowed edit files bound the proposed change",
            ),
            confidence_component(
                "runtime_corroboration",
                runtime_corroboration,
                0.05,
                "runtime traces, incidents, or error signals that support the context",
            ),
            confidence_component(
                "test_coverage",
                test_coverage,
                0.10,
                "selected tests with runnable commands",
            ),
        ];
        components.sort_by(|a, b| a.signal.cmp(&b.signal));
        let mut overall_score = score_component_total(&components).clamp(0.0, 1.0);
        if input.primary_file_count == 0 {
            overall_score = overall_score.min(0.35);
        }
        if input.exact_reference_count == 0
            && input.validation_count == 0
            && input.runtime_signal_count == 0
        {
            overall_score = overall_score.min(0.55);
        }
        if input.negative_evidence_count > 0 {
            overall_score = overall_score.min(0.60);
        }

        blockers.sort();
        blockers.dedup();
        caveats.sort();
        caveats.dedup();
        if !caveats.is_empty() {
            overall_score = overall_score.min(0.94);
        }

        Self {
            overall_enum: Confidence::from_score(overall_score),
            overall_score,
            components,
            blockers,
            caveats,
        }
    }
}

fn confidence_component(
    signal: &'static str,
    value: f32,
    weight: f32,
    rationale: &'static str,
) -> ScoreComponent {
    ScoreComponent::new(
        signal,
        value,
        value,
        weight,
        value * weight,
        Vec::new(),
        rationale,
    )
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
    GitHistory,
    StaticAnalysis,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScoreComponent {
    pub signal: String,
    pub raw_value: f32,
    pub normalized_value: f32,
    pub weight: f32,
    pub contribution: f32,
    pub evidence_ids: Vec<String>,
    pub rationale: String,
}

impl ScoreComponent {
    pub fn new(
        signal: impl Into<String>,
        raw_value: f32,
        normalized_value: f32,
        weight: f32,
        contribution: f32,
        evidence_ids: Vec<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            signal: signal.into(),
            raw_value,
            normalized_value,
            weight,
            contribution,
            evidence_ids,
            rationale: rationale.into(),
        }
    }

    pub fn single(
        signal: impl Into<String>,
        score: f32,
        evidence_ids: Vec<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self::new(
            signal,
            score,
            score.clamp(0.0, 1.0),
            1.0,
            score,
            evidence_ids,
            rationale,
        )
    }

    pub fn adjustment(
        signal: impl Into<String>,
        contribution: f32,
        evidence_ids: Vec<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self::new(
            signal,
            contribution,
            contribution.clamp(-1.0, 1.0),
            1.0,
            contribution,
            evidence_ids,
            rationale,
        )
    }
}

pub fn score_component_total(components: &[ScoreComponent]) -> f32 {
    components
        .iter()
        .map(|component| component.contribution)
        .sum()
}

pub fn reconcile_score_breakdown(
    score: f32,
    components: &mut Vec<ScoreComponent>,
    fallback_signal: &str,
    evidence_ids: Vec<String>,
    rationale: &str,
) {
    if components.is_empty() {
        components.push(ScoreComponent::single(
            fallback_signal,
            score,
            evidence_ids,
            rationale,
        ));
        return;
    }

    let delta = score - score_component_total(components);
    if delta.abs() > 0.001 {
        components.push(ScoreComponent::adjustment(
            "score_reconciliation",
            delta,
            evidence_ids,
            format!("adjusted component total to match surfaced score: {rationale}"),
        ));
    }
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
pub struct AnalysisFact {
    pub id: String,
    pub file_id: FileId,
    pub symbol_id: Option<SymbolId>,
    pub target: String,
    pub target_kind: GraphNodeType,
    pub edge_type: GraphEdgeType,
    pub range: Option<LineRange>,
    pub confidence: Confidence,
    pub source: String,
    pub source_type: EvidenceSourceType,
    pub message: String,
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
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub score_breakdown: Vec<ScoreComponent>,
}

impl TestTarget {
    pub fn reconcile_score_breakdown(&mut self) {
        if self.evidence_refs.is_empty() {
            self.evidence_refs.push(format!("test:{}", self.id));
        }
        reconcile_score_breakdown(
            self.confidence.score(),
            &mut self.score_breakdown,
            "test_confidence",
            self.evidence_refs.clone(),
            &self.reason,
        );
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Owner {
    pub name: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerRole {
    Reviewer,
    Approver,
    Author,
    Committer,
    Owner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitCommitRecord {
    pub id: GitCommitId,
    #[serde(default)]
    pub parent_ids: Vec<GitCommitId>,
    pub author: Owner,
    pub committer: Option<Owner>,
    pub authored_at: DateTime<Utc>,
    pub committed_at: DateTime<Utc>,
    pub summary: String,
    pub message: String,
    pub file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitFileTouch {
    pub id: HistoryRecordId,
    pub commit_id: GitCommitId,
    pub path: PathBuf,
    pub previous_path: Option<PathBuf>,
    pub change_kind: GitChangeKind,
    pub additions: Option<u32>,
    pub deletions: Option<u32>,
    pub touched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GitSymbolTouch {
    pub id: HistoryRecordId,
    pub commit_id: GitCommitId,
    pub symbol_id: Option<SymbolId>,
    pub qualified_name: String,
    pub file_path: PathBuf,
    pub change_kind: GitChangeKind,
    #[serde(default)]
    pub line_ranges: Vec<LineRange>,
    #[serde(default = "default_history_confidence")]
    pub confidence: Confidence,
    #[serde(default)]
    pub uncertainty: Vec<String>,
    pub touched_at: DateTime<Utc>,
}

fn default_history_confidence() -> Confidence {
    Confidence::Low
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProvenanceTouch {
    pub commit: GitCommitRecord,
    pub path: PathBuf,
    pub previous_path: Option<PathBuf>,
    pub symbol_id: Option<SymbolId>,
    pub qualified_name: Option<String>,
    pub change_kind: GitChangeKind,
    #[serde(default)]
    pub line_ranges: Vec<LineRange>,
    pub confidence: Confidence,
    #[serde(default)]
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FileProvenance {
    pub path: PathBuf,
    pub first_seen: Option<ProvenanceTouch>,
    pub last_touched: Option<ProvenanceTouch>,
    #[serde(default)]
    pub recent_touches: Vec<ProvenanceTouch>,
    pub confidence: Confidence,
    pub truncated: bool,
    #[serde(default)]
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SymbolProvenance {
    pub symbol_id: SymbolId,
    pub qualified_name: String,
    pub file_path: PathBuf,
    pub range: Option<LineRange>,
    pub first_seen: Option<ProvenanceTouch>,
    pub last_touched: Option<ProvenanceTouch>,
    #[serde(default)]
    pub recent_touches: Vec<ProvenanceTouch>,
    pub confidence: Confidence,
    pub truncated: bool,
    #[serde(default)]
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GitCochangeEdge {
    pub id: HistoryRecordId,
    pub path: PathBuf,
    pub cochanged_path: PathBuf,
    pub commit_count: usize,
    pub recency_weight: f32,
    pub last_changed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub sample_commits: Vec<GitCommitId>,
    pub test_corun: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewerEvidence {
    pub id: HistoryRecordId,
    pub commit_id: Option<GitCommitId>,
    pub path: Option<PathBuf>,
    pub reviewer: Owner,
    pub role: ReviewerRole,
    pub observed_at: DateTime<Utc>,
    pub source: String,
    pub confidence: Confidence,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HistorySnapshot {
    pub schema_version: u32,
    #[serde(default)]
    pub commits: Vec<GitCommitRecord>,
    #[serde(default)]
    pub file_touches: Vec<GitFileTouch>,
    #[serde(default)]
    pub symbol_touches: Vec<GitSymbolTouch>,
    #[serde(default)]
    pub cochange_edges: Vec<GitCochangeEdge>,
    #[serde(default)]
    pub reviewer_evidence: Vec<ReviewerEvidence>,
}

impl HistorySnapshot {
    pub fn empty() -> Self {
        Self {
            schema_version: HISTORY_SCHEMA_VERSION,
            commits: Vec::new(),
            file_touches: Vec::new(),
            symbol_touches: Vec::new(),
            cochange_edges: Vec::new(),
            reviewer_evidence: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HistorySummary {
    pub path: PathBuf,
    pub recent_commits: Vec<GitCommitRecord>,
    pub file_touches: Vec<GitFileTouch>,
    pub symbol_touches: Vec<GitSymbolTouch>,
    pub cochange_neighbors: Vec<GitCochangeEdge>,
    pub reviewer_evidence: Vec<ReviewerEvidence>,
    pub truncated: bool,
    #[serde(default)]
    pub uncertainty: Vec<String>,
}

impl HistorySummary {
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            recent_commits: Vec::new(),
            file_touches: Vec::new(),
            symbol_touches: Vec::new(),
            cochange_neighbors: Vec::new(),
            reviewer_evidence: Vec::new(),
            truncated: false,
            uncertainty: vec!["no persisted history evidence is available for this path".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ArchitectureComponent {
    pub id: String,
    pub name: String,
    pub paths: Vec<String>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyComponentMatch {
    pub component_id: String,
    pub matched_glob: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResolvedArchitectureNode {
    pub file_path: PathBuf,
    pub symbol_id: Option<SymbolId>,
    pub components: Vec<PolicyComponentMatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnmappedPolicyTarget {
    pub file_path: PathBuf,
    pub symbol_id: Option<SymbolId>,
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
    #[serde(default)]
    pub build_systems: Vec<String>,
    #[serde(default)]
    pub codeql_databases: usize,
    #[serde(default)]
    pub coverage_reports: usize,
    #[serde(default)]
    pub junit_reports: usize,
    #[serde(default)]
    pub static_analysis_facts: usize,
    #[serde(default)]
    pub runtime_analysis_facts: usize,
    #[serde(default)]
    pub git_history_facts: usize,
    #[serde(default)]
    pub architecture_facts: usize,
    #[serde(default)]
    pub semantic_provider_notes: Vec<String>,
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
    BelongsTo,
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
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub confidence: f32,
    #[serde(default)]
    pub score_breakdown: Vec<ScoreComponent>,
}

impl SearchResult {
    pub fn derived_evidence_ids(&self) -> Vec<String> {
        if !self.evidence_refs.is_empty() {
            return self.evidence_refs.clone();
        }
        search_result_evidence_ids(&self.path, &self.line_range, self.evidence.len())
    }

    pub fn reconcile_score_breakdown(&mut self) {
        if self.evidence_refs.is_empty() {
            self.evidence_refs =
                search_result_evidence_ids(&self.path, &self.line_range, self.evidence.len());
        }
        reconcile_score_breakdown(
            self.score,
            &mut self.score_breakdown,
            "search_score",
            self.evidence_refs.clone(),
            &self.match_reason,
        );
    }

    pub fn add_score_component(&mut self, component: ScoreComponent) {
        self.score_breakdown.push(component);
    }
}

pub fn search_result_evidence_ids(
    path: &Path,
    line_range: &Option<LineRange>,
    evidence_len: usize,
) -> Vec<String> {
    let range = line_range
        .as_ref()
        .map(|range| format!("{}-{}", range.start, range.end))
        .unwrap_or_else(|| "unknown".into());
    let count = evidence_len.max(1);
    (0..count)
        .map(|index| format!("search:{}:{range}:{index}", path.display()))
        .collect()
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct BoundaryFileRule {
    pub path: PathBuf,
    pub reason: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub symbols: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct BoundaryForbiddenRule {
    pub pattern: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct BoundaryExpansionRequirement {
    pub reason: String,
    #[serde(default)]
    pub required_evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct BoundarySignalHooks {
    #[serde(default)]
    pub architecture_components: Vec<String>,
    #[serde(default)]
    pub ownership_sources: Vec<String>,
    #[serde(default)]
    pub cochange_sources: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ChangeBoundary {
    pub allowed_files: Vec<PathBuf>,
    pub caution_files: Vec<PathBuf>,
    pub forbidden_files: Vec<PathBuf>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub allowed_symbols: Vec<String>,
    #[serde(default)]
    pub allowed_rules: Vec<BoundaryFileRule>,
    #[serde(default)]
    pub caution_rules: Vec<BoundaryFileRule>,
    #[serde(default)]
    pub forbidden_rules: Vec<BoundaryForbiddenRule>,
    #[serde(default)]
    pub expansion_requirements: Vec<BoundaryExpansionRequirement>,
    #[serde(default)]
    pub signal_hooks: BoundarySignalHooks,
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
    #[serde(default)]
    pub score_breakdown: Vec<ScoreComponent>,
}

impl ImpactReport {
    pub fn reconcile_score_breakdown(&mut self) {
        reconcile_score_breakdown(
            self.risk_report.score,
            &mut self.score_breakdown,
            "impact_risk",
            self.evidence
                .iter()
                .map(|evidence| evidence.id.0.clone())
                .collect(),
            "impact risk score",
        );
        for result in &mut self.direct_impacts {
            result.reconcile_score_breakdown();
        }
        for result in &mut self.indirect_impacts {
            result.reconcile_score_breakdown();
        }
    }
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
    #[serde(default)]
    pub negative_evidence: Vec<NegativeEvidence>,
    pub confidence_summary: String,
    #[serde(default)]
    pub confidence_breakdown: ConfidenceBreakdown,
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
    #[serde(default)]
    pub runtime_signals: Vec<RuntimeSignal>,
    pub evidence: Vec<Evidence>,
    #[serde(default)]
    pub evidence_by_section: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub negative_evidence: Vec<NegativeEvidence>,
    pub confidence_summary: String,
    #[serde(default)]
    pub confidence_breakdown: ConfidenceBreakdown,
    #[serde(default)]
    pub score_breakdown: Vec<ScoreComponent>,
}

impl PlanReport {
    pub fn reconcile_score_breakdown(&mut self) {
        reconcile_score_breakdown(
            self.risk.score,
            &mut self.score_breakdown,
            "plan_risk",
            self.evidence
                .iter()
                .map(|evidence| evidence.id.0.clone())
                .collect(),
            "plan risk score",
        );
        for result in &mut self.primary_context {
            result.reconcile_score_breakdown();
        }
        self.impact.reconcile_score_breakdown();
        for test in &mut self.validation {
            test.reconcile_score_breakdown();
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{
        reconcile_score_breakdown, score_component_total, Confidence, ConfidenceBreakdown,
        ConfidenceSignalInput, GitChangeKind, GitCommitId, GitCommitRecord, GitFileTouch,
        GitSymbolTouch, HistoryRecordId, HistorySnapshot, HistorySummary, Owner, ScoreComponent,
        SymbolId, HISTORY_SCHEMA_VERSION,
    };
    use chrono::{TimeZone, Utc};

    #[test]
    fn reconciliation_adds_delta_to_match_surfaced_score() {
        let mut components = vec![ScoreComponent::single(
            "base",
            0.4,
            vec!["ev:base".into()],
            "base signal",
        )];

        reconcile_score_breakdown(
            0.65,
            &mut components,
            "fallback",
            vec!["ev:adjust".into()],
            "test score",
        );

        assert_eq!(components.len(), 2);
        assert!((score_component_total(&components) - 0.65).abs() < 0.001);
        assert_eq!(components[1].signal, "score_reconciliation");
    }

    #[test]
    fn reconciliation_creates_fallback_for_empty_components() {
        let mut components = Vec::new();

        reconcile_score_breakdown(
            0.85,
            &mut components,
            "confidence",
            vec!["test:id".into()],
            "test confidence",
        );

        assert_eq!(components.len(), 1);
        assert_eq!(components[0].signal, "confidence");
        assert!((score_component_total(&components) - 0.85).abs() < 0.001);
    }

    #[test]
    fn confidence_breakdown_is_stable_for_same_signals() {
        let input = ConfidenceSignalInput {
            primary_file_count: 2,
            evidence_count: 8,
            exact_reference_count: 2,
            validation_count: 2,
            validation_with_command_count: 1,
            negative_evidence_count: 0,
            allowed_file_count: 2,
            runtime_signal_count: 1,
        };

        let first = ConfidenceBreakdown::from_signals(input);
        let second = ConfidenceBreakdown::from_signals(input);

        assert_eq!(first.overall_enum, second.overall_enum);
        assert_eq!(first.overall_score, second.overall_score);
        assert_eq!(first.components, second.components);
        assert!(first.caveats.is_empty());
        assert!(first.blockers.is_empty());
    }

    #[test]
    fn confidence_drops_without_exact_tests_or_runtime() {
        let grounded = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            primary_file_count: 1,
            evidence_count: 6,
            exact_reference_count: 1,
            validation_count: 1,
            validation_with_command_count: 1,
            negative_evidence_count: 0,
            allowed_file_count: 1,
            runtime_signal_count: 1,
        });
        let thin = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            primary_file_count: 1,
            evidence_count: 6,
            exact_reference_count: 0,
            validation_count: 0,
            validation_with_command_count: 0,
            negative_evidence_count: 0,
            allowed_file_count: 1,
            runtime_signal_count: 0,
        });

        assert!(thin.overall_score < grounded.overall_score);
        assert_eq!(thin.overall_enum, Confidence::Medium);
        assert!(thin
            .caveats
            .iter()
            .any(|caveat| caveat.contains("exact symbol/reference")));
        assert!(thin
            .caveats
            .iter()
            .any(|caveat| caveat.contains("no validation")));
        assert!(thin
            .caveats
            .iter()
            .any(|caveat| caveat.contains("runtime corroboration")));
    }

    #[test]
    fn negative_evidence_prevents_false_high_confidence() {
        let breakdown = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            primary_file_count: 3,
            evidence_count: 12,
            exact_reference_count: 3,
            validation_count: 3,
            validation_with_command_count: 3,
            negative_evidence_count: 1,
            allowed_file_count: 3,
            runtime_signal_count: 1,
        });

        assert!(breakdown.overall_score <= 0.60);
        assert_ne!(breakdown.overall_enum, Confidence::High);
        assert!(!breakdown.blockers.is_empty());
    }

    #[test]
    fn history_snapshot_round_trips_with_versioned_records() {
        let committed_at = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        let commit = GitCommitRecord {
            id: GitCommitId::new("abc123"),
            parent_ids: vec![GitCommitId::new("parent123")],
            author: Owner {
                name: "Ada".into(),
                email: Some("ada@example.com".into()),
            },
            committer: None,
            authored_at: committed_at,
            committed_at,
            summary: "Add typed history".into(),
            message: "Add typed history\n\nPersist first-class records.".into(),
            file_count: 1,
        };
        let touch = GitFileTouch {
            id: HistoryRecordId::new("touch-1"),
            commit_id: commit.id.clone(),
            path: "src/history.rs".into(),
            previous_path: None,
            change_kind: GitChangeKind::Added,
            additions: Some(42),
            deletions: Some(0),
            touched_at: committed_at,
        };
        let snapshot = HistorySnapshot {
            schema_version: HISTORY_SCHEMA_VERSION,
            commits: vec![commit],
            file_touches: vec![touch],
            symbol_touches: Vec::new(),
            cochange_edges: Vec::new(),
            reviewer_evidence: Vec::new(),
        };

        let json = serde_json::to_string(&snapshot).unwrap();
        let decoded: HistorySnapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, snapshot);
        assert_eq!(
            HistorySnapshot::empty().schema_version,
            HISTORY_SCHEMA_VERSION
        );
    }

    #[test]
    fn empty_history_summary_exposes_uncertainty() {
        let summary = HistorySummary::empty("src/missing.rs");

        assert!(summary.recent_commits.is_empty());
        assert!(!summary.uncertainty.is_empty());
        assert!(summary.uncertainty[0].contains("no persisted history evidence"));
    }

    #[test]
    fn legacy_symbol_touch_json_defaults_new_mapping_evidence() {
        let decoded: GitSymbolTouch = serde_json::from_value(serde_json::json!({
            "id": "touch",
            "commit_id": "abc123",
            "symbol_id": "symbol",
            "qualified_name": "crate::symbol",
            "file_path": "src/lib.rs",
            "change_kind": "modified",
            "touched_at": "2026-06-01T12:00:00Z"
        }))
        .unwrap();

        assert_eq!(decoded.symbol_id, Some(SymbolId::new("symbol")));
        assert!(decoded.line_ranges.is_empty());
        assert_eq!(decoded.confidence, Confidence::Low);
        assert!(decoded.uncertainty.is_empty());
    }
}
