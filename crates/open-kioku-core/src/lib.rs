use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

pub mod abstention;
pub mod analysis_semantics;
pub mod cargo_manifest;
pub mod identity;
pub mod process;
pub mod relationship;

pub use analysis_semantics::*;

pub use relationship::{
    graph_edge_authority, graph_edge_window_key, graph_edge_window_rank, graph_edge_window_tier,
    graph_route_authorities, graph_route_hop_authority, is_containment_edge_type,
    is_untyped_walk_excluded, normalize_relationship_proofs, relationship_authority,
    sort_graph_edges_for_window, strongest_shortest_route, RelationshipAuthority,
    RelationshipProof, RelationshipProofFilter, RelationshipProofKind, RouteSearch,
    GRAPH_EDGE_WINDOW_RANKS_PER_TIER, GRAPH_EDGE_WINDOW_RANK_VERSION, GRAPH_EDGE_WINDOW_TIER_MAX,
    RELATIONSHIP_PROOFS_PROPERTY, UNTYPED_WALK_EXCLUDED_EDGE_TYPES,
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
/// A stored manifest whose version is *newer* than this one is refused by the reader with
/// an upgrade-or-reindex message rather than deserialized on a best-effort basis.
/// Bumped to 2 in 4.0.0 for the compact graph tables, and to 3 for the typed quality notes.
pub const INDEX_MANIFEST_SCHEMA_VERSION: u32 = 3;

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
    /// A programming language the index holds too little of for an absence to be evidence,
    /// per [`crate::IndexCoverage::gaps`]. Priced by the `index_coverage` caps, not counted.
    pub const COVERAGE: &str = "coverage";
}

impl NegativeEvidence {
    /// Whether this item is priced by the `negative_evidence` confidence component.
    ///
    /// Absent exact references, validation, and runtime evidence each lower confidence
    /// through their own component and cap already; counting them here as well priced one
    /// absence twice, which is how `ok plan` reported "3 negative evidence signal(s)" for a
    /// task whose only defect was a repository without SCIP. Absent history and a
    /// docs-or-tests-only selection are reported but not priced: history contributes only
    /// positive score components in plans, and the boundary item classifies what matched
    /// rather than naming missing evidence. A `coverage` item is priced by the `index_coverage`
    /// caps in [`ConfidenceBreakdown::from_signals`]. What counts here is evidence that retrieval
    /// itself missed: no primary context at all, or a task identifier the selected context
    /// does not spell.
    pub fn lowers_confidence(&self) -> bool {
        matches!(
            self.scope.as_str(),
            negative_evidence_scope::PRIMARY_CONTEXT | negative_evidence_scope::ANCHOR
        )
    }

    /// The `coverage` item for `coverage`, whatever state it is in: the gap item when a record
    /// was read, and otherwise the item saying what is not known and why.
    pub fn for_coverage_input(query: &str, coverage: &CoverageInput) -> Option<Self> {
        match coverage {
            CoverageInput::Recorded(gaps) => Self::for_coverage_gaps(query, gaps),
            CoverageInput::Unavailable => Some(Self {
                query: query.into(),
                scope: negative_evidence_scope::COVERAGE.into(),
                inspected_sources: vec!["index_manifest.quality.coverage".into()],
                reason: UNRECORDED_COVERAGE_CAVEAT.into(),
                confidence: 0.80,
                suggested_next_probe: Some(
                    "Run `ok index .` to record which source files the index skipped and why. A cross-project index publishes no coverage record of its own; an imported snapshot carries whatever the exporting index recorded."
                        .into(),
                ),
            }),
            CoverageInput::Unreadable => Some(Self {
                query: query.into(),
                scope: negative_evidence_scope::COVERAGE.into(),
                inspected_sources: vec!["index_manifest.quality.coverage".into()],
                reason: UNREADABLE_COVERAGE_CAVEAT.into(),
                confidence: 0.80,
                suggested_next_probe: Some(
                    "Run `ok doctor .` to see whether the index manifest is readable, and `ok index .` to rebuild it if it is not."
                        .into(),
                ),
            }),
        }
    }

    /// The `coverage` item a context pack or plan publishes for `gaps`, or `None` without any.
    /// Its inspected sources are the gaps' evidence ids, so the item traces to the manifest
    /// coverage record `repo_status` reports. Context and plan both build it here, so the two
    /// surfaces word it identically.
    pub fn for_coverage_gaps(query: &str, gaps: &[CoverageGap]) -> Option<Self> {
        let first = gaps.first()?;
        Some(Self {
            query: query.into(),
            scope: negative_evidence_scope::COVERAGE.into(),
            inspected_sources: std::iter::once("index_manifest.quality.coverage".to_owned())
                .chain(gaps.iter().map(CoverageGap::evidence_id))
                .collect(),
            reason: gaps
                .iter()
                .map(CoverageGap::caveat)
                .collect::<Vec<_>>()
                .join("; "),
            confidence: 0.90,
            suggested_next_probe: Some(first.next_probe()),
        })
    }
}

/// The `negative_evidence_count` confidence input for a pack or plan: the items of its
/// reported `negative_evidence` list that [`NegativeEvidence::lowers_confidence`]. Context
/// and plan both derive the count from the list they publish, so the blocker
/// "N negative evidence signal(s) lowered confidence" is always traceable to N listed items.
pub fn negative_evidence_signal_count(items: &[NegativeEvidence]) -> usize {
    items.iter().filter(|item| item.lowers_confidence()).count()
}

/// Distinct evidence facts in a pack or plan. Records count once per id, except that the
/// per-line retrieval records of one result, `search:<path>:<range>:<index>`, count once for
/// their path and range: each line restates the same match under another query variant, so
/// counting them would grow density with query variants, not with evidence.
pub fn distinct_evidence_count(evidence: &[Evidence]) -> usize {
    evidence
        .iter()
        .map(|item| evidence_fact_key(item.id.0.as_str()))
        .collect::<BTreeSet<_>>()
        .len()
}

fn evidence_fact_key(id: &str) -> &str {
    if !id.starts_with("search:") {
        return id;
    }
    match id.rsplit_once(':') {
        Some((fact, index)) if !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()) => {
            fact
        }
        _ => id,
    }
}

#[cfg(test)]
mod evidence_count_tests {
    use super::*;

    fn record(id: &str) -> Evidence {
        Evidence {
            id: EvidenceId::new(id),
            ..Default::default()
        }
    }

    #[test]
    fn restated_retrieval_lines_of_one_result_count_as_one_fact() {
        // One primary file whose BM25, query-variant and region lines are all the same match.
        let evidence = vec![
            record("search:src/auth.rs:1-4:0"),
            record("search:src/auth.rs:1-4:1"),
            record("search:src/auth.rs:1-4:2"),
        ];
        let evidence_count = distinct_evidence_count(&evidence);
        assert_eq!(evidence_count, 1);

        let breakdown = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            primary_file_count: 1,
            evidence_count,
            task_relevance: 1.0,
            ..Default::default()
        });
        assert!(
            breakdown
                .caveats
                .iter()
                .any(|caveat| caveat == "evidence density is thin"),
            "{:?}",
            breakdown.caveats
        );
    }

    #[test]
    fn distinct_ranges_files_and_producers_stay_separate_facts() {
        let evidence = vec![
            record("search:src/auth.rs:1-4:0"),
            record("search:src/auth.rs:9-12:0"),
            record("search:src/lib.rs:1-4:0"),
            record("impact:src/auth.rs"),
            record("history-churn:src/auth.rs"),
        ];
        assert_eq!(distinct_evidence_count(&evidence), 5);
    }
}

/// `evidence` with repeated ids removed, keeping the first record for each id and the order
/// the producers emitted them. Every point that merges evidence from several producers
/// passes through this, so an `evidence_refs` id names one record rather than several.
pub fn dedupe_evidence_by_id(evidence: impl IntoIterator<Item = Evidence>) -> Vec<Evidence> {
    let mut seen = BTreeSet::new();
    evidence
        .into_iter()
        .filter(|item| seen.insert(item.id.clone()))
        .collect()
}

const DEFAULT_EVIDENCE_FRESHNESS_MAX_AGE_DAYS: i64 = 7;

/// The evidence-quality caveat for a manifest without SCIP exact references. Named so a
/// report that found exact references through another typed channel can retract it.
pub const EXACT_REFERENCE_UNAVAILABLE_CAVEAT: &str =
    "exact symbol/reference evidence is unavailable";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceQuality {
    pub index_mode: String,
    pub freshness: String,
    /// True when this report found a typed exact reference (SCIP evidence or an
    /// exact-authority selection), not whether the manifest holds SCIP references.
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

    /// The manifest flag only knows SCIP. A plan that counted exact references through
    /// another typed channel - an exact-authority anchor or an indexed occurrence - records
    /// that here, so one report cannot say `exact_references 1.00` and "exact evidence is
    /// unavailable" in the same breath.
    pub fn record_exact_references(&mut self, exact_reference_count: usize) {
        if exact_reference_count == 0 || self.exact_reference_available {
            return;
        }
        self.exact_reference_available = true;
        self.caveats
            .retain(|caveat| caveat != EXACT_REFERENCE_UNAVAILABLE_CAVEAT);
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
            caveats.push(EXACT_REFERENCE_UNAVAILABLE_CAVEAT.into());
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
            let note = note.message.to_ascii_lowercase();
            note.contains(&source) && note.contains(&needle)
        })
        .count()
}

fn failed_optional_passes(quality: &IndexQuality) -> Vec<String> {
    let mut passes = Vec::new();
    for note in quality
        .quality_notes
        .iter()
        .map(|note| &note.message)
        .chain(
            quality
                .phase_reports
                .iter()
                .flat_map(|report| report.warnings.iter()),
        )
    {
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
    /// [`named_anchors`] extracts them. Zero for a prose-only task; weak anchors are not
    /// counted.
    pub named_anchor_count: usize,
    /// The named and weak anchors no selected context spells, per
    /// [`unmatched_named_anchors`]. When every named identifier is unmatched the repository
    /// does not know the thing the task is about, and no structural completeness may present
    /// that as Medium.
    pub unmatched_anchors: Vec<String>,
    /// The task's [`weak_named_anchors`]: hyphenated lowercase words that may be prose. An
    /// unmatched one is named in a caveat, but never counts toward the all-unmatched blocker
    /// or its 0.50 cap, because its spelling does not establish that the task named code.
    pub weak_anchors: Vec<String>,
    /// What the manifest says about coverage: [`IndexCoverage::gaps`] when it recorded any,
    /// or [`CoverageInput::Unavailable`] when it published no record. A gap is a file set the
    /// index never read, so an absence among those files is not evidence; see
    /// [`COVERAGE_GAP_MAJORITY_SHARE`] for the caps.
    pub coverage: CoverageInput,
    /// Language keys (`rust`, `python`) of the primary selections, sorted and deduplicated,
    /// compared with each majority coverage gap's language. Callers fill it only when a majority
    /// gap exists, the only case that reads it, so a pack without one looks nothing up.
    pub primary_language_keys: Vec<String>,
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

/// Directories inside a test path that hold what tests read rather than tests: Go's toolchain
/// skips `testdata/`, and fixture and snapshot directories hold inputs and expected outputs.
const DATA_ONLY_TEST_DIRS: [&str; 6] = [
    "testdata",
    "test_data",
    "test-data",
    "fixtures",
    "__fixtures__",
    "__snapshots__",
];

/// Whether a path is test code rather than test data: an [`is_test_path`] path that is not under
/// a data-only directory. Test-target extraction and validation availability judge files by it;
/// ranking keeps `is_test_path`, so a fixture still ranks as test material.
pub fn is_test_code_path(path: &str) -> bool {
    if !is_test_path(path) {
        return false;
    }
    let normalized = path.replace('\\', "/");
    let mut segments = normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    segments.pop();
    !segments.iter().any(|segment| {
        DATA_ONLY_TEST_DIRS
            .iter()
            .any(|dir| segment.eq_ignore_ascii_case(dir))
    })
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

/// `QuotaEnforcerTests`, `QuotaReloadedIT`, `internalClusterTest`: a test
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
    // Whole words and CamelCase parts: "Add BenchmarkRingBuffer" is about a benchmark, which in
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

/// Code identifiers a task names - a capital after the first character (`IssueToken`),
/// `snake_case`, `kebab-case`, or an upper-cased token with digits - as opposed to its
/// prose. A sentence-initial capital (`Reap the doctor's ...`) is not an identifier, and
/// ticket references (`ABC-123`) are not anchors. Shared by context and plan so both
/// surfaces agree on what the task named and therefore on what counts as missing.
///
/// An all-lowercase token whose only separator is `-` is spelled the same whether it is a
/// compound word (`re-index`, `best-effort`) or a lowercase kebab-case name (`get-or-load`,
/// `serde-json`), so its spelling alone cannot establish that the task named code. Such a
/// token is a weak anchor, returned by [`weak_named_anchors`] and not here, unless the task
/// marks it as code: quoted in backticks, written as a flag (`--allow-network`), or joined to
/// a path, module, scope, or assignment by `/`, `::`, `@`, `=`, or a `.` with a word on its
/// other side (`crates/open-kioku-cor/src/lib.rs`, `drive-by.rs`). A capital, a digit, or an
/// `_` in the token also keeps it named. A task with an odd number of backticks has no
/// reliable quoted spans, and none of its tokens is weak.
pub fn named_anchors(task: &str) -> Vec<String> {
    task_anchors(task)
        .into_iter()
        .filter(|(_, strength)| *strength == AnchorStrength::Named)
        .map(|(anchor, _)| anchor)
        .collect()
}

/// The weak anchors of `task`: all-lowercase tokens whose only separator is `-` and which the
/// task does not mark as code. See [`named_anchors`].
pub fn weak_named_anchors(task: &str) -> Vec<String> {
    task_anchors(task)
        .into_iter()
        .filter(|(_, strength)| *strength == AnchorStrength::Weak)
        .map(|(anchor, _)| anchor)
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnchorStrength {
    Named,
    Weak,
}

/// Every anchor of `task` once, in order of first appearance. A token quoted anywhere in the
/// task is named even where it also appears unquoted.
fn task_anchors(task: &str) -> Vec<(String, AnchorStrength)> {
    let mut anchors: Vec<(String, AnchorStrength)> = Vec::new();
    // A stray backtick shifts every later span between quoted and unquoted, which would demote
    // a quoted name to a weak anchor; with an odd count no token is demoted.
    let balanced_backticks = task.matches('`').count() % 2 == 0;
    for (index, span) in task.split('`').enumerate() {
        // Odd spans sit between a pair of backticks.
        let quoted = index % 2 == 1;
        for (start, raw) in identifier_runs(span) {
            // Classified on the raw run and its neighbours: trimming first would turn the flag
            // `--allow-network` and the path segment in `crates/open-kioku-cor/src` into the
            // same bare word as `re-index`.
            let marked_as_code = quoted
                || raw.starts_with("--")
                || touches_code_syntax(span, start, start + raw.len());
            let token = raw.trim_matches('-');
            if token.len() < 3 || is_ticket_anchor(token) {
                continue;
            }
            let has_lower = token.chars().any(|ch| ch.is_ascii_lowercase());
            let has_upper = token.chars().any(|ch| ch.is_ascii_uppercase());
            let has_digit = token.chars().any(|ch| ch.is_ascii_digit());
            let has_underscore = token.contains('_');
            let has_hyphen = token.contains('-');
            // A capital after the first character is what separates `IssueToken` from a
            // capitalized verb; `Reap` at the start of a sentence names nothing.
            let has_internal_upper = token.chars().skip(1).any(|ch| ch.is_ascii_uppercase());
            if !((has_lower && has_internal_upper)
                || has_underscore
                || has_hyphen
                || (has_digit && has_upper))
            {
                continue;
            }
            let strength = if balanced_backticks
                && !marked_as_code
                && has_hyphen
                && !has_underscore
                && !has_upper
                && !has_digit
            {
                AnchorStrength::Weak
            } else {
                AnchorStrength::Named
            };
            match anchors.iter_mut().find(|(existing, _)| existing == token) {
                Some(existing) if strength == AnchorStrength::Named => {
                    existing.1 = AnchorStrength::Named;
                }
                Some(_) => {}
                None => anchors.push((token.to_string(), strength)),
            }
        }
    }
    anchors
}

/// Maximal runs of identifier characters (ASCII alphanumerics, `_`, `-`) in `text`, each with
/// its byte offset.
fn identifier_runs(text: &str) -> Vec<(usize, &str)> {
    let mut runs = Vec::new();
    let mut start = None;
    for (offset, ch) in text.char_indices() {
        let part = ch.is_ascii_alphanumeric() || ch == '_' || ch == '-';
        match (part, start) {
            (true, None) => start = Some(offset),
            (false, Some(begin)) => {
                runs.push((begin, &text[begin..offset]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(begin) = start {
        runs.push((begin, &text[begin..]));
    }
    runs
}

/// Whether `text[start..end]` is joined to code syntax: a path or module separator (`/`,
/// `::`), a scope or assignment (`@`, `=`), or a `.` with a word on its other side
/// (`drive-by.rs`). A `.` followed by a space or the end of the task is sentence punctuation.
fn touches_code_syntax(text: &str, start: usize, end: usize) -> bool {
    let is_word = |ch: Option<char>| ch.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    let joins = |adjacent: Option<char>, beyond: Option<char>| match adjacent {
        Some('/' | '@' | '=') => true,
        Some(':') => beyond == Some(':'),
        Some('.') => is_word(beyond),
        _ => false,
    };
    let mut before = text[..start].chars().rev();
    let previous = before.next();
    let before_previous = before.next();
    let mut after = text[end..].chars();
    let next = after.next();
    let after_next = after.next();
    joins(previous, before_previous) || joins(next, after_next)
}

/// The [`named_anchors`] and [`weak_named_anchors`] of `task` that none of the top five
/// selected results spells, in its path, snippet, or symbol names, either verbatim or split
/// into words (`IssueTokenService` matches `issue token service`), in task order. Empty when
/// the task names nothing or nothing was selected: an empty selection is reported on its own.
pub fn unmatched_named_anchors(task: &str, selected: &[SearchResult]) -> Vec<String> {
    let anchors = task_anchors(task)
        .into_iter()
        .map(|(anchor, _)| anchor)
        .collect::<Vec<_>>();
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
        // A weak anchor may be a compound word, so it neither decides that every identifier
        // is unknown nor appears in a blocker asserting that it names nothing.
        let (weak_unmatched, named_unmatched): (Vec<&str>, Vec<&str>) = input
            .unmatched_anchors
            .iter()
            .map(String::as_str)
            .partition(|anchor| input.weak_anchors.iter().any(|weak| weak == anchor));
        let every_anchor_unmatched =
            input.named_anchor_count > 0 && named_unmatched.len() >= input.named_anchor_count;
        if every_anchor_unmatched {
            blockers.push(format!(
                "{} task identifier(s) name nothing in the selected context: {}",
                named_unmatched.len(),
                named_unmatched.join(", ")
            ));
        } else if !named_unmatched.is_empty() {
            caveats.push(format!(
                "{} of {} task identifier(s) name nothing in the selected context: {}",
                named_unmatched.len(),
                input.named_anchor_count,
                named_unmatched.join(", ")
            ));
        }
        if !weak_unmatched.is_empty() {
            caveats.push(format!(
                "{} hyphenated task word(s) appear in no selected context: {}",
                weak_unmatched.len(),
                weak_unmatched.join(", ")
            ));
        }
        // Files the index never read: an absence among them is not evidence, so every gap is
        // named. A gap alone lowers nothing: most repositories git-ignore a virtualenv or emitted
        // code, and an answer found in indexed code is still found. A majority gap lowers
        // confidence only beside a symptom it could explain.
        let gaps = input.coverage.gaps();
        // Majority by possibly-first-party source: a gap that is mostly installed dependencies
        // is reported below like any other, but those files hold no caller of the code under
        // edit and are never the file to change, so it caps nothing. Unclassified files count
        // as source.
        let majority_gaps = gaps
            .iter()
            .filter(|gap| gap.is_source_majority())
            .collect::<Vec<_>>();
        // The absence symptom: a named identifier the selected context does not spell, or no
        // primary context. A hyphenated word may be prose, so it is not one.
        let coverage_explains_a_miss = !majority_gaps.is_empty()
            && (!named_unmatched.is_empty() || input.primary_file_count == 0);
        // The selection is in a language the index mostly never read, so the right file may be
        // among the excluded ones. Whether the task happened to spell an identifier says
        // nothing about that: the label must follow what the index holds, not how the task was
        // phrased, so a matched identifier does not lift this cap.
        let gaps_in_selected_languages = if coverage_explains_a_miss {
            Vec::new()
        } else {
            majority_gaps
                .iter()
                .filter(|gap| input.primary_language_keys.contains(&gap.language))
                .map(|gap| gap.summary())
                .collect::<Vec<_>>()
        };
        // Reported, never capping on their own; merged into `caveats` after the 0.94 decision.
        let coverage_caveats = gaps.iter().map(CoverageGap::caveat).collect::<Vec<_>>();
        // An index that measured nothing is not an index that found nothing, and this caveat
        // caps like any other: the pack cannot claim completeness it never checked.
        if let Some(caveat) = input.coverage.caveat() {
            caveats.push(caveat.into());
        }
        if coverage_explains_a_miss {
            blockers.push(format!(
                "the task may name code in source the index excluded: {}",
                majority_gaps
                    .iter()
                    .map(|gap| gap.summary())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        } else if !gaps_in_selected_languages.is_empty() {
            blockers.push(format!(
                "the selected context is in a language the index mostly excluded: {}",
                gaps_in_selected_languages.join(", ")
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
                "selections backed by exact-authority retrieval, indexed symbol references, or SCIP evidence; a plan also counts proven cross-file dependents",
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
        // Zero weight: a gap is priced by the caps below, and a component that is always
        // present would move every breakdown's total. It exists to carry the signal name and
        // the evidence ids a reader traces the caps to.
        if !gaps.is_empty() {
            let indexed_share = gaps
                .iter()
                .map(|gap| 1.0 - gap.missing_share())
                .fold(1.0_f64, f64::min) as f32;
            components.push(ScoreComponent::new(
                "index_coverage",
                indexed_share,
                indexed_share,
                0.0,
                0.0,
                gaps.iter().map(CoverageGap::evidence_id).collect(),
                "lowest indexed share among languages with a coverage gap; priced by caps, not weight",
            ));
        }
        // Emitted only when the 0.74 language cap applies, so a reader - and `ok preflight` -
        // can tell "this index barely read the language you are editing" from a gap elsewhere.
        if !gaps_in_selected_languages.is_empty() {
            let matched = majority_gaps
                .iter()
                .filter(|gap| input.primary_language_keys.contains(&gap.language))
                .collect::<Vec<_>>();
            let indexed_share = matched
                .iter()
                .map(|gap| 1.0 - gap.source_missing_share())
                .fold(1.0_f64, f64::min) as f32;
            components.push(ScoreComponent::new(
                COVERAGE_SELECTED_LANGUAGE_SIGNAL,
                indexed_share,
                indexed_share,
                0.0,
                0.0,
                matched.iter().map(|gap| gap.evidence_id()).collect(),
                "a majority coverage gap in the selected context's language; the right file may be among those the index did not read",
            ));
        }
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
        // Beside an absence symptom a majority gap is no better than a task whose every
        // identifier is unknown; beside a selection in the excluded language, `High` would
        // claim knowledge of the files the index did not read.
        if coverage_explains_a_miss {
            overall_score = overall_score.min(0.50);
        } else if !gaps_in_selected_languages.is_empty() {
            overall_score = overall_score.min(0.74);
        }

        blockers.sort();
        blockers.dedup();
        // The 0.94 any-caveat cap is decided before the coverage caveats join the list, so a
        // gap reports without capping while every other caveat still caps. Deciding it by
        // construction rather than by matching caveat text means a future caveat cannot become
        // exempt by how it happens to be worded.
        let caveats_cap = !caveats.is_empty();
        caveats.extend(coverage_caveats);
        caveats.sort();
        caveats.dedup();
        if caveats_cap {
            overall_score = overall_score.min(0.94);
        }

        // `Exact` is a provenance claim, not a score band: it is reachable only when at
        // least one selection is backed by exact-authority evidence. The 0.74 cap above
        // already implies this; stating it keeps a future weight change from labelling
        // heuristic evidence `Exact` again.
        Self {
            overall_enum: Self::label_for(overall_score, input.exact_reference_count),
            overall_score,
            components,
            blockers,
            caveats,
        }
    }

    fn label_for(overall_score: f32, exact_reference_count: usize) -> Confidence {
        match Confidence::from_score(overall_score) {
            Confidence::Exact if exact_reference_count == 0 => Confidence::High,
            label => label,
        }
    }

    /// Attach caveats a caller learned after scoring - a plan's evidence-quality caveats - and
    /// re-derive the label through the `Exact` gate. Every caller passes caveats that cap, so
    /// adding one caps the score at 0.94; adding nothing leaves the score alone. Appending them
    /// without this left `Exact (1.00)` reachable above "index is stale".
    pub fn add_caveats(
        &mut self,
        caveats: impl IntoIterator<Item = String>,
        exact_reference_count: usize,
    ) {
        let mut added = false;
        for caveat in caveats {
            if !self.caveats.contains(&caveat) {
                self.caveats.push(caveat);
                added = true;
            }
        }
        if added {
            self.overall_score = self.overall_score.min(0.94);
        }
        self.overall_enum = Self::label_for(self.overall_score, exact_reference_count);
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

impl EvidenceSourceType {
    /// Whether a symbol occurrence from this source is an exact reference: SCIP and LSP index
    /// data and tree-sitter occurrences. Listed explicitly, so a variant added later is not
    /// exact until someone decides it is.
    pub fn is_exact_reference_source(&self) -> bool {
        matches!(self, Self::Scip | Self::TreeSitter | Self::Lsp)
    }
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
    CSharp,
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
                | Self::CSharp
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
            Self::CSharp => "c_sharp",
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
    /// Which rule flagged the file `is_generated`, so a demotion it causes can be traced to its
    /// evidence. `None` on a file that is not generated, and on one indexed before the rule was
    /// recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_by: Option<GeneratedBy>,
}

/// The evidence a file is generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedBy {
    /// A generation banner in its first lines (`// <auto-generated>`, `Code generated .. DO NOT
    /// EDIT`): the file says so itself. Recorded when the name rule matches too.
    Banner,
    /// A name a .NET build tool gives what it writes (`*.g.cs`, `*.Designer.cs`, an
    /// `obj/**/*.AssemblyInfo.cs`), with no banner the detector reads.
    BuildToolName,
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
    /// What the binding's initializer says of its type. A Rust initializer is recorded as the
    /// type of a struct literal (`Entry`), a call through a path or a name (`Entry::new()`,
    /// `Entry()`), or a path's value (`=Marker`); a call or a value proves a type only as
    /// resolution reads it.
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
    /// A Rust `pub use`, which makes what it imports nameable through the importing module from
    /// other crates. A restricted `pub(crate) use` is not one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reexported: bool,
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

/// A Rust `mod name;` or `mod name { ... }` declaration. A module path leads to a file only through
/// a declaration with no body, made outside any inline module, that has no `path` attribute or
/// sets its path only through `cfg_attr`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModuleDeclarationSite {
    pub file_id: FileId,
    /// Scope enclosing the declaration, which tells a declaration nested in an inline module apart.
    pub scope_id: Option<ScopeId>,
    pub name: String,
    pub has_body: bool,
    pub has_path_attribute: bool,
    /// The file paths the item's `path` attributes name, as written, including one set through
    /// `cfg_attr`. Empty when `has_path_attribute` is set but no string literal could be read.
    #[serde(default)]
    pub path_attributes: Vec<String>,
    /// Set when every `path` attribute of the item is inside a `cfg_attr`, so the module is
    /// compiled from its default location (`name.rs` or `name/mod.rs`) whenever no condition
    /// holds. Unset for an item with no `path` attribute, with one that always applies, or whose
    /// `cfg_attr` conditions hold on every build: `all()`, or a condition beside its own `not(..)`.
    #[serde(default)]
    pub path_is_conditional: bool,
    pub range: SourceRange,
}

/// A Go type alias, `type Entry = store.Entry`, declared by the symbol `symbol_id`. The alias is
/// the type it names, so a use of the alias is a use of that type. `target_name` is unset when the
/// alias names no declared type by name (`= []byte`, `= *Entry`, `= func()`); a generic
/// instantiation (`= Page[int]`) names its generic type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TypeAliasSite {
    pub symbol_id: SymbolId,
    /// The package qualifier the target is written with (`store` of `store.Entry`), as the
    /// alias's file imports it; unset for a type of the alias's own package or a predeclared one.
    pub target_package: Option<String>,
    pub target_name: Option<String>,
}

/// The repository type a Go type alias stands for, as the symbol registry placed it through the
/// alias file's imports (`TypeAliasSite`), following an alias of an alias to its end. Recorded on
/// the alias's `Symbol` only when the registry placed one type declaration; an alias of a
/// predeclared, composite or other module's type carries none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TypeAliasTarget {
    pub symbol_id: SymbolId,
    pub qualified_name: String,
}

/// The package a file declares: a Java `package org.example;` or a Go `package store` clause.
/// Neither need match the file's directory: a Java file may sit anywhere its build puts it, and a
/// Go `_test.go` file may declare the external test package `store_test` beside `store`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PackageDeclarationSite {
    pub file_id: FileId,
    /// The name as declared: `org.example` or `store_test`.
    pub name: String,
}

/// The variants a Rust `enum`, the symbol `enum_symbol_id`, declares by name: what a glob `use`
/// of the enum (`pub use Shape::*;`) brings in, and no item the index records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RustEnumVariants {
    pub enum_symbol_id: SymbolId,
    pub variants: Vec<String>,
}

/// A Rust `type` alias, the symbol `alias_symbol_id`, and the type it is written to stand for,
/// as written: `Disk` of `type Store = Disk;` and `Arc<Inner>` of `type Shared = Arc<Inner>;`
/// (#639). Not recorded when that type is a type parameter of the alias, directly or inside
/// `Box`, `Rc` or `Arc` (`type Own<T> = Box<T>;`), so the alias names no declared type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RustTypeAlias {
    pub alias_symbol_id: SymbolId,
    pub target: String,
}

/// A Rust `impl` block, by the scope of the block, which is the scope its methods are declared
/// in (#639).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RustImplBlock {
    pub scope_id: ScopeId,
    /// The trait written after `impl`, `None` for an inherent `impl`.
    pub trait_name: Option<String>,
    /// The block applies to every instantiation of its type: each generic argument the type is
    /// written with is a distinct type or const parameter of the `impl`, or a lifetime, and no
    /// type parameter carries a bound other than `?Sized` and no `where` clause is written.
    /// `impl Store`, `impl<'a> Store<'a>` and `impl<T: ?Sized> Store<T>` do; `impl Store<u8>`
    /// and `impl<T: Clone> Store<T>` do not.
    pub covers_every_instantiation: bool,
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
    #[serde(default)]
    pub module_declarations: Vec<ModuleDeclarationSite>,
    #[serde(default)]
    pub type_aliases: Vec<TypeAliasSite>,
    #[serde(default)]
    pub package_declaration: Option<PackageDeclarationSite>,
    /// The file parsed with syntax errors and its facts are what error recovery kept: set only
    /// for C#, the one language read through its errors rather than set aside whole.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub syntax_errors: bool,
    /// A Rust file whose top level invokes a macro (`cfg_if! { .. }`, `make_items!();`), which
    /// may expand to items and `use` declarations no other fact here records. Macros that
    /// declare no name (`compile_error!`, `assert!`, `include_str!`) do not count, and
    /// `thread_local!` counts only through the names written in it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invokes_item_macro: bool,
    /// The names a Rust file's top-level `thread_local!` declares, which it does as items of
    /// the file's module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_macro_names: Vec<String>,
    /// The Rust type items that are not also values: a braced `struct`, an `enum`, a `union`
    /// and a `type` alias live in the type namespace alone, while a tuple or unit struct is also
    /// the value its constructor is. A call names a value and never one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rust_type_only_items: Vec<SymbolId>,
    /// The variants of each Rust `enum` the file declares.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rust_enum_variants: Vec<RustEnumVariants>,
    /// The Rust unit structs the file declares (`struct Marker;`): the value of the name is an
    /// instance of the struct, where a tuple struct's is its constructor function (#654).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rust_unit_structs: Vec<SymbolId>,
    /// The Rust `type` aliases the file declares, with the type each stands for (#639).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rust_type_aliases: Vec<RustTypeAlias>,
    /// The Rust `impl` blocks the file writes (#639).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rust_impl_blocks: Vec<RustImplBlock>,
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
    /// Set on a Go type alias whose target the index placed: the symbol is that type under
    /// another name, and its `kind` is the target's. Search ranks it just below the target
    /// (`type_alias_below_target`), and symbol listings put it just after the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias_of: Option<TypeAliasTarget>,
}

/// A symbol together with as much of its definition as the index can actually
/// prove, plus a plain statement of whatever it could not.
///
/// Every text field here is recovered from indexed chunk text. Nothing is read
/// back from the working tree and nothing is inferred, so an empty field means
/// the evidence is missing — which `caveats` says out loud rather than letting
/// the caller read absence as a short definition.
/// The definition a name lookup picked, with the others it passed over.
///
/// A name can have several definitions: overloads, the parts of a C# `partial` type, `Entry`
/// and `Entry<T>`, or a namespace and a type of one qualified name. The lookup answers with the
/// first by rank; `other_definitions` counts the rest and `caveats` says so, so one record is
/// never read as the only one. Serialized as the symbol's own fields, with the two added only
/// when another definition exists.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SymbolDefinition {
    #[serde(flatten)]
    pub symbol: Symbol,
    /// Indexed definitions other than `symbol` that the same query matched.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub other_definitions: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caveats: Vec<String>,
}

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
    /// The indexed symbol `target` names, when the fact resolved to one. The graph draws the
    /// fact's edge to that symbol's node rather than to a node made from the label, which no
    /// symbol-keyed read (impact, callers) reaches. Naming the symbol says where the edge ends,
    /// not how sure the fact is: authority still comes from the edge's proofs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_symbol_id: Option<SymbolId>,
    pub edge_type: GraphEdgeType,
    pub range: Option<LineRange>,
    pub confidence: Confidence,
    pub source: SharedStr,
    pub source_type: EvidenceSourceType,
    pub message: SharedStr,
    /// Why the fact may name the wrong target, when the pass that wrote it knows: a symbol-registry
    /// match made by name alone, which another item of that name (one outside the repository
    /// included) could equally answer. The graph copies it onto the fact's edge, so every reader
    /// that asks whether an edge is ambiguous gets the answer the pass recorded rather than one
    /// parsed from `message`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambiguity: Vec<String>,
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
    /// Where the target came from. Stored targets written before this field read as
    /// [`TestTargetOrigin::Symbol`].
    #[serde(default)]
    pub origin: TestTargetOrigin,
}

/// Where a [`TestTarget`] came from. Provenance is carried on the target because the surfaces
/// that filter validation candidates cannot recover it from a name: a registered test is named
/// by a sentence, and a disabled one reads like any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum TestTargetOrigin {
    /// A declared symbol outside a test file, matched by a test annotation or naming convention.
    #[default]
    Symbol,
    /// A declared symbol in a file the shared test-path rule recognises that the language's
    /// runner discovers as a test: a `#[test]` or `@Test` callable, a `test`-prefixed Python
    /// function in a test module, a Go `TestX` in a `_test.go` file. The runner decides, not the
    /// name, so a JUnit `shouldRoundHalfUp` is one.
    TestFileSymbol,
    /// A declared symbol in a test-path file that matches no default runner discovery rule:
    /// usually a helper, fixture, builder or lifecycle hook (`setUp`, `TestMain`, `makeClient`),
    /// or any callable in a support module such as `conftest.py` or a `testutil/` package. It is
    /// still test code, so it is kept, but it is not counted as validation. Runner configuration
    /// is not read (beyond pytest's discovery options), so a test a configured runner collects
    /// can carry this origin; surfaces report such targets as withheld, never as absent.
    TestFileHelper,
    /// A JavaScript or TypeScript runner call such as `test("name", fn)`.
    RegistrationCall,
    /// A registration call the runner will not execute: `test.skip`, `it.todo`, `test.failing`.
    DisabledRegistrationCall,
}

/// Why an indexed [`TestTarget`] cannot stand as validation evidence. Surfaces that withhold
/// such targets count them by this reason, so an empty recommendation list says whether the
/// change has no tests or has tests none of which run.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TestExclusionReason {
    /// A registration the runner will not execute: `test.skip`, `it.todo`, `test.failing`.
    Disabled,
    /// A test-file callable that matches no default runner discovery rule: usually a helper,
    /// fixture or lifecycle hook. Runner configuration is not read, so a test a configured or
    /// custom runner collects can land here too; surfaces say so rather than call it absent.
    Helper,
}

impl TestExclusionReason {
    /// The phrase every surface uses for a target excluded for this reason, so `ok status`,
    /// `ok tests`, `find_tests_for_change` and the context pack describe one index one way.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Disabled => DISABLED_TEST_TARGET,
            Self::Helper => HELPER_TEST_TARGET,
        }
    }

    /// What would make targets excluded for this reason count again, as one sentence.
    pub fn remedy(self) -> &'static str {
        match self {
            Self::Disabled => {
                "Enable the skipped tests (`test.skip`, `it.todo`, `test.failing`) before relying on validation recommendations."
            }
            Self::Helper => {
                "If your test runner is configured to collect these callables, run them yourself: runner configuration is not read. Otherwise add tests that match a default discovery rule (`#[test]`, `@Test`, `def test_*`, `func TestX`, `test(..)`/`it(..)`)."
            }
        }
    }

    /// Why a target excluded for this reason stays optional however strongly it overlaps a
    /// change.
    pub fn tier_justification(self) -> &'static str {
        match self {
            Self::Disabled => {
                "the runner skips this test (`skip`, `todo`, `failing`), so running it validates nothing"
            }
            Self::Helper => {
                "this test-file callable matches no default runner discovery rule (runner configuration is not read), so it is not counted as validation"
            }
        }
    }
}

/// The disclosure a plan carries when it planned nothing and found no withheld callable near the
/// change, but the index withheld some elsewhere: the selector may simply not link their files
/// to the change. `None` when there were none.
pub fn withheld_test_file_callables_in_index(count: usize) -> Option<String> {
    (count > 0).then(|| {
        format!(
            "{count} indexed test-file callable(s) matched no default runner discovery rule (runner configuration is not read), none of them linked to this change"
        )
    })
}

/// The disclosure plans and context packs carry when test-file callables near a change were
/// withheld as matching no discovery rule. A misclassified test must read as withheld, never as
/// absent. `None` when there were none.
pub fn withheld_test_file_callables(count: usize) -> Option<String> {
    (count > 0).then(|| {
        format!(
            "{count} test-file callable(s) near this change matched no default runner discovery rule (runner configuration is not read)"
        )
    })
}

/// How a disabled registration is described wherever it is withheld from validation.
pub const DISABLED_TEST_TARGET: &str = "disabled test the runner skips";

/// How a test-file callable that matches no runner discovery rule is described wherever it is
/// withheld from validation. It says what the index checked, not what a runner will do.
pub const HELPER_TEST_TARGET: &str =
    "test-file callable matching no default runner discovery rule (runner configuration is not read)";

/// The sentence every surface uses when an index holds test targets and every one is excluded,
/// so `ok status` and the context pack's validation caveat word one index one way. `None` when
/// nothing is excluded.
pub fn every_test_target_excluded(
    excluded: &BTreeMap<TestExclusionReason, usize>,
) -> Option<String> {
    match excluded.keys().collect::<Vec<_>>().as_slice() {
        [] => None,
        [reason] => Some(format!(
            "every indexed test target is a {}",
            reason.describe()
        )),
        _ => Some(format!(
            "every indexed test target is excluded ({})",
            excluded
                .iter()
                .map(|(reason, count)| format!("{count} {}", reason.describe()))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
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
    /// Whether this target can stand as validation evidence. A disabled registration is a test
    /// the runner skips and a test-file helper is one it never runs, so counting either would
    /// let a file of `test.skip` calls or `withTempRepo` helpers satisfy a task family that
    /// requires validation evidence.
    pub fn counts_as_validation_evidence(&self) -> bool {
        self.validation_exclusion().is_none()
    }

    /// Why this target cannot stand as validation evidence, or `None` when it can.
    pub fn validation_exclusion(&self) -> Option<TestExclusionReason> {
        match self.origin {
            TestTargetOrigin::DisabledRegistrationCall => Some(TestExclusionReason::Disabled),
            TestTargetOrigin::TestFileHelper => Some(TestExclusionReason::Helper),
            TestTargetOrigin::Symbol
            | TestTargetOrigin::TestFileSymbol
            | TestTargetOrigin::RegistrationCall => None,
        }
    }

    /// Whether provenance alone settles what this target is: the index extracted it from a test
    /// file and judged it by the runner's discovery rule, or a runner call registered it. Only
    /// targets matched outside a test file, by annotation or naming convention, need a name
    /// heuristic to judge them.
    pub fn has_test_provenance(&self) -> bool {
        matches!(
            self.origin,
            TestTargetOrigin::TestFileSymbol
                | TestTargetOrigin::TestFileHelper
                | TestTargetOrigin::RegistrationCall
                | TestTargetOrigin::DisabledRegistrationCall
        )
    }

    /// Whether a runner call registered this target rather than a declared symbol.
    pub fn is_registration_call(&self) -> bool {
        matches!(
            self.origin,
            TestTargetOrigin::RegistrationCall | TestTargetOrigin::DisabledRegistrationCall
        )
    }

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
    /// Present only on an index published by `ok snapshot import`: which revision the
    /// imported rows describe relative to this checkout, and what the local policy removed.
    /// `ok index` publishes a manifest without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotProvenance>,
}

/// Where an imported index came from. Recorded when the snapshot is imported; the counts
/// describe the checkout at that moment, not a later one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SnapshotProvenance {
    /// The commit the exporting index was built from, as the artifact records it.
    pub imported_from_commit: String,
    /// The local `HEAD` at import time; null outside a Git work tree.
    pub local_commit: Option<String>,
    pub relation: SnapshotRevisionRelation,
    /// Commits the local `HEAD` has that the artifact's commit does not: null when the two
    /// share no verifiable history.
    pub commits_behind: Option<usize>,
    /// Commits the artifact's commit has that the local `HEAD` does not.
    pub commits_ahead: Option<usize>,
    /// Files whose working-tree content differs from the artifact's commit: tracked files
    /// changed since it, committed or not, and untracked files Git does not ignore; null when
    /// that could not be determined.
    pub changed_files: Option<usize>,
    /// Paths removed from the imported index because the local index policy excludes them:
    /// indexed files and documents excluded by any rule (secret-like and denied paths, hidden
    /// files, `[index] exclude`, `.gitignore`, `.okignore`), and paths named only in Git
    /// history or by a graph node no file owns that are secret-like or denied.
    pub policy_filtered: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotRevisionRelation {
    /// The artifact was built from the local `HEAD`.
    SameCommit,
    /// The artifact's commit shares history with the local `HEAD` but is not it.
    Related,
    /// Imported with `--allow-foreign`: the artifact's commit is absent from this repository,
    /// shares no history with `HEAD`, was not recorded, or there is no local `HEAD`.
    Foreign,
}

impl SnapshotProvenance {
    /// The caveat every answer from this index carries, or `None` when the artifact was built
    /// from the checked-out commit and no tracked or untracked, non-ignored file differs from
    /// it. The exporter's own uncommitted changes are not recorded, so `None` rests on the
    /// artifact having been exported from its commit's content.
    pub fn caveat(&self) -> Option<String> {
        let short = |commit: &str| commit.chars().take(12).collect::<String>();
        let from = short(&self.imported_from_commit);
        let changed = match self.changed_files {
            Some(count) => format!("{count} file(s) in the working tree differ from it"),
            None => "the files that differ from it are unknown".into(),
        };
        let revision = match self.relation {
            SnapshotRevisionRelation::SameCommit if self.changed_files == Some(0) => return None,
            SnapshotRevisionRelation::SameCommit => {
                format!("the index was imported from a snapshot of the checked-out commit {from}, but {changed}")
            }
            SnapshotRevisionRelation::Related => format!(
                "the index was imported from a snapshot of commit {from}, {} commit(s) behind and {} ahead of the local HEAD; {changed}",
                self.commits_behind.map_or_else(|| "an unknown number of".into(), |n| n.to_string()),
                self.commits_ahead.map_or_else(|| "an unknown number".into(), |n| n.to_string()),
            ),
            SnapshotRevisionRelation::Foreign => format!(
                "the index was imported with --allow-foreign from a snapshot of commit {from}, whose relation to this checkout could not be verified"
            ),
        };
        Some(format!(
            "{revision}; results may describe code that is not in this checkout until `ok index` rebuilds it"
        ))
    }
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
    /// A directory discovery cut from the walk as build output or installed dependencies
    /// (`IndexCoverage::pruned`), or, in coverage, a git-tracked source file under a
    /// directory pruned as undeclared build output (`PruneReason::UndeclaredBuildDir`), which the
    /// index therefore does not hold.
    Pruned,
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

/// The names `ssh-keygen` gives a key pair, one per key type. A path component starting with
/// one is an SSH key unless it ends in a [`KEY_STEM_SOURCE_EXTENSIONS`] extension
/// ([`is_secret_like_path`]).
pub const SSH_KEY_STEMS: &[&str] = &["id_dsa", "id_ecdsa", "id_ed25519", "id_rsa", "id_xmss"];

/// Programming-language source extensions the indexer reads. A name that starts with an
/// [`SSH_KEY_STEMS`] stem and ends in one of these (`id_rsa_loader.py`) is read as source,
/// while every other suffix (`.pub`, `.bak`, `.old`, `.txt`, `-cert.pub`, `_deploy`, `4096`) is how
/// a key is copied or renamed. A name alone cannot prove a file holds no key, so source is not
/// trusted on its name: `open-kioku-ingest::redaction::redact_private_keys` replaces any
/// private-key PEM body in programming-language source before it is indexed. Every entry must
/// be an extension `open-kioku-languages::detect_language` reads as a programming language, or
/// one it does not read at all, which discovery never indexes (`cs` until C# is supported); a
/// test there holds the two together, so no entry can name data, config or prose.
pub const KEY_STEM_SOURCE_EXTENSIONS: &[&str] = &[
    "cjs", "cs", "go", "java", "js", "jsx", "mjs", "py", "rs", "ts", "tsx",
];

/// Extensions of key and certificate material, a .NET strong-name key pair (`snk`) included.
/// A name carrying one as any extension, the last or an earlier one, matches: `server.key`,
/// but also `id_rsa.pem.ts`, `x.key.js` and `Ledger.snk.md`, a key renamed with another
/// extension appended.
pub const KEY_MATERIAL_EXTENSIONS: &[&str] =
    &["jks", "key", "keystore", "p12", "pem", "pfx", "snk"];

/// Paths that match a secret-path pattern, which are never read, whatever the file's language.
/// A path matches when any component is one of:
///
/// - an environment file, `.env` or `.env.*` (`.env.production`, `.env.example`);
/// - the `.aws` or `.ssh` directory;
/// - an SSH key: a name starting with an [`SSH_KEY_STEMS`] stem (`id_rsa`, `id_rsa.pub`,
///   `id_ed25519_deploy`, `id_rsa4096`), unless it ends in a [`KEY_STEM_SOURCE_EXTENSIONS`]
///   extension (`id_rsa_loader.py` is code; #676);
/// - key or certificate material by extension ([`KEY_MATERIAL_EXTENSIONS`]): `*.pem`, `*.key`,
///   `*.p12`, `*.pfx`, `*.jks`, `*.keystore`, `*.snk`, as the last extension or an earlier one
///   (`id_rsa.pem.ts`, `x.key.js`).
///
/// A .NET user-secrets store (`UserSecrets/<id>/secrets.json`) matches as well (#684).
///
/// Discovery skips them as `secret_policy` and the semantic corpus excludes them: one rule for
/// both. The rule judges a path by its name alone and cannot know what a file holds, so a
/// match is reported as a pattern match, never as a description of the content. A file merely
/// named for a secret is not matched. A class named `CredentialsProviderTest` or a module
/// named `secrets.go` is code (a name rule silently dropped 25 Java files from one
/// repository), and a `secrets.yaml`, `credentials.json`, or `SECRETS.md` is indexed with its
/// secret-like values replaced before anything is derived from its text
/// (`open-kioku-ingest::redaction`, #379).
pub fn is_secret_like_path(path: &Path) -> bool {
    path.components().any(|component| {
        let value = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        value == ".env"
            || value.starts_with(".env.")
            || matches!(value.as_str(), ".aws" | ".ssh")
            || names_ssh_key(&value)
            || has_key_material_extension(&value)
    }) || is_user_secrets_store(path)
}

/// `value` (lower-case) has a [`KEY_MATERIAL_EXTENSIONS`] entry as any of its extensions: a
/// segment after a `.` that is not the name's first.
fn has_key_material_extension(value: &str) -> bool {
    value
        .split('.')
        .skip(1)
        .any(|extension| KEY_MATERIAL_EXTENSIONS.contains(&extension))
}

/// `value` (lower-case) starts with an SSH key stem and does not end in a source extension.
/// The stem is matched as a prefix, not a whole name, because keys are routinely renamed
/// (`id_rsa_github`, `id_rsa.bak`, `id_rsa4096`); only the source-extension exemption narrows
/// it.
fn names_ssh_key(value: &str) -> bool {
    SSH_KEY_STEMS.iter().any(|stem| {
        value.strip_prefix(stem).is_some_and(|rest| {
            !rest
                .rsplit_once('.')
                .is_some_and(|(_, extension)| KEY_STEM_SOURCE_EXTENSIONS.contains(&extension))
        })
    })
}

/// The caveat every search surface attaches when the index holds redacted values. A query for
/// a value that was replaced returns nothing, and without this an empty answer reads as "absent
/// from the repository" rather than "absent from what the index stores". `ok search`,
/// `ok search --regex`, MCP `search_code` and `regex_search` all render this one text, so they
/// cannot disagree about the same index (#379).
pub fn redaction_search_caveat(redacted_files: Option<usize>) -> Option<String> {
    let redacted = redacted_files?;
    (redacted > 0).then(|| {
        format!(
            "{redacted} file(s) are indexed with secret-like values replaced by `[REDACTED]`, so a redacted value cannot be found by searching for it"
        )
    })
}

/// A path whose name says it holds credentials: a component containing `secret`, `credential`
/// or `password`, or one ending in `_key` or `-key`. This no longer decides whether a file is
/// indexed ([`is_secret_like_path`] does) — it decides how the file's content is read. A file
/// named for secrets is where a bare token is pasted, so `docs/SECRETS.md` and `secret_key.txt`
/// are redacted under the config rules rather than the prose ones (#379).
pub fn is_secret_named_path(path: &Path) -> bool {
    path.components().any(|component| {
        let value = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        value.contains("secret")
            || value.contains("credential")
            || value.contains("password")
            || value.ends_with("_key")
            || value.ends_with("-key")
    })
}

/// A .NET user-secrets store, which [`is_secret_like_path`] blocks like key material:
/// `UserSecrets/<id>/secrets.json` as `dotnet user-secrets` writes it, copied into a
/// repository, or a `secrets.json` anywhere below a `UserSecrets` directory. A `secrets.json`
/// elsewhere is a config file named for secrets, indexed with its values redacted like
/// `credentials.json` (#684).
fn is_user_secrets_store(path: &Path) -> bool {
    let is_store = path
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("secrets.json"));
    is_store
        && path.parent().is_some_and(|parent| {
            parent
                .components()
                .any(|part| part.as_os_str().eq_ignore_ascii_case("usersecrets"))
        })
}

#[cfg(test)]
mod secret_path_tests {
    use super::{is_secret_like_path, is_secret_named_path};
    use std::path::Path;

    /// .NET key material and user-secrets stores are blocked; files that only share a name with
    /// them are not (#684).
    #[test]
    fn dotnet_key_pairs_and_user_secrets_stores_are_secret_like() {
        for blocked in [
            "src/Ledger/Ledger.snk",
            "keys/Signing.SNK",
            "certs/ledger.pfx",
            "UserSecrets/7f3c2a1e-0b4d-4e8a-9c61-5d2f8e0a7b13/secrets.json",
            "ops/Microsoft/UserSecrets/ledger-dev/secrets.json",
            "ops/usersecrets/secrets.JSON",
            // A key extension before the last is still one (#676), as for `id_rsa.pem.ts`.
            "src/Ledger/Ledger.snk.md",
        ] {
            assert!(is_secret_like_path(Path::new(blocked)), "{blocked}");
        }
        for indexed in [
            "config/secrets.json",
            "secrets.json",
            "UserSecrets/readme.md",
            "UserSecrets/ledger-dev/appsettings.json",
            "src/UserSecretsLoader/secrets.json",
            "src/Ledger/UserSecrets.cs",
            "UserSecrets",
            "docs/snk/notes.md",
            "src/Ledger/snk.cs",
        ] {
            assert!(!is_secret_like_path(Path::new(indexed)), "{indexed}");
        }
    }

    #[test]
    fn secret_path_rule_blocks_key_material_and_environment_entries_only() {
        // Key material and environment entries, whatever the file's language.
        for blocked in [
            ".env",
            ".env.local",
            ".aws/credentials.json",
            ".ssh/id_rsa.pub",
            "deploy/id_ed25519",
            "config/server.key",
            "certs/tls.PEM",
            "certs/client.p12",
            "certs/client.pfx",
            "android/release.jks",
            "android/release.keystore",
        ] {
            assert!(is_secret_like_path(Path::new(blocked)), "{blocked}");
        }
        // Named for a secret but not key material: indexed, source as written and data,
        // config, and prose with secret-like values redacted.
        // The looser name rule no longer decides indexing; it decides how content is read.
        for named in [
            "docs/SECRETS.md",
            "secret_key.txt",
            "notes/credentials.md",
            "config/passwords.yaml",
        ] {
            assert!(is_secret_named_path(Path::new(named)), "{named}");
            assert!(!is_secret_like_path(Path::new(named)), "{named}");
        }
        for ordinary in ["docs/architecture.md", "src/lib.rs", "config/app.yaml"] {
            assert!(!is_secret_named_path(Path::new(ordinary)), "{ordinary}");
        }

        for indexed in [
            "src/CredentialsProvider.java",
            "internal/secrets.go",
            "config/credentials.yaml",
            "credentials.json",
            "secret_key.txt",
            "docs/SECRETS.md",
            "config/server.yaml",
            "config/.environment.yaml",
        ] {
            assert!(!is_secret_like_path(Path::new(indexed)), "{indexed}");
        }
    }

    /// Every row of the secret-path table in `docs/security-model.md` (#676). A key-file stem
    /// is a prefix, so a renamed or copied key stays blocked; only a programming-source
    /// extension takes a name that starts with one out of the rule.
    #[test]
    fn secret_path_table_blocks_keys_and_env_files_and_passes_source_named_after_them() {
        let must_stay_blocked = [
            // Environment files, a template included: one may hold real values.
            ".env",
            ".env.local",
            ".env.production",
            "config/.env.production",
            ".env.example",
            ".ENV.Production",
            // Credential directories, whatever is under them.
            ".aws/credentials",
            ".aws/credentials.json",
            ".ssh/config",
            ".ssh/known_hosts",
            ".ssh/build/keys.py",
            // SSH keys under every `ssh-keygen` default name, public halves included.
            "id_rsa",
            "id_rsa.pub",
            "id_dsa",
            "id_ecdsa",
            "id_ecdsa_sk",
            "id_ed25519",
            "id_ed25519.pub",
            "id_ed25519_sk.pub",
            "id_xmss",
            "deploy/id_rsa",
            "deploy/ID_RSA",
            // A key renamed, copied, certified, or backed up keeps its stem.
            "keys/id_rsa_github",
            "keys/id_ed25519-work",
            "keys/id_rsa4096",
            "keys/id_rsa-cert.pub",
            "keys/id_rsa.bak",
            "keys/id_rsa.old",
            "keys/id_rsa.orig",
            "keys/id_rsa.txt",
            "keys/id_rsa.md",
            "keys/id_rsa.json",
            "keys/id_rsa_loader.yaml",
            // A directory named for a key blocks everything under it, source included.
            "id_rsa/loader.py",
            "keys/id_ed25519_deploy/main.go",
            // A source extension does not rescue a non-stem pattern.
            "src/.env.rs",
            // Key and certificate material by extension.
            "config/server.key",
            "certs/tls.PEM",
            "certs/ca.pem",
            "certs/client.p12",
            "certs/client.pfx",
            "android/release.jks",
            "android/release.keystore",
            // A key extension before the last one: a key renamed with a source extension.
            "keys/id_rsa.pem.ts",
            "keys/x.key.js",
            "certs/client.p12.py",
            "android/release.jks.java",
            "keys/id_rsa.key.cs",
            "keys/ID_RSA.PEM.TS",
            "certs/.pem",
        ];
        for blocked in must_stay_blocked {
            assert!(
                is_secret_like_path(Path::new(blocked)),
                "{blocked} must stay blocked"
            );
        }

        let now_pass = [
            // Source named after a key type: blocked before #676.
            "loaders/id_rsa_loader.py",
            "src/id_rsa.rs",
            "src/id_rsa_tool.rs",
            "pkg/id_ed25519_signer.go",
            "web/id_rsa_parser.ts",
            "web/id_rsa_form.tsx",
            "web/id_rsa.d.ts",
            "lib/id_dsa.js",
            "lib/id_ecdsa_util.mjs",
            "lib/id_ecdsa_util.cjs",
            "ui/id_rsa_view.jsx",
            "codec/id_rsa_codec.java",
            "src/IdRsa/id_rsa_loader.cs",
            "loaders/ID_RSA_LOADER.PY",
        ];
        for source in now_pass {
            assert!(
                !is_secret_like_path(Path::new(source)),
                "{source} must pass"
            );
        }

        let still_pass = [
            // Named for a secret, indexed with values redacted (#379); never path-blocked.
            "secrets.yaml",
            "config/credentials.yaml",
            "credentials.json",
            "docs/SECRETS.md",
            "secret_key.txt",
            "src/CredentialsProvider.java",
            "internal/secrets.go",
            // Near misses of an environment file or a key extension.
            ".envrc",
            "config/.environment.yaml",
            "src/env.rs",
            "src/keyboard.rs",
            "src/monkey.rs",
            "docs/key.md",
            "src/pem_reader.rs",
            "src/identity.rs",
            "src/key.rs",
            "src/pem.ts",
            "src/keys.pemfile.rs",
        ];
        for ordinary in still_pass {
            assert!(
                !is_secret_like_path(Path::new(ordinary)),
                "{ordinary} must pass"
            );
        }
    }
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
            Self::Pruned => "pruned",
        }
    }

    /// A skip a policy chose — ignore rules, the hidden-file rule, security and
    /// vendor rules, the index mode — as opposed to one the index did not intend
    /// (`too-large`, `binary`, `error`, `unsupported-language`, `pruned`). Policy exclusions
    /// are reported beside the coverage ratio, never inside its denominator: on a
    /// repository whose git-ignored agent worktrees live under a hidden directory,
    /// 1,485 hidden `.rs` files read as 24.9% coverage of a fully indexed tree.
    ///
    /// `pruned` is judged: in coverage it counts only git-tracked source under a directory
    /// pruned as undeclared build output, which someone committed and the index still does not hold.
    pub fn is_policy(self) -> bool {
        matches!(
            self,
            Self::Ignored
                | Self::Denied
                | Self::Hidden
                | Self::Generated
                | Self::Vendor
                | Self::FastMode
                | Self::SecretPolicy
                | Self::SymlinkPolicy
        )
    }
}

impl SkipSource {
    /// The `ok.toml` key, ignore file, or flag that governs skips from this source, when
    /// one exists; advice that names `[index] exclude` for a `hidden` skip sends the
    /// reader to the wrong key.
    pub fn governing_setting(self) -> Option<&'static str> {
        match self {
            Self::HiddenPolicy => Some("`[security] allow_hidden_files`"),
            Self::ConfigExclude => Some("`[index] exclude`"),
            Self::GitIgnore => Some("`.gitignore`"),
            Self::OkIgnore => Some("`.okignore`"),
            Self::SecurityPolicy => Some("`[paths] deny` or the built-in secret-path rule"),
            Self::SizeLimit => Some("`[index] max_file_size`"),
            Self::FastMode => Some("`ok index --mode full`"),
            Self::Detector
            | Self::SymlinkPolicy
            | Self::LanguageSupport
            | Self::Filesystem
            | Self::Parser => None,
        }
    }

    /// Label for summaries (`hidden-policy`, `git-ignore`).
    pub fn label(self) -> &'static str {
        match self {
            Self::SecurityPolicy => "security-policy",
            Self::HiddenPolicy => "hidden-policy",
            Self::ConfigExclude => "config-exclude",
            Self::GitIgnore => "git-ignore",
            Self::OkIgnore => "ok-ignore",
            Self::Detector => "detector",
            Self::FastMode => "fast-mode",
            Self::SizeLimit => "size-limit",
            Self::SymlinkPolicy => "symlink-policy",
            Self::LanguageSupport => "language-support",
            Self::Filesystem => "filesystem",
            Self::Parser => "parser",
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
    /// Indexed files that parsed with syntax errors, whose symbols are only what error recovery
    /// kept: a file counts as indexed whether recovery kept every declaration or none.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub parsed_with_errors: usize,
    /// Of `parsed_with_errors`, files whose types were named by line patterns because recovery
    /// kept none of them.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub pattern_fallback: usize,
}

impl LanguageCoverage {
    /// Discovered files a policy excluded (`SkipReason::is_policy`), derived from
    /// `skipped` so a manifest written before the distinction existed reads the same way.
    pub fn excluded_by_policy(&self) -> usize {
        self.skipped
            .iter()
            .filter(|(reason, _)| reason.is_policy())
            .map(|(_, count)| *count)
            .sum()
    }

    /// Files the index would consider under the current policy: the coverage
    /// denominator.
    pub fn considered(&self) -> usize {
        self.discovered.saturating_sub(self.excluded_by_policy())
    }

    /// `indexed` over `considered`; `None` when policy left nothing to consider.
    pub fn percent(&self) -> Option<f64> {
        let considered = self.considered();
        (considered > 0).then(|| self.indexed as f64 * 100.0 / considered as f64)
    }
}

impl LanguageCoverage {
    /// The skip reason behind the most files among policy (`policy`) or non-policy reasons,
    /// ties broken by reason order.
    fn dominant_skip_reason(&self, policy: bool) -> Option<SkipReason> {
        self.skipped
            .iter()
            .filter(|(reason, count)| reason.is_policy() == policy && **count > 0)
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(reason, _)| *reason)
    }
}

/// A coverage gap whose missing files are at least this share of its language's judged files
/// is a majority gap: most of that language's source is absent from the index. A gap whose
/// possibly-first-party files are a majority by the same share ([`CoverageGap::is_source_majority`])
/// lowers a pack or plan only beside a symptom it could explain: 0.50 with a named task
/// identifier the selected context does not spell or no primary context, 0.74 when the
/// selected context is in the gap's language. Any other gap, one made of installed
/// dependencies included, is reported and changes no score: most repositories git-ignore a
/// virtualenv or emitted code, and an answer found in indexed code is still found.
pub const COVERAGE_GAP_MAJORITY_SHARE: f64 = 0.5;

/// Every rule file git reads; ingest attributes all of them to [`SkipSource::GitIgnore`] in a
/// git work tree, so advice cannot name `.gitignore` alone.
const GIT_IGNORE_RULES: &str =
    "git ignore rules (`.gitignore`, `.git/info/exclude`, or `core.excludesFile`)";

/// Why a programming language's source is missing from the index.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CoverageGapCause {
    /// Git ignore rules set aside most of the language; they are written for git, not for
    /// this index.
    GitIgnore,
    /// Policy left no programming-language source to consider at all.
    ExcludedByPolicy,
    /// Files the index did not intend to drop (`too-large`, `binary`, unreadable), under the
    /// doctor's per-language threshold.
    Omitted,
}

impl CoverageGapCause {
    /// The serialized value, used in evidence ids (`git_ignore`).
    pub fn key(self) -> &'static str {
        match self {
            Self::GitIgnore => "git_ignore",
            Self::ExcludedByPolicy => "excluded_by_policy",
            Self::Omitted => "omitted",
        }
    }
}

/// One programming language whose missing source changes what an absence means, from
/// [`IndexCoverage::gaps`]. Context packs and plans price it through the `index_coverage`
/// confidence signal and report it as `coverage` negative evidence; `repo_status` and
/// `ok --json status` list it under `coverage_gaps`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageGap {
    /// Language key, as in `IndexCoverage::by_language`.
    pub language: String,
    pub cause: CoverageGapCause,
    /// Files of the language absent from the index for this cause.
    pub missing_files: usize,
    /// Files the cause is judged against: git-ignored plus considered files for `git_ignore`,
    /// discovered files for `excluded_by_policy`, considered files for `omitted`.
    pub language_files: usize,
    /// Label of the skip source or reason behind most of the missing files (`git-ignore`,
    /// `hidden-policy`, `too-large`): the reason category a caveat names.
    pub reason: String,
    /// The setting that governs `reason`, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governing_setting: Option<String>,
    /// Of `missing_files`, those under a directory that evidence shows holds installed
    /// third-party packages ([`DependencyEvidence`]), across every directory, listed or not.
    /// Zero on a manifest written before directories were recorded per language, which prices
    /// every missing file as first-party source.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub dependency_files: usize,
    /// The directories holding the most missing files, at most [`COVERAGE_GAP_DIRS_LISTED`] of
    /// each class, most files first. Empty for an `omitted` gap, whose files were considered,
    /// and on a manifest written before directories were recorded per language.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_dirs: Vec<CoverageGapDir>,
}

/// At most this many directories of each [`ExcludedDirClass`] are named on a [`CoverageGap`].
pub const COVERAGE_GAP_DIRS_LISTED: usize = 3;

/// One directory behind a [`CoverageGap`]'s missing files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageGapDir {
    /// `/`-separated, relative to the repository root: the directory holding installed packages
    /// for a `dependencies` entry, the top-level directory otherwise (`.` for the root).
    pub path: String,
    /// Missing files of the gap's language under it, for the gap's cause.
    pub files: usize,
    pub class: ExcludedDirClass,
    /// What showed the directory holds installed packages; absent when `class` is
    /// `unclassified`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<DependencyEvidence>,
}

impl CoverageGapDir {
    /// `generated/ (40 unclassified)`, `env/lib/python3.12/site-packages/ (340 dependencies:
    /// python-environment)`
    pub fn label(&self) -> String {
        let files = group_thousands(self.files);
        match self.evidence {
            Some(evidence) => format!(
                "{}/ ({files} dependencies: {})",
                self.path,
                evidence.label()
            ),
            None => format!("{}/ ({files} {})", self.path, self.class.label()),
        }
    }
}

/// What a directory of policy-excluded files is, as far as evidence shows.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExcludedDirClass {
    /// Installed third-party packages, on [`DependencyEvidence`]. Such files cannot hold callers
    /// of this repository's code, and are never the file a task edits.
    Dependencies,
    /// No evidence either way. Priced as first-party source: a directory's name is not evidence,
    /// and a git-ignored `generated/` tree of first-party code plausibly holds callers.
    Unclassified,
}

impl ExcludedDirClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Dependencies => "dependencies",
            Self::Unclassified => "unclassified",
        }
    }
}

/// The on-disk evidence that a directory holds installed third-party packages. Each is a file
/// a package manager writes; none is a directory name on its own.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DependencyEvidence {
    /// The `lib/python*/site-packages` (or Windows `Lib/site-packages`) of a directory holding
    /// `pyvenv.cfg` (venv, virtualenv 20+) or `conda-meta/` (conda): a Python environment's
    /// installed packages, whatever the environment is named. The marker covers only that
    /// directory: the environment's `bin/`, and anything else beside the marker, stay
    /// unclassified.
    PythonEnvironment,
    /// A `site-packages` or `dist-packages` directory holding installed-distribution metadata
    /// (`*.dist-info`, `*.egg-info`), as pip and setuptools write it.
    SitePackages,
    /// A `vendor` directory holding `modules.txt`, which `go mod vendor` writes.
    GoVendor,
    /// A `vendor` directory holding `composer/installed.json`, which Composer writes.
    ComposerVendor,
    /// A package directory `<Id>.<Version>/` directly inside a `packages` directory, in NuGet's
    /// `packages.config` layout: it holds the `<Id>.<Version>.nupkg` or `<Id>.nuspec` restore
    /// extracted. Only that package is classed; the rest of `packages/` stays unclassified.
    NugetPackages,
    /// A package directory `<id>/` directly inside a `packages` or `.packages` directory, in the
    /// layout of NuGet's global packages folder: a `<version>/` under it holds the
    /// `.nupkg.metadata` or `<id>.<version>.nupkg.sha512` restore writes, as when
    /// `globalPackagesFolder` or `NUGET_PACKAGES` points inside the repository.
    NugetGlobalPackages,
}

impl DependencyEvidence {
    /// Label for summaries (`python-environment`).
    pub fn label(self) -> &'static str {
        match self {
            Self::PythonEnvironment => "python-environment",
            Self::SitePackages => "site-packages",
            Self::GoVendor => "go-vendor",
            Self::ComposerVendor => "composer-vendor",
            Self::NugetPackages => "nuget-packages",
            Self::NugetGlobalPackages => "nuget-global-packages",
        }
    }
}

/// Policy-excluded files of one language under one directory, from
/// [`IndexCoverage::policy_excluded_dirs_by_language`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExcludedDir {
    /// Files by the rule that excluded them.
    pub by_source: BTreeMap<SkipSource, usize>,
    /// Set when the directory holds installed third-party packages; absent when nothing shows
    /// it does, which prices its files as first-party source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency: Option<DependencyEvidence>,
}

impl ExcludedDir {
    pub fn class(&self) -> ExcludedDirClass {
        if self.dependency.is_some() {
            ExcludedDirClass::Dependencies
        } else {
            ExcludedDirClass::Unclassified
        }
    }

    /// Files excluded by `source`, or by any source when `None`.
    fn files(&self, source: Option<SkipSource>) -> usize {
        match source {
            Some(source) => self.by_source.get(&source).copied().unwrap_or(0),
            None => self.by_source.values().sum(),
        }
    }
}

impl CoverageGap {
    /// `missing_files` over `language_files`, in 0..=1.
    pub fn missing_share(&self) -> f64 {
        if self.language_files == 0 {
            return 0.0;
        }
        (self.missing_files as f64 / self.language_files as f64).min(1.0)
    }

    /// See [`COVERAGE_GAP_MAJORITY_SHARE`]. Decides what is reported; the caps are decided by
    /// [`Self::is_source_majority`].
    pub fn is_majority(&self) -> bool {
        self.missing_share() >= COVERAGE_GAP_MAJORITY_SHARE
    }

    /// Missing files not shown to be installed dependencies: those that may be first-party
    /// source, unclassified files included.
    pub fn source_missing_files(&self) -> usize {
        self.missing_files.saturating_sub(self.dependency_files)
    }

    /// [`Self::source_missing_files`] over the language's files less its installed
    /// dependencies, in 0..=1: how much of what may be first-party source is missing. Equal to
    /// [`Self::missing_share`] when no dependency tree was shown.
    pub fn source_missing_share(&self) -> f64 {
        let dependency_files = self.dependency_files.min(self.missing_files);
        let source_files = self.language_files.saturating_sub(dependency_files);
        if source_files == 0 {
            return 0.0;
        }
        (self.source_missing_files() as f64 / source_files as f64).min(1.0)
    }

    /// Whether most of the language's possibly-first-party source is missing: the test the
    /// 0.50 and 0.74 caps apply. A gap made of installed dependencies is a majority gap and is
    /// reported, but those files cannot hold callers of the code under edit, so it does not
    /// cap. Only evidence moves a file out of the source count; an unclassified directory
    /// stays in it.
    pub fn is_source_majority(&self) -> bool {
        self.source_missing_files() > 0
            && self.source_missing_share() >= COVERAGE_GAP_MAJORITY_SHARE
    }

    /// `coverage:<language>:<cause>`: names the manifest coverage entries the gap is derived
    /// from, `by_language.<language>`, `policy_excluded_by_language.<language>` and
    /// `policy_excluded_dirs_by_language.<language>`, which `repo_status` and
    /// `ok --json status` report.
    pub fn evidence_id(&self) -> String {
        format!("coverage:{}:{}", self.language, self.cause.key())
    }

    /// `git-ignore`, or `git-ignore: env/lib/python3.12/site-packages/ (340 dependencies:
    /// python-environment), generated/ (40 unclassified)` when the directories are known.
    fn reason_detail(&self) -> String {
        if self.excluded_dirs.is_empty() {
            return self.reason.clone();
        }
        format!(
            "{}: {}",
            self.reason,
            self.excluded_dirs
                .iter()
                .map(CoverageGapDir::label)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// `rust (25 of 27 files, git-ignore)`, with the main directories when they are known.
    pub fn summary(&self) -> String {
        format!(
            "{} ({} of {} files, {})",
            self.language,
            group_thousands(self.missing_files),
            group_thousands(self.language_files),
            self.reason_detail()
        )
    }

    /// The confidence caveat: the excluded share, the reason category, the main directories
    /// and, when some are installed dependencies, how much of the rest may be source.
    pub fn caveat(&self) -> String {
        let dependencies = if self.dependency_files > 0 {
            format!(
                "; {} are installed dependencies, so {} of {} possibly first-party files ({:.1}%) are missing",
                group_thousands(self.dependency_files.min(self.missing_files)),
                group_thousands(self.source_missing_files()),
                group_thousands(
                    self.language_files
                        .saturating_sub(self.dependency_files.min(self.missing_files))
                ),
                self.source_missing_share() * 100.0
            )
        } else {
            String::new()
        };
        format!(
            "index coverage: {} of {} {} source files ({:.1}%) are not indexed ({}){dependencies}; an absence among them is not evidence",
            group_thousands(self.missing_files),
            group_thousands(self.language_files),
            self.language,
            self.missing_share() * 100.0,
            self.reason_detail()
        )
    }

    /// What to do before reading an absence as evidence, naming the governing setting.
    pub fn next_probe(&self) -> String {
        let files = group_thousands(self.missing_files);
        match (self.cause, self.governing_setting.as_deref()) {
            (CoverageGapCause::GitIgnore, _) => format!(
                "The index follows {GIT_IGNORE_RULES}, which exclude {files} {} file(s); search them directly before concluding a name is absent. Remove the rule if they are source an agent should see, or list the paths under `[index] exclude` if the exclusion is intended, then run `ok index .`.",
                self.language
            ),
            (_, Some(setting)) => format!(
                "{setting} governs the {files} {} file(s) missing from the index ({}); search them directly before concluding a name is absent, and change the setting if they should be indexed, then run `ok index .`.",
                self.language, self.reason
            ),
            (_, None) => format!(
                "No ok.toml key governs the {files} {} file(s) missing from the index ({}); search them directly before concluding a name is absent, and review `ok --json status` `coverage.by_language` and `quality.skipped_paths`.",
                self.language, self.reason
            ),
        }
    }
}

/// What a pack or plan knows about its index's coverage. A recorded record with no gaps and
/// no record at all are different facts: the first says nothing material is missing, the
/// second says nobody measured. A manifest written before coverage recording, a cross-project
/// index, an imported snapshot, and a manifest that could not be read are all `Unavailable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverageInput {
    /// The index published a coverage record; these are the gaps it implies, empty when the
    /// index holds what discovery found.
    Recorded(Vec<CoverageGap>),
    /// No coverage record, so what the index omitted is unknown.
    Unavailable,
    /// The coverage record could not be read: the manifest failed to decode, or the store
    /// returned an error. Distinct from [`Self::Unavailable`], which says the index published
    /// no record; this says nobody could tell.
    Unreadable,
}

impl Default for CoverageInput {
    /// A recorded record with no gaps: the shape a caller that says nothing should get, so a
    /// synthetic input never claims coverage was unmeasured.
    fn default() -> Self {
        Self::Recorded(Vec::new())
    }
}

impl CoverageInput {
    /// The gaps of a recorded record; empty when coverage is unavailable, because an
    /// unmeasured index implies no particular gap.
    pub fn gaps(&self) -> &[CoverageGap] {
        match self {
            Self::Recorded(gaps) => gaps,
            Self::Unavailable | Self::Unreadable => &[],
        }
    }

    /// The caveat this state carries, or `None` when coverage was recorded. Both non-recorded
    /// states cap like any other caveat: a pack cannot claim completeness it never checked.
    pub fn caveat(&self) -> Option<&'static str> {
        match self {
            Self::Recorded(_) => None,
            Self::Unavailable => Some(UNRECORDED_COVERAGE_CAVEAT),
            Self::Unreadable => Some(UNREADABLE_COVERAGE_CAVEAT),
        }
    }
}

/// Score-component signal emitted when a majority coverage gap matches the selected context's
/// language. Zero weight like `index_coverage`: it carries the fact, not a score. `ok preflight`
/// reads it to withhold `SafeToStart`, so it is a stable name rather than prose to match on.
pub const COVERAGE_SELECTED_LANGUAGE_SIGNAL: &str = "index_coverage_selected_language";

/// The caveat for a coverage record that could not be read. Says the read failed, not that the
/// index omitted nothing - the two are different facts and only one is about the repository.
pub const UNREADABLE_COVERAGE_CAVEAT: &str =
    "index coverage could not be read from the manifest, so what this index omitted is unknown";

/// The caveat for an index that published no coverage record. Worded to say the opposite of a
/// gap caveat: a gap names what is missing, this says nothing is known about what is missing.
pub const UNRECORDED_COVERAGE_CAVEAT: &str = "index coverage is unrecorded: this index does not report which source files it omitted, so an absence in it is not evidence of absence";

/// Coverage below this fraction is reported as a warning: a tenth of a corpus vanishing
/// behind an ingest rule is exactly the failure this summary exists to expose.
pub const INDEX_COVERAGE_WARN_PERCENT: f64 = 98.0;

/// A programming language is judged by percentage only once it has this many
/// discovered files; below it one hidden file swings the ratio without meaning.
pub const INDEX_COVERAGE_LANGUAGE_FLOOR: usize = 50;

/// A programming language missing at least this many files warns regardless of
/// percentage: 25 of 10,012 Java files is 99.75% and still a dropped package.
pub const INDEX_COVERAGE_MISSING_FILES_WARN: usize = 20;

/// At most this many pruned directories are named in a status summary of
/// [`IndexCoverage::pruned`] ([`IndexCoverage::status_view`]); the rest are counted in
/// `pruned_unlisted`. A monorepo with a `node_modules` per package would otherwise put
/// thousands of entries into every `repo_status` answer.
pub const PRUNED_DIRS_LISTED: usize = 50;

/// Why discovery cut a directory from the walk.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PruneReason {
    /// Build output on strong evidence: a `target`, `build` or `dist` holding a `CACHEDIR.TAG`
    /// or sitting beside its project's build manifest (`Cargo.toml`, `pom.xml`, `package.json`,
    /// ...). Files committed under it are listed, not counted as missing: a committed `dist/`
    /// bundle beside a `package.json` is still a bundle.
    BuildOutput,
    /// A `build` or `dist` directory pruned only because no module, package or build manifest
    /// accounts for it. The weak rule: a source directory it misclassifies is plausible, so
    /// git-tracked source under it counts as missing (`SkipReason::Pruned`).
    UndeclaredBuildDir,
    /// Installed packages: `node_modules`.
    Dependencies,
    /// A Python environment: `.venv` or `venv` holding `pyvenv.cfg` or `conda-meta`.
    VirtualEnv,
    /// A nested Git work tree inside this repository: a checked-out submodule, or a repository
    /// cloned or a worktree added inside it. Git records at most a gitlink for it, never its
    /// files, so its content is another repository's source, and neither this repository's
    /// diffs nor `ok verify` see an edit inside it.
    Submodule,
    /// MSBuild output: a `bin` beside an MSBuild project file (`*.csproj`, `*.fsproj`,
    /// `*.vbproj`) holding build artifacts (`*.dll`, `*.pdb`, `*.exe`, `*.deps.json`, or a
    /// `Debug`/`Release` directory), or an `obj` holding a NuGet restore's output
    /// (`project.assets.json`, `*.nuget.g.props`) or a `Debug`/`Release` directory beside a
    /// project file. `bin` is also where scripts are kept, and MSBuild writes nothing a script
    /// directory lacks the name of, so git-tracked source under it counts as missing and is not
    /// forbidden, as under an undeclared build directory: a committed `bin/release_tool.py`
    /// beside the build's `bin/Debug/` is source the index does not hold.
    MsbuildOutput,
}

impl PruneReason {
    /// Label for summaries (`build-output`).
    pub fn label(self) -> &'static str {
        match self {
            Self::BuildOutput => "build-output",
            Self::UndeclaredBuildDir => "undeclared-build-dir",
            Self::Dependencies => "dependencies",
            Self::VirtualEnv => "virtual-env",
            Self::Submodule => "submodule",
            Self::MsbuildOutput => "msbuild-output",
        }
    }

    /// Whether git-tracked source under a directory pruned for this reason is counted as
    /// missing from the index. Only the weak rule's guess can hide real source, and a nested
    /// work tree that is not a submodule: Git tracks only a real submodule's gitlink, never a
    /// file under it, so a tracked file there means this repository owns the directory and a
    /// stray `.git` (a `git init`, a tool's scratch clone) cut it from the walk. MSBuild output
    /// counts too: MSBuild commits nothing, so a tracked file under its `bin` or `obj` is
    /// someone's script or source sharing the directory with the build (#684).
    pub fn counts_tracked_source(self) -> bool {
        matches!(
            self,
            Self::UndeclaredBuildDir | Self::Submodule | Self::MsbuildOutput
        )
    }
}

/// One directory discovery pruned, by repository-relative path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PrunedDir {
    /// `/`-separated, relative to the repository root.
    pub path: String,
    pub reason: PruneReason,
    /// Git-tracked programming-language files under the directory. `None` outside a Git work
    /// tree, where nothing says whether the directory holds committed source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_source_files: Option<usize>,
}

impl PrunedDir {
    /// `dist/ (30 tracked source files)`, or `target/` when it holds none or Git cannot say:
    /// a source directory pruned by mistake shows its committed files at a glance.
    pub fn label(&self) -> String {
        match self.tracked_source_files {
            Some(count) if count > 0 => format!(
                "{}/ ({} tracked source {})",
                self.path,
                group_thousands(count),
                if count == 1 { "file" } else { "files" }
            ),
            _ => format!("{}/", self.path),
        }
    }
}

/// What discovery found versus what the index holds, for every recognised language.
///
/// `discovered` counts only files the walker visited, plus git-tracked programming-language
/// files under a `build` or `dist` pruned only because nothing declares it (each also skipped
/// as `pruned`, so the index visibly lacks them). Two things the walk cannot see are counted
/// beside the ratio so it is never read as more than it is: `pruned_dirs`, directories cut
/// from the walk as build output, dependencies or submodules (`.git` and `.ok` are not counted,
/// they are never user source), named in `pruned`; and `walk_errors`, directory reads that
/// failed, whose files were never discovered. Untracked build output, and anything under a
/// directory pruned on strong evidence (a cache tag, a build manifest beside it), is never
/// counted against the ratio. Files
/// whose language is unknown are not source files and are not counted; their skips remain
/// in `skip_counts`. Files admitted to the document corpus count as indexed.
///
/// The ratio is `indexed` over `considered()`: discovered files minus those a policy
/// excluded (`SkipReason::is_policy`). Policy exclusions stay in `skipped` and are
/// reported beside the ratio with their governing setting and top directories.
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
    /// Directories pruned before discovery as build output, dependencies or submodules, listed
    /// or not.
    #[serde(default)]
    pub pruned_dirs: usize,
    /// The pruned directories by path: undeclared build directories holding tracked source
    /// first, then those holding any tracked source, then by path. The manifest stores every
    /// one, since plans forbid edits under each; status summaries show the first
    /// [`PRUNED_DIRS_LISTED`] ([`IndexCoverage::status_view`]). A manifest written by 4.0
    /// releases after #477 stored at most that many. Empty on a manifest written before paths
    /// were recorded, where `pruned_dirs` alone says something was pruned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pruned: Vec<PrunedDir>,
    /// Pruned directories counted in `pruned_dirs` but not in `pruned`: secret-like paths that
    /// are not shown, and in a status summary those past the cap.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub pruned_unlisted: usize,
    /// Directory reads that failed (`skip_counts.error`); their files are unknown.
    #[serde(default)]
    pub walk_errors: usize,
    /// Policy-excluded source files by the rule that excluded them, so advice can name
    /// the setting that governs the dominant one. Empty on manifests written before it
    /// was recorded.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub policy_excluded_by_source: BTreeMap<SkipSource, usize>,
    /// Policy-excluded source files by top-level directory (`.claude`, `.github`);
    /// files at the root count under `.`. Redacted paths are not recorded.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub policy_excluded_dirs: BTreeMap<String, usize>,
    /// `policy_excluded_by_source` split by language key, so a verdict can tell a
    /// language `.gitignore` mostly set aside from one the hidden-file rule trimmed.
    /// Empty on manifests written before it was recorded, which read as no per-language
    /// data rather than as nothing excluded.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub policy_excluded_by_language: BTreeMap<String, BTreeMap<SkipSource, usize>>,
    /// Policy-excluded source files per language key and directory, so a gap can name what was
    /// excluded and price installed dependencies apart from first-party source. A file under a
    /// directory evidence shows holds installed packages counts under that directory
    /// (`env/lib/python3.12/site-packages`, `svc/vendor`) with its [`DependencyEvidence`]; any
    /// other file under its top-level directory, unclassified. Redacted paths are not recorded.
    /// Empty on manifests written before it was recorded, which read as no per-language
    /// directory data: every missing file is priced as first-party source, as before.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub policy_excluded_dirs_by_language: BTreeMap<String, BTreeMap<String, ExcludedDir>>,
}

impl IndexCoverage {
    pub fn record_discovered(&mut self, language: &Language) {
        self.discovered += 1;
        self.by_language
            .entry(language.key().to_owned())
            .or_default()
            .discovered += 1;
    }

    /// Records an indexed file that parsed with syntax errors, and whether patterns named its
    /// types.
    pub fn record_parsed_with_errors(&mut self, language: &Language, pattern_fallback: bool) {
        let entry = self
            .by_language
            .entry(language.key().to_owned())
            .or_default();
        entry.parsed_with_errors += 1;
        entry.pattern_fallback += usize::from(pattern_fallback);
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

    /// The detail `record_skipped` cannot carry for a policy skip: which rule chose it,
    /// across the repository and for `language`, and where the file lives. `top_dir` is
    /// the first path component, or `None` for a redacted path.
    pub fn record_policy_exclusion(
        &mut self,
        language: &Language,
        source: SkipSource,
        top_dir: Option<&str>,
    ) {
        self.record_classified_policy_exclusion(language, source, top_dir, None);
    }

    /// [`Self::record_policy_exclusion`] for a file that lies under `dependency`, the directory
    /// evidence shows holds installed packages (repository-relative, `/`-separated), when it
    /// does. The file counts under that directory instead of its top-level one in
    /// `policy_excluded_dirs_by_language`; `policy_excluded_dirs` stays by top-level directory.
    /// Nothing is recorded per directory for a redacted path (`top_dir` `None`).
    pub fn record_classified_policy_exclusion(
        &mut self,
        language: &Language,
        source: SkipSource,
        top_dir: Option<&str>,
        dependency: Option<(&str, DependencyEvidence)>,
    ) {
        *self.policy_excluded_by_source.entry(source).or_default() += 1;
        *self
            .policy_excluded_by_language
            .entry(language.key().to_owned())
            .or_default()
            .entry(source)
            .or_default() += 1;
        let Some(top_dir) = top_dir else {
            return;
        };
        *self
            .policy_excluded_dirs
            .entry(top_dir.to_owned())
            .or_default() += 1;
        let (dir, evidence) = match dependency {
            Some((dir, evidence)) => (dir, Some(evidence)),
            None => (top_dir, None),
        };
        let dirs = self
            .policy_excluded_dirs_by_language
            .entry(language.key().to_owned())
            .or_default();
        let entry = match dirs.get_mut(dir) {
            Some(entry) => {
                // Ingest never records one path both ways: a dependency entry is keyed by its
                // evidence directory, every file below which is classified alike, and an
                // unclassified entry by a top-level directory, which equals a dependency key
                // only when that top-level directory is itself the evidence directory. A caller
                // that mixes them gets the conservative answer.
                if entry.dependency != evidence {
                    entry.dependency = None;
                }
                entry
            }
            None => dirs.entry(dir.to_owned()).or_insert(ExcludedDir {
                by_source: BTreeMap::new(),
                dependency: evidence,
            }),
        };
        *entry.by_source.entry(source).or_default() += 1;
    }

    /// The directories behind `language`'s excluded files for `source` (any source when
    /// `None`): at most [`COVERAGE_GAP_DIRS_LISTED`] of each class, so a large dependency tree
    /// never hides the source directory that prices the gap, most files first; with the files
    /// under installed dependencies counted across every directory.
    fn gap_dirs(&self, language: &str, source: Option<SkipSource>) -> (Vec<CoverageGapDir>, usize) {
        let Some(dirs) = self.policy_excluded_dirs_by_language.get(language) else {
            return (Vec::new(), 0);
        };
        let mut all = dirs
            .iter()
            .map(|(path, dir)| CoverageGapDir {
                path: path.clone(),
                files: dir.files(source),
                class: dir.class(),
                evidence: dir.dependency,
            })
            .filter(|dir| dir.files > 0)
            .collect::<Vec<_>>();
        let dependency_files = all
            .iter()
            .filter(|dir| dir.class == ExcludedDirClass::Dependencies)
            .map(|dir| dir.files)
            .sum();
        let by_files = |a: &CoverageGapDir, b: &CoverageGapDir| {
            b.files.cmp(&a.files).then(a.path.cmp(&b.path))
        };
        all.sort_by(by_files);
        let mut listed = Vec::new();
        for class in [
            ExcludedDirClass::Unclassified,
            ExcludedDirClass::Dependencies,
        ] {
            listed.extend(
                all.iter()
                    .filter(|dir| dir.class == class)
                    .take(COVERAGE_GAP_DIRS_LISTED)
                    .cloned(),
            );
        }
        listed.sort_by(by_files);
        (listed, dependency_files)
    }

    /// Record every directory discovery pruned: `listed` by path, `unlisted` (secret-like
    /// paths) by count. Undeclared build directories holding tracked source sort first, most
    /// files first, so the cap never hides the ones that matter; then any other directory
    /// holding tracked source, most first, so a summary that names three shows them; the rest
    /// follow by path.
    pub fn record_pruned_dirs(&mut self, mut listed: Vec<PrunedDir>, unlisted: usize) {
        let missing_source = |dir: &PrunedDir| {
            if dir.reason.counts_tracked_source() {
                dir.tracked_source_files.unwrap_or(0)
            } else {
                0
            }
        };
        let tracked = |dir: &PrunedDir| dir.tracked_source_files.unwrap_or(0);
        listed.sort_by(|a, b| {
            missing_source(b)
                .cmp(&missing_source(a))
                .then_with(|| tracked(b).cmp(&tracked(a)))
                .then_with(|| a.path.cmp(&b.path))
        });
        self.pruned_dirs = listed.len() + unlisted;
        self.pruned_unlisted = unlisted;
        self.pruned = listed;
    }

    /// This record as a status summary shows it: at most [`PRUNED_DIRS_LISTED`] pruned
    /// directories, the rest counted in `pruned_unlisted`. A monorepo with a `node_modules` per
    /// package would otherwise put thousands of entries into every `repo_status` answer; the
    /// stored record, and `--full` / `detail: "full"`, keep them all.
    pub fn status_view(&self) -> IndexCoverage {
        let mut view = self.clone();
        if view.pruned.len() > PRUNED_DIRS_LISTED {
            view.pruned_unlisted += view.pruned.len() - PRUNED_DIRS_LISTED;
            view.pruned.truncate(PRUNED_DIRS_LISTED);
        }
        view
    }

    /// An indexed file discovery would never have discovered: one an imported index holds
    /// under a directory this repository prunes on strong evidence, where discovery counts no
    /// file. Removed from `indexed` and `discovered` alike, so the ratio reads as `ok index`
    /// here would leave it.
    pub fn record_indexed_undiscovered(&mut self, language: &Language, generated: bool) {
        self.discovered = self.discovered.saturating_sub(1);
        self.indexed = self.indexed.saturating_sub(1);
        if generated {
            self.generated = self.generated.saturating_sub(1);
        }
        if let Some(entry) = self.by_language.get_mut(language.key()) {
            entry.discovered = entry.discovered.saturating_sub(1);
            entry.indexed = entry.indexed.saturating_sub(1);
            if generated {
                entry.generated = entry.generated.saturating_sub(1);
            }
        }
    }

    /// Git-tracked programming-language files the index does not hold because a directory
    /// above them was pruned as undeclared build output.
    pub fn pruned_source_files(&self) -> usize {
        self.skipped.get(&SkipReason::Pruned).copied().unwrap_or(0)
    }

    /// Listed directories whose tracked source counts as missing (undeclared build directories,
    /// and nested work trees that are not submodules) and that hold some, most first.
    pub fn pruned_source_dirs(&self) -> Vec<&PrunedDir> {
        self.pruned
            .iter()
            .filter(|dir| {
                dir.reason.counts_tracked_source()
                    && dir.tracked_source_files.is_some_and(|count| count > 0)
            })
            .collect()
    }

    /// Discovered files a policy excluded, across every recognised language.
    pub fn excluded_by_policy(&self) -> usize {
        self.skipped
            .iter()
            .filter(|(reason, _)| reason.is_policy())
            .map(|(_, count)| *count)
            .sum()
    }

    /// Files the index would consider under the current policy: the denominator.
    pub fn considered(&self) -> usize {
        self.discovered.saturating_sub(self.excluded_by_policy())
    }

    /// The all-languages ratio, reported everywhere. `None` when nothing was
    /// discovered or policy left nothing to consider: a ratio over zero files is not
    /// evidence.
    pub fn percent(&self) -> Option<f64> {
        let considered = self.considered();
        (considered > 0).then(|| self.indexed as f64 * 100.0 / considered as f64)
    }

    /// `(considered, indexed)` over programming languages only.
    pub fn programming_totals(&self) -> (usize, usize) {
        self.by_language
            .iter()
            .filter(|(language, _)| language_key_is_programming(language))
            .fold((0, 0), |(considered, indexed), (_, coverage)| {
                (
                    considered + coverage.considered(),
                    indexed + coverage.indexed,
                )
            })
    }

    /// `(discovered, excluded by policy)` over programming languages only.
    pub fn programming_policy_totals(&self) -> (usize, usize) {
        self.by_language
            .iter()
            .filter(|(language, _)| language_key_is_programming(language))
            .fold((0, 0), |(discovered, excluded), (_, coverage)| {
                (
                    discovered + coverage.discovered,
                    excluded + coverage.excluded_by_policy(),
                )
            })
    }

    /// The ratio the warning is judged on. Hidden `.github/*.yml` and `.vscode/*.json`
    /// drag the all-languages ratio under the threshold on almost every repository; a
    /// warning that always fires stops being read, so the verdict follows the source.
    pub fn programming_percent(&self) -> Option<f64> {
        let (considered, indexed) = self.programming_totals();
        (considered > 0).then(|| indexed as f64 * 100.0 / considered as f64)
    }

    pub fn below_warn_threshold(&self) -> bool {
        self.programming_percent()
            .is_some_and(|percent| percent < INDEX_COVERAGE_WARN_PERCENT)
    }

    /// Something the ratio cannot account for: unreadable or pruned directories.
    pub fn has_blind_spots(&self) -> bool {
        self.walk_errors > 0 || self.pruned_dirs > 0
    }

    /// Non-policy skip reasons — the omissions the ratio is judged on — by descending
    /// count, ties broken by reason order, at most `limit`.
    pub fn top_skip_reasons(&self, limit: usize) -> Vec<(SkipReason, usize)> {
        let mut reasons = self
            .skipped
            .iter()
            .filter(|(reason, count)| !reason.is_policy() && **count > 0)
            .map(|(reason, count)| (*reason, *count))
            .collect::<Vec<_>>();
        reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        reasons.truncate(limit);
        reasons
    }

    /// Policy skip reasons by descending count, ties broken by reason order.
    pub fn policy_skip_reasons(&self) -> Vec<(SkipReason, usize)> {
        let mut reasons = self
            .skipped
            .iter()
            .filter(|(reason, count)| reason.is_policy() && **count > 0)
            .map(|(reason, count)| (*reason, *count))
            .collect::<Vec<_>>();
        reasons.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        reasons
    }

    /// Top-level directories holding the most policy-excluded source files, most first,
    /// at most `limit`.
    pub fn top_policy_excluded_dirs(&self, limit: usize) -> Vec<(&str, usize)> {
        let mut dirs = self
            .policy_excluded_dirs
            .iter()
            .map(|(dir, count)| (dir.as_str(), *count))
            .collect::<Vec<_>>();
        dirs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        dirs.truncate(limit);
        dirs
    }

    /// The policy source that excluded the most source files, with its count; `None`
    /// on a manifest that predates source recording or excluded nothing.
    pub fn dominant_policy_source(&self) -> Option<(SkipSource, usize)> {
        self.policy_excluded_by_source
            .iter()
            .filter(|(_, count)| **count > 0)
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(source, count)| (*source, *count))
    }

    /// What the headline's policy count is made of: `2,903 hidden, 8 ignored; 2,900
    /// under .claude/, 8 under .github/; `[security] allow_hidden_files` governs the
    /// largest share`. `None` when policy excluded nothing.
    pub fn policy_exclusion_detail(&self) -> Option<String> {
        if self.excluded_by_policy() == 0 {
            return None;
        }
        let mut detail = format_skip_reasons(&self.policy_skip_reasons());
        let dirs = self.top_policy_excluded_dirs(3);
        if !dirs.is_empty() {
            detail.push_str("; ");
            detail.push_str(
                &dirs
                    .iter()
                    .map(|(dir, count)| format!("{} under {dir}/", group_thousands(*count)))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        if let Some(setting) = self
            .dominant_policy_source()
            .and_then(|(source, _)| source.governing_setting())
        {
            detail.push_str(&format!("; {setting} governs the largest share"));
        }
        Some(detail)
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
                let considered = coverage.considered();
                let missing = considered.saturating_sub(coverage.indexed);
                let by_ratio = considered >= INDEX_COVERAGE_LANGUAGE_FLOOR
                    && percent < INDEX_COVERAGE_WARN_PERCENT;
                let by_count = missing >= INDEX_COVERAGE_MISSING_FILES_WARN;
                (by_ratio || by_count).then_some((language.as_str(), percent, missing))
            })
            .collect::<Vec<_>>();
        languages.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(b.0)));
        languages
    }

    /// Programming languages where `source` excluded at least
    /// `INDEX_COVERAGE_MISSING_FILES_WARN` files and more files than the language has
    /// considered, as `(language, excluded, considered)`, most excluded first. A rule
    /// that sets aside most of a language changes what an absence means even when the
    /// ratio over what remains is 100%. Empty on a manifest written before per-language
    /// sources were recorded: missing data is not evidence of a dominant rule.
    pub fn languages_mostly_excluded_by(&self, source: SkipSource) -> Vec<(&str, usize, usize)> {
        let mut languages = self
            .policy_excluded_by_language
            .iter()
            .filter(|(language, _)| language_key_is_programming(language))
            .filter_map(|(language, sources)| {
                let excluded = sources.get(&source).copied().unwrap_or(0);
                let considered = self
                    .by_language
                    .get(language)
                    .map_or(0, LanguageCoverage::considered);
                (excluded >= INDEX_COVERAGE_MISSING_FILES_WARN && excluded > considered)
                    .then_some((language.as_str(), excluded, considered))
            })
            .collect::<Vec<_>>();
        languages.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        languages
    }

    /// Programming languages whose missing source changes what an absence means, most missing
    /// files first. This is the verdict context packs and plans price and `repo_status`
    /// reports, built from the predicates `ok doctor`'s coverage check applies, so they cannot
    /// disagree about which languages qualify:
    ///
    /// - `excluded_by_policy`: discovery found programming-language source and policy left
    ///   none of it to consider. Every such language is a gap under its dominant source.
    /// - `git_ignore`: [`Self::languages_mostly_excluded_by`] for [`SkipSource::GitIgnore`].
    /// - `omitted`: [`Self::languages_below_warn_threshold`].
    ///
    /// The index's own settings (`hidden`, `vendor`, `fast_mode`, `denied`, `[index] exclude`,
    /// `.okignore`) are not gaps while some source remains considered: they state an intended
    /// exclusion, and a caveat that fires on every repository stops being read. The doctor's
    /// repository-wide ratio and walk errors are not per-language and stay in its own check.
    pub fn gaps(&self) -> Vec<CoverageGap> {
        let mut gaps = Vec::new();
        let (programming_discovered, _) = self.programming_policy_totals();
        if programming_discovered > 0 && self.programming_percent().is_none() {
            for (language, coverage) in &self.by_language {
                if !language_key_is_programming(language) || coverage.discovered == 0 {
                    continue;
                }
                let source = self
                    .policy_excluded_by_language
                    .get(language)
                    .and_then(|sources| {
                        sources
                            .iter()
                            .filter(|(_, count)| **count > 0)
                            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
                            .map(|(source, _)| *source)
                    });
                let reason = match (source, coverage.dominant_skip_reason(true)) {
                    (Some(source), _) => source.label().to_owned(),
                    (None, Some(reason)) => reason.label().to_owned(),
                    (None, None) => "unattributed".to_owned(),
                };
                let governing_setting = source.and_then(|source| match source {
                    SkipSource::GitIgnore => Some(GIT_IGNORE_RULES),
                    other => other.governing_setting(),
                });
                let missing_files = coverage.excluded_by_policy();
                let (excluded_dirs, dependency_files) = self.gap_dirs(language, None);
                gaps.push(CoverageGap {
                    language: language.clone(),
                    cause: CoverageGapCause::ExcludedByPolicy,
                    missing_files,
                    language_files: coverage.discovered,
                    reason,
                    governing_setting: governing_setting.map(str::to_owned),
                    dependency_files: dependency_files.min(missing_files),
                    excluded_dirs,
                });
            }
        } else {
            for (language, excluded, considered) in
                self.languages_mostly_excluded_by(SkipSource::GitIgnore)
            {
                let (excluded_dirs, dependency_files) =
                    self.gap_dirs(language, Some(SkipSource::GitIgnore));
                gaps.push(CoverageGap {
                    language: language.to_owned(),
                    cause: CoverageGapCause::GitIgnore,
                    missing_files: excluded,
                    language_files: excluded + considered,
                    reason: SkipSource::GitIgnore.label().to_owned(),
                    governing_setting: Some(GIT_IGNORE_RULES.to_owned()),
                    dependency_files: dependency_files.min(excluded),
                    excluded_dirs,
                });
            }
            for (language, _, missing) in self.languages_below_warn_threshold() {
                let Some(coverage) = self.by_language.get(language) else {
                    continue;
                };
                let reason = coverage.dominant_skip_reason(false);
                gaps.push(CoverageGap {
                    language: language.to_owned(),
                    cause: CoverageGapCause::Omitted,
                    missing_files: missing,
                    language_files: coverage.considered(),
                    reason: reason.map_or_else(
                        || "unattributed".to_owned(),
                        |reason| reason.label().to_owned(),
                    ),
                    governing_setting: (reason == Some(SkipReason::TooLarge))
                        .then(|| SkipSource::SizeLimit.governing_setting())
                        .flatten()
                        .map(str::to_owned),
                    // Considered files the index failed to hold: no exclusion rule chose them,
                    // so there is no excluded directory to name.
                    dependency_files: 0,
                    excluded_dirs: Vec::new(),
                });
            }
        }
        gaps.sort_by(|a, b| {
            b.missing_files
                .cmp(&a.missing_files)
                .then_with(|| a.language.cmp(&b.language))
                .then(a.cause.cmp(&b.cause))
        });
        gaps
    }

    /// The counts, judged ratio first: `921 of 922 programming-language files indexed
    /// (99.9%); 1,417 of 1,461 recognised files indexed (97.0%) overall; 2,911 excluded
    /// by policy`. Denominators are files considered under the current policy; the
    /// policy count follows so the reader knows what was set aside. Shared by the
    /// index summary line and the doctor check so the two can never disagree.
    pub fn headline(&self) -> String {
        if self.discovered == 0 {
            return "no source files discovered".into();
        }
        let policy = match self.excluded_by_policy() {
            0 => String::new(),
            excluded => format!("; {} excluded by policy", group_thousands(excluded)),
        };
        let Some(overall) = self.percent() else {
            return format!("no source files considered under the current policy{policy}");
        };
        let overall = format!(
            "{} of {} recognised files indexed ({overall:.1}%)",
            group_thousands(self.indexed),
            group_thousands(self.considered())
        );
        let (considered, indexed) = self.programming_totals();
        match self.programming_percent() {
            Some(percent) => format!(
                "{} of {} programming-language files indexed ({percent:.1}%); {overall} overall{policy}",
                group_thousands(indexed),
                group_thousands(considered)
            ),
            None if self.programming_policy_totals().0 == 0 => {
                format!("no programming-language files discovered; {overall}{policy}")
            }
            None => format!(
                "no programming-language files considered under the current policy; {overall}{policy}"
            ),
        }
    }

    /// One line: the headline, the policy exclusions with their top directories and
    /// governing setting, what else was skipped, and what the ratio cannot see.
    pub fn summary_line(&self) -> String {
        if self.discovered == 0 {
            return "no source files discovered".into();
        }
        let mut line = self.headline();
        // The headline ends with the policy count exactly when there is detail to add.
        if let Some(detail) = self.policy_exclusion_detail() {
            line.push_str(&format!(" ({detail})"));
        }
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
        let source_dirs = self.pruned_source_dirs();
        if self.pruned_source_files() > 0 {
            let count_of = |reason| {
                source_dirs
                    .iter()
                    .filter(|dir| dir.reason == reason)
                    .count()
            };
            let all = source_dirs.len();
            let kind = if count_of(PruneReason::UndeclaredBuildDir) == all {
                "undeclared build"
            } else if count_of(PruneReason::Submodule) == all {
                "nested repository"
            } else if count_of(PruneReason::MsbuildOutput) == all {
                "MSBuild output"
            } else {
                "pruned"
            };
            caveats.push(format!(
                "{} git-tracked source {} not indexed under {kind} {}: {}",
                group_thousands(self.pruned_source_files()),
                if self.pruned_source_files() == 1 {
                    "file"
                } else {
                    "files"
                },
                if source_dirs.len() == 1 {
                    "directory"
                } else {
                    "directories"
                },
                name_some(
                    source_dirs.iter().map(|dir| format!("{}/", dir.path)),
                    source_dirs.len()
                )
            ));
        }
        if self.pruned_dirs > 0 {
            let noun = if self.pruned_dirs == 1 {
                "directory"
            } else {
                "directories"
            };
            if self.pruned.is_empty() && self.pruned_unlisted == 0 {
                // Written before pruned paths were recorded: the count is all there is.
                caveats.push(format!(
                    "{} {noun} pruned by name (contents not counted)",
                    group_thousands(self.pruned_dirs)
                ));
            } else {
                // A submodule is another repository, not output: the lead names it only when one
                // is listed, and among build directories each one says which it is.
                let submodules = self
                    .pruned
                    .iter()
                    .filter(|dir| dir.reason == PruneReason::Submodule)
                    .count();
                let (kinds, mark_submodules) = match submodules {
                    0 => ("build output or dependencies", false),
                    1 if self.pruned_dirs == 1 => ("a submodule", false),
                    all if all == self.pruned.len() && self.pruned_unlisted == 0 => {
                        ("submodules", false)
                    }
                    _ => ("build output, dependencies or submodules", true),
                };
                caveats.push(format!(
                    "{} {noun} pruned as {kinds}: {}",
                    group_thousands(self.pruned_dirs),
                    name_some(
                        self.pruned.iter().map(|dir| {
                            if !(mark_submodules && dir.reason == PruneReason::Submodule) {
                                return dir.label();
                            }
                            // Keep the tracked count: on a submodule it says the `.git` is stray.
                            match dir.tracked_source_files {
                                Some(count) if count > 0 => format!(
                                    "{}/ (submodule, {} tracked source {})",
                                    dir.path,
                                    group_thousands(count),
                                    if count == 1 { "file" } else { "files" }
                                ),
                                _ => format!("{}/ (submodule)", dir.path),
                            }
                        }),
                        self.pruned_dirs
                    )
                ));
            }
        }
        // Indexed is not the same as read whole: a file recovery kept little of still counts.
        for (language, entry) in &self.by_language {
            if entry.parsed_with_errors == 0 {
                continue;
            }
            let files = if entry.parsed_with_errors == 1 {
                "file"
            } else {
                "files"
            };
            let mut caveat = format!(
                "{} {language} {files} parsed with syntax errors (their symbols are what error recovery kept",
                group_thousands(entry.parsed_with_errors)
            );
            if entry.pattern_fallback > 0 {
                caveat.push_str(&format!(
                    "; {} named by pattern only",
                    group_thousands(entry.pattern_fallback)
                ));
            }
            caveat.push(')');
            caveats.push(caveat);
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
        "rust" | "java" | "type_script" | "java_script" | "python" | "go" | "c_sharp" | "sql"
    )
}

/// The first three of `names`, then how many of `total` were left out: `target/, dist/ and
/// 4 more`. A summary line names enough to act on; the full list is in the coverage record.
fn name_some(names: impl Iterator<Item = String>, total: usize) -> String {
    let shown = names.take(3).collect::<Vec<_>>();
    let rest = total.saturating_sub(shown.len());
    match (shown.is_empty(), rest) {
        (true, _) => format!("{} unlisted", group_thousands(rest)),
        (false, 0) => shown.join(", "),
        (false, _) => format!("{} and {} more", shown.join(", "), group_thousands(rest)),
    }
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

/// Where a quality note came from. The kind is assigned by the producer, never
/// recovered from the message text, and is what `repo_status` groups counts by.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum QualityNoteKind {
    /// Discovery skipped paths; the counts are in `skip_counts`.
    Discovery,
    /// SCIP import, generation, or availability.
    Scip,
    /// Exact reference evidence is unavailable, so impact and tests are heuristic.
    ExactReferences,
    /// What the chosen index mode does not do.
    IndexMode,
    /// One import the resolver could not settle with certainty.
    ImportResolverCaveat,
    /// The resolver stopped reading manifests or aliases at a cap.
    ImportResolverCap,
    /// One token the symbol registry resolved ambiguously.
    SymbolRegistryCaveat,
    /// One token the symbol registry could not resolve at all.
    SymbolRegistryUnresolved,
    /// Relationship resolution suppressed authoritative emission: a candidate cap was hit, or a
    /// Rust package has no crate root the module tree can place.
    RelationshipResolution,
    /// The git history scan skipped commits whose patch it could not read.
    GitHistory,
    /// A test runner's configuration changes which callables it discovers, so test targets
    /// under it are judged by the test-path rule instead of the runner's default rules.
    TestDiscovery,
    /// A note from a manifest written before notes carried a kind.
    Unclassified,
}

/// One quality note: a kind for grouping and the human-readable message. Manifests
/// written before kinds were recorded stored bare strings; those deserialize as
/// `Unclassified`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
pub struct QualityNote {
    pub kind: QualityNoteKind,
    pub message: String,
}

impl QualityNote {
    pub fn new(kind: QualityNoteKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl<'de> Deserialize<'de> for QualityNote {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Legacy(String),
            Typed {
                kind: QualityNoteKind,
                message: String,
            },
        }
        Ok(match Stored::deserialize(deserializer)? {
            Stored::Legacy(message) => Self::new(QualityNoteKind::Unclassified, message),
            Stored::Typed { kind, message } => Self { kind, message },
        })
    }
}

/// How much of the manifest's per-item lists a status payload carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusDetail {
    /// Counts by kind or reason plus a bounded sample; the default.
    #[default]
    Summary,
    /// Every quality note and skipped path, as the manifest stores them.
    Full,
}

impl StatusDetail {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "summary" => Some(Self::Summary),
            "full" => Some(Self::Full),
            _ => None,
        }
    }
}

/// How many notes or paths a summary carries verbatim. On a 380-file repository the
/// full lists were 1.4 MB of a 1.5 MB status payload, most of it one caveat repeated
/// per symbol; the counts say the same thing and the sample shows what one looks like.
pub const STATUS_SAMPLE_LIMIT: usize = 20;

/// `quality_notes` as `repo_status` and `ok --json status` carry it by default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct QualityNotesSummary {
    pub total: usize,
    pub by_kind: BTreeMap<QualityNoteKind, usize>,
    /// Round-robin across kinds so a kind with one note is never crowded out by a
    /// kind with thousands.
    pub sample: Vec<QualityNote>,
}

/// `skipped_paths` as `repo_status` and `ok --json status` carry it by default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkippedPathsSummary {
    pub total: usize,
    pub by_reason: BTreeMap<SkipReason, usize>,
    /// Round-robin across reasons, in stored order within each reason.
    pub sample: Vec<SkippedPath>,
}

/// Up to `limit` items, one from each group in turn, preserving order within a group.
fn round_robin_sample<'a, T: Clone + 'a, K: Ord>(
    items: impl IntoIterator<Item = &'a T>,
    key: impl Fn(&T) -> K,
    limit: usize,
) -> Vec<T> {
    let mut groups: BTreeMap<K, Vec<&T>> = BTreeMap::new();
    for item in items {
        groups.entry(key(item)).or_default().push(item);
    }
    let mut cursors = groups
        .values()
        .map(|group| group.iter())
        .collect::<Vec<_>>();
    let mut sample = Vec::with_capacity(limit.min(cursors.len()));
    'rounds: loop {
        let mut progressed = false;
        for cursor in &mut cursors {
            if sample.len() >= limit {
                break 'rounds;
            }
            if let Some(item) = cursor.next() {
                sample.push((*item).clone());
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
    }
    sample
}

impl IndexQuality {
    pub fn quality_notes_summary(&self, sample_limit: usize) -> QualityNotesSummary {
        let mut by_kind = BTreeMap::new();
        for note in &self.quality_notes {
            *by_kind.entry(note.kind).or_default() += 1;
        }
        QualityNotesSummary {
            total: self.quality_notes.len(),
            by_kind,
            sample: round_robin_sample(&self.quality_notes, |note| note.kind, sample_limit),
        }
    }

    pub fn skipped_paths_summary(&self, sample_limit: usize) -> SkippedPathsSummary {
        let mut by_reason = BTreeMap::new();
        for skipped in &self.skipped_paths {
            *by_reason.entry(skipped.reason).or_default() += 1;
        }
        SkippedPathsSummary {
            total: self.skipped_paths.len(),
            by_reason,
            sample: round_robin_sample(&self.skipped_paths, |path| path.reason, sample_limit),
        }
    }
}

impl IndexManifest {
    /// The manifest as a status payload: the whole record, with `quality.quality_notes`
    /// and `quality.skipped_paths` replaced by their summaries unless `Full` is asked
    /// for. Both `ok --json status` and MCP `repo_status` start from this so the two
    /// cannot drift; the manifest itself keeps the full lists.
    /// Written before secret-value redaction existed: such an index stored data, config, and
    /// prose files as read, so replacing it must also drop those bytes from the database.
    pub fn predates_secret_redaction(&self) -> bool {
        self.quality.redacted_files.is_none()
    }

    /// Whether this index still owes the one-time clearing of bytes stored before redaction:
    /// it predates redaction, or a previous run's attempt did not finish. Publishing a
    /// manifest that records the work as outstanding is what makes the next run retry it.
    pub fn needs_pre_redaction_compaction(&self) -> bool {
        self.predates_secret_redaction() || self.quality.pending_pre_redaction_compaction
    }

    pub fn status_value(&self, detail: StatusDetail) -> serde_json::Result<serde_json::Value> {
        let mut value = serde_json::to_value(self)?;
        if detail == StatusDetail::Full {
            return Ok(value);
        }
        if let Some(quality) = value
            .get_mut("quality")
            .and_then(serde_json::Value::as_object_mut)
        {
            quality.insert(
                "quality_notes".into(),
                serde_json::to_value(self.quality.quality_notes_summary(STATUS_SAMPLE_LIMIT))?,
            );
            quality.insert(
                "skipped_paths".into(),
                serde_json::to_value(self.quality.skipped_paths_summary(STATUS_SAMPLE_LIMIT))?,
            );
            if let Some(coverage) = &self.quality.coverage {
                quality.insert(
                    "coverage".into(),
                    serde_json::to_value(coverage.status_view())?,
                );
            }
        }
        Ok(value)
    }
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
    /// Indexed test targets that can stand as validation evidence.
    pub test_count: usize,
    /// Indexed test targets left out of `test_count` because they cannot stand as validation
    /// evidence, by reason. Kept apart so a repository whose tests are all skipped reads as
    /// such rather than as one with no tests. `None` on manifests written before it was
    /// recorded, so a reader can tell "not recorded" from "nothing excluded" (`{}`) and say
    /// that a re-index is needed rather than guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_test_targets: Option<BTreeMap<TestExclusionReason, usize>>,
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
    /// Files indexed with at least one secret-like value replaced by `[REDACTED]` before
    /// storage: data, config and prose files, and source holding a private-key PEM block
    /// (`docs/security-model.md`). `null` on manifests written before redaction existed: those
    /// indexes stored such files' values as read. Serialized
    /// even when absent, so a client reading `quality.redacted_files ?? 0` cannot render an
    /// unredacted index as one with nothing to redact.
    #[serde(default)]
    pub redacted_files: Option<usize>,
    /// Bytes an index written before redaction stored as read are still to be cleared from the
    /// database's free pages, its write-ahead log, and the semantic vector store. Set when a
    /// run detects such an index and cleared only once that work succeeds, so a blocked pass is
    /// retried by the next run instead of being reported as done.
    #[serde(default)]
    pub pending_pre_redaction_compaction: bool,
    /// Rows this or an earlier run deleted may still be readable in the database file or its
    /// write-ahead log: the compaction after a path the policy excludes lost its rows, or the
    /// truncating checkpoint every run ends with, did not complete. Set before the manifest
    /// is published and cleared once that work succeeds, so a crash or a blocked checkpoint
    /// stays reported and the next `ok index` retries it (#553). Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending_deleted_content_clearing: bool,
    /// Text of a path the index no longer holds may remain in the stores derived from it, the
    /// semantic vector store and stored context handles, because removing it from them failed
    /// (#564). Kept apart from `pending_deleted_content_clearing` so that retrying one never
    /// costs the other: a failed prune does not make the next run compact the database, and a
    /// pending compaction does not make every `ok watch` event prune (#585). Carried to each
    /// manifest a writer publishes until a prune succeeds. Omitted when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending_derived_store_pruning: bool,
    /// Every note, typed by producer. Status payloads summarize this list; see
    /// `IndexManifest::status_value`.
    #[serde(default)]
    pub quality_notes: Vec<QualityNote>,
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
    /// One sentence per clause of the graph query language, as the parser accepts it.
    #[serde(default)]
    pub syntax: Vec<String>,
    #[serde(default)]
    pub examples: Vec<GraphQueryExample>,
    /// Forms the parser or executor rejects, each with the accepted way to ask instead.
    #[serde(default)]
    pub unsupported: Vec<UnsupportedGraphQueryForm>,
    #[serde(default)]
    pub optional_evidence: Vec<OptionalEvidenceSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caveats: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct GraphQueryExample {
    pub query: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnsupportedGraphQueryForm {
    pub form: String,
    pub example: String,
    pub alternative: String,
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
    /// Source of the indexed symbol occurrence this result was produced from. Consumers read
    /// exact-reference provenance here, never from `match_reason`: a deduplicated result takes
    /// the prose of whichever duplicate scored higher, and a lexical hit on the same range
    /// would erase it. Absent on results that are not symbol occurrences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_reference_provenance: Option<EvidenceSourceType>,
}

impl SearchResult {
    /// The exact-reference source this result carries. A provenance that is not an exact
    /// reference source does not make the result exact.
    pub fn exact_reference_source(&self) -> Option<&EvidenceSourceType> {
        self.exact_reference_provenance
            .as_ref()
            .filter(|source| source.is_exact_reference_source())
    }

    pub fn is_exact_reference(&self) -> bool {
        self.exact_reference_source().is_some()
    }

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
    /// Capped per rule so a plan does not repeat a long ref list for every file; see
    /// `evidence_refs_omitted` for how many refs the cap left out.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    /// Refs this rule could have cited past the cap on `evidence_refs`. Counted, not listed,
    /// and never folded into the ref list as text.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub evidence_refs_omitted: usize,
    #[serde(default)]
    pub symbols: Vec<String>,
}

fn is_zero_count(count: &usize) -> bool {
    *count == 0
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
    /// Capped; see `direct_impacts_omitted` for how many the cap left out.
    pub direct_impacts: Vec<SearchResult>,
    /// Capped; see `indirect_impacts_omitted` for how many the cap left out.
    pub indirect_impacts: Vec<SearchResult>,
    /// Direct impacts found but cut by the cap on `direct_impacts`. Counted, not listed, so a
    /// short list is not read as the whole blast radius.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub direct_impacts_omitted: usize,
    /// Indirect impacts found but cut by the cap on `indirect_impacts`.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub indirect_impacts_omitted: usize,
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
    /// Proven dependents read but cut by the cap on `proven_impact`. The cap never cuts a
    /// dependent of a symbol the caller said the change touches; among the rest it keeps
    /// dependents in other files ahead of the changed file's own. Counted, not listed, so a
    /// capped list is not read as every proven dependent. A lower bound when
    /// `relationship_impact_reads` says a read stopped short.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub proven_impact_omitted: usize,
    /// Of the dependent files a `proven_impact_omitted` entry is in, those `proven_impact` does
    /// not name at all: whole files a reader of the list would not know are proven dependents.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub proven_impact_omitted_files: usize,
    /// Dependents reached only through heuristic or corroborating relationships. Presented as
    /// possibilities, never as structural facts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub possible_impact: Vec<RelationshipImpact>,
    /// Possible dependents read but cut by the cap on `possible_impact`, which keeps dependents
    /// of the symbols the change touches first and, among the rest, dependents in other files
    /// ahead of the changed file's own. Counted, not listed, so a capped list is not read as every
    /// possibility. A lower bound when `relationship_impact_reads` says a read stopped short.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub possible_impact_omitted: usize,
    /// Of the dependent files a `possible_impact_omitted` entry is in, those `possible_impact`
    /// does not name at all.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub possible_impact_omitted_files: usize,
    /// What the relationship reads behind `proven_impact` and `possible_impact` left unread, in
    /// prose: changed symbols with dependents whose edges were not read, an inbound read that
    /// stopped at its limit, a list cut by its cap. Empty when nothing was left out, so an empty
    /// list beside zero omitted counts means every dependent read is listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationship_impact_caveats: Vec<String>,
    /// The same reads as numbers a consumer can act on. Absent when no relationship read ran (no
    /// graph store, or a target the index does not hold).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship_impact_reads: Option<RelationshipImpactReads>,
}

/// How far impact's relationship reads went for one changed file.
///
/// Impact reads the inbound edges of the changed file and of its symbols, most important first:
/// the symbols the change touches, then public ones, then those with the most inbound edges. A
/// symbol with no inbound edge that can carry impact needs no read. Each read takes a bounded
/// window of one edge type into one node, strongest edges first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RelationshipImpactReads {
    /// Symbols the changed file defines.
    pub symbols_total: usize,
    /// Of those, the symbols the caller said the change touches (by changed line or by name).
    /// Zero when the caller named no change, and impact then ranks the file's symbols alone.
    #[serde(default)]
    pub symbols_touched: usize,
    /// Symbols whose inbound edges impact read.
    pub symbols_read: usize,
    /// Symbols left unread that have at least one inbound edge that can carry impact, so their
    /// dependents are missing from both lists. `None` when the graph store cannot count edges:
    /// then any unread symbol may have dependents.
    pub symbols_unread_with_dependents: Option<usize>,
    /// Inbound edges that can carry impact and were not read: every edge of an unread symbol,
    /// and those past the window of a read that stopped at its limit. `None` when the graph
    /// store cannot count edges.
    pub edges_unread: Option<usize>,
    /// Of `edges_unread`, the proven ones: proven dependents missing from the report, past any
    /// cap. `None` when the graph store cannot count edges or cannot tell which are proven.
    #[serde(default)]
    pub proven_edges_unread: Option<usize>,
    /// Reads (one edge type into one node) that held more edges than their window.
    pub windows_at_limit: usize,
    /// Of those, reads whose first unread edge is proven, or that cannot tell. Windows keep
    /// proven edges first, so when this is zero every edge such a read left out is heuristic:
    /// every proven dependent of the nodes read was read, and is in `proven_impact` or counted in
    /// `proven_impact_omitted`.
    pub windows_cutting_proven: usize,
    /// Reads whose window was widened past the usual edge limit because the store counted more
    /// proven edges in it than that limit holds. A widened read takes every proven edge up to a
    /// bound, so it is at its limit, or cuts a proven edge, only past that bound.
    #[serde(default)]
    pub windows_widened: usize,
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
    /// Plausible validation targets the plan's bound on `validation` left out. Only the bound's
    /// drops are counted, not targets the plausibility predicate or the per-file suite preference
    /// removed; a line in `risk.reasons` states the same count, with no effect on confidence.
    #[serde(default, skip_serializing_if = "is_zero_count")]
    pub validation_omitted: usize,
    /// The ids behind `validation_omitted`, so `ok verify` can say a recommendation was left out
    /// by the plan's bound rather than implying the plan never considered it. Bounded by the
    /// upstream candidate limits: at most 50 candidates (the context pack's ten validation tests
    /// plus eight per source path for five paths) less the eight planned, so at most 42 ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_omitted_ids: Vec<String>,
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
            exact_reference_provenance: None,
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
        count_resolution_notes, named_anchors, negative_evidence_scope,
        negative_evidence_signal_count, reconcile_score_breakdown, score_component_total,
        task_relevance_score, unmatched_named_anchors, weak_named_anchors, Confidence,
        ConfidenceBreakdown, ConfidenceSignalInput, CoverageInput, EdgeId, Evidence,
        EvidenceQuality, EvidenceSourceType, FileRange, GitChangeKind, GitCommitId,
        GitCommitRecord, GitFileTouch, GitSymbolTouch, GraphEdge, GraphEdgeType, GraphNode,
        GraphNodeType, HistoryRecordId, HistorySnapshot, HistorySummary, IndexCoverage,
        IndexManifest, IndexMode, IndexQuality, Language, LineRange, NegativeEvidence, NodeId,
        Owner, PathInterner, PruneReason, PrunedDir, QualityNote, QualityNoteKind, Repository,
        RepositoryId, ScopeId, ScoreComponent, SearchResult, SharedPath, SharedStr, SkipReason,
        SkipSource, SkippedPath, SourceRange, StatusDetail, StringInterner, Symbol, SymbolId,
        Visibility, COVERAGE_SELECTED_LANGUAGE_SIGNAL, HISTORY_SCHEMA_VERSION, PRUNED_DIRS_LISTED,
        STATUS_SAMPLE_LIMIT, UNREADABLE_COVERAGE_CAVEAT, UNRECORDED_COVERAGE_CAVEAT,
    };
    use chrono::{TimeZone, Utc};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn shared_path_serializes_exactly_as_a_pathbuf() {
        let raw = "extensions/lang-rules/src/main/java/com/acme/Script.java";
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
                QualityNote::new(
                    QualityNoteKind::ImportResolverCaveat,
                    "import resolver caveat in src/lib.rs for `crate::missing`: unresolved import",
                ),
                QualityNote::new(
                    QualityNoteKind::SymbolRegistryUnresolved,
                    "symbol registry unresolved `documentation_word` in chunk abc",
                ),
                QualityNote::new(
                    QualityNoteKind::Unclassified,
                    "ambiguous wording in a non-resolver diagnostic",
                ),
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
    fn quality_note_deserializes_legacy_strings_as_unclassified() {
        let notes: Vec<QualityNote> = serde_json::from_str(
            r#"["SCIP disabled; heuristics", {"kind": "scip", "message": "typed"}]"#,
        )
        .unwrap();
        assert_eq!(
            notes,
            vec![
                QualityNote::new(QualityNoteKind::Unclassified, "SCIP disabled; heuristics"),
                QualityNote::new(QualityNoteKind::Scip, "typed"),
            ]
        );
        let json = serde_json::to_value(&notes[1]).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"kind": "scip", "message": "typed"})
        );
    }

    #[test]
    fn status_summary_bounds_notes_and_paths_and_samples_across_groups() {
        let mut quality = IndexQuality::default();
        for index in 0..1_000 {
            quality.quality_notes.push(QualityNote::new(
                QualityNoteKind::SymbolRegistryCaveat,
                format!("symbol registry caveat for `t{index}`"),
            ));
        }
        quality.quality_notes.push(QualityNote::new(
            QualityNoteKind::Scip,
            "SCIP disabled; symbol references use tree-sitter/import heuristics",
        ));
        for index in 0..500 {
            quality.skipped_paths.push(SkippedPath {
                path: PathBuf::from(format!(".claude/w{index}.rs")),
                reason: SkipReason::Hidden,
                source: SkipSource::HiddenPolicy,
                safe_to_show: true,
            });
        }
        quality.skipped_paths.push(SkippedPath {
            path: PathBuf::from("big.rs"),
            reason: SkipReason::TooLarge,
            source: SkipSource::SizeLimit,
            safe_to_show: true,
        });

        let notes = quality.quality_notes_summary(STATUS_SAMPLE_LIMIT);
        assert_eq!(notes.total, 1_001);
        assert_eq!(notes.by_kind[&QualityNoteKind::SymbolRegistryCaveat], 1_000);
        assert_eq!(notes.by_kind[&QualityNoteKind::Scip], 1);
        assert_eq!(notes.sample.len(), STATUS_SAMPLE_LIMIT);
        // The lone SCIP note is in the sample even though it is one in a thousand.
        assert!(notes
            .sample
            .iter()
            .any(|note| note.kind == QualityNoteKind::Scip));

        let paths = quality.skipped_paths_summary(STATUS_SAMPLE_LIMIT);
        assert_eq!(paths.total, 501);
        assert_eq!(paths.by_reason[&SkipReason::Hidden], 500);
        assert_eq!(paths.by_reason[&SkipReason::TooLarge], 1);
        assert_eq!(paths.sample.len(), STATUS_SAMPLE_LIMIT);
        assert!(paths
            .sample
            .iter()
            .any(|path| path.reason == SkipReason::TooLarge));

        let manifest = IndexManifest {
            repository: Repository {
                id: RepositoryId::new("repo"),
                name: "repo".into(),
                root: PathBuf::from("."),
                branch: None,
                commit: None,
                indexed_at: None,
            },
            file_count: 0,
            symbol_count: 0,
            chunk_count: 0,
            indexed_at: Utc::now(),
            schema_version: 2,
            analysis_semantics: None,
            index_mode: IndexMode::Full,
            phase_reports: Vec::new(),
            quality,
            snapshot: None,
        };
        let summary = manifest.status_value(StatusDetail::Summary).unwrap();
        assert_eq!(summary["quality"]["quality_notes"]["total"], 1_001);
        assert_eq!(summary["quality"]["skipped_paths"]["total"], 501);
        let full = manifest.status_value(StatusDetail::Full).unwrap();
        assert_eq!(
            full["quality"]["quality_notes"].as_array().unwrap().len(),
            1_001
        );
        assert_eq!(
            full["quality"]["skipped_paths"].as_array().unwrap().len(),
            501
        );
        assert_eq!(
            full["quality"]["skip_counts"],
            summary["quality"]["skip_counts"]
        );

        // Pruned directories: every one in the stored record and the full payload, the
        // first fifty in a summary with the rest counted.
        let mut manifest = manifest;
        let mut coverage = IndexCoverage::default();
        coverage.record_pruned_dirs(
            (0..PRUNED_DIRS_LISTED + 10)
                .map(|index| PrunedDir {
                    path: format!("p{index:03}/dist"),
                    reason: PruneReason::BuildOutput,
                    tracked_source_files: Some(0),
                })
                .collect(),
            0,
        );
        manifest.quality.coverage = Some(coverage);
        let summary = manifest.status_value(StatusDetail::Summary).unwrap();
        let summary_coverage = &summary["quality"]["coverage"];
        assert_eq!(
            summary_coverage["pruned"].as_array().unwrap().len(),
            PRUNED_DIRS_LISTED
        );
        assert_eq!(summary_coverage["pruned_unlisted"], 10);
        let full = manifest.status_value(StatusDetail::Full).unwrap();
        assert_eq!(
            full["quality"]["coverage"]["pruned"]
                .as_array()
                .unwrap()
                .len(),
            PRUNED_DIRS_LISTED + 10
        );
        assert!(full["quality"]["coverage"].get("pruned_unlisted").is_none());
    }

    #[test]
    fn coverage_ratio_excludes_policy_skips_from_the_denominator() {
        let mut coverage = IndexCoverage::default();
        for _ in 0..241 {
            coverage.record_discovered(&Language::Rust);
            coverage.record_indexed(&Language::Rust, false);
        }
        for _ in 0..1_485 {
            coverage.record_discovered(&Language::Rust);
            coverage.record_skipped(&Language::Rust, SkipReason::Hidden);
            coverage.record_policy_exclusion(
                &Language::Rust,
                SkipSource::HiddenPolicy,
                Some(".claude"),
            );
        }
        coverage.record_discovered(&Language::Rust);
        coverage.record_skipped(&Language::Rust, SkipReason::TooLarge);

        assert_eq!(coverage.discovered, 1_727);
        assert_eq!(coverage.excluded_by_policy(), 1_485);
        assert_eq!(coverage.considered(), 242);
        assert_eq!(coverage.programming_totals(), (242, 241));
        assert!(!coverage.below_warn_threshold());
        assert!(coverage.languages_below_warn_threshold().is_empty());
        assert_eq!(
            coverage.top_skip_reasons(3),
            vec![(SkipReason::TooLarge, 1)]
        );
        assert_eq!(
            coverage.policy_skip_reasons(),
            vec![(SkipReason::Hidden, 1_485)]
        );
        assert_eq!(
            coverage.dominant_policy_source(),
            Some((SkipSource::HiddenPolicy, 1_485))
        );
        let line = coverage.summary_line();
        assert!(
            line.starts_with("241 of 242 programming-language files indexed (99.6%)"),
            "{line}"
        );
        assert!(
            line.contains("1,485 excluded by policy (1,485 hidden; 1,485 under .claude/; `[security] allow_hidden_files` governs the largest share)"),
            "{line}"
        );
        assert!(line.ends_with("; skipped: 1 too-large"), "{line}");

        // A manifest written before the source map existed still reads the same ratio.
        let legacy: IndexCoverage = serde_json::from_value(serde_json::json!({
            "discovered": 10, "indexed": 2, "skipped": {"hidden": 8},
            "by_language": {"rust": {"discovered": 10, "indexed": 2, "skipped": {"hidden": 8}}}
        }))
        .unwrap();
        assert_eq!(legacy.percent(), Some(100.0));
        assert!(legacy.dominant_policy_source().is_none());
        assert_eq!(
            legacy.policy_exclusion_detail().as_deref(),
            Some("8 hidden")
        );
    }

    /// `considered` indexed Rust files beside `git_ignored` files git ignore rules excluded.
    fn git_ignored_rust(considered: usize, git_ignored: usize) -> IndexCoverage {
        let mut coverage = IndexCoverage::default();
        for _ in 0..considered {
            coverage.record_discovered(&Language::Rust);
            coverage.record_indexed(&Language::Rust, false);
        }
        for _ in 0..git_ignored {
            coverage.record_discovered(&Language::Rust);
            coverage.record_skipped(&Language::Rust, SkipReason::Ignored);
            coverage.record_policy_exclusion(&Language::Rust, SkipSource::GitIgnore, Some("src"));
        }
        coverage
    }

    /// `indexed` Rust files beside `too_large` files the size limit dropped.
    fn too_large_rust(indexed: usize, too_large: usize) -> IndexCoverage {
        let mut coverage = git_ignored_rust(indexed, 0);
        for _ in 0..too_large {
            coverage.record_discovered(&Language::Rust);
            coverage.record_skipped(&Language::Rust, SkipReason::TooLarge);
        }
        coverage
    }

    #[test]
    fn coverage_gaps_apply_the_doctor_thresholds_and_never_fire_on_complete_coverage() {
        // Complete coverage, and a repository with nothing discovered.
        assert!(git_ignored_rust(40, 0).gaps().is_empty());
        assert!(IndexCoverage::default().gaps().is_empty());

        // Git ignore rules: at least 20 files, and more than the language has considered.
        assert!(git_ignored_rust(2, 19).gaps().is_empty());
        assert!(git_ignored_rust(20, 20).gaps().is_empty());
        let gaps = git_ignored_rust(2, 25).gaps();
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        let gap = &gaps[0];
        assert_eq!(gap.cause, super::CoverageGapCause::GitIgnore);
        assert_eq!(
            (gap.language.as_str(), gap.missing_files, gap.language_files),
            ("rust", 25, 27)
        );
        assert!(gap.is_majority());
        assert_eq!(gap.evidence_id(), "coverage:rust:git_ignore");
        assert_eq!(
            gap.summary(),
            "rust (25 of 27 files, git-ignore: src/ (25 unclassified))"
        );
        assert_eq!(
            gap.caveat(),
            "index coverage: 25 of 27 rust source files (92.6%) are not indexed (git-ignore: src/ (25 unclassified)); an absence among them is not evidence"
        );
        let probe = gap.next_probe();
        assert!(
            probe.contains("`.git/info/exclude`") && probe.contains("`[index] exclude`"),
            "{probe}"
        );
        assert!(!probe.contains("does not exist"), "{probe}");

        // The index's own settings are intended exclusions while source remains considered.
        let mut hidden = git_ignored_rust(2, 0);
        for _ in 0..1_485 {
            hidden.record_discovered(&Language::Rust);
            hidden.record_skipped(&Language::Rust, SkipReason::Hidden);
            hidden.record_policy_exclusion(
                &Language::Rust,
                SkipSource::HiddenPolicy,
                Some(".claude"),
            );
        }
        assert!(hidden.gaps().is_empty());

        // ... unless policy left no programming-language source to consider at all.
        let mut emptied = IndexCoverage::default();
        for _ in 0..3 {
            emptied.record_discovered(&Language::Rust);
            emptied.record_skipped(&Language::Rust, SkipReason::Hidden);
            emptied.record_policy_exclusion(
                &Language::Rust,
                SkipSource::HiddenPolicy,
                Some(".claude"),
            );
        }
        let gaps = emptied.gaps();
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!(gaps[0].cause, super::CoverageGapCause::ExcludedByPolicy);
        assert_eq!((gaps[0].missing_files, gaps[0].language_files), (3, 3));
        assert_eq!(gaps[0].reason, "hidden-policy");
        assert_eq!(
            gaps[0].governing_setting.as_deref(),
            Some("`[security] allow_hidden_files`")
        );

        // Omissions the index did not intend: under 98% with at least 50 considered files, or
        // at least 20 missing regardless of percentage.
        assert!(too_large_rust(48, 1).gaps().is_empty());
        let gaps = too_large_rust(48, 2).gaps();
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!(gaps[0].cause, super::CoverageGapCause::Omitted);
        assert_eq!((gaps[0].missing_files, gaps[0].language_files), (2, 50));
        assert_eq!(gaps[0].reason, "too-large");
        assert_eq!(
            gaps[0].governing_setting.as_deref(),
            Some("`[index] max_file_size`")
        );
        assert!(!gaps[0].is_majority());
        assert_eq!(too_large_rust(10_000, 20).gaps().len(), 1);
        assert!(too_large_rust(10_000, 19).gaps().is_empty());

        // A manifest written before per-language sources were recorded: no git ignore gap.
        let legacy: IndexCoverage = serde_json::from_value(serde_json::json!({
            "discovered": 27, "indexed": 2, "skipped": {"ignored": 25},
            "by_language": {"rust": {"discovered": 27, "indexed": 2, "skipped": {"ignored": 25}}}
        }))
        .unwrap();
        assert!(legacy.gaps().is_empty());

        // The verdict serializes with its cause as the evidence-id key.
        let value = serde_json::to_value(&git_ignored_rust(2, 25).gaps()[0]).unwrap();
        assert_eq!(value["cause"], "git_ignore");
        assert_eq!(value["missing_files"], 25);
    }

    /// `considered` indexed Python files beside git-ignored ones: `dependencies` in the
    /// `site-packages` of `env`, a Python environment, and `generated` under `generated`, which nothing classifies.
    fn git_ignored_python(
        considered: usize,
        dependencies: usize,
        generated: usize,
    ) -> IndexCoverage {
        let mut coverage = IndexCoverage::default();
        for _ in 0..considered {
            coverage.record_discovered(&Language::Python);
            coverage.record_indexed(&Language::Python, false);
        }
        for (count, top_dir, dependency) in [
            (
                dependencies,
                "env",
                Some((
                    "env/lib/python3.12/site-packages",
                    super::DependencyEvidence::PythonEnvironment,
                )),
            ),
            (generated, "generated", None),
        ] {
            for _ in 0..count {
                coverage.record_discovered(&Language::Python);
                coverage.record_skipped(&Language::Python, SkipReason::Ignored);
                coverage.record_classified_policy_exclusion(
                    &Language::Python,
                    SkipSource::GitIgnore,
                    Some(top_dir),
                    dependency,
                );
            }
        }
        coverage
    }

    /// #503: a gap names the directories behind it with their class, and only files that may
    /// be first-party source decide the caps. Installed dependencies are reported, never hidden.
    #[test]
    fn coverage_gaps_name_their_directories_and_price_installed_dependencies_apart() {
        use super::{CoverageGap, CoverageGapDir, DependencyEvidence, ExcludedDirClass};

        let mixed = git_ignored_python(30, 340, 40).gaps();
        assert_eq!(mixed.len(), 1, "{mixed:?}");
        let gap = &mixed[0];
        assert_eq!((gap.missing_files, gap.language_files), (380, 410));
        assert_eq!(gap.dependency_files, 340);
        assert_eq!(
            gap.excluded_dirs,
            vec![
                CoverageGapDir {
                    path: "env/lib/python3.12/site-packages".into(),
                    files: 340,
                    class: ExcludedDirClass::Dependencies,
                    evidence: Some(DependencyEvidence::PythonEnvironment),
                },
                CoverageGapDir {
                    path: "generated".into(),
                    files: 40,
                    class: ExcludedDirClass::Unclassified,
                    evidence: None,
                },
            ]
        );
        assert_eq!(
            gap.summary(),
            "python (380 of 410 files, git-ignore: env/lib/python3.12/site-packages/ (340 dependencies: python-environment), generated/ (40 unclassified))"
        );
        assert_eq!(
            gap.caveat(),
            "index coverage: 380 of 410 python source files (92.7%) are not indexed (git-ignore: env/lib/python3.12/site-packages/ (340 dependencies: python-environment), generated/ (40 unclassified)); 340 are installed dependencies, so 40 of 70 possibly first-party files (57.1%) are missing; an absence among them is not evidence"
        );
        // 40 of the 70 files that may be source are missing: still a majority, so it caps.
        assert!(gap.is_majority() && gap.is_source_majority());
        let value = serde_json::to_value(gap).unwrap();
        assert_eq!(value["dependency_files"], 340);
        assert_eq!(value["excluded_dirs"][0]["class"], "dependencies");
        assert_eq!(value["excluded_dirs"][0]["evidence"], "python_environment");
        assert_eq!(value["excluded_dirs"][1]["class"], "unclassified");
        assert!(value["excluded_dirs"][1].get("evidence").is_none());

        // Only installed dependencies: still a reported majority gap, but nothing to cap on.
        let dependencies = git_ignored_python(30, 340, 0).gaps().remove(0);
        assert!(dependencies.is_majority());
        assert!(!dependencies.is_source_majority());
        assert_eq!(dependencies.source_missing_share(), 0.0);
        // Only an unclassified tree of the same size: priced exactly as before #503.
        let generated = git_ignored_python(30, 0, 340).gaps().remove(0);
        assert!(generated.is_source_majority());
        assert_eq!(generated.dependency_files, 0);
        assert_eq!(
            generated.excluded_dirs[0].class,
            ExcludedDirClass::Unclassified
        );

        let selection = ConfidenceSignalInput {
            primary_file_count: 3,
            evidence_count: 12,
            exact_reference_count: 2,
            validation_count: 3,
            validation_with_command_count: 3,
            allowed_file_count: 3,
            runtime_signal_count: 1,
            task_relevance: 1.0,
            primary_language_keys: vec!["python".into()],
            ..Default::default()
        };
        let price = |gap: &CoverageGap, unmatched: bool| {
            ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
                coverage: CoverageInput::Recorded(vec![gap.clone()]),
                named_anchor_count: usize::from(unmatched) * 2,
                unmatched_anchors: if unmatched {
                    vec!["LedgerEntry".into()]
                } else {
                    Vec::new()
                },
                ..selection.clone()
            })
        };
        let has_language_signal = |breakdown: &ConfidenceBreakdown| {
            breakdown
                .components
                .iter()
                .any(|component| component.signal == COVERAGE_SELECTED_LANGUAGE_SIGNAL)
        };
        for unmatched in [false, true] {
            let source = price(&generated, unmatched);
            let installed = price(&dependencies, unmatched);
            let both = price(gap, unmatched);
            // The source gap caps; the same share of installed packages does not.
            let cap = if unmatched { 0.50 } else { 0.74 };
            assert!(source.overall_score <= cap, "{source:?}");
            assert!(both.overall_score <= cap, "{both:?}");
            assert!(installed.overall_score > cap, "{installed:?}");
            assert!(
                !installed
                    .blockers
                    .iter()
                    .any(|blocker| blocker.contains("index")),
                "{installed:?}"
            );
            assert_eq!(has_language_signal(&source), !unmatched);
            assert!(!has_language_signal(&installed));
            // ... and is reported all the same: its caveat, naming the directory and class,
            // and the `index_coverage` component carrying its evidence id.
            assert!(
                installed.caveats.contains(&dependencies.caveat()),
                "{installed:?}"
            );
            assert!(dependencies.caveat().contains(
                "env/lib/python3.12/site-packages/ (340 dependencies: python-environment)"
            ));
            let component = installed
                .components
                .iter()
                .find(|component| component.signal == "index_coverage")
                .expect("index_coverage component");
            assert_eq!(component.evidence_ids, vec!["coverage:python:git_ignore"]);
        }
        let both = price(gap, false);
        assert!(both
            .blockers
            .iter()
            .any(|blocker| blocker.contains("generated/ (40 unclassified)")));

        // A caller recording one path both classified and unclassified gets unclassified.
        let mut disagreeing = IndexCoverage::default();
        for dependency in [Some(("vendor", super::DependencyEvidence::GoVendor)), None] {
            disagreeing.record_classified_policy_exclusion(
                &Language::Go,
                SkipSource::GitIgnore,
                Some("vendor"),
                dependency,
            );
        }
        let dirs = &disagreeing.policy_excluded_dirs_by_language["go"];
        assert_eq!(dirs["vendor"].dependency, None);
        assert_eq!(dirs["vendor"].by_source[&SkipSource::GitIgnore], 2);

        // A manifest written before directories were recorded per language: same verdict and
        // price as before, every missing file counted as source.
        let mut legacy = serde_json::to_value(git_ignored_python(30, 340, 0)).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("policy_excluded_dirs_by_language")
            .expect("the field is serialized when recorded");
        let legacy: IndexCoverage = serde_json::from_value(legacy).unwrap();
        let legacy_gap = legacy.gaps().remove(0);
        assert!(legacy_gap.excluded_dirs.is_empty());
        assert_eq!(legacy_gap.dependency_files, 0);
        assert!(legacy_gap.is_source_majority());
        assert_eq!(
            legacy_gap.summary(),
            "python (340 of 370 files, git-ignore)"
        );
    }

    #[test]
    fn coverage_gaps_are_always_reported_and_lower_confidence_only_beside_a_symptom() {
        let complete = ConfidenceSignalInput {
            primary_file_count: 3,
            evidence_count: 12,
            exact_reference_count: 2,
            validation_count: 3,
            validation_with_command_count: 3,
            negative_evidence_count: 0,
            allowed_file_count: 3,
            runtime_signal_count: 1,
            task_relevance: 1.0,
            ..Default::default()
        };
        let baseline = ConfidenceBreakdown::from_signals(complete.clone());
        assert_eq!(baseline.overall_enum, Confidence::Exact);
        assert!(baseline
            .components
            .iter()
            .all(|component| component.signal != "index_coverage"));
        let git_ignore_gap = git_ignored_rust(2, 25).gaps().remove(0);
        let omitted_gap = too_large_rust(48, 2).gaps().remove(0);

        // A gap alone is reported, not priced: `Exact` survives a minority and a majority gap.
        for gap in [omitted_gap.clone(), git_ignore_gap.clone()] {
            let reported = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
                coverage: CoverageInput::Recorded(vec![gap.clone()]),
                ..complete.clone()
            });
            assert_eq!(reported.overall_enum, Confidence::Exact, "{reported:?}");
            assert!((reported.overall_score - baseline.overall_score).abs() < f32::EPSILON);
            assert!(reported.blockers.is_empty(), "{reported:?}");
            assert!(reported.caveats.contains(&gap.caveat()), "{reported:?}");
            let component = reported
                .components
                .iter()
                .find(|component| component.signal == "index_coverage")
                .expect("index_coverage component");
            assert_eq!(component.evidence_ids, vec![gap.evidence_id()]);
            assert!(component.weight.abs() < f32::EPSILON);
            assert!(component.contribution.abs() < f32::EPSILON);
            assert!((f64::from(component.raw_value) - (1.0 - gap.missing_share())).abs() < 0.001);
        }

        // A task naming no identifier whose selection is in the language the index mostly
        // excluded: below `High`, with a blocker naming the gap.
        let rust_selection = ConfidenceSignalInput {
            coverage: CoverageInput::Recorded(vec![git_ignore_gap.clone()]),
            primary_language_keys: vec!["rust".into()],
            ..complete.clone()
        };
        let in_language = ConfidenceBreakdown::from_signals(rust_selection.clone());
        assert_eq!(in_language.overall_enum, Confidence::Medium);
        assert!(in_language.overall_score <= 0.74, "{in_language:?}");
        assert_eq!(
            in_language.blockers,
            vec![
                "the selected context is in a language the index mostly excluded: rust (25 of 27 files, git-ignore: src/ (25 unclassified))"
                    .to_string()
            ]
        );
        // A task whose every named identifier the selection spells is capped just the same: the
        // label follows what the index holds, not how the task was phrased. Before this, the
        // same repository and selection returned `Exact` for "fix `refresh_session_token`
        // expiry" and `Medium` for "fix the session expiry bug".
        let named_and_matched = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            named_anchor_count: 1,
            ..rust_selection.clone()
        });
        assert_eq!(named_and_matched.overall_enum, Confidence::Medium);
        assert!(
            named_and_matched.overall_score <= 0.74,
            "{named_and_matched:?}"
        );
        assert_eq!(named_and_matched.blockers, in_language.blockers);
        // The typed signal `ok preflight` branches on, emitted with the cap and not without it.
        let has_language_signal = |breakdown: &ConfidenceBreakdown| {
            breakdown
                .components
                .iter()
                .any(|component| component.signal == COVERAGE_SELECTED_LANGUAGE_SIGNAL)
        };
        assert!(has_language_signal(&in_language));
        assert!(has_language_signal(&named_and_matched));

        // Positive controls: with the gap below the majority share, or in a language the
        // selection does not use, the cap must not apply at all. If a bug stopped applying the
        // cap these stay green while the three assertions above turn red, and if a bug applied
        // it everywhere these turn red instead.
        for unchanged in [
            ConfidenceSignalInput {
                coverage: CoverageInput::Recorded(vec![omitted_gap]),
                ..rust_selection.clone()
            },
            ConfidenceSignalInput {
                primary_language_keys: vec!["python".into()],
                ..rust_selection.clone()
            },
        ] {
            let breakdown = ConfidenceBreakdown::from_signals(unchanged);
            assert_eq!(breakdown.overall_enum, Confidence::Exact, "{breakdown:?}");
            assert!(breakdown.blockers.is_empty(), "{breakdown:?}");
            assert!(
                breakdown
                    .components
                    .iter()
                    .all(|component| component.signal != COVERAGE_SELECTED_LANGUAGE_SIGNAL),
                "{breakdown:?}"
            );
        }

        // The absence symptom beside a majority gap: a named identifier the context does not
        // spell moves the label from Medium to Low, and the blocker names the exclusion.
        let partial = ConfidenceSignalInput {
            named_anchor_count: 2,
            unmatched_anchors: vec!["reticulate_splines".into()],
            negative_evidence_count: 1,
            task_relevance: 0.8,
            ..complete.clone()
        };
        let without_gap = ConfidenceBreakdown::from_signals(partial.clone());
        assert_eq!(without_gap.overall_enum, Confidence::Medium);
        let with_gap = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            coverage: CoverageInput::Recorded(vec![git_ignore_gap.clone()]),
            ..partial
        });
        assert_eq!(with_gap.overall_enum, Confidence::Low);
        assert!(with_gap.overall_score <= 0.50, "{with_gap:?}");
        assert!(with_gap.blockers.iter().any(|blocker| blocker
            == "the task may name code in source the index excluded: rust (25 of 27 files, git-ignore: src/ (25 unclassified))"));
        // No primary context is the same symptom.
        let empty = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            coverage: CoverageInput::Recorded(vec![git_ignore_gap.clone()]),
            ..Default::default()
        });
        assert!(empty
            .blockers
            .iter()
            .any(|blocker| blocker
                .starts_with("the task may name code in source the index excluded")));

        // A hyphenated word may be prose: no coverage cap or blocker, only the 0.60 count cap.
        let weak = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            unmatched_anchors: vec!["drive-by".into()],
            weak_anchors: vec!["drive-by".into()],
            negative_evidence_count: 1,
            task_relevance: 0.8,
            coverage: CoverageInput::Recorded(vec![git_ignore_gap.clone()]),
            ..complete.clone()
        });
        assert!(
            weak.overall_score > 0.50 && weak.overall_score <= 0.60,
            "{weak:?}"
        );
        assert!(weak
            .blockers
            .iter()
            .all(|blocker| !blocker.contains("index")));

        // Caveats attached after scoring: coverage caveats alone still do not cap; any other does.
        let mut late = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            coverage: CoverageInput::Recorded(vec![git_ignore_gap]),
            ..complete
        });
        late.add_caveats(std::iter::empty(), 2);
        assert_eq!(late.overall_enum, Confidence::Exact);
        late.add_caveats(["runtime evidence is unavailable".to_string()], 2);
        assert_eq!(late.overall_enum, Confidence::High);
        assert!(late.overall_score <= 0.94);
    }

    #[test]
    fn unrecorded_coverage_is_reported_and_caps_like_any_other_caveat() {
        let complete = ConfidenceSignalInput {
            primary_file_count: 3,
            evidence_count: 12,
            exact_reference_count: 2,
            validation_count: 3,
            validation_with_command_count: 3,
            allowed_file_count: 3,
            runtime_signal_count: 1,
            task_relevance: 1.0,
            ..Default::default()
        };
        // A recorded record with no gaps says nothing is missing: unchanged, and `Exact` stands.
        let recorded = ConfidenceBreakdown::from_signals(complete.clone());
        assert_eq!(recorded.overall_enum, Confidence::Exact);
        assert!(recorded.caveats.is_empty(), "{recorded:?}");

        // No record at all is a different fact, and it caps like any other caveat.
        let unavailable = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            coverage: CoverageInput::Unavailable,
            ..complete
        });
        assert_eq!(unavailable.overall_enum, Confidence::High);
        assert!(unavailable.overall_score <= 0.94, "{unavailable:?}");
        assert!(unavailable
            .caveats
            .contains(&UNRECORDED_COVERAGE_CAVEAT.to_string()));
        // It reads as the opposite of a gap caveat rather than as one more gap.
        assert!(!UNRECORDED_COVERAGE_CAVEAT.starts_with("index coverage: "));
        assert!(unavailable
            .components
            .iter()
            .all(|component| component.signal != "index_coverage"));

        let item = NegativeEvidence::for_coverage_input("task", &CoverageInput::Unavailable)
            .expect("unavailable coverage is reported");
        assert_eq!(item.scope, negative_evidence_scope::COVERAGE);
        assert!(!item.lowers_confidence());
        // The probe must not claim an imported snapshot carries no coverage: import republishes
        // the exporting index's manifest, so it carries whatever that recorded.
        let probe = item.suggested_next_probe.as_deref().expect("probe");
        assert!(
            probe.contains("cross-project index publishes no coverage record"),
            "{probe}"
        );
        assert!(
            probe.contains("imported snapshot carries whatever the exporting index recorded"),
            "{probe}"
        );

        // A read failure is a different fact from an index that published nothing.
        let unreadable = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            coverage: CoverageInput::Unreadable,
            ..Default::default()
        });
        assert!(unreadable
            .caveats
            .contains(&UNREADABLE_COVERAGE_CAVEAT.to_string()));
        assert_ne!(UNREADABLE_COVERAGE_CAVEAT, UNRECORDED_COVERAGE_CAVEAT);
        let unreadable_item =
            NegativeEvidence::for_coverage_input("task", &CoverageInput::Unreadable)
                .expect("unreadable coverage is reported");
        assert_eq!(unreadable_item.reason, UNREADABLE_COVERAGE_CAVEAT);
    }

    #[test]
    fn gaps_agree_with_the_doctor_coverage_predicates() {
        // `docs/ranking.md` says the confidence verdict and the doctor's check cannot disagree
        // about which languages qualify. They share these predicates; this holds them to it.
        for coverage in [
            git_ignored_rust(2, 25),
            git_ignored_rust(20, 20),
            git_ignored_rust(40, 0),
            too_large_rust(48, 2),
            too_large_rust(10_000, 20),
            IndexCoverage::default(),
        ] {
            let gaps = coverage.gaps();
            let git_ignored = coverage
                .languages_mostly_excluded_by(SkipSource::GitIgnore)
                .into_iter()
                .map(|(language, _, _)| language.to_owned())
                .collect::<std::collections::BTreeSet<_>>();
            let omitted = coverage
                .languages_below_warn_threshold()
                .into_iter()
                .map(|(language, _, _)| language.to_owned())
                .collect::<std::collections::BTreeSet<_>>();
            let from_gaps = |cause: super::CoverageGapCause| {
                gaps.iter()
                    .filter(|gap| gap.cause == cause)
                    .map(|gap| gap.language.clone())
                    .collect::<std::collections::BTreeSet<_>>()
            };
            assert_eq!(
                from_gaps(super::CoverageGapCause::GitIgnore),
                git_ignored,
                "{coverage:?}"
            );
            assert_eq!(
                from_gaps(super::CoverageGapCause::Omitted),
                omitted,
                "{coverage:?}"
            );
        }
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
            weak_anchors: Vec::new(),
            coverage: CoverageInput::default(),
            primary_language_keys: Vec::new(),
        };
        let all_missing = ConfidenceBreakdown::from_signals(base.clone());
        assert_eq!(all_missing.overall_enum, Confidence::Low);
        assert!(all_missing.overall_score <= 0.50, "{all_missing:?}");
        assert!(all_missing
            .blockers
            .iter()
            .any(|blocker| blocker.contains("FrobnicateWidgetManager")
                && blocker.contains("reticulate_splines")));

        // A partial miss is listed as `anchor` negative evidence, which the wiring counts:
        // the 0.60 cap and the count blocker apply, but not the 0.50 all-unmatched cap.
        let some_missing = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            unmatched_anchors: vec!["reticulate_splines".into()],
            negative_evidence_count: 1,
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
        assert!(some_missing.overall_score > 0.50 && some_missing.overall_score <= 0.60);
        assert_eq!(some_missing.overall_enum, Confidence::Medium);
    }

    #[test]
    fn late_caveats_cap_the_score_and_the_exact_label() {
        let mut exact = ConfidenceBreakdown {
            overall_enum: Confidence::Exact,
            overall_score: 1.0,
            ..Default::default()
        };
        exact.add_caveats(
            [
                "index is stale; re-index before relying on exact impact or verification gates"
                    .to_string(),
            ],
            2,
        );
        assert!(exact.overall_score <= 0.94);
        assert_eq!(exact.overall_enum, Confidence::High);

        // The gate still applies when the caller has no exact evidence at all.
        let mut ungated = ConfidenceBreakdown {
            overall_enum: Confidence::Exact,
            overall_score: 1.0,
            ..Default::default()
        };
        ungated.add_caveats(std::iter::empty(), 0);
        assert_eq!(ungated.overall_enum, Confidence::High);
    }

    #[test]
    fn exact_references_found_elsewhere_retract_the_manifest_caveat() {
        // A manifest without SCIP references: the flag is false and the caveat is present.
        let mut quality = EvidenceQuality::from_manifest(None);
        quality
            .caveats
            .push(super::EXACT_REFERENCE_UNAVAILABLE_CAVEAT.into());
        assert!(!quality.exact_reference_available);
        quality.record_exact_references(0);
        assert!(!quality.exact_reference_available);
        quality.record_exact_references(1);
        assert!(quality.exact_reference_available);
        assert!(!quality
            .caveats
            .iter()
            .any(|caveat| caveat.contains("exact") && caveat.contains("unavailable")));
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
            "coverage",
        ]
        .map(item);
        // Absent exact/runtime/validation/history evidence is priced by its own component, and
        // a coverage gap by the `index_coverage` caps.
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
        // A sentence-initial capital, an all-caps acronym, and prose are not identifiers.
        assert!(named_anchors("Reap the doctor's MCP probe child process").is_empty());
        assert!(named_anchors("Add HTTP 504 Gateway Timeout mapping").is_empty());
        assert_eq!(
            named_anchors("Fix FrobnicateWidgetManager"),
            vec!["FrobnicateWidgetManager".to_string()]
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
    fn hyphenated_lowercase_words_are_weak_anchors_unless_quoted() {
        let drive_by = "re-index after a drive-by edit";
        assert!(named_anchors(drive_by).is_empty());
        assert_eq!(weak_named_anchors(drive_by), vec!["re-index", "drive-by"]);
        let retries = "make retries best-effort instead of fail-closed";
        assert!(named_anchors(retries).is_empty());
        assert_eq!(
            weak_named_anchors(retries),
            vec!["best-effort", "fail-closed"]
        );
        assert_eq!(
            named_anchors("Fix FrobnicateWidgetManager"),
            vec!["FrobnicateWidgetManager"]
        );
        assert_eq!(named_anchors("rename get_or_load"), vec!["get_or_load"]);

        // Lowercase kebab-case is spelled like a compound word; quoting or code syntax names it.
        assert!(named_anchors("rename get-or-load").is_empty());
        assert_eq!(
            weak_named_anchors("rename get-or-load"),
            vec!["get-or-load"]
        );
        assert_eq!(named_anchors("rename `get-or-load`"), vec!["get-or-load"]);
        assert!(weak_named_anchors("rename `get-or-load`").is_empty());
        let quoted_once = "rename get-or-load to `get-or-load`";
        assert_eq!(named_anchors(quoted_once), vec!["get-or-load"]);
        assert!(weak_named_anchors(quoted_once).is_empty());

        // An odd number of backticks demotes nothing: a stray one would otherwise put a quoted
        // name in an unquoted span.
        let stray = "use the ` separator; rename `alpha-token-cache` for the alpha token";
        assert_eq!(named_anchors(stray), vec!["alpha-token-cache"]);
        assert!(weak_named_anchors(stray).is_empty());
        assert_eq!(named_anchors("rename `get-or-load"), vec!["get-or-load"]);

        // A flag, a path, a module, a scope, an assignment, or a `.` joining another word marks
        // a token as code; sentence punctuation does not.
        assert_eq!(
            named_anchors("add the --allow-network flag to the alpha token"),
            vec!["allow-network"]
        );
        assert_eq!(
            named_anchors("fix the alpha token in crates/open-kioku-cor/src/lib.rs"),
            vec!["open-kioku-cor"]
        );
        assert_eq!(
            named_anchors("fix the alpha token in `crates/open-kioku-cor/src/lib.rs`"),
            vec!["open-kioku-cor"]
        );
        assert_eq!(
            named_anchors("rename the drive-by.rs module"),
            vec!["drive-by"]
        );
        assert_eq!(
            named_anchors("import @acme/left-pad and ops::drive-by"),
            vec!["left-pad", "drive-by"]
        );
        assert_eq!(
            named_anchors("default to mode=fail-closed"),
            vec!["fail-closed"]
        );
        let sentence = "make retries best-effort instead of fail-closed.";
        assert!(named_anchors(sentence).is_empty());
        assert_eq!(
            weak_named_anchors(sentence),
            vec!["best-effort", "fail-closed"]
        );
        // A bare package name is spelled like a compound word and stays weak.
        assert!(named_anchors("bump serde-json for the alpha token").is_empty());
        assert_eq!(
            weak_named_anchors("bump serde-json for the alpha token"),
            vec!["serde-json"]
        );

        // A capital, a digit, or an underscore keeps a hyphenated token a named anchor.
        assert_eq!(
            named_anchors("set X-Request-Id on retry-v2 and max_retry-count"),
            vec!["X-Request-Id", "retry-v2", "max_retry-count"]
        );

        // A weak anchor no selected context spells is still reported as unmatched.
        let selected = vec![relevance_probe("src/index.rs", "fn reindex() {}")];
        assert_eq!(
            unmatched_named_anchors(drive_by, &selected),
            vec!["re-index", "drive-by"]
        );
    }

    #[test]
    fn unmatched_weak_anchors_never_set_the_identifier_blocker_or_its_cap() {
        let base = ConfidenceSignalInput {
            primary_file_count: 3,
            evidence_count: 12,
            exact_reference_count: 2,
            validation_count: 3,
            validation_with_command_count: 3,
            // Context and plan list unmatched weak anchors as `anchor` negative evidence.
            negative_evidence_count: 1,
            allowed_file_count: 3,
            runtime_signal_count: 1,
            task_relevance: 0.8,
            named_anchor_count: 0,
            unmatched_anchors: vec!["re-index".into(), "drive-by".into()],
            weak_anchors: vec!["re-index".into(), "drive-by".into()],
            coverage: CoverageInput::default(),
            primary_language_keys: Vec::new(),
        };
        let names_identifier_blocker = |breakdown: &ConfidenceBreakdown| -> bool {
            breakdown
                .blockers
                .iter()
                .any(|blocker| blocker.contains("task identifier(s) name nothing"))
        };

        let weak_only = ConfidenceBreakdown::from_signals(base.clone());
        assert!(!names_identifier_blocker(&weak_only), "{weak_only:?}");
        assert!(weak_only.overall_score > 0.50 && weak_only.overall_score <= 0.60);
        assert_eq!(weak_only.overall_enum, Confidence::Medium);
        assert!(weak_only.caveats.iter().any(|caveat| caveat
            == "2 hyphenated task word(s) appear in no selected context: re-index, drive-by"));

        // A matched identifier beside an unmatched weak word: the miss count once equalled
        // the identifier count here and read as every identifier unmatched.
        let named_matched = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            named_anchor_count: 1,
            unmatched_anchors: vec!["drive-by".into()],
            weak_anchors: vec!["drive-by".into()],
            ..base.clone()
        });
        assert!(
            !names_identifier_blocker(&named_matched),
            "{named_matched:?}"
        );
        assert!(named_matched.overall_score > 0.50, "{named_matched:?}");

        // Every identifier unmatched: the blocker and cap apply and name the identifier only.
        let named_missing = ConfidenceBreakdown::from_signals(ConfidenceSignalInput {
            named_anchor_count: 1,
            unmatched_anchors: vec!["FrobnicateWidgetManager".into(), "drive-by".into()],
            weak_anchors: vec!["drive-by".into()],
            ..base
        });
        assert_eq!(named_missing.overall_enum, Confidence::Low);
        assert!(named_missing.overall_score <= 0.50, "{named_missing:?}");
        assert!(named_missing.blockers.iter().any(|blocker| {
            blocker
            == "1 task identifier(s) name nothing in the selected context: FrobnicateWidgetManager"
        }));
        assert!(named_missing
            .caveats
            .iter()
            .any(|caveat| caveat.ends_with("appear in no selected context: drive-by")));
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
        coverage.record_policy_exclusion(
            &Language::Java,
            SkipSource::SecurityPolicy,
            Some("config"),
        );
        coverage.record_discovered(&Language::Python);
        coverage.record_skipped(&Language::Python, SkipReason::TooLarge);
        coverage.record_discovered(&Language::Python);
        coverage.record_skipped(&Language::Python, SkipReason::Error);

        assert_eq!(coverage.discovered, 5);
        assert_eq!(coverage.indexed, 2);
        assert_eq!(coverage.generated, 1);
        assert_eq!(coverage.by_language["java"].indexed, 2);
        assert_eq!(coverage.by_language["java"].generated, 1);
        assert_eq!(coverage.by_language["java"].excluded_by_policy(), 1);
        assert_eq!(coverage.by_language["java"].considered(), 2);
        assert_eq!(coverage.by_language["python"].percent(), Some(0.0));
        // Ties fall back to declaration order, so the output is deterministic; policy
        // skips are listed separately from the omissions the ratio is judged on.
        assert_eq!(
            coverage.top_skip_reasons(3),
            vec![(SkipReason::TooLarge, 1), (SkipReason::Error, 1)]
        );
        assert_eq!(
            coverage.policy_skip_reasons(),
            vec![(SkipReason::SecretPolicy, 1)]
        );
        // Every file here is a programming-language file, so both ratios agree; the
        // secret-policy skip is outside the denominator.
        assert_eq!(coverage.programming_totals(), (4, 2));
        assert!(coverage.below_warn_threshold());
        // Four files are under the per-language floor: the overall ratio warns, the
        // per-language rule does not.
        assert!(coverage.languages_below_warn_threshold().is_empty());
        assert_eq!(
            coverage.summary_line(),
            "2 of 4 programming-language files indexed (50.0%); 2 of 4 recognised files indexed (50.0%) overall; 1 excluded by policy (1 secret-policy; 1 under config/; `[paths] deny` or the built-in secret-path rule governs the largest share); skipped: 1 too-large, 1 error"
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
        let mut add = |language: &Language, discovered: usize, indexed: usize, reason| {
            for _ in 0..discovered {
                coverage.record_discovered(language);
            }
            for _ in 0..indexed {
                coverage.record_indexed(language, false);
            }
            for _ in indexed..discovered {
                coverage.record_skipped(language, reason);
            }
        };
        // The motivating incident: 25 of 10,012 is 99.75% and still a dropped package.
        add(&Language::Java, 10_012, 9_987, SkipReason::TooLarge);
        // Under the ratio with enough files to mean it.
        add(&Language::Python, 100, 90, SkipReason::Error);
        // Under the floor: one unreadable file, no verdict.
        add(&Language::Rust, 4, 1, SkipReason::Error);
        // Config formats never qualify however low they sit.
        add(&Language::Yaml, 900, 100, SkipReason::TooLarge);
        // Policy exclusions are not omissions: 300 hidden Go files leave 700 of 700.
        add(&Language::Go, 1_000, 700, SkipReason::Hidden);

        assert_eq!(
            coverage.languages_below_warn_threshold(),
            vec![
                ("java", 9_987.0 * 100.0 / 10_012.0, 25),
                ("python", 90.0, 10)
            ]
        );
        assert_eq!(coverage.by_language["go"].percent(), Some(100.0));
    }

    /// A policy exclusion is counted for its language as well as for the repository, and
    /// a rule dominates a programming language only with at least
    /// `INDEX_COVERAGE_MISSING_FILES_WARN` files and more than the language considers.
    #[test]
    fn policy_exclusions_are_counted_per_language_and_source() {
        let mut coverage = IndexCoverage::default();
        let mut add = |language: &Language,
                       indexed: usize,
                       excluded: usize,
                       reason: SkipReason,
                       source: SkipSource| {
            for _ in 0..indexed {
                coverage.record_discovered(language);
                coverage.record_indexed(language, false);
            }
            for _ in 0..excluded {
                coverage.record_discovered(language);
                coverage.record_skipped(language, reason);
                coverage.record_policy_exclusion(language, source, Some("src"));
            }
        };
        // Dominated: 25 git-ignored beside 3 considered.
        add(
            &Language::Rust,
            3,
            25,
            SkipReason::Ignored,
            SkipSource::GitIgnore,
        );
        // A hidden worktree in the same language is counted apart from the git-ignored files.
        add(
            &Language::Rust,
            0,
            30,
            SkipReason::Hidden,
            SkipSource::HiddenPolicy,
        );
        // Under the file count: 19 git-ignored beside nothing considered.
        add(
            &Language::Go,
            0,
            19,
            SkipReason::Ignored,
            SkipSource::GitIgnore,
        );
        // Not more than considered: 40 git-ignored beside 40 indexed.
        add(
            &Language::Java,
            40,
            40,
            SkipReason::Ignored,
            SkipSource::GitIgnore,
        );
        // Config languages never qualify however many files are ignored.
        add(
            &Language::Yaml,
            0,
            90,
            SkipReason::Ignored,
            SkipSource::GitIgnore,
        );

        assert_eq!(
            coverage.policy_excluded_by_language["rust"],
            BTreeMap::from([(SkipSource::GitIgnore, 25), (SkipSource::HiddenPolicy, 30)])
        );
        assert_eq!(
            coverage.policy_excluded_by_language["yaml"],
            BTreeMap::from([(SkipSource::GitIgnore, 90)])
        );
        assert_eq!(
            coverage.policy_excluded_by_source[&SkipSource::GitIgnore],
            25 + 19 + 40 + 90
        );
        for (language, entry) in &coverage.by_language {
            let recorded = coverage
                .policy_excluded_by_language
                .get(language)
                .map_or(0, |sources| sources.values().sum::<usize>());
            assert_eq!(recorded, entry.excluded_by_policy(), "{language}");
        }
        assert_eq!(
            coverage.languages_mostly_excluded_by(SkipSource::GitIgnore),
            vec![("rust", 25, 3)]
        );
        assert_eq!(
            coverage.languages_mostly_excluded_by(SkipSource::HiddenPolicy),
            vec![("rust", 30, 3)]
        );

        let encoded = serde_json::to_value(&coverage).unwrap();
        let decoded: IndexCoverage = serde_json::from_value(encoded.clone()).unwrap();
        assert_eq!(decoded, coverage);

        // A manifest written before the map existed has no per-language data to judge.
        let mut legacy = encoded;
        legacy
            .as_object_mut()
            .unwrap()
            .remove("policy_excluded_by_language")
            .unwrap();
        let legacy: IndexCoverage = serde_json::from_value(legacy).unwrap();
        assert!(legacy.policy_excluded_by_language.is_empty());
        assert!(legacy
            .languages_mostly_excluded_by(SkipSource::GitIgnore)
            .is_empty());
        assert_eq!(
            legacy.policy_excluded_by_source,
            coverage.policy_excluded_by_source
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

    /// Pruned directories are named, capped at `PRUNED_DIRS_LISTED` with the rest counted,
    /// and a manifest written before paths were recorded reads as before.
    #[test]
    fn pruned_directories_are_named_and_capped() {
        let mut coverage = IndexCoverage::default();
        coverage.record_discovered(&Language::Go);
        coverage.record_indexed(&Language::Go, false);
        let dirs = (0..PRUNED_DIRS_LISTED + 5)
            .map(|index| PrunedDir {
                path: format!("svc{index:03}/node_modules"),
                reason: PruneReason::Dependencies,
                tracked_source_files: Some(0),
            })
            .collect::<Vec<_>>();
        coverage.record_pruned_dirs(dirs, 2);
        assert_eq!(coverage.pruned_dirs, PRUNED_DIRS_LISTED + 7);
        // The record keeps every directory a plan must forbid; only the two secret-like
        // ones go unnamed. A status summary shows the first fifty.
        assert_eq!(coverage.pruned.len(), PRUNED_DIRS_LISTED + 5);
        assert_eq!(coverage.pruned_unlisted, 2);
        let view = coverage.status_view();
        assert_eq!(view.pruned.len(), PRUNED_DIRS_LISTED);
        assert_eq!(view.pruned_unlisted, 7);
        assert_eq!(view.pruned_dirs, PRUNED_DIRS_LISTED + 7);
        assert_eq!(view.pruned[..], coverage.pruned[..PRUNED_DIRS_LISTED]);
        assert_eq!(coverage.pruned_source_files(), 0);
        assert!(coverage.summary_line().ends_with(
            "57 directories pruned as build output or dependencies: svc000/node_modules/, svc001/node_modules/, svc002/node_modules/ and 54 more"
        ));

        let mut encoded = serde_json::to_value(&coverage).unwrap();
        let object = encoded.as_object_mut().unwrap();
        object.remove("pruned").unwrap();
        object.remove("pruned_unlisted").unwrap();
        let legacy: IndexCoverage = serde_json::from_value(encoded).unwrap();
        assert!(legacy.pruned.is_empty());
        assert!(legacy
            .summary_line()
            .ends_with("57 directories pruned by name (contents not counted)"));
    }

    /// A pruned directory's tracked source count is shown beside its name wherever it is
    /// non-zero, and directories holding committed files sort ahead of empty ones, so a summary
    /// naming three shows them even past the alphabet.
    #[test]
    fn pruned_directories_show_their_tracked_source_counts() {
        let mut coverage = IndexCoverage::default();
        coverage.record_discovered(&Language::Go);
        coverage.record_indexed(&Language::Go, false);
        let dir = |path: &str, reason, tracked| PrunedDir {
            path: path.into(),
            reason,
            tracked_source_files: tracked,
        };
        coverage.record_pruned_dirs(
            vec![
                dir("aaa/node_modules", PruneReason::Dependencies, Some(0)),
                dir("abc/build", PruneReason::BuildOutput, None),
                dir("web/dist", PruneReason::BuildOutput, Some(1)),
                dir("zz/dist", PruneReason::BuildOutput, Some(1_200)),
                dir("tools/build", PruneReason::UndeclaredBuildDir, Some(2)),
            ],
            1,
        );
        assert_eq!(
            coverage
                .pruned
                .iter()
                .map(|dir| dir.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "tools/build",
                "zz/dist",
                "web/dist",
                "aaa/node_modules",
                "abc/build"
            ]
        );
        assert!(coverage.summary_line().ends_with(
            "6 directories pruned as build output or dependencies: tools/build/ (2 tracked source files), zz/dist/ (1,200 tracked source files), web/dist/ (1 tracked source file) and 3 more"
        ), "{}", coverage.summary_line());
        assert!(!coverage.summary_line().contains("contents not counted"));
        // No count, or a count of zero, is a bare name; the secret-like directory stays unnamed.
        assert_eq!(coverage.pruned[3].label(), "aaa/node_modules/");
        assert_eq!(coverage.pruned[4].label(), "abc/build/");
        assert_eq!(coverage.pruned_unlisted, 1);
    }

    /// A submodule is another repository, not build output: the summary says so, and names
    /// which directory is one when it lists build directories beside it (#677).
    #[test]
    fn pruned_submodules_are_not_called_build_output() {
        let dir = |path: &str, reason| PrunedDir {
            path: path.into(),
            reason,
            tracked_source_files: Some(0),
        };
        let mut coverage = IndexCoverage::default();
        coverage.record_pruned_dirs(
            vec![
                dir("vendor/ledger", PruneReason::Submodule),
                dir("vendor/store", PruneReason::Submodule),
            ],
            0,
        );
        assert!(coverage
            .blind_spot_caveats()
            .join("; ")
            .ends_with("2 directories pruned as submodules: vendor/ledger/, vendor/store/"));

        let mut coverage = IndexCoverage::default();
        coverage.record_pruned_dirs(
            vec![
                dir("target", PruneReason::BuildOutput),
                dir("vendor/ledger", PruneReason::Submodule),
            ],
            0,
        );
        assert!(coverage.blind_spot_caveats().join("; ").ends_with(
            "2 directories pruned as build output, dependencies or submodules: target/, vendor/ledger/ (submodule)"
        ), "{}", coverage.blind_spot_caveats().join("; "));

        // An unnamed directory may be anything, so the lead cannot claim they are all submodules.
        let mut coverage = IndexCoverage::default();
        coverage.record_pruned_dirs(vec![dir("vendor/ledger", PruneReason::Submodule)], 1);
        assert!(coverage.blind_spot_caveats().join("; ").contains(
            "2 directories pruned as build output, dependencies or submodules: vendor/ledger/ (submodule)"
        ), "{}", coverage.blind_spot_caveats().join("; "));
        let mut coverage = IndexCoverage::default();
        coverage.record_pruned_dirs(vec![dir("vendor/ledger", PruneReason::Submodule)], 0);
        assert_eq!(
            coverage.blind_spot_caveats(),
            vec!["1 directory pruned as a submodule: vendor/ledger/".to_string()]
        );
        assert_eq!(PruneReason::Submodule.label(), "submodule");
        assert!(PruneReason::Submodule.counts_tracked_source());

        // Tracked source under a directory with its own `.git` says this repository owns it and
        // the `.git` is stray: it is missing source, listed first, and named as such.
        let mut coverage = IndexCoverage::default();
        coverage.record_discovered(&Language::Rust);
        coverage.record_skipped(&Language::Rust, SkipReason::Pruned);
        coverage.record_pruned_dirs(
            vec![
                dir("target", PruneReason::BuildOutput),
                PrunedDir {
                    path: "tools/ledger".into(),
                    reason: PruneReason::Submodule,
                    tracked_source_files: Some(1),
                },
            ],
            0,
        );
        assert_eq!(coverage.pruned_source_dirs()[0].path, "tools/ledger");
        assert_eq!(
            coverage.blind_spot_caveats(),
            vec![
                "1 git-tracked source file not indexed under nested repository directory: tools/ledger/".to_string(),
                "2 directories pruned as build output, dependencies or submodules: tools/ledger/ (submodule, 1 tracked source file), target/".to_string(),
            ]
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

        // Hidden files are a policy exclusion: reported, outside both denominators.
        assert_eq!(coverage.percent(), Some(100.0));
        assert_eq!(coverage.programming_percent(), Some(100.0));
        assert_eq!(coverage.excluded_by_policy(), 100);
        assert!(!coverage.below_warn_threshold());
        assert!(coverage.languages_below_warn_threshold().is_empty());
        assert_eq!(
            coverage.summary_line(),
            "900 of 900 programming-language files indexed (100.0%); 900 of 900 recognised files indexed (100.0%) overall; 100 excluded by policy (100 hidden)"
        );

        // A source file going missing still warns, at the same overall ratio.
        coverage.record_discovered(&Language::TypeScript);
        coverage.record_skipped(&Language::TypeScript, SkipReason::TooLarge);
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

        // Source that exists but is entirely policy-excluded is a different statement.
        coverage.record_discovered(&Language::Rust);
        coverage.record_skipped(&Language::Rust, SkipReason::Hidden);
        assert_eq!(coverage.programming_percent(), None);
        assert_eq!(
            coverage.summary_line(),
            "no programming-language files considered under the current policy; 10 of 10 recognised files indexed (100.0%); 1 excluded by policy (1 hidden)"
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
            Language::CSharp,
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
    fn files_parsed_with_errors_are_named_beside_the_ratio() {
        let mut coverage = IndexCoverage::default();
        for _ in 0..3 {
            coverage.record_discovered(&Language::CSharp);
            coverage.record_indexed(&Language::CSharp, false);
        }
        coverage.record_parsed_with_errors(&Language::CSharp, false);
        coverage.record_parsed_with_errors(&Language::CSharp, true);
        // Every file is indexed, so the ratio is complete; the caveat says what that hides.
        assert_eq!(coverage.programming_percent(), Some(100.0));
        assert!(
            coverage.summary_line().ends_with(
                "; 2 c_sharp files parsed with syntax errors (their symbols are what error recovery kept; 1 named by pattern only)"
            ),
            "{}",
            coverage.summary_line()
        );
        let entry = &coverage.by_language["c_sharp"];
        assert_eq!((entry.parsed_with_errors, entry.pattern_fallback), (2, 1));
        // Absent from JSON when zero, so other languages' records are unchanged.
        let json = serde_json::to_value(LanguageCoverage::default()).unwrap();
        assert!(json.get("parsed_with_errors").is_none());
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
            Language::CSharp,
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
    use super::{is_test_code_path, is_test_path, query_wants_tests};

    #[test]
    fn test_code_paths_exclude_data_only_directories() {
        for path in [
            "src/rates_test.ts",
            "src/__tests__/rates.ts",
            "test/rates.js",
            "pkg/store/store_test.go",
        ] {
            assert!(is_test_code_path(path), "{path} should be test code");
        }
        for path in [
            "pkg/store/testdata/input_test.go",
            "tests/fixtures/invoice.ts",
            "src/__snapshots__/invoice.test.ts",
            "tests/Test_Data/sample.py",
            "src/rates.ts",
        ] {
            assert!(!is_test_code_path(path), "{path} should not be test code");
        }
        assert!(is_test_path("tests/fixtures/invoice.ts"));
    }

    #[test]
    fn gradle_source_sets_and_java_suffixes_are_tests() {
        for path in [
            "extensions/admission/src/internalClusterTest/java/org/acme/QuotaReloaderIT.java",
            "extensions/admission/src/yamlRestTest/java/org/acme/QuotaLedgerTestHelper.java",
            "extensions/admission/src/test/java/org/acme/QuotaEnforcerTests.java",
            "extensions/admission/qa/quota-reloaded/src/javaRestTest/java/QuotaReloadedIT.java",
            "core/src/testFixtures/java/org/acme/QXTestCase.java",
            "src/main/java/org/acme/AbstractQuotaEnforcerTestCase.java",
            "src/main/java/org/acme/RoutingSpec.java",
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
            "extensions/admission/src/main/java/org/acme/QuotaEnforcer.java",
            "core/src/main/java/org/acme/ledger/LedgerState.java",
            "src/latest_news.rs",
            "src/attestation/verify.rs",
            "src/contest/scoring.py",
            "src/main/java/org/acme/UNIT.java",
            "src/main/java/org/acme/Test.java",
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
        assert!(query_wants_tests("add tests for the quota enforcer"));
        assert!(query_wants_tests("which spec covers routing"));
        assert!(query_wants_tests("identity: Add BenchmarkRingBuffer"));
        assert!(!query_wants_tests("quota enforcer"));
        assert!(!query_wants_tests("latest ledger state publication"));
    }
}
