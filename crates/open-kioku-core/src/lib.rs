use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

pub mod abstention;
pub mod analysis_semantics;
pub mod identity;
pub mod process;
pub mod relationship;

pub use analysis_semantics::*;

pub use relationship::{
    normalize_relationship_proofs, relationship_authority, RelationshipAuthority,
    RelationshipProof, RelationshipProofFilter, RelationshipProofKind,
    RELATIONSHIP_PROOFS_PROPERTY,
};

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
id_type!(ScopeId);
id_type!(CallSiteId);
id_type!(BindingId);
id_type!(ModuleId);

pub const HISTORY_SCHEMA_VERSION: u32 = 1;

/// Version of the on-disk index layout an [`IndexManifest`] was written for.
///
/// A stored manifest whose version differs from this one is not partially indexable
/// (`partial_index_supported`), so the next `ok index` on an index written by an older layout
/// is a full rebuild rather than an update onto rows the current reader cannot interpret.
/// Bumped to 2 in 4.0.0 for the compact graph tables.
pub const INDEX_MANIFEST_SCHEMA_VERSION: u32 = 2;

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

/// Scopes of [`NegativeEvidence`] items shared by context packs and plans. The scope is
/// the stable key a reader traces a confidence blocker back to.
pub mod negative_evidence_scope {
    pub const PRIMARY_CONTEXT: &str = "primary_context";
    pub const EXACT_REFERENCES: &str = "exact_references";
    pub const VALIDATION: &str = "validation";
    pub const RUNTIME: &str = "runtime";
    pub const HISTORY: &str = "history";
    pub const BOUNDARY: &str = "boundary";
    /// A named identifier in the task that no selected context spells.
    pub const ANCHOR: &str = "anchor";
}

impl NegativeEvidence {
    /// Whether this item is priced by the `negative_evidence` confidence component.
    ///
    /// Absent exact references, validation, runtime, and history evidence each lower
    /// confidence through their own component and cap already. Counting them here as well
    /// priced one absence twice, which is how `ok plan` reported "3 negative evidence
    /// signal(s)" for a task whose only defect was a repository without SCIP. What counts
    /// here is evidence that retrieval itself missed: no primary context at all, or a task
    /// identifier the selected context does not spell.
    pub fn lowers_confidence(&self) -> bool {
        matches!(
            self.scope.as_str(),
            negative_evidence_scope::PRIMARY_CONTEXT | negative_evidence_scope::ANCHOR
        )
    }
}

/// The `negative_evidence_count` confidence input for a pack or plan: the items of its
/// reported `negative_evidence` list that [`NegativeEvidence::lowers_confidence`]. Context
/// and plan both derive the count from the list they publish, so the blocker
/// "N negative evidence signal(s) lowered confidence" is always traceable to N listed items.
pub fn negative_evidence_signal_count(items: &[NegativeEvidence]) -> usize {
    items.iter().filter(|item| item.lowers_confidence()).count()
}

/// Distinct evidence records in a pack or plan. `evidence` carries one entry per evidence
/// line of each result, so its length grows with matched query variants, not with evidence.
pub fn distinct_evidence_count(evidence: &[Evidence]) -> usize {
    evidence
        .iter()
        .map(|item| &item.id)
        .collect::<BTreeSet<_>>()
        .len()
}

const DEFAULT_EVIDENCE_FRESHNESS_MAX_AGE_DAYS: i64 = 7;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceQuality {
    pub index_mode: String,
    pub freshness: String,
    pub exact_reference_available: bool,
    pub runtime_available: bool,
    pub history_available: bool,
    pub test_coverage_available: bool,
    pub skipped_path_count: usize,
    pub unresolved_import_count: usize,
    pub ambiguous_edge_count: usize,
    pub failed_optional_passes: Vec<String>,
    pub caveats: Vec<String>,
}

impl Default for EvidenceQuality {
    fn default() -> Self {
        Self {
            index_mode: "unknown".into(),
            freshness: "missing".into(),
            exact_reference_available: false,
            runtime_available: false,
            history_available: false,
            test_coverage_available: false,
            skipped_path_count: 0,
            unresolved_import_count: 0,
            ambiguous_edge_count: 0,
            failed_optional_passes: Vec::new(),
            caveats: vec![
                "no index manifest was available; evidence quality could not be verified".into(),
            ],
        }
    }
}

impl EvidenceQuality {
    pub fn from_manifest(manifest: Option<&IndexManifest>) -> Self {
        Self::from_manifest_with_counts(manifest, 0, 0)
    }

    pub fn from_manifest_with_counts(
        manifest: Option<&IndexManifest>,
        unresolved_import_count: usize,
        ambiguous_edge_count: usize,
    ) -> Self {
        let Some(manifest) = manifest else {
            return Self::default();
        };
        let quality = &manifest.quality;
        let unresolved_import_count = unresolved_import_count.max(count_resolution_notes(
            quality,
            "import resolver caveat",
            "unresolved import",
        ));
        let ambiguous_edge_count = ambiguous_edge_count.max(count_resolution_notes(
            quality,
            "import resolver caveat",
            "ambiguous import",
        ));
        let failed_optional_passes = failed_optional_passes(quality);
        let mut value = Self {
            index_mode: manifest.index_mode.to_string(),
            freshness: evidence_freshness(manifest.indexed_at),
            exact_reference_available: quality.scip_exact_references > 0,
            runtime_available: quality.runtime_analysis_facts > 0,
            history_available: quality.git_history_facts > 0,
            test_coverage_available: quality.coverage_reports > 0 || quality.junit_reports > 0,
            skipped_path_count: quality.skipped_paths.len(),
            unresolved_import_count,
            ambiguous_edge_count,
            failed_optional_passes,
            caveats: Vec::new(),
        };
        value.refresh_caveats();
        value
    }

    pub fn is_fresh(&self) -> bool {
        self.freshness == "fresh"
    }

    pub fn is_stale(&self) -> bool {
        self.freshness == "stale"
    }

    pub fn is_missing(&self) -> bool {
        self.freshness == "missing" || self.index_mode == "unknown"
    }

    pub fn refresh_caveats(&mut self) {
        let mut caveats = Vec::new();
        match self.freshness.as_str() {
            "stale" => caveats.push(
                "index is stale; re-index before relying on exact impact or verification gates"
                    .into(),
            ),
            "missing" => caveats.push(
                "no index manifest was available; evidence quality could not be verified".into(),
            ),
            _ => {}
        }
        match self.index_mode.as_str() {
            "fast" => caveats.push(
                "fast index mode may skip expensive code analysis for examples, testdata, generated, vendor, unsupported, and oversized paths; documentation is handled by the lightweight document corpus when available".into(),
            ),
            "balanced" => caveats.push(
                "balanced index mode may skip expensive optional evidence passes".into(),
            ),
            "cross_project" => caveats.push(
                "cross-project index mode links already-indexed projects without full source parsing".into(),
            ),
            _ => {}
        }
        if !self.exact_reference_available {
            caveats.push("exact symbol/reference evidence is unavailable".into());
        }
        if !self.runtime_available {
            caveats.push("runtime evidence is unavailable".into());
        }
        if !self.history_available {
            caveats.push("history evidence is unavailable".into());
        }
        if !self.test_coverage_available {
            caveats.push("coverage or JUnit evidence is unavailable".into());
        }
        if self.skipped_path_count > 0 {
            caveats.push(format!(
                "index skipped {} path(s); evidence may be incomplete for skipped areas",
                self.skipped_path_count
            ));
        }
        if self.unresolved_import_count > 0 {
            caveats.push(format!(
                "{} unresolved import(s) reduce dependency evidence confidence",
                self.unresolved_import_count
            ));
        }
        if self.ambiguous_edge_count > 0 {
            caveats.push(format!(
                "{} ambiguous edge(s) reduce impact and policy confidence",
                self.ambiguous_edge_count
            ));
        }
        for pass in &self.failed_optional_passes {
            caveats.push(format!("optional evidence pass did not complete: {pass}"));
        }
        self.caveats = dedup_strings(caveats);
    }
}

fn evidence_freshness(indexed_at: DateTime<Utc>) -> String {
    let max_age = chrono::Duration::days(DEFAULT_EVIDENCE_FRESHNESS_MAX_AGE_DAYS);
    if Utc::now().signed_duration_since(indexed_at) > max_age {
        "stale".into()
    } else {
        "fresh".into()
    }
}

fn count_resolution_notes(quality: &IndexQuality, source: &str, needle: &str) -> usize {
    let source = source.to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase();
    quality
        .quality_notes
        .iter()
        .filter(|note| {
            let note = note.to_ascii_lowercase();
            note.contains(&source) && note.contains(&needle)
        })
        .count()
}

fn failed_optional_passes(quality: &IndexQuality) -> Vec<String> {
    let mut passes = Vec::new();
    for note in quality.quality_notes.iter().chain(
        quality
            .phase_reports
            .iter()
            .flat_map(|report| report.warnings.iter()),
    ) {
        let lowered = note.to_ascii_lowercase();
        if lowered.contains("failed")
            || lowered.contains("timed out")
            || lowered.contains("timedout")
            || lowered.contains("was enabled but no scip index was imported")
        {
            passes.push(note.clone());
        }
    }
    dedup_strings(passes)
}

fn dedup_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
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

#[derive(Debug, Clone, Default)]
pub struct ConfidenceSignalInput {
    pub primary_file_count: usize,
    /// Distinct evidence records attached to the pack, not evidence lines: a result carries
    /// one line per matched query variant, so counting lines saturated the density signal
    /// for any non-empty pack.
    pub evidence_count: usize,
    /// Selections backed by exact provenance: an exact-authority retrieval trace, an
    /// indexed symbol reference, or SCIP-sourced evidence. Never derived from result prose.
    pub exact_reference_count: usize,
    pub validation_count: usize,
    pub validation_with_command_count: usize,
    /// See [`negative_evidence_signal_count`].
    pub negative_evidence_count: usize,
    pub allowed_file_count: usize,
    pub runtime_signal_count: usize,
    /// Named identifiers in the task (`IssueTokenService`, `reticulate_splines`) as
    /// [`named_anchors`] extracts them. Zero for a prose-only task.
    pub named_anchor_count: usize,
    /// The named identifiers no selected context spells, per [`unmatched_named_anchors`].
    /// When every named identifier is unmatched the repository does not know the thing the
    /// task is about, and no structural completeness may present that as Medium.
    pub unmatched_anchors: Vec<String>,
    /// Fraction of the task's content terms that appear anywhere in the selected
    /// context, in 0..=1. See [`task_relevance_score`].
    ///
    /// Every other input counts how *complete* the pack is. None of them asks
    /// whether it has anything to do with the question, which is why a pack of
    /// twenty unrelated files scored maximum confidence.
    pub task_relevance: f32,
}

/// Fraction of a task's content terms that appear anywhere in the selected
/// context - path, snippet, or symbol name.
///
/// This is deliberately lexical and deliberately crude. It is not a relevance
/// model; it is a floor. A task whose every word is absent from every selected
/// file is one the repository cannot answer, and no amount of structural
/// completeness should make that look confident.
/// Below this share of task terms present in the selected context, a pack is capped at Low
/// confidence and a retrieval strategy is treated as having abstained. Shared with the
/// retrieval benchmark so the measured and the deployed abstention rule cannot drift.
pub const WEAK_TASK_RELEVANCE: f32 = 0.34;

pub fn task_relevance_score(task: &str, selected: &[SearchResult]) -> f32 {
    let terms = task_content_terms(task);
    if terms.is_empty() {
        // Nothing to disprove. Do not manufacture doubt from an empty query.
        return 1.0;
    }
    if selected.is_empty() {
        return 0.0;
    }
    // Whole tokens, not substrings. Substring matching counted `repo` as a hit
    // against `reporting`, which is how a query reading "zzzzqqq nonsense not in
    // repo" scored as relevant to a shipping report.
    let mut haystack: std::collections::HashSet<String> = std::collections::HashSet::new();
    for result in selected {
        collect_identifier_tokens(&result.path.to_string_lossy(), &mut haystack);
        collect_identifier_tokens(&result.snippet, &mut haystack);
        if let Some(symbol) = &result.symbol {
            collect_identifier_tokens(&symbol.name, &mut haystack);
            collect_identifier_tokens(&symbol.qualified_name, &mut haystack);
        }
    }
    let matched = terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count();
    matched as f32 / terms.len() as f32
}

/// Whether a repository-relative path is test code, judged by its directory
/// segments and file name rather than by substring.
///
/// The `contains("test")` rule this replaces matched `latest`, `attest`, and
/// `contest`, and missed the Gradle/Maven source-set layout that large Java
/// repositories use (`src/internalClusterTest/java`, `src/javaRestTest`,
/// `src/testFixtures`, `qa/`), so integration tests passed as source there.
pub fn is_test_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let segments = normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let Some((file, dirs)) = segments.split_last() else {
        return false;
    };
    dirs.iter().any(|dir| is_test_dir_segment(dir)) || is_test_file_name(file)
}

fn is_test_dir_segment(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "test"
            | "tests"
            | "testing"
            | "spec"
            | "specs"
            | "__tests__"
            | "__test__"
            | "testdata"
            | "e2e"
            | "cypress"
            | "qa"
    ) || lower.starts_with("test")
        || lower.ends_with("-spec")
        || lower.ends_with("_spec")
        || has_camel_test_suffix(segment)
}

fn is_test_file_name(name: &str) -> bool {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let lower = stem.to_ascii_lowercase();
    // `tests.rs` is a test module; `Test.java` is a class that happens to be named Test.
    matches!(stem, "test" | "tests" | "conftest")
        || lower.starts_with("test_")
        || lower.ends_with("_test")
        || lower.ends_with("_tests")
        || lower.ends_with("_spec")
        || lower.ends_with("-test")
        || lower.ends_with("-spec")
        || lower.ends_with(".test")
        || lower.ends_with(".spec")
        || lower.ends_with(".e2e")
        || has_camel_test_suffix(stem)
}

/// `GeoIpProcessorTests`, `GeoIpReindexedIT`, `internalClusterTest`: a test
/// suffix in CamelCase, recognised only at a case boundary so `UNIT` and a
/// bare `Test` do not count.
fn has_camel_test_suffix(value: &str) -> bool {
    ["TestCase", "Tests", "Test", "Spec", "IT"]
        .iter()
        .any(|suffix| {
            value
                .strip_suffix(suffix)
                .and_then(|prefix| prefix.chars().last())
                .is_some_and(|last| last.is_ascii_lowercase() || last.is_ascii_digit())
        })
}

/// Whether the task is asking about tests rather than about source.
///
/// Deliberately narrow: the cost of a false positive is promoting test files
/// over source for an ordinary query, which is the regression this guards.
pub fn query_wants_tests(query: &str) -> bool {
    // Whole words and CamelCase parts: "Add BenchmarkHashString" is about a benchmark, which in
    // Go and Rust lives in the test files; "LatestFoo" splits to latest/foo and stays clear.
    let mut tokens = std::collections::HashSet::new();
    collect_identifier_tokens(query, &mut tokens);
    tokens.iter().any(|word| {
        matches!(
            word.as_str(),
            "test"
                | "tests"
                | "testing"
                | "spec"
                | "specs"
                | "covering"
                | "coverage"
                | "fixture"
                | "benchmark"
                | "benchmarks"
                | "bench"
        )
    })
}

/// Split text into lowercase word tokens, breaking camelCase and snake_case so
/// `issueToken` contributes both `issue` and `token`.
fn collect_identifier_tokens(text: &str, out: &mut std::collections::HashSet<String>) {
    let mut current = String::new();
    let mut previous_lower = false;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            if ch.is_ascii_uppercase() && previous_lower && !current.is_empty() {
                if current.len() >= 3 {
                    out.insert(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            }
            current.push(ch.to_ascii_lowercase());
            previous_lower = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else {
            if current.len() >= 3 {
                out.insert(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            previous_lower = false;
        }
    }
    if current.len() >= 3 {
        out.insert(current);
    }
}

fn task_content_terms(task: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "with", "that", "this", "from", "into", "when", "should", "must",
        "add", "use", "using", "make", "run", "get", "set", "new", "all", "any", "not", "but",
        "are", "was", "were", "has", "have", "had", "its", "our", "out", "how", "why", "who",
        "can", "may", "will", "would", "could", "then", "than", "them", "they", "there", "these",
        "those", "some", "such", "only", "also", "each", "more", "most", "other", "over", "after",
        "before", "between", "under", "while", "where", "which", "what",
    ];
    // Tokenized identically to the haystack, so `issueToken` in a task matches
    // `issueToken` in a snippet through the same `issue` + `token` split.
    let mut tokens = std::collections::HashSet::new();
    collect_identifier_tokens(task, &mut tokens);
    let mut terms: Vec<String> = tokens
        .into_iter()
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

/// Code identifiers a task names - mixed case, `snake_case`, `kebab-case`, or an
/// upper-cased token with digits - as opposed to its prose. Ticket references (`ABC-123`)
/// are not anchors. Shared by context and plan so both surfaces agree on what the task
/// named and therefore on what counts as missing.
pub fn named_anchors(task: &str) -> Vec<String> {
    let mut anchors = Vec::new();
    for token in task.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')) {
        let token = token.trim_matches('-');
        if token.len() < 3 || is_ticket_anchor(token) {
            continue;
        }
        let has_lower = token.chars().any(|ch| ch.is_ascii_lowercase());
        let has_upper = token.chars().any(|ch| ch.is_ascii_uppercase());
        let has_digit = token.chars().any(|ch| ch.is_ascii_digit());
        let has_separator = token.contains('_') || token.contains('-');
        if ((has_lower && has_upper) || has_separator || (has_digit && has_upper))
            && !anchors.iter().any(|existing| existing == token)
        {
            anchors.push(token.to_string());
        }
    }
    anchors
}

/// The [`named_anchors`] of `task` that none of the top five selected results spells, in
/// its path, snippet, or symbol names, either verbatim or split into words
/// (`IssueTokenService` matches `issue token service`). Empty when the task names nothing
/// or nothing was selected: an empty selection is reported on its own.
pub fn unmatched_named_anchors(task: &str, selected: &[SearchResult]) -> Vec<String> {
    let anchors = named_anchors(task);
    if anchors.is_empty() || selected.is_empty() {
        return Vec::new();
    }
    let top_context = selected
        .iter()
        .take(5)
        .map(|result| {
            format!(
                "{} {} {} {}",
                result.path.display(),
                result.snippet,
                result
                    .symbol
                    .as_ref()
                    .map(|symbol| symbol.name.as_str())
                    .unwrap_or_default(),
                result
                    .symbol
                    .as_ref()
                    .map(|symbol| symbol.qualified_name.as_str())
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    anchors
        .into_iter()
        .filter(|anchor| {
            let lower = anchor.to_ascii_lowercase();
            !top_context.contains(&lower) && !top_context.contains(&normalize_anchor(anchor))
        })
        .collect()
}

fn is_ticket_anchor(value: &str) -> bool {
    let Some((prefix, number)) = value.split_once('-') else {
        return false;
    };
    prefix.len() >= 2
        && prefix.chars().all(|ch| ch.is_ascii_uppercase())
        && number.len() >= 2
        && number.chars().all(|ch| ch.is_ascii_digit())
}

fn normalize_anchor(value: &str) -> String {
    let mut out = String::new();
    let mut previous_lower_or_digit = false;
    for ch in value.chars() {
        if ch == '_' || ch == '-' {
            out.push(' ');
            previous_lower_or_digit = false;
            continue;
        }
        if ch.is_ascii_uppercase() && previous_lower_or_digit {
            out.push(' ');
        }
        out.push(ch.to_ascii_lowercase());
        previous_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
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
        let every_anchor_unmatched = input.named_anchor_count > 0
            && input.unmatched_anchors.len() >= input.named_anchor_count;
        if every_anchor_unmatched {
            blockers.push(format!(
                "{} task identifier(s) name nothing in the selected context: {}",
                input.unmatched_anchors.len(),
                input.unmatched_anchors.join(", ")
            ));
        } else if !input.unmatched_anchors.is_empty() {
            caveats.push(format!(
                "{} of {} task identifier(s) name nothing in the selected context: {}",
                input.unmatched_anchors.len(),
                input.named_anchor_count,
                input.unmatched_anchors.join(", ")
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
                "task_relevance",
                input.task_relevance.clamp(0.0, 1.0),
                0.20,
                "share of the task's terms that appear in the selected context",
            ),
            confidence_component(
                "evidence_density",
                evidence_density,
                0.10,
                "distinct evidence records over twice the selected primary files, capped at 1.0",
            ),
            confidence_component(
                "exact_references",
                exact_reference,
                0.20,
                "selections backed by exact-authority retrieval, indexed symbol references, or SCIP evidence",
            ),
            confidence_component(
                "validation_availability",
                validation_availability,
                0.15,
                "at least one validation target was selected near the primary context",
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
                "at least one selected validation target carries a runnable command",
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
        // Exact evidence absent is on its own a reason not to be *highly*
        // confident. The cap above is an `&&`, so synthesizing a single
        // validation target was enough to escape it while claiming High.
        if input.exact_reference_count == 0 {
            overall_score = overall_score.min(0.74);
        }
        // A task whose terms appear nowhere in the selected context is one the
        // repository cannot answer. No amount of structural completeness should
        // outvote that.
        if input.task_relevance <= 0.0 {
            blockers.push("no task term appears in the selected context".into());
            overall_score = overall_score.min(0.30);
        } else if input.task_relevance < WEAK_TASK_RELEVANCE {
            // Strictly below the Medium threshold: a pack whose selected context contains
            // fewer than a third of the task's terms must not present itself as Medium.
            // At 0.55 the cap sat exactly on that threshold and a nonsense query with one
            // incidental word match reported Medium.
            caveats.push("most task terms are absent from the selected context".into());
            overall_score = overall_score.min(0.50);
        }
        if input.negative_evidence_count > 0 {
            overall_score = overall_score.min(0.60);
        }
        // Below Medium, above the no-term floor: the task's every identifier is unknown to
        // the selected context, which is worse than a right file without SCIP and better
        // than a task with no words in the repository at all.
        if every_anchor_unmatched {
            overall_score = overall_score.min(0.50);
        }

        blockers.sort();
        blockers.dedup();
        caveats.sort();
        caveats.dedup();
        if !caveats.is_empty() {
            overall_score = overall_score.min(0.94);
        }

        // `Exact` is a provenance claim, not a score band: it is reachable only when at
        // least one selection is backed by exact-authority evidence. The 0.74 cap above
        // already implies this; stating it keeps a future weight change from labelling
        // heuristic evidence `Exact` again.
        let mut overall_enum = Confidence::from_score(overall_score);
        if overall_enum == Confidence::Exact && input.exact_reference_count == 0 {
            overall_enum = Confidence::High;
        }

        Self {
            overall_enum,
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DocumentType {
    Markdown,
    Mdx,
    Readme,
    Adr,
    Architecture,
    Guide,
    PlainText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocumentSection {
    pub path: PathBuf,
    pub heading_path: Vec<String>,
    pub line_range: LineRange,
    pub content_hash: String,
    pub content: String,
    pub document_type: DocumentType,
}

/// An immutable shared filesystem path.
///
/// Evidence is dominated by repeated paths: across 156,515 graph edges on a
/// 1,751-file Java corpus, `file_range.path` totalled 15,225,615 bytes drawn
/// from 2,388 distinct paths. Sharing the allocation turns each edge's copy
/// into a refcount bump.
///
/// Serializes exactly as `PathBuf` does — a plain JSON string — so the wire
/// format, stored rows, and golden snapshots are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SharedPath(std::sync::Arc<std::path::Path>);

impl SharedPath {
    pub fn as_path(&self) -> &std::path::Path {
        &self.0
    }

    pub fn display(&self) -> std::path::Display<'_> {
        self.0.display()
    }
}

impl Default for SharedPath {
    fn default() -> Self {
        Self(std::sync::Arc::from(std::path::Path::new("")))
    }
}

impl std::ops::Deref for SharedPath {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}

impl AsRef<std::path::Path> for SharedPath {
    fn as_ref(&self) -> &std::path::Path {
        &self.0
    }
}

impl std::borrow::Borrow<std::path::Path> for SharedPath {
    fn borrow(&self) -> &std::path::Path {
        &self.0
    }
}

impl From<PathBuf> for SharedPath {
    fn from(value: PathBuf) -> Self {
        Self(std::sync::Arc::from(value.as_path()))
    }
}

impl From<&std::path::Path> for SharedPath {
    fn from(value: &std::path::Path) -> Self {
        Self(std::sync::Arc::from(value))
    }
}

impl From<&str> for SharedPath {
    fn from(value: &str) -> Self {
        Self(std::sync::Arc::from(std::path::Path::new(value)))
    }
}

impl From<String> for SharedPath {
    fn from(value: String) -> Self {
        Self(std::sync::Arc::from(std::path::Path::new(&value)))
    }
}

impl From<SharedPath> for PathBuf {
    fn from(value: SharedPath) -> Self {
        value.0.to_path_buf()
    }
}

impl Serialize for SharedPath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Mirrors `PathBuf`'s own impl, which refuses non-UTF-8 rather than
        // silently emitting something a reader cannot round-trip.
        match self.0.to_str() {
            Some(text) => serializer.serialize_str(text),
            None => Err(serde::ser::Error::custom(
                "path contains invalid UTF-8 characters",
            )),
        }
    }
}

impl<'de> Deserialize<'de> for SharedPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        PathBuf::deserialize(deserializer).map(Self::from)
    }
}

impl JsonSchema for SharedPath {
    fn schema_name() -> String {
        PathBuf::schema_name()
    }

    fn json_schema(generator: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        PathBuf::json_schema(generator)
    }

    fn is_referenceable() -> bool {
        PathBuf::is_referenceable()
    }
}

/// Deduplicates repeated paths produced during one indexing run.
pub struct PathInterner {
    shards: Vec<std::sync::Mutex<std::collections::HashSet<SharedPath>>>,
}

impl PathInterner {
    const SHARDS: usize = 16;

    pub fn new() -> Self {
        Self {
            shards: (0..Self::SHARDS)
                .map(|_| std::sync::Mutex::new(std::collections::HashSet::new()))
                .collect(),
        }
    }

    pub fn intern(&self, path: &std::path::Path) -> SharedPath {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        path.hash(&mut hasher);
        let shard = &self.shards[(hasher.finish() as usize) % Self::SHARDS];
        let mut set = shard.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = set.get(path) {
            return existing.clone();
        }
        let shared = SharedPath::from(path);
        set.insert(shared.clone());
        shared
    }

    pub fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.lock().unwrap_or_else(|e| e.into_inner()).len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for PathInterner {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileRange {
    pub path: SharedPath,
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

/// An immutable shared string used by evidence fields.
///
/// Backed by `Arc<str>` rather than `String` for two reasons that only matter at
/// corpus scale. It is 16 bytes inline instead of 24; and, more importantly,
/// cloning is a refcount bump, so the graph builder no longer duplicates every
/// `AnalysisFact` message into the `Evidence` on its edge while the fact itself
/// is still resident.
///
/// Serializes and deserializes exactly as a JSON string, so the wire format and the golden
/// MCP snapshots are unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SharedStr(std::sync::Arc<str>);

impl SharedStr {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Default for SharedStr {
    fn default() -> Self {
        Self(std::sync::Arc::from(""))
    }
}

impl std::ops::Deref for SharedStr {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for SharedStr {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for SharedStr {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SharedStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for SharedStr {
    fn from(value: String) -> Self {
        // `Arc::from(String)` copies into an exactly sized allocation, which also
        // drops any spare capacity `format!` left behind.
        Self(std::sync::Arc::from(value))
    }
}

impl From<&str> for SharedStr {
    fn from(value: &str) -> Self {
        Self(std::sync::Arc::from(value))
    }
}

impl From<SharedStr> for String {
    fn from(value: SharedStr) -> Self {
        value.0.to_string()
    }
}

impl PartialEq<str> for SharedStr {
    fn eq(&self, other: &str) -> bool {
        &*self.0 == other
    }
}

impl PartialEq<&str> for SharedStr {
    fn eq(&self, other: &&str) -> bool {
        &*self.0 == *other
    }
}

impl Serialize for SharedStr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SharedStr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from)
    }
}

impl JsonSchema for SharedStr {
    fn schema_name() -> String {
        String::schema_name()
    }

    fn json_schema(generator: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        String::json_schema(generator)
    }

    fn is_referenceable() -> bool {
        String::is_referenceable()
    }
}

/// Deduplicates repeated strings produced during one indexing run.
///
/// Evidence narration is highly repetitive: on a 1,751-file Java corpus the
/// symbol registry emitted 82,444 messages drawn from only 10,246 distinct
/// strings. Interning collapses those to one allocation each, and because
/// [`SharedStr`] is `Arc`-backed the facts then share rather than copy.
///
/// Sharded because the largest producer runs under `rayon`. The interner is
/// created per indexing run and dropped with it, so nothing accumulates across
/// runs in a long-lived process such as the MCP server.
pub struct StringInterner {
    shards: Vec<std::sync::Mutex<std::collections::HashSet<SharedStr>>>,
}

impl StringInterner {
    const SHARDS: usize = 16;

    pub fn new() -> Self {
        Self {
            shards: (0..Self::SHARDS)
                .map(|_| std::sync::Mutex::new(std::collections::HashSet::new()))
                .collect(),
        }
    }

    /// Return a shared handle for `text`, allocating only on first sight.
    pub fn intern(&self, text: String) -> SharedStr {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.as_str().hash(&mut hasher);
        let shard = &self.shards[(hasher.finish() as usize) % Self::SHARDS];

        // A poisoned shard still holds valid entries; recovering keeps a panic in
        // one producer from turning every later message into a fresh allocation.
        let mut set = shard.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = set.get(text.as_str()) {
            return existing.clone();
        }
        let message = SharedStr::from(text);
        set.insert(message.clone());
        message
    }

    /// Number of distinct messages held. Intended for diagnostics and tests.
    pub fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|s| s.lock().unwrap_or_else(|e| e.into_inner()).len())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for StringInterner {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    pub id: EvidenceId,
    pub source: SharedStr,
    pub source_type: EvidenceSourceType,
    pub file_range: Option<FileRange>,
    pub symbol_id: Option<SymbolId>,
    pub confidence: Confidence,
    pub message: SharedStr,
    pub indexed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
}

impl Default for Evidence {
    fn default() -> Self {
        Self {
            id: EvidenceId::new(""),
            source: SharedStr::default(),
            source_type: EvidenceSourceType::Lexical,
            file_range: None,
            symbol_id: None,
            confidence: Confidence::Low,
            message: SharedStr::default(),
            indexed_at: Utc::now(),
            confidence_score: None,
            confidence_reason: None,
            freshness: None,
        }
    }
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

impl Language {
    /// Languages whose files are program source, as opposed to data, config, or prose.
    /// Coverage warnings are judged on these; the table still shows every language.
    pub fn is_programming(&self) -> bool {
        matches!(
            self,
            Self::Rust
                | Self::Java
                | Self::TypeScript
                | Self::JavaScript
                | Self::Python
                | Self::Go
                | Self::Sql
        )
    }

    /// The serialized (snake_case) name, used as the key of every per-language map so
    /// JSON consumers see one spelling everywhere.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Java => "java",
            Self::TypeScript => "type_script",
            Self::JavaScript => "java_script",
            Self::Python => "python",
            Self::Go => "go",
            Self::Yaml => "yaml",
            Self::Json => "json",
            Self::Toml => "toml",
            Self::Sql => "sql",
            Self::Markdown => "markdown",
            Self::Text => "text",
            Self::Unknown => "unknown",
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SourceRange {
    pub start_line: u32,
    pub start_column: u32,
    pub end_line: u32,
    pub end_column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Protected,
    Package,
    Crate,
    Private,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    File,
    Module,
    Namespace,
    Class,
    Interface,
    Trait,
    Function,
    Method,
    Closure,
    Block,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Scope {
    pub id: ScopeId,
    pub file_id: FileId,
    pub parent_id: Option<ScopeId>,
    pub owner_symbol_id: Option<SymbolId>,
    pub kind: ScopeKind,
    pub range: SourceRange,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Binding {
    pub id: BindingId,
    pub file_id: FileId,
    pub scope_id: ScopeId,
    pub name: String,
    pub declared_type: Option<String>,
    pub inferred_type: Option<String>,
    pub range: SourceRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverKind {
    None,
    Self_,
    Super,
    Value,
    Type,
    Module,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CallSite {
    pub id: CallSiteId,
    pub file_id: FileId,
    pub scope_id: ScopeId,
    pub caller_symbol_id: Option<SymbolId>,
    pub callee_name: String,
    pub receiver: Option<String>,
    pub receiver_kind: ReceiverKind,
    pub range: SourceRange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImportedName {
    pub imported: String,
    pub local: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImportSite {
    pub file_id: FileId,
    pub scope_id: Option<ScopeId>,
    pub source: String,
    pub bindings: Vec<ImportedName>,
    pub is_glob: bool,
    pub is_type_only: bool,
    pub range: SourceRange,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExportSite {
    pub file_id: FileId,
    pub exported_name: String,
    pub local_name: Option<String>,
    pub source_module: Option<String>,
    pub is_glob: bool,
    pub range: SourceRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InheritanceKind {
    Extends,
    Implements,
    TraitImpl,
    Embeds,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InheritanceSite {
    pub child_symbol_id: SymbolId,
    pub parent_name: String,
    pub kind: InheritanceKind,
    pub order: u16,
    pub range: SourceRange,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SyntaxFacts {
    pub symbols: Vec<Symbol>,
    pub scopes: Vec<Scope>,
    pub imports: Vec<ImportSite>,
    pub exports: Vec<ExportSite>,
    pub calls: Vec<CallSite>,
    pub bindings: Vec<Binding>,
    pub inheritance: Vec<InheritanceSite>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionEvidenceKind {
    LexicalScope,
    TypedBinding,
    ExactImport,
    ExplicitImport,
    ImplicitSelf,
    SameFile,
    InheritedMember,
    InheritanceGraph,
    SCIPOccurrence,
    FallbackHeuristic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResolutionEvidence {
    pub kind: ResolutionEvidenceKind,
    pub source_type: EvidenceSourceType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_range: Option<FileRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_id: Option<SymbolId>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResolvedRelationship {
    pub from: SymbolId,
    pub to: SymbolId,
    pub edge_type: GraphEdgeType,
    pub confidence: Confidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_site: Option<SourceRange>,
    #[serde(default)]
    pub evidence: Vec<ResolutionEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proofs: Vec<RelationshipProof>,
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

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_id: Option<ModuleId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_symbol_id: Option<SymbolId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<ScopeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default)]
    pub visibility: Visibility,
}

/// A symbol together with as much of its definition as the index can actually
/// prove, plus a plain statement of whatever it could not.
///
/// Every text field here is recovered from indexed chunk text. Nothing is read
/// back from the working tree and nothing is inferred, so an empty field means
/// the evidence is missing — which `caveats` says out loud rather than letting
/// the caller read absence as a short definition.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SymbolContext {
    pub symbol: Symbol,
    /// Repository-relative path of the defining file, when its row is still indexed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Definition text recovered from the indexed chunks covering the symbol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Lines `body` actually spans, which is not always the symbol's own range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_range: Option<LineRange>,
    /// Indexed lines immediately above the definition, verbatim and in order.
    /// Documentation comments appear here when the indexer chunked them; they
    /// are never parsed out or synthesized, and the field stays empty when the
    /// lines above the definition fall outside every chunk.
    #[serde(default)]
    pub leading_lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leading_range: Option<LineRange>,
    /// Indexed lines immediately below `body`, verbatim and in order.
    #[serde(default)]
    pub trailing_lines: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailing_range: Option<LineRange>,
    /// True when the definition ran past the bound and was cut short.
    #[serde(default)]
    pub truncated: bool,
    /// Where each returned span came from.
    #[serde(default)]
    pub evidence: Vec<String>,
    /// What is missing, and why the caller should not read more into this
    /// bundle than the index supports.
    #[serde(default)]
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SymbolOccurrence {
    pub symbol_id: SymbolId,
    pub file_id: FileId,
    pub range: Option<LineRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_range: Option<SourceRange>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionStatus {
    Resolved,
    Ambiguous { candidates: usize },
    ExternalPackage,
    Builtin,
    Unresolved,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImportResolution {
    pub import: Import,
    pub status: ResolutionStatus,
    pub target_file: Option<FileId>,
    pub target_symbol: Option<SymbolId>,
    pub confidence: Confidence,
    pub strategy: String,
    pub caveats: Vec<String>,
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
    pub source: SharedStr,
    pub source_type: EvidenceSourceType,
    pub message: SharedStr,
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
    /// How strongly selection evidence justifies running this test. Heuristic name/path
    /// similarity alone can never raise a test above [`TestSelectionTier::Optional`].
    #[serde(default)]
    pub selection_tier: TestSelectionTier,
    /// The authority-grade or policy-accepted evidence behind a non-optional tier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tier_justification: Vec<String>,
}

/// Selection strength for a validation candidate.
///
/// Encodes the RI3.7 rule for test selection: required or strongly recommended tests must be
/// justified by authoritative evidence (exact reference overlap, exact coverage or test
/// mapping) or policy-accepted corroborating evidence (runtime, bounded git history). A fuzzy
/// structural or lexical match alone cannot make a test required.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TestSelectionTier {
    /// Heuristic-only justification; a suggestion, never a requirement.
    #[default]
    Optional,
    /// Authority-grade or policy-accepted corroborating evidence supports running this test.
    Recommended,
    /// Strong evidence with a high blended score; treat as the validation baseline.
    Required,
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
    pub line_ranges: Vec<LineRange>,
    pub confidence: Confidence,
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FileProvenance {
    pub path: PathBuf,
    pub first_seen: Option<ProvenanceTouch>,
    pub last_touched: Option<ProvenanceTouch>,
    pub recent_touches: Vec<ProvenanceTouch>,
    pub confidence: Confidence,
    pub truncated: bool,
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipSourceType {
    Codeowners,
    GitHistory,
    RepoMemory,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OwnershipEvidence {
    pub source_type: OwnershipSourceType,
    pub owner: Owner,
    pub source: String,
    pub message: String,
    pub confidence: Confidence,
    pub observed_at: Option<DateTime<Utc>>,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OwnershipConfidenceBreakdown {
    pub codeowners: f32,
    pub git_history: f32,
    pub memory: f32,
    pub freshness: f32,
    pub ambiguity_penalty: f32,
    pub final_score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OwnerSuggestion {
    pub owner: Owner,
    pub rationale: String,
    pub confidence: Confidence,
    pub score: f32,
    pub source_types: Vec<OwnershipSourceType>,
    pub stale: bool,
    pub evidence: Vec<OwnershipEvidence>,
    pub confidence_breakdown: OwnershipConfidenceBreakdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OwnershipReport {
    pub path: PathBuf,
    #[serde(default)]
    pub components: Vec<PolicyComponentMatch>,
    pub generated_at: DateTime<Utc>,
    pub owners: Vec<OwnerSuggestion>,
    #[serde(default)]
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerSignalSourceType {
    ReviewEvidence,
    Ownership,
    GitAuthor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerAvailability {
    ActualReviewEvidence,
    InferredFromOwnershipAndAuthors,
    InferredFromOwnership,
    InferredFromAuthors,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewerSignal {
    pub source_type: ReviewerSignalSourceType,
    pub reviewer: Owner,
    pub source: String,
    pub role: Option<ReviewerRole>,
    pub message: String,
    pub confidence: Confidence,
    pub observed_at: Option<DateTime<Utc>>,
    pub stale: bool,
    pub actual_review_evidence: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewerConfidenceBreakdown {
    pub review_evidence: f32,
    pub ownership: f32,
    pub author_history: f32,
    pub freshness: f32,
    pub ambiguity_penalty: f32,
    pub final_score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewerSuggestion {
    pub reviewer: Owner,
    pub rationale: String,
    pub confidence: Confidence,
    pub score: f32,
    pub availability: ReviewerAvailability,
    pub source_types: Vec<ReviewerSignalSourceType>,
    pub inferred_from_authors: bool,
    pub actual_review_evidence: bool,
    pub stale: bool,
    pub signals: Vec<ReviewerSignal>,
    pub confidence_breakdown: ReviewerConfidenceBreakdown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewerSuggestionReport {
    pub path: PathBuf,
    pub generated_at: DateTime<Utc>,
    pub availability: ReviewerAvailability,
    pub suggestions: Vec<ReviewerSuggestion>,
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
    pub recent_touches: Vec<ProvenanceTouch>,
    pub confidence: Confidence,
    pub truncated: bool,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HistorySignalQuery {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default)]
    pub symbols: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HistorySignalSummary {
    pub path: PathBuf,
    pub generated_at: DateTime<Utc>,
    #[serde(default)]
    pub components: Vec<ScoreComponent>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub reasons: Vec<String>,
    pub similar_change_count: usize,
    pub distinct_author_count: usize,
    pub reviewer_count: usize,
    #[serde(default)]
    pub uncertainty: Vec<String>,
}

impl HistorySignalSummary {
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            generated_at: Utc::now(),
            components: Vec::new(),
            evidence_refs: Vec::new(),
            reasons: Vec::new(),
            similar_change_count: 0,
            distinct_author_count: 0,
            reviewer_count: 0,
            uncertainty: vec!["no bounded history signals were available for this path".into()],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SimilarChangeQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub symbols: Vec<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SimilarityEvidenceSource {
    TaskText,
    Path,
    Symbol,
    Churn,
    Cochange,
    CommitMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimilarityEvidence {
    pub source_type: SimilarityEvidenceSource,
    pub score: f32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_id: Option<GitCommitId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HistoricalChangeSummary {
    pub commit: GitCommitRecord,
    #[serde(default)]
    pub touched_paths: Vec<PathBuf>,
    #[serde(default)]
    pub touched_symbols: Vec<String>,
    #[serde(default)]
    pub cochange_paths: Vec<PathBuf>,
    pub churn_hotspot_score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimilarChangeHit {
    pub change: HistoricalChangeSummary,
    pub score: f32,
    pub confidence: Confidence,
    pub evidence: Vec<SimilarityEvidence>,
    #[serde(default)]
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SimilarChangeReport {
    pub query: SimilarChangeQuery,
    pub generated_at: DateTime<Utc>,
    pub hits: Vec<SimilarChangeHit>,
    pub truncated: bool,
    #[serde(default)]
    pub uncertainty: Vec<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChurnEntityKind {
    File,
    Module,
    Symbol,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChurnStats {
    pub all_time: usize,
    pub last_30d: usize,
    pub last_90d: usize,
    pub recency_weighted: f32,
    pub touch_count: usize,
    pub hotspot_score: f32,
}

impl ChurnStats {
    pub fn empty() -> Self {
        Self {
            all_time: 0,
            last_30d: 0,
            last_90d: 0,
            recency_weighted: 0.0,
            touch_count: 0,
            hotspot_score: 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChurnSummary {
    pub entity_kind: ChurnEntityKind,
    pub key: String,
    pub path: Option<PathBuf>,
    pub symbol_id: Option<SymbolId>,
    pub qualified_name: Option<String>,
    pub generated_at: DateTime<Utc>,
    pub stats: ChurnStats,
    pub confidence: Confidence,
    #[serde(default)]
    pub uncertainty: Vec<String>,
}

impl ChurnSummary {
    pub fn missing(entity_kind: ChurnEntityKind, key: impl Into<String>) -> Self {
        let key = key.into();
        Self {
            entity_kind,
            key: key.clone(),
            path: None,
            symbol_id: None,
            qualified_name: None,
            generated_at: Utc::now(),
            stats: ChurnStats::empty(),
            confidence: Confidence::Low,
            uncertainty: vec![format!(
                "no persisted churn summary is available for `{key}`"
            )],
        }
    }
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EnforcedEdgeType {
    Imports,
    References,
    Calls,
}

impl EnforcedEdgeType {
    pub fn graph_edge_type(self) -> GraphEdgeType {
        match self {
            Self::Imports => GraphEdgeType::Imports,
            Self::References => GraphEdgeType::References,
            Self::Calls => GraphEdgeType::Calls,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyMatchEvidence {
    pub edge_id: String,
    pub edge_type: EnforcedEdgeType,
    pub source_node: String,
    pub target_node: String,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub confidence: Confidence,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyViolation {
    pub rule_id: String,
    pub severity: String,
    pub source_component: String,
    pub target_component: String,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub edge_type: EnforcedEdgeType,
    pub evidence: PolicyMatchEvidence,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnknownPolicyEdge {
    pub reason: String,
    pub evidence: PolicyMatchEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyExemptionEvidence {
    pub exemption_id: String,
    pub rule_id: String,
    pub scope: String,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub evidence: PolicyMatchEvidence,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyViolationEvidenceRef {
    pub id: String,
    pub rule_id: String,
    pub severity: String,
    pub source_path: PathBuf,
    pub target_path: PathBuf,
    pub edge_type: EnforcedEdgeType,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicySignalSummary {
    pub configured: bool,
    pub evaluated_edge_count: usize,
    pub allowed_edges: usize,
    pub violation_count: usize,
    pub public_api_violation_count: usize,
    pub exempted_violation_count: usize,
    pub unknown_edge_count: usize,
    pub evidence_refs: Vec<String>,
    pub violation_refs: Vec<PolicyViolationEvidenceRef>,
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublicApiBoundaryReport {
    pub configured: bool,
    pub evaluated_edge_count: usize,
    pub violation_count: usize,
    pub exempted_violation_count: usize,
    pub violations: Vec<PolicyViolation>,
    pub exemptions: Vec<PolicyExemptionEvidence>,
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyCheckReport {
    pub configured: bool,
    pub evaluated_edge_count: usize,
    pub allowed_edges: usize,
    pub violation_count: usize,
    #[serde(default)]
    pub public_api_violation_count: usize,
    #[serde(default)]
    pub exempted_violation_count: usize,
    pub unknown_edge_count: usize,
    pub unknown_sample_count: usize,
    pub unknown_edges_truncated: bool,
    pub violations: Vec<PolicyViolation>,
    #[serde(default)]
    pub exemptions: Vec<PolicyExemptionEvidence>,
    pub unknown_edges: Vec<UnknownPolicyEdge>,
    pub uncertainty: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct IndexManifest {
    pub repository: Repository,
    pub file_count: usize,
    pub symbol_count: usize,
    pub chunk_count: usize,
    pub indexed_at: DateTime<Utc>,
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis_semantics: Option<AnalysisSemanticsState>,
    #[serde(default)]
    pub index_mode: IndexMode,
    #[serde(default)]
    pub phase_reports: Vec<IndexPhaseReport>,
    #[serde(default)]
    pub quality: IndexQuality,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IndexMode {
    #[default]
    Full,
    Balanced,
    Fast,
    CrossProject,
}

impl fmt::Display for IndexMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Full => "full",
            Self::Balanced => "balanced",
            Self::Fast => "fast",
            Self::CrossProject => "cross_project",
        };
        f.write_str(value)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct IndexPhaseReport {
    pub phase: String,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    pub scanned_files: usize,
    pub indexed_files: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_files: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_sections: Option<usize>,
    pub nodes_added: usize,
    pub edges_added: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Ignored,
    Denied,
    Hidden,
    UnsupportedLanguage,
    Binary,
    TooLarge,
    Generated,
    Vendor,
    FastMode,
    SecretPolicy,
    SymlinkPolicy,
    Error,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SkipSource {
    SecurityPolicy,
    HiddenPolicy,
    ConfigExclude,
    GitIgnore,
    OkIgnore,
    Detector,
    FastMode,
    SizeLimit,
    SymlinkPolicy,
    LanguageSupport,
    Filesystem,
    Parser,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkippedPath {
    pub path: PathBuf,
    pub reason: SkipReason,
    pub source: SkipSource,
    pub safe_to_show: bool,
}

impl SkipReason {
    /// Human-readable label for summaries (`secret-policy`, `too-large`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Ignored => "ignored",
            Self::Denied => "denied",
            Self::Hidden => "hidden",
            Self::UnsupportedLanguage => "unsupported-language",
            Self::Binary => "binary",
            Self::TooLarge => "too-large",
            Self::Generated => "generated",
            Self::Vendor => "vendor",
            Self::FastMode => "fast-mode",
            Self::SecretPolicy => "secret-policy",
            Self::SymlinkPolicy => "symlink-policy",
            Self::Error => "error",
        }
    }
}

/// Coverage of one recognised language: files discovery saw on disk versus files the
/// index holds, with every omission attributed to a skip reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LanguageCoverage {
    pub discovered: usize,
    pub indexed: usize,
    /// Indexed files flagged `is_generated`. Generated source is indexed and flagged so
    /// it stays in coverage; only document-corpus files are skipped as `generated`.
    #[serde(default)]
    pub generated: usize,
    #[serde(default)]
    pub skipped: BTreeMap<SkipReason, usize>,
}

impl LanguageCoverage {
    pub fn percent(&self) -> Option<f64> {
        (self.discovered > 0).then(|| self.indexed as f64 * 100.0 / self.discovered as f64)
    }
}

/// Coverage below this fraction is reported as a warning: a tenth of a corpus vanishing
/// behind an ingest rule is exactly the failure this summary exists to expose.
pub const INDEX_COVERAGE_WARN_PERCENT: f64 = 98.0;

/// A programming language is judged by percentage only once it has this many
/// discovered files; below it one hidden file swings the ratio without meaning.
pub const INDEX_COVERAGE_LANGUAGE_FLOOR: usize = 50;

/// A programming language missing at least this many files warns regardless of
/// percentage: 25 of 10,012 Java files is 99.75% and still a dropped package.
pub const INDEX_COVERAGE_MISSING_FILES_WARN: usize = 20;

/// What discovery found versus what the index holds, for every recognised language.
///
/// `discovered` counts only files the walker visited. Two things it cannot see are
/// counted beside it so the ratio is never read as more than it is: `pruned_dirs`,
/// directories cut from the walk by name (`target`, `node_modules`, `dist`, `build`,
/// `.venv`; `.git` and `.ok` are not counted, they are never user source), whose
/// contents are unknown; and `walk_errors`, directory reads that failed, whose files
/// were never discovered. Files whose language is unknown are not source files and
/// are not counted; their skips remain in `skip_counts`. Files admitted to the
/// document corpus count as indexed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IndexCoverage {
    pub discovered: usize,
    pub indexed: usize,
    #[serde(default)]
    pub generated: usize,
    #[serde(default)]
    pub skipped: BTreeMap<SkipReason, usize>,
    #[serde(default)]
    pub by_language: BTreeMap<String, LanguageCoverage>,
    /// Directories pruned by name before discovery; a real package named `build`
    /// lands here, not in `discovered`.
    #[serde(default)]
    pub pruned_dirs: usize,
    /// Directory reads that failed (`skip_counts.error`); their files are unknown.
    #[serde(default)]
    pub walk_errors: usize,
}

impl IndexCoverage {
    pub fn record_discovered(&mut self, language: &Language) {
        self.discovered += 1;
        self.by_language
            .entry(language.key().to_owned())
            .or_default()
            .discovered += 1;
    }

    pub fn record_indexed(&mut self, language: &Language, generated: bool) {
        self.indexed += 1;
        let entry = self
            .by_language
            .entry(language.key().to_owned())
            .or_default();
        entry.indexed += 1;
        if generated {
            self.generated += 1;
            entry.generated += 1;
        }
    }

    /// A file discovery counted as indexed that a later phase dropped — an unreadable file or a
    /// grammar that crashed on it. Moves it from `indexed` to `skipped[reason]` so
    /// `discovered == indexed + sum(skipped)` still holds for the language.
    pub fn record_indexed_dropped(
        &mut self,
        language: &Language,
        generated: bool,
        reason: SkipReason,
    ) {
        self.indexed = self.indexed.saturating_sub(1);
        if generated {
            self.generated = self.generated.saturating_sub(1);
        }
        if let Some(entry) = self.by_language.get_mut(language.key()) {
            entry.indexed = entry.indexed.saturating_sub(1);
            if generated {
                entry.generated = entry.generated.saturating_sub(1);
            }
        }
        self.record_skipped(language, reason);
    }

    pub fn record_skipped(&mut self, language: &Language, reason: SkipReason) {
        *self.skipped.entry(reason).or_default() += 1;
        *self
            .by_language
            .entry(language.key().to_owned())
            .or_default()
            .skipped
            .entry(reason)
            .or_default() += 1;
    }

    /// The all-languages ratio, reported everywhere. `None` when nothing was
    /// discovered: a ratio over zero files is not evidence.
    pub fn percent(&self) -> Option<f64> {
        (self.discovered > 0).then(|| self.indexed as f64 * 100.0 / self.discovered as f64)
    }

    /// `(discovered, indexed)` over programming languages only.
    pub fn programming_totals(&self) -> (usize, usize) {
        self.by_language
            .iter()
            .filter(|(language, _)| language_key_is_programming(language))
            .fold((0, 0), |(discovered, indexed), (_, coverage)| {
                (discovered + coverage.discovered, indexed + coverage.indexed)
            })
    }

    /// The ratio the warning is judged on. Hidden `.github/*.yml` and `.vscode/*.json`
    /// drag the all-languages ratio under the threshold on almost every repository; a
    /// warning that always fires stops being read, so the verdict follows the source.
    pub fn programming_percent(&self) -> Option<f64> {
        let (discovered, indexed) = self.programming_totals();
        (discovered > 0).then(|| indexed as f64 * 100.0 / discovered as f64)
    }

    pub fn below_warn_threshold(&self) -> bool {
        self.programming_percent()
            .is_some_and(|percent| percent < INDEX_COVERAGE_WARN_PERCENT)
    }

    /// Something the ratio cannot account for: unreadable or pruned directories.
    pub fn has_blind_spots(&self) -> bool {
        self.walk_errors > 0 || self.pruned_dirs > 0
    }

    /// Skip reasons by descending count, ties broken by reason order, at most `limit`.
    pub fn top_skip_reasons(&self, limit: usize) -> Vec<(SkipReason, usize)> {
        let mut reasons = self
            .skipped
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(reason, count)| (*reason, *count))
            .collect::<Vec<_>>();
        reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        reasons.truncate(limit);
        reasons
    }

    /// Programming languages that warrant a warning, as `(language, percent, files
    /// missing)`, most missing files first. A language qualifies when it is under the
    /// percentage threshold with at least `INDEX_COVERAGE_LANGUAGE_FLOOR` files
    /// discovered, or is missing `INDEX_COVERAGE_MISSING_FILES_WARN` files regardless
    /// of percentage. Config and prose languages never qualify: hidden `.github/*.yml`
    /// files drag yaml under 98% on almost every repository and say nothing about the
    /// source; they stay visible in the table.
    pub fn languages_below_warn_threshold(&self) -> Vec<(&str, f64, usize)> {
        let mut languages = self
            .by_language
            .iter()
            .filter(|(language, _)| language_key_is_programming(language))
            .filter_map(|(language, coverage)| {
                let percent = coverage.percent()?;
                let missing = coverage.discovered.saturating_sub(coverage.indexed);
                let by_ratio = coverage.discovered >= INDEX_COVERAGE_LANGUAGE_FLOOR
                    && percent < INDEX_COVERAGE_WARN_PERCENT;
                let by_count = missing >= INDEX_COVERAGE_MISSING_FILES_WARN;
                (by_ratio || by_count).then_some((language.as_str(), percent, missing))
            })
            .collect::<Vec<_>>();
        languages.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(b.0)));
        languages
    }

    /// The counts, judged ratio first: `921 of 922 programming-language files indexed
    /// (99.9%); 1,417 of 1,461 recognised files overall`. Shared by the index summary
    /// line and the doctor check so the two can never disagree.
    pub fn headline(&self) -> String {
        let Some(overall) = self.percent() else {
            return "no source files discovered".into();
        };
        let overall = format!(
            "{} of {} recognised files indexed ({overall:.1}%)",
            group_thousands(self.indexed),
            group_thousands(self.discovered)
        );
        let (discovered, indexed) = self.programming_totals();
        match self.programming_percent() {
            Some(percent) => format!(
                "{} of {} programming-language files indexed ({percent:.1}%); {overall} overall",
                group_thousands(indexed),
                group_thousands(discovered)
            ),
            None => format!("no programming-language files discovered; {overall}"),
        }
    }

    /// One line: the headline plus what was skipped and what the ratio cannot see.
    pub fn summary_line(&self) -> String {
        if self.percent().is_none() {
            return "no source files discovered".into();
        }
        let mut line = self.headline();
        let skipped = self.top_skip_reasons(usize::MAX);
        if !skipped.is_empty() {
            line.push_str("; skipped: ");
            line.push_str(&format_skip_reasons(&skipped));
        }
        for caveat in self.blind_spot_caveats() {
            line.push_str("; ");
            line.push_str(&caveat);
        }
        line
    }

    /// What the ratio does not cover, phrased for a summary line or a doctor check.
    pub fn blind_spot_caveats(&self) -> Vec<String> {
        let mut caveats = Vec::new();
        if self.pruned_dirs > 0 {
            caveats.push(format!(
                "{} {} pruned by name (contents not counted)",
                group_thousands(self.pruned_dirs),
                if self.pruned_dirs == 1 {
                    "directory"
                } else {
                    "directories"
                }
            ));
        }
        if self.walk_errors > 0 {
            caveats.push(format!(
                "{} walk {} (files under unreadable directories were never discovered)",
                group_thousands(self.walk_errors),
                if self.walk_errors == 1 {
                    "error"
                } else {
                    "errors"
                }
            ));
        }
        caveats
    }
}

/// `by_language` keys are `Language::key()` values; this is the inverse for the one
/// property the warning rule needs.
fn language_key_is_programming(key: &str) -> bool {
    matches!(
        key,
        "rust" | "java" | "type_script" | "java_script" | "python" | "go" | "sql"
    )
}

/// `25 secret-policy, 5 too-large`
pub fn format_skip_reasons(reasons: &[(SkipReason, usize)]) -> String {
    reasons
        .iter()
        .map(|(reason, count)| format!("{} {}", group_thousands(*count), reason.label()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `10012` -> `10,012`
pub fn group_thousands(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipResolutionQuality {
    pub candidates_considered: usize,
    pub proven: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    pub heuristic_candidates_retained: usize,
    #[serde(default)]
    pub proof_kind_counts: BTreeMap<String, usize>,
    #[serde(default)]
    pub resolver_strategy_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LanguageResolutionQuality {
    pub occurrences: usize,
    pub candidates_considered: usize,
    pub proven: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    pub candidate_cap_hits: usize,
    pub enrichment_time_us: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResolutionQualityReport {
    pub call_sites: usize,
    pub resolved_exact: usize,
    pub resolved_high: usize,
    pub ambiguous: usize,
    pub unresolved: usize,
    pub external: usize,
    pub legacy_only: usize,
    pub semantic_only: usize,
    pub disagreement: usize,
    #[serde(default)]
    pub candidate_cap_hits: usize,
    #[serde(default)]
    pub by_language: BTreeMap<String, LanguageResolutionQuality>,
    #[serde(default)]
    pub by_relationship: BTreeMap<String, RelationshipResolutionQuality>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct IndexQuality {
    #[serde(default)]
    pub index_mode: IndexMode,
    #[serde(default)]
    pub phase_reports: Vec<IndexPhaseReport>,
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
    #[serde(default)]
    pub skip_counts: BTreeMap<SkipReason, usize>,
    #[serde(default)]
    pub skipped_paths: Vec<SkippedPath>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_quality: Option<ResolutionQualityReport>,
    /// Absent on manifests written before coverage was recorded; readers must say so
    /// rather than treat absence as full coverage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<IndexCoverage>,
    pub quality_notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceGraphSchema {
    pub version: String,
    pub node_types: Vec<NodeTypeSpec>,
    pub edge_types: Vec<EdgeTypeSpec>,
    pub property_specs: Vec<PropertySpec>,
    pub feature_flags: Vec<String>,
    #[serde(default)]
    pub evidence_source_types: Vec<String>,
    #[serde(default)]
    pub query_features: Vec<String>,
    #[serde(default)]
    pub optional_evidence: Vec<OptionalEvidenceSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caveats: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NodeTypeSpec {
    pub name: String,
    pub stable: bool,
    pub description: String,
    pub required_fields: Vec<String>,
    pub optional_fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_available: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EdgeTypeSpec {
    pub name: String,
    pub stable: bool,
    pub description: String,
    pub source_types: Vec<String>,
    pub target_types: Vec<String>,
    pub required_evidence: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_available: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PropertySpec {
    pub name: String,
    pub type_name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OptionalEvidenceSpec {
    pub name: String,
    pub available: bool,
    pub status: String,
    pub evidence_count: usize,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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
    Resource,
    ArchitectureComponent,
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GraphEdgeType {
    Contains,
    Defines,
    References,
    UsesType,
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
    TestCovers,
    Validates,
    OwnedBy,
    ChangedBy,
    FailedIn,
    BelongsTo,
    MentionedIn,
    RelatedToTicket,
    SimilarTo,
    SemanticallyRelated,
    /// `from` is produced from, or exists to exercise or describe, `to`: a generated file and the
    /// source its header names, a test and the module it tests, a `.d.ts` and its implementation.
    /// The two are siblings of one edit; the edge's proof (declared origin) or its absence
    /// (naming convention) says how much that can be trusted.
    DerivedFrom,
}

/// Source label of a `DERIVED_FROM` fact whose origin the derived file's own header declares.
/// Shared between the ingest pass that emits the fact and the graph builder that attaches the
/// declared-origin proof to it.
pub const DERIVED_FILE_DECLARED_ORIGIN_SOURCE: &str = "open-kioku-derived/declared-origin";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphNode {
    pub id: NodeId,
    pub node_type: GraphNodeType,
    pub label: String,
    pub file_id: Option<FileId>,
    pub symbol_id: Option<SymbolId>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pass: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extractor_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambiguity: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quality_notes: Vec<String>,
}

impl Default for GraphNode {
    fn default() -> Self {
        Self {
            id: NodeId::new(""),
            node_type: GraphNodeType::File,
            label: String::new(),
            file_id: None,
            symbol_id: None,
            properties: BTreeMap::new(),
            schema_version: None,
            source_pass: None,
            index_mode: None,
            extractor_version: None,
            ambiguity: vec![],
            quality_notes: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphEdge {
    pub id: EdgeId,
    pub from: NodeId,
    pub to: NodeId,
    pub edge_type: GraphEdgeType,
    pub evidence: Evidence,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_pass: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extractor_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambiguity: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quality_notes: Vec<String>,
}

impl Default for GraphEdge {
    fn default() -> Self {
        Self {
            id: EdgeId::new(""),
            from: NodeId::new(""),
            to: NodeId::new(""),
            edge_type: GraphEdgeType::References,
            evidence: Evidence::default(),
            properties: BTreeMap::new(),
            schema_version: None,
            source_pass: None,
            index_mode: None,
            extractor_version: None,
            ambiguity: vec![],
            quality_notes: vec![],
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalSourceKind {
    Lexical,
    Document,
    ExactSemantic,
    Graph,
    SemanticVector,
    Validation,
    GitHistory,
    Runtime,
    /// A file admitted because the graph records it as a derived sibling of a candidate, not
    /// because a retrieval stream ranked it. Deliberately distinct from [`Self::Graph`]: budget
    /// selection gives graph and validation evidence priority over score, and an admitted
    /// sibling has no score of its own to earn that with.
    DerivedSibling,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalAuthority {
    Heuristic,
    Corroborating,
    Exact,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetrievalContribution {
    pub source: RetrievalSourceKind,
    pub rank: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_score: Option<f32>,
    pub rrf_contribution: f32,
    pub authority: RetrievalAuthority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_id: Option<SymbolId>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub rationale: String,
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
pub struct RetrievalUnitKey {
    pub path: String,
    #[serde(default)]
    pub line_start: Option<u32>,
    #[serde(default)]
    pub line_end: Option<u32>,
    #[serde(default)]
    pub symbol_id: Option<SymbolId>,
}

impl RetrievalUnitKey {
    pub fn from_result(result: &SearchResult) -> Self {
        Self::from_parts(
            &result.path,
            result.line_range.as_ref(),
            result.symbol.as_ref().map(|symbol| &symbol.id),
        )
    }

    pub fn from_parts(
        path: &Path,
        line_range: Option<&LineRange>,
        symbol_id: Option<&SymbolId>,
    ) -> Self {
        Self {
            path: path
                .to_string_lossy()
                .replace('\\', "/")
                .trim_start_matches("./")
                .to_string(),
            line_start: line_range.map(|range| range.start),
            line_end: line_range.map(|range| range.end),
            symbol_id: symbol_id.cloned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetrievalTrace {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_key: Option<RetrievalUnitKey>,
    pub fused_score: f32,
    pub authority: RetrievalAuthority,
    #[serde(default)]
    pub contributions: Vec<RetrievalContribution>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextBudget {
    pub max_tokens: usize,
    pub reserve_for_instructions: usize,
    pub reserve_for_validation: usize,
    pub max_per_file: usize,
    pub max_primary_files: usize,
    /// How many of the top-ranked distinct primary files have their selected regions widened
    /// (enclosing symbol, the file's other ranked units, adjacent chunks) after selection.
    /// Widening never reorders selection or displaces another file's first unit.
    #[serde(default = "ContextBudget::default_region_files")]
    pub region_files: usize,
    /// Estimated-token ceiling a widened file may reach, including its originally selected
    /// units. Zero disables widening.
    #[serde(default = "ContextBudget::default_region_tokens_per_file")]
    pub region_tokens_per_file: usize,
}

impl ContextBudget {
    pub const DEFAULT_REGION_FILES: usize = 3;
    pub const DEFAULT_REGION_TOKENS_PER_FILE: usize = 1_200;

    pub fn available_context_tokens(&self) -> usize {
        self.max_tokens
            .saturating_sub(self.reserve_for_instructions)
            .saturating_sub(self.reserve_for_validation)
    }

    /// Whether `max_tokens` is a real ceiling. `from_file_limit` fills `max_tokens` and
    /// `max_per_file` with a sentinel so that only the file limit binds; renderers ask this
    /// rather than print a nineteen-digit sentinel as if it were a budget.
    pub fn has_token_ceiling(&self) -> bool {
        self.max_tokens < usize::MAX / 8 || self.max_per_file < usize::MAX / 8
    }

    pub fn from_file_limit(limit: usize) -> Self {
        Self {
            max_tokens: usize::MAX / 4,
            reserve_for_instructions: 0,
            reserve_for_validation: 0,
            max_per_file: usize::MAX / 4,
            max_primary_files: limit,
            region_files: Self::DEFAULT_REGION_FILES,
            region_tokens_per_file: Self::DEFAULT_REGION_TOKENS_PER_FILE,
        }
    }

    fn default_region_files() -> usize {
        Self::DEFAULT_REGION_FILES
    }

    fn default_region_tokens_per_file() -> usize {
        Self::DEFAULT_REGION_TOKENS_PER_FILE
    }
}

impl Default for ContextBudget {
    fn default() -> Self {
        Self {
            max_tokens: 8_000,
            reserve_for_instructions: 1_000,
            reserve_for_validation: 1_000,
            max_per_file: 2,
            max_primary_files: 8,
            region_files: Self::DEFAULT_REGION_FILES,
            region_tokens_per_file: Self::DEFAULT_REGION_TOKENS_PER_FILE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextSelectedUnit {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<LineRange>,
    pub estimated_tokens: usize,
    pub authority: RetrievalAuthority,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub rationale: String,
    /// Which part of the pack this unit accounts for. Consumers that measure what retrieval
    /// selected must read this rather than the free-text rationale: the two kinds are costed
    /// differently and mixing them makes a metric's basis depend on the build.
    #[serde(default)]
    pub kind: ContextUnitKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextUnitKind {
    /// A unit retrieval selected under the context budget, including any region widening.
    #[default]
    Primary,
    /// A supporting file the pack lists from impact expansion, costed at its listing size.
    Supporting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RetrievalSourceCount {
    pub source: RetrievalSourceKind,
    pub selected_file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContextSelectionDiagnostics {
    pub budget: ContextBudget,
    pub available_context_tokens: usize,
    pub estimated_tokens_selected: usize,
    #[serde(default)]
    pub source_stream_mix: Vec<RetrievalSourceCount>,
    #[serde(default)]
    pub exact_evidence_count: usize,
    #[serde(default)]
    pub ambiguity_unresolved_count: usize,
    #[serde(default)]
    pub unattributed_selected_file_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_confidence: Option<Confidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstention_reason: Option<String>,
    #[serde(default)]
    pub selected_units: Vec<ContextSelectedUnit>,
    #[serde(default)]
    pub per_file_tokens: BTreeMap<PathBuf, usize>,
    #[serde(default)]
    pub omitted_due_to_budget: Vec<String>,
    #[serde(default)]
    pub omitted_due_to_caps: Vec<String>,
    #[serde(default)]
    pub omitted_high_value: Vec<String>,
    #[serde(default)]
    pub redundancy_omissions: Vec<String>,
    #[serde(default)]
    pub caveats: Vec<String>,
}

impl Default for ContextSelectionDiagnostics {
    fn default() -> Self {
        Self {
            budget: ContextBudget {
                max_tokens: 0,
                reserve_for_instructions: 0,
                reserve_for_validation: 0,
                max_per_file: 0,
                max_primary_files: 0,
                region_files: 0,
                region_tokens_per_file: 0,
            },
            available_context_tokens: 0,
            estimated_tokens_selected: 0,
            source_stream_mix: Vec::new(),
            exact_evidence_count: 0,
            ambiguity_unresolved_count: 0,
            unattributed_selected_file_count: 0,
            retrieval_confidence: None,
            abstention_reason: None,
            selected_units: Vec::new(),
            per_file_tokens: BTreeMap::new(),
            omitted_due_to_budget: Vec::new(),
            omitted_due_to_caps: Vec::new(),
            omitted_high_value: Vec::new(),
            redundancy_omissions: Vec::new(),
            caveats: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskFamily {
    IssueToCode,
    CodeToTest,
    TraceToCode,
    CommentToContext,
    EditToRipple,
    Documentation,
    MixedCodeDocs,
    #[default]
    General,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryShape {
    ExactIdentifier,
    QualifiedSymbol,
    PathReference,
    ErrorTrace,
    ApiResource,
    Conceptual,
    MixedStructuredNaturalLanguage,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetrievalRoutingDiagnostics {
    pub task_family: TaskFamily,
    pub confidence: f32,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub enabled_sources: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub required_evidence: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub query_shape: QueryShape,
    #[serde(default)]
    pub query_shape_confidence: f32,
    #[serde(default)]
    pub query_shape_signals: Vec<String>,
    #[serde(default)]
    pub query_shape_ambiguities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_shape_fallback_reason: Option<String>,
}

impl Default for RetrievalRoutingDiagnostics {
    fn default() -> Self {
        Self {
            task_family: TaskFamily::General,
            confidence: 0.0,
            reasons: Vec::new(),
            enabled_sources: Vec::new(),
            required_evidence: Vec::new(),
            query_shape: QueryShape::Unknown,
            query_shape_confidence: 0.0,
            query_shape_signals: Vec::new(),
            query_shape_ambiguities: Vec::new(),
            query_shape_fallback_reason: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, Default)]
pub struct RetrievalDiagnostics {
    #[serde(default)]
    pub traces: Vec<RetrievalTrace>,
    #[serde(default)]
    pub caveats: Vec<String>,
    #[serde(default)]
    pub sources_attempted: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub sources_succeeded: Vec<RetrievalSourceKind>,
    #[serde(default)]
    pub selection: ContextSelectionDiagnostics,
    #[serde(default)]
    pub routing: RetrievalRoutingDiagnostics,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ValidationPlan {
    pub commands: Vec<String>,
    pub tests: Vec<TestTarget>,
    pub requires_approval: bool,
    pub evidence: Vec<Evidence>,
}

/// One dependent reached through a typed relationship edge, labeled with the authority that
/// justifies (or fails to justify) presenting it as structural truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipImpact {
    /// Repository-relative path of the impacted file.
    pub path: PathBuf,
    /// Qualified name of the impacted symbol, when the edge endpoint is a symbol node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    /// The changed symbol (or file) this impact was derived from.
    pub source: String,
    pub edge_type: GraphEdgeType,
    /// Effective authority recomputed from the edge's typed proofs.
    pub authority: RelationshipAuthority,
    /// Proof kinds present on the edge, in stable sorted order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proof_kinds: Vec<RelationshipProofKind>,
    /// Whether the edge or any proof records unresolved ambiguity.
    #[serde(default)]
    pub ambiguous: bool,
    /// Human-readable derivation, e.g. "calls edge into `issue_token` (exact call site)".
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImpactReport {
    pub target: String,
    pub direct_impacts: Vec<SearchResult>,
    pub indirect_impacts: Vec<SearchResult>,
    pub risk_report: RiskReport,
    pub evidence: Vec<Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture_policy: Option<PolicyCheckReport>,
    #[serde(default)]
    pub score_breakdown: Vec<ScoreComponent>,
    /// Dependents whose relationship to the target is structurally proven (authoritative typed
    /// proofs). A heuristic edge can never appear here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proven_impact: Vec<RelationshipImpact>,
    /// Dependents reached only through heuristic or corroborating relationships. Presented as
    /// possibilities, never as structural facts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub possible_impact: Vec<RelationshipImpact>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ContextPack {
    pub task: String,
    pub intent: String,
    #[serde(default)]
    pub retrieval_diagnostics: RetrievalDiagnostics,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture_policy: Option<PolicyCheckReport>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub architecture_policy: Option<PolicyCheckReport>,
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
    #[serde(default)]
    pub evidence_quality: EvidenceQuality,
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

    fn relevance_probe(path: &str, snippet: &str) -> SearchResult {
        SearchResult {
            path: std::path::PathBuf::from(path),
            line_range: None,
            snippet: snippet.into(),
            symbol: None,
            score: 12.0,
            match_reason: "lexical match".into(),
            evidence: vec!["BM25 lexical match from local Tantivy index".into()],
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
        }
    }

    #[test]
    fn a_task_with_no_terms_in_the_context_scores_zero_relevance() {
        let selected = vec![
            relevance_probe("java/auth/AuthService.java", "public Token issueToken()"),
            relevance_probe("go/shipping/service.go", "func Quote(order Order) Price"),
        ];
        assert_eq!(
            task_relevance_score("calibrate the nightly seismograph ledger", &selected),
            0.0,
            "no task term appears in the selected context"
        );
        assert!(
            task_relevance_score("issueToken for AuthService", &selected) > 0.5,
            "a task about the selected code must score relevant"
        );
    }

    #[test]
    fn a_structurally_complete_pack_about_nothing_cannot_be_confident() {
        // Every completeness signal maxed, as a pack full of irrelevant files
        // produces: validation targets, a tight boundary, dense evidence. Only
        // relevance dissents. Before this, that scored High (0.81).
        let irrelevant = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            primary_file_count: 8,
            evidence_count: 40,
            exact_reference_count: 0,
            validation_count: 6,
            validation_with_command_count: 6,
            negative_evidence_count: 0,
            allowed_file_count: 3,
            runtime_signal_count: 4,
            task_relevance: 0.0,
            ..Default::default()
        });
        assert_eq!(
            irrelevant.overall_enum,
            Confidence::Low,
            "a pack whose task terms appear nowhere in it must not be confident, got {:?} ({})",
            irrelevant.overall_enum,
            irrelevant.overall_score
        );
        assert!(irrelevant
            .blockers
            .iter()
            .any(|b| b.contains("no task term")));
    }

    #[test]
    fn absent_exact_evidence_caps_confidence_below_high() {
        // The previous cap required exact, validation and runtime to *all* be
        // zero, so synthesizing one validation target bought back High.
        let no_exact = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            primary_file_count: 5,
            evidence_count: 30,
            exact_reference_count: 0,
            validation_count: 4,
            validation_with_command_count: 4,
            negative_evidence_count: 0,
            allowed_file_count: 2,
            runtime_signal_count: 2,
            task_relevance: 1.0,
            ..Default::default()
        });
        assert!(
            no_exact.overall_score <= 0.74,
            "without exact evidence confidence must stay below High, got {}",
            no_exact.overall_score
        );
        assert_ne!(no_exact.overall_enum, Confidence::High);
    }

    use super::{
        count_resolution_notes, named_anchors, negative_evidence_signal_count,
        reconcile_score_breakdown, score_component_total, task_relevance_score,
        unmatched_named_anchors, Confidence, ConfidenceBreakdown, ConfidenceSignalInput, EdgeId,
        Evidence, EvidenceSourceType, FileRange, GitChangeKind, GitCommitId, GitCommitRecord,
        GitFileTouch, GitSymbolTouch, GraphEdge, GraphEdgeType, GraphNode, GraphNodeType,
        HistoryRecordId, HistorySnapshot, HistorySummary, IndexQuality, LineRange,
        NegativeEvidence, NodeId, Owner, PathInterner, ScopeId, ScoreComponent, SearchResult,
        SharedPath, SharedStr, SourceRange, StringInterner, Symbol, SymbolId, Visibility,
        HISTORY_SCHEMA_VERSION,
    };
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;

    #[test]
    fn shared_path_serializes_exactly_as_a_pathbuf() {
        let raw = "modules/lang-expression/src/main/java/com/acme/Script.java";
        let as_pathbuf = serde_json::to_string(&std::path::PathBuf::from(raw)).unwrap();
        let as_shared = serde_json::to_string(&SharedPath::from(raw)).unwrap();
        assert_eq!(
            as_pathbuf, as_shared,
            "the wire format must be indistinguishable from PathBuf"
        );
    }

    #[test]
    fn shared_path_round_trips_and_matches_stored_rows() {
        let range: FileRange =
            serde_json::from_str(r#"{"path":"src/auth.rs","line_range":{"start":18,"end":18}}"#)
                .expect("a row written when the field was a PathBuf must still load");
        assert_eq!(range.path.as_path(), std::path::Path::new("src/auth.rs"));
        let json = serde_json::to_string(&range).unwrap();
        assert_eq!(
            json,
            r#"{"path":"src/auth.rs","line_range":{"start":18,"end":18}}"#
        );
    }

    #[test]
    fn path_interner_shares_one_allocation_per_distinct_path() {
        let interner = PathInterner::new();
        let a = interner.intern(std::path::Path::new("src/auth.rs"));
        let b = interner.intern(std::path::Path::new("src/auth.rs"));
        let c = interner.intern(std::path::Path::new("src/other.rs"));
        assert!(
            std::ptr::eq(a.as_path(), b.as_path()),
            "a repeated path must share one allocation"
        );
        assert_eq!(c.as_path(), std::path::Path::new("src/other.rs"));
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn interner_returns_one_allocation_for_repeated_messages() {
        let interner = StringInterner::new();
        let text = "symbol registry resolved `X` to `Y` via unique-project-name";
        let first = interner.intern(text.to_string());
        let second = interner.intern(text.to_string());
        assert!(
            std::ptr::eq(first.as_str().as_ptr(), second.as_str().as_ptr()),
            "repeated messages must share one allocation"
        );
        assert_eq!(
            interner.len(),
            1,
            "identical text must not add a second entry"
        );
    }

    #[test]
    fn interner_keeps_distinct_messages_apart() {
        let interner = StringInterner::new();
        let a = interner.intern("first".to_string());
        let b = interner.intern("second".to_string());
        assert_eq!(a.as_str(), "first");
        assert_eq!(b.as_str(), "second");
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn interner_is_consistent_under_concurrent_use() {
        use std::sync::Arc as StdArc;
        let interner = StdArc::new(StringInterner::new());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let interner = StdArc::clone(&interner);
                std::thread::spawn(move || {
                    for i in 0..200 {
                        let m = interner.intern(format!("message {}", i % 25));
                        assert_eq!(m.as_str(), format!("message {}", i % 25));
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker thread panicked");
        }
        assert_eq!(
            interner.len(),
            25,
            "concurrent interning must not create duplicate entries"
        );
    }

    #[test]
    fn evidence_message_serializes_exactly_as_a_json_string() {
        let text = "symbol registry resolved `RiskReport` to `crates::core::RiskReport` via unique-project-name; candidates=1";
        let as_string = serde_json::to_string(&text.to_string()).unwrap();
        let as_message = serde_json::to_string(&SharedStr::from(text.to_string())).unwrap();
        assert_eq!(
            as_string, as_message,
            "the wire format must be indistinguishable from String"
        );
    }

    #[test]
    fn evidence_message_round_trips_through_json() {
        for text in [
            "",
            "plain",
            "with \"quotes\" and \\backslash",
            "multi\nline",
            "unicode \u{1f600}",
        ] {
            let original = SharedStr::from(text);
            let json = serde_json::to_string(&original).unwrap();
            let restored: SharedStr = serde_json::from_str(&json).unwrap();
            assert_eq!(original, restored, "round trip changed {text:?}");
            assert_eq!(restored.as_str(), text);
        }
    }

    #[test]
    fn evidence_message_deserializes_from_a_plain_string_field() {
        // MCP payloads and stored documents written when this field was a plain `String`
        // must still load.
        let evidence: Evidence = serde_json::from_str(
            r#"{"id":"e1","source":"s","source_type":"lexical","file_range":null,
                "symbol_id":null,"confidence":"high","message":"stored as a plain string",
                "indexed_at":"2026-01-01T00:00:00Z"}"#,
        )
        .expect("legacy row must deserialize");
        assert_eq!(evidence.message.as_str(), "stored as a plain string");
    }

    #[test]
    fn cloning_an_evidence_message_shares_one_allocation() {
        let original = SharedStr::from("shared".to_string());
        let copy = original.clone();
        assert!(
            std::ptr::eq(original.as_str().as_ptr(), copy.as_str().as_ptr()),
            "clone must be a refcount bump, not a copy - this is the whole point"
        );
    }

    #[test]
    fn quality_counts_only_import_resolver_notes() {
        let quality = IndexQuality {
            quality_notes: vec![
                "import resolver caveat in src/lib.rs for `crate::missing`: unresolved import"
                    .into(),
                "symbol registry unresolved `documentation_word` in chunk abc".into(),
                "ambiguous wording in a non-resolver diagnostic".into(),
            ],
            ..Default::default()
        };

        assert_eq!(
            count_resolution_notes(&quality, "import resolver caveat", "unresolved import"),
            1
        );
        assert_eq!(
            count_resolution_notes(&quality, "import resolver caveat", "ambiguous import"),
            0
        );
    }

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
            task_relevance: 1.0,
            ..Default::default()
        };

        let first = ConfidenceBreakdown::from_signals(input.clone());
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
            task_relevance: 1.0,
            ..Default::default()
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
            task_relevance: 1.0,
            ..Default::default()
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
            task_relevance: 1.0,
            ..Default::default()
        });

        assert!(breakdown.overall_score <= 0.60);
        assert_ne!(breakdown.overall_enum, Confidence::High);
        assert!(!breakdown.blockers.is_empty());
    }

    #[test]
    fn exact_label_requires_exact_reference_evidence() {
        // Every completeness signal maxed and nothing to caveat: the only thing missing is
        // exact provenance. The label must stop at High, and the score under 0.75.
        let complete = ConfidenceSignalInput {
            primary_file_count: 2,
            evidence_count: 8,
            exact_reference_count: 0,
            validation_count: 2,
            validation_with_command_count: 2,
            negative_evidence_count: 0,
            allowed_file_count: 2,
            runtime_signal_count: 1,
            task_relevance: 1.0,
            ..Default::default()
        };
        let without_exact = ConfidenceBreakdown::from_signals(complete.clone());
        assert_ne!(without_exact.overall_enum, Confidence::Exact);
        assert!(without_exact.overall_score <= 0.74, "{without_exact:?}");
        assert!(without_exact
            .caveats
            .iter()
            .any(|caveat| caveat == "exact symbol/reference evidence is absent"));

        let with_exact = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            exact_reference_count: 1,
            ..complete
        });
        assert_eq!(with_exact.overall_enum, Confidence::Exact);
    }

    #[test]
    fn unmatched_task_identifiers_cap_confidence_below_medium_and_name_them() {
        let base = ConfidenceSignalInput {
            primary_file_count: 3,
            evidence_count: 12,
            exact_reference_count: 2,
            validation_count: 3,
            validation_with_command_count: 3,
            negative_evidence_count: 0,
            allowed_file_count: 3,
            runtime_signal_count: 1,
            task_relevance: 0.8,
            named_anchor_count: 2,
            unmatched_anchors: vec![
                "FrobnicateWidgetManager".into(),
                "reticulate_splines".into(),
            ],
        };
        let all_missing = ConfidenceBreakdown::from_signals(base.clone());
        assert_eq!(all_missing.overall_enum, Confidence::Low);
        assert!(all_missing.overall_score <= 0.50, "{all_missing:?}");
        assert!(all_missing
            .blockers
            .iter()
            .any(|blocker| blocker.contains("FrobnicateWidgetManager")
                && blocker.contains("reticulate_splines")));

        let some_missing = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            unmatched_anchors: vec!["reticulate_splines".into()],
            ..base
        });
        assert!(some_missing
            .blockers
            .iter()
            .all(|blocker| !blocker.contains("reticulate_splines")));
        assert!(some_missing
            .caveats
            .iter()
            .any(|caveat| caveat.contains("1 of 2") && caveat.contains("reticulate_splines")));
        assert!(some_missing.overall_score > 0.50);
    }

    #[test]
    fn negative_evidence_signal_count_counts_retrieval_misses_only() {
        let item = |scope: &str| NegativeEvidence {
            query: "task".into(),
            scope: scope.into(),
            inspected_sources: Vec::new(),
            reason: scope.into(),
            confidence: 0.8,
            suggested_next_probe: None,
        };
        let items = [
            "exact_references",
            "runtime",
            "validation",
            "history",
            "boundary",
            "anchor",
            "primary_context",
        ]
        .map(item);
        // Absent exact/runtime/validation/history evidence is priced by its own component.
        assert_eq!(negative_evidence_signal_count(&items), 2);
    }

    #[test]
    fn named_anchors_are_code_identifiers_and_unmatched_ones_are_reported() {
        let task = "fix the null check in FrobnicateWidgetManager::reticulate_splines for ABC-123";
        assert_eq!(
            named_anchors(task),
            vec![
                "FrobnicateWidgetManager".to_string(),
                "reticulate_splines".to_string()
            ]
        );
        let selected = vec![relevance_probe(
            "src/widgets.rs",
            "impl FrobnicateWidgetManager { fn frobnicate(&self) {} }",
        )];
        assert_eq!(
            unmatched_named_anchors(task, &selected),
            vec!["reticulate_splines".to_string()]
        );
        assert!(unmatched_named_anchors(task, &[]).is_empty());
        assert!(unmatched_named_anchors("make it faster", &selected).is_empty());
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
    fn legacy_symbol_touch_json_remains_compatible() {
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

    #[test]
    fn uses_type_edge_type_has_stable_json_contract() {
        let json = serde_json::to_string(&GraphEdgeType::UsesType).unwrap();
        assert_eq!(json, "\"USES_TYPE\"");
        let decoded: GraphEdgeType = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, GraphEdgeType::UsesType);
    }

    #[test]
    fn derived_from_edge_type_has_stable_json_contract() {
        let json = serde_json::to_string(&GraphEdgeType::DerivedFrom).unwrap();
        assert_eq!(json, "\"DERIVED_FROM\"");
        let decoded: GraphEdgeType = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, GraphEdgeType::DerivedFrom);
    }

    #[test]
    fn legacy_graph_json_deserializes_with_default_metadata() {
        let decoded_node: GraphNode = serde_json::from_value(serde_json::json!({
            "id": "node:file",
            "node_type": "file",
            "label": "src/lib.rs",
            "file_id": "file:src/lib.rs",
            "symbol_id": null
        }))
        .unwrap();
        assert!(decoded_node.properties.is_empty());
        assert!(decoded_node.schema_version.is_none());
        assert!(decoded_node.ambiguity.is_empty());
        assert!(decoded_node.quality_notes.is_empty());

        let decoded_edge: GraphEdge = serde_json::from_value(serde_json::json!({
            "id": "edge:defines",
            "from": "node:file",
            "to": "node:symbol",
            "edge_type": "DEFINES",
            "evidence": {
                "id": "evidence:legacy",
                "source": "tree-sitter",
                "source_type": "tree_sitter",
                "file_range": {
                    "path": "src/lib.rs",
                    "line_range": { "start": 1, "end": 3 }
                },
                "symbol_id": "symbol:main",
                "confidence": "high",
                "message": "legacy graph evidence",
                "indexed_at": "2026-06-01T12:00:00Z"
            }
        }))
        .unwrap();
        assert!(decoded_edge.properties.is_empty());
        assert!(decoded_edge.schema_version.is_none());
        assert!(decoded_edge.quality_notes.is_empty());
        assert!(decoded_edge.evidence.confidence_score.is_none());
        assert!(decoded_edge.evidence.confidence_reason.is_none());
        assert!(decoded_edge.evidence.freshness.is_none());
    }

    #[test]
    fn enriched_graph_json_round_trips_metadata() {
        let indexed_at = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        let node = GraphNode {
            id: NodeId::new("node:file"),
            node_type: GraphNodeType::File,
            label: "src/lib.rs".into(),
            file_id: None,
            symbol_id: Some(SymbolId::new("symbol:main")),
            properties: BTreeMap::from([(
                "qualified_name".into(),
                serde_json::Value::String("crate::main".into()),
            )]),
            schema_version: Some("graph-v1".into()),
            source_pass: Some("tree_sitter".into()),
            index_mode: Some("scip".into()),
            extractor_version: Some("open-kioku-test".into()),
            ambiguity: vec!["overloaded symbol name".into()],
            quality_notes: vec!["exact definition".into()],
        };
        let edge = GraphEdge {
            id: EdgeId::new("edge:defines"),
            from: NodeId::new("node:file"),
            to: NodeId::new("node:symbol"),
            edge_type: GraphEdgeType::Defines,
            evidence: Evidence {
                id: super::EvidenceId::new("evidence:rich"),
                source: "scip".into(),
                source_type: EvidenceSourceType::Scip,
                file_range: Some(FileRange {
                    path: "src/lib.rs".into(),
                    line_range: Some(LineRange { start: 1, end: 1 }),
                }),
                symbol_id: Some(SymbolId::new("symbol:main")),
                confidence: Confidence::Exact,
                message: "exact reference".into(),
                indexed_at,
                confidence_score: Some(0.99),
                confidence_reason: Some("SCIP exact occurrence".into()),
                freshness: Some("fresh".into()),
            },
            properties: BTreeMap::from([("call_kind".into(), serde_json::json!("direct"))]),
            schema_version: Some("graph-v1".into()),
            source_pass: Some("scip".into()),
            index_mode: Some("full".into()),
            extractor_version: Some("scip-cli".into()),
            ambiguity: vec!["dynamic dispatch not expanded".into()],
            quality_notes: vec!["exact edge".into()],
        };

        let decoded_node: GraphNode =
            serde_json::from_str(&serde_json::to_string(&node).unwrap()).unwrap();
        let decoded_edge: GraphEdge =
            serde_json::from_str(&serde_json::to_string(&edge).unwrap()).unwrap();

        assert_eq!(decoded_node.properties, node.properties);
        assert_eq!(decoded_node.schema_version, Some("graph-v1".into()));
        assert_eq!(decoded_node.quality_notes, vec!["exact definition"]);
        assert_eq!(decoded_edge.properties, edge.properties);
        assert_eq!(decoded_edge.evidence.confidence_score, Some(0.99));
        assert_eq!(
            decoded_edge.evidence.confidence_reason.as_deref(),
            Some("SCIP exact occurrence")
        );
        assert_eq!(decoded_edge.evidence.freshness.as_deref(), Some("fresh"));
    }

    #[test]
    fn test_pr1_semantic_ir_serde_backwards_compatibility() {
        let old_symbol_json = r#"{
            "id": "symbol:123",
            "name": "cancel",
            "qualified_name": "com.acme.ReservationService.cancel",
            "kind": "method",
            "file_id": "file:src/Service.java",
            "range": {"start": 10, "end": 20},
            "language": "java",
            "confidence": "high",
            "provenance": "tree_sitter"
        }"#;

        let symbol: Symbol = serde_json::from_str(old_symbol_json).unwrap();
        assert_eq!(symbol.id.0, "symbol:123");
        assert_eq!(symbol.module_id, None);
        assert_eq!(symbol.parent_symbol_id, None);
        assert_eq!(symbol.scope_id, None);
        assert_eq!(symbol.signature, None);
        assert_eq!(symbol.visibility, Visibility::Unknown);

        let scope_id = ScopeId::new("scope:1");
        assert_eq!(serde_json::to_string(&scope_id).unwrap(), r#""scope:1""#);

        let range = SourceRange {
            start_line: 1,
            start_column: 5,
            end_line: 2,
            end_column: 15,
        };
        assert_eq!(range.start_line, 1);
        assert_eq!(range.start_column, 5);
        assert_eq!(range.end_line, 2);
        assert_eq!(range.end_column, 15);
    }
}

#[cfg(test)]
mod ri3_resolution_quality_core_tests {
    use super::*;

    #[test]
    fn old_index_quality_without_resolution_report_remains_readable() {
        let encoded = serde_json::to_value(IndexQuality::default()).unwrap();
        assert!(encoded.get("resolution_quality").is_none());
        let decoded: IndexQuality = serde_json::from_value(encoded).unwrap();
        assert!(decoded.resolution_quality.is_none());
    }

    #[test]
    fn resolution_report_round_trips_with_deterministic_relationship_order() {
        let mut report = ResolutionQualityReport::default();
        report.by_relationship.insert(
            "uses_type".into(),
            RelationshipResolutionQuality {
                candidates_considered: 2,
                proven: 1,
                heuristic_candidates_retained: 1,
                ..RelationshipResolutionQuality::default()
            },
        );
        report.by_relationship.insert(
            "calls".into(),
            RelationshipResolutionQuality {
                candidates_considered: 1,
                proven: 1,
                ..RelationshipResolutionQuality::default()
            },
        );
        let quality = IndexQuality {
            resolution_quality: Some(report.clone()),
            ..IndexQuality::default()
        };

        let first = serde_json::to_string(&quality).unwrap();
        let second = serde_json::to_string(&quality).unwrap();
        assert_eq!(first, second);
        assert!(first.find("\"calls\"").unwrap() < first.find("\"uses_type\"").unwrap());

        let decoded: IndexQuality = serde_json::from_str(&first).unwrap();
        assert_eq!(decoded.resolution_quality, Some(report));
    }
}

#[cfg(test)]
mod index_coverage_tests {
    use super::*;

    #[test]
    fn manifest_without_coverage_loads_as_unrecorded() {
        let encoded = serde_json::to_value(IndexQuality::default()).unwrap();
        assert!(encoded.get("coverage").is_none());
        let decoded: IndexQuality = serde_json::from_value(encoded).unwrap();
        assert!(decoded.coverage.is_none());
    }

    #[test]
    fn coverage_tallies_per_language_and_round_trips() {
        let mut coverage = IndexCoverage::default();
        for _ in 0..3 {
            coverage.record_discovered(&Language::Java);
        }
        coverage.record_indexed(&Language::Java, false);
        coverage.record_indexed(&Language::Java, true);
        coverage.record_skipped(&Language::Java, SkipReason::SecretPolicy);
        coverage.record_discovered(&Language::Python);
        coverage.record_skipped(&Language::Python, SkipReason::TooLarge);

        assert_eq!(coverage.discovered, 4);
        assert_eq!(coverage.indexed, 2);
        assert_eq!(coverage.generated, 1);
        assert_eq!(coverage.by_language["java"].indexed, 2);
        assert_eq!(coverage.by_language["java"].generated, 1);
        assert_eq!(coverage.by_language["python"].percent(), Some(0.0));
        // Ties fall back to declaration order, so the output is deterministic.
        assert_eq!(
            coverage.top_skip_reasons(3),
            vec![(SkipReason::TooLarge, 1), (SkipReason::SecretPolicy, 1)]
        );
        // Every file here is a programming-language file, so both ratios agree.
        assert_eq!(coverage.programming_totals(), (4, 2));
        assert!(coverage.below_warn_threshold());
        // Four files are under the per-language floor: the overall ratio warns, the
        // per-language rule does not.
        assert!(coverage.languages_below_warn_threshold().is_empty());
        assert_eq!(
            coverage.summary_line(),
            "2 of 4 programming-language files indexed (50.0%); 2 of 4 recognised files indexed (50.0%) overall; skipped: 1 too-large, 1 secret-policy"
        );

        let quality = IndexQuality {
            coverage: Some(coverage.clone()),
            ..IndexQuality::default()
        };
        let decoded: IndexQuality =
            serde_json::from_str(&serde_json::to_string(&quality).unwrap()).unwrap();
        assert_eq!(decoded.coverage, Some(coverage));
    }

    #[test]
    fn language_warning_targets_programming_languages_and_absolute_losses() {
        let mut coverage = IndexCoverage::default();
        let mut add = |language: &Language, discovered: usize, indexed: usize| {
            for _ in 0..discovered {
                coverage.record_discovered(language);
            }
            for _ in 0..indexed {
                coverage.record_indexed(language, false);
            }
            for _ in indexed..discovered {
                coverage.record_skipped(language, SkipReason::Hidden);
            }
        };
        // The motivating incident: 25 of 10,012 is 99.75% and still a dropped package.
        add(&Language::Java, 10_012, 9_987);
        // Under the ratio with enough files to mean it.
        add(&Language::Python, 100, 90);
        // Under the floor: one hidden file, no verdict.
        add(&Language::Rust, 4, 1);
        // Config formats never qualify however low they sit.
        add(&Language::Yaml, 900, 100);

        assert_eq!(
            coverage.languages_below_warn_threshold(),
            vec![
                ("java", 9_987.0 * 100.0 / 10_012.0, 25),
                ("python", 90.0, 10)
            ]
        );
    }

    #[test]
    fn blind_spots_are_named_in_the_summary() {
        let mut coverage = IndexCoverage::default();
        coverage.record_discovered(&Language::Go);
        coverage.record_indexed(&Language::Go, false);
        coverage.pruned_dirs = 2;
        coverage.walk_errors = 1;
        assert!(coverage.has_blind_spots());
        assert_eq!(
            coverage.summary_line(),
            "1 of 1 programming-language files indexed (100.0%); 1 of 1 recognised files indexed (100.0%) overall; 2 directories pruned by name (contents not counted); 1 walk error (files under unreadable directories were never discovered)"
        );
    }

    /// The motivating shape of a real repository: every source file indexed, while
    /// hidden `.github/*.yml` and `.vscode/*.json` sink the all-languages ratio. The
    /// verdict follows the source; the reported counts still show both.
    #[test]
    fn config_files_drag_the_overall_ratio_without_causing_a_warning() {
        let mut coverage = IndexCoverage::default();
        for _ in 0..900 {
            coverage.record_discovered(&Language::TypeScript);
            coverage.record_indexed(&Language::TypeScript, false);
        }
        for _ in 0..100 {
            coverage.record_discovered(&Language::Yaml);
        }
        for _ in 0..100 {
            coverage.record_skipped(&Language::Yaml, SkipReason::Hidden);
        }

        assert_eq!(coverage.percent(), Some(90.0));
        assert_eq!(coverage.programming_percent(), Some(100.0));
        assert!(!coverage.below_warn_threshold());
        assert!(coverage.languages_below_warn_threshold().is_empty());
        assert_eq!(
            coverage.summary_line(),
            "900 of 900 programming-language files indexed (100.0%); 900 of 1,000 recognised files indexed (90.0%) overall; skipped: 100 hidden"
        );

        // A source file going missing still warns, at the same overall ratio.
        coverage.record_discovered(&Language::TypeScript);
        coverage.record_skipped(&Language::TypeScript, SkipReason::SecretPolicy);
        for _ in 0..19 {
            coverage.record_discovered(&Language::TypeScript);
            coverage.record_indexed(&Language::TypeScript, false);
        }
        assert!(coverage.programming_percent().unwrap() > 99.8);
        assert!(!coverage.below_warn_threshold());
        // Under the ratio rule it is invisible; the absolute rule is what catches it.
        assert_eq!(coverage.languages_below_warn_threshold(), vec![]);
    }

    /// A repository with no recognised programming source at all: the ratio has no
    /// verdict to give, and the headline says so instead of implying one.
    #[test]
    fn a_docs_only_repository_has_no_programming_ratio() {
        let mut coverage = IndexCoverage::default();
        for _ in 0..10 {
            coverage.record_discovered(&Language::Markdown);
            coverage.record_indexed(&Language::Markdown, false);
        }
        assert_eq!(coverage.programming_percent(), None);
        assert!(!coverage.below_warn_threshold());
        assert_eq!(
            coverage.summary_line(),
            "no programming-language files discovered; 10 of 10 recognised files indexed (100.0%)"
        );
    }

    #[test]
    fn language_key_programming_flag_matches_the_enum() {
        for language in [
            Language::Rust,
            Language::Java,
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Go,
            Language::Yaml,
            Language::Json,
            Language::Toml,
            Language::Sql,
            Language::Markdown,
            Language::Text,
            Language::Unknown,
        ] {
            assert_eq!(
                language_key_is_programming(language.key()),
                language.is_programming(),
                "{language:?}"
            );
        }
    }

    #[test]
    fn empty_coverage_is_not_a_ratio() {
        let coverage = IndexCoverage::default();
        assert_eq!(coverage.percent(), None);
        assert!(!coverage.below_warn_threshold());
        assert_eq!(coverage.summary_line(), "no source files discovered");
    }

    #[test]
    fn language_key_matches_serialized_name() {
        for language in [
            Language::Rust,
            Language::Java,
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Go,
            Language::Yaml,
            Language::Json,
            Language::Toml,
            Language::Sql,
            Language::Markdown,
            Language::Text,
            Language::Unknown,
        ] {
            let serialized = serde_json::to_value(&language).unwrap();
            assert_eq!(serialized.as_str(), Some(language.key()));
        }
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_thousands(10012), "10,012");
        assert_eq!(group_thousands(1234567), "1,234,567");
    }
}

#[cfg(test)]
mod test_path_tests {
    use super::{is_test_path, query_wants_tests};

    #[test]
    fn gradle_source_sets_and_java_suffixes_are_tests() {
        for path in [
            "modules/ip-location/src/internalClusterTest/java/org/es/GeoIpDownloaderIT.java",
            "modules/ip-location/src/yamlRestTest/java/org/es/GeoIpDatabaseTestHelper.java",
            "modules/ip-location/src/test/java/org/es/GeoIpProcessorTests.java",
            "modules/ip-location/qa/geoip-reindexed/src/javaRestTest/java/GeoIpReindexedIT.java",
            "server/src/testFixtures/java/org/es/ESTestCase.java",
            "src/main/java/org/es/AbstractStringProcessorTestCase.java",
            "src/main/java/org/es/RoutingSpec.java",
            "crates/core/tests/api.rs",
            "crates/core/src/tests.rs",
            "pkg/store/store_test.go",
            "crates/open-kioku-tests/tests/integration.rs",
            "pkg/store/testdata/fixture.json",
            "src/components/__tests__/Button.tsx",
            "src/components/Button.test.tsx",
            "src/components/Button.spec.ts",
            "e2e/login.e2e.ts",
            "tests/test_auth.py",
            "app/conftest.py",
            "spec/models/user_spec.rb",
        ] {
            assert!(is_test_path(path), "{path} should be a test path");
        }
    }

    #[test]
    fn substring_lookalikes_and_source_are_not_tests() {
        for path in [
            "modules/ip-location/src/main/java/org/es/GeoIpProcessor.java",
            "server/src/main/java/org/es/cluster/ClusterState.java",
            "src/latest_news.rs",
            "src/attestation/verify.rs",
            "src/contest/scoring.py",
            "src/main/java/org/es/UNIT.java",
            "src/main/java/org/es/Test.java",
            "docs/testing-guide.md",
            // A crate or package *named* after tests is product code: this one is the test
            // selector. Directory suffixes `-tests`/`_tests` are therefore not a test rule;
            // test files inside such a directory still qualify by their own names.
            "crates/open-kioku-tests/src/lib.rs",
            "packages/e2e-tests-runner/src/index.ts",
            "src/greatest.rs",
            "",
        ] {
            assert!(!is_test_path(path), "{path} should not be a test path");
        }
    }

    #[test]
    fn query_wants_tests_is_narrow() {
        assert!(query_wants_tests("add tests for the geoip processor"));
        assert!(query_wants_tests("which spec covers routing"));
        assert!(query_wants_tests("identity: Add BenchmarkHashString"));
        assert!(!query_wants_tests("geoip processor"));
        assert!(!query_wants_tests("latest cluster state publication"));
    }
}
