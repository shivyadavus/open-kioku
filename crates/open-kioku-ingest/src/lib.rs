use chrono::Utc;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use open_kioku_config::OkConfig;
use open_kioku_core::{
    AnalysisFact, CodeChunk, Confidence, DocumentSection, DocumentType, EvidenceSourceType, File,
    FileId, GitCochangeEdge, GitCommitId, GitSymbolTouch, GraphEdgeType, GraphNodeType,
    HistoryRecordId, HistorySnapshot, Import, IndexCoverage, IndexManifest, IndexMode,
    IndexPhaseReport, IndexQuality, Language, LineRange, PruneReason, PrunedDir, QualityNote,
    QualityNoteKind, Repository, RepositoryId, SkipReason, SkipSource, SkippedPath, Symbol,
    SymbolId, SymbolOccurrence, TestExclusionReason, TestTarget, HISTORY_SCHEMA_VERSION,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_languages::{
    detect_language, is_supported_code, likely_generated, likely_generated_path, likely_vendor_path,
};
use open_kioku_parse::{HeuristicParser, Parser};
use open_kioku_scip::ScipIndexReport;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

mod cargo_facts;
mod cycle_memo;
mod dependency_trees;
pub mod derived;
mod git_ignore;
pub mod path_policy;
mod prune;

/// Trim a freshly formatted evidence message to its exact length.
///
/// `format!` sizes its buffer by estimate and over-allocates on multi-argument
/// templates — measured at 1.9x on the symbol-registry message, which is the
/// largest single producer of message bytes. These strings are retained for the
/// whole of an indexing run, so at corpus scale the slack is worth one realloc.
/// Templates with one or two arguments show no slack and do not need this.
pub(crate) fn compact_message(mut message: String) -> String {
    message.shrink_to_fit();
    message
}
pub mod imports;
pub mod project_model;
pub mod redaction;
pub mod relationships;
pub mod resolver;
pub mod runtime;
mod rust_use_path;
pub mod symbol_registry;
mod test_discovery;
pub mod validation;

pub use open_kioku_core::{RelationshipResolutionQuality, ResolutionQualityReport};

const MAX_HISTORY_COCHANGE_EDGES: usize = 5000;

#[derive(Debug, Clone)]
pub struct IndexSnapshot {
    pub manifest: IndexManifest,
    pub files: Vec<File>,
    pub symbols: Vec<Symbol>,
    pub chunks: Vec<CodeChunk>,
    pub document_sections: Vec<DocumentSection>,
    pub tests: Vec<TestTarget>,
    pub imports: Vec<Import>,
    pub import_resolutions: Vec<open_kioku_core::ImportResolution>,
    pub occurrences: Vec<SymbolOccurrence>,
    pub analysis_facts: Vec<AnalysisFact>,
    pub scip: Option<ScipIndexReport>,
    pub phase_reports: Vec<IndexPhaseReport>,
    pub skipped_paths: Vec<SkippedPath>,
    pub scopes: Vec<open_kioku_core::Scope>,
    pub bindings: Vec<open_kioku_core::Binding>,
    pub call_sites: Vec<open_kioku_core::CallSite>,
    pub resolved_relationships: Vec<open_kioku_resolution::ResolvedRelationship>,
    pub resolution_diffs: Vec<ResolutionDiff>,
    pub resolution_quality: Option<ResolutionQualityReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionDiff {
    pub call_site_id: open_kioku_core::CallSiteId,
    pub caller_symbol_id: Option<SymbolId>,
    pub callee_name: String,
    pub legacy_target: Option<SymbolId>,
    pub semantic_target: Option<SymbolId>,
    pub agreement: bool,
}

trait ResolutionQualityReportExt {
    fn record_outcome(
        &mut self,
        language: &Language,
        edge_type: &GraphEdgeType,
        outcome: &open_kioku_resolution::ResolutionOutcome,
        enrichment_time_us: u64,
    );

    fn record_reference_occurrence(&mut self, occurrence: &SymbolOccurrence);
}

impl ResolutionQualityReportExt for ResolutionQualityReport {
    fn record_outcome(
        &mut self,
        language: &Language,
        edge_type: &GraphEdgeType,
        outcome: &open_kioku_resolution::ResolutionOutcome,
        enrichment_time_us: u64,
    ) {
        let language_metrics = self
            .by_language
            .entry(language_metric_key(language))
            .or_default();
        language_metrics.occurrences += 1;
        language_metrics.candidates_considered += outcome.candidates_considered();
        language_metrics.enrichment_time_us = language_metrics
            .enrichment_time_us
            .saturating_add(enrichment_time_us);
        if outcome.candidate_cap_hit() {
            self.candidate_cap_hits += 1;
            language_metrics.candidate_cap_hits += 1;
        }
        match outcome {
            open_kioku_resolution::ResolutionOutcome::Proven { .. } => {
                language_metrics.proven += 1;
            }
            // Candidates configuration chooses between are kept but none is proven.
            open_kioku_resolution::ResolutionOutcome::Ambiguous { .. }
            | open_kioku_resolution::ResolutionOutcome::Alternatives { .. } => {
                language_metrics.ambiguous += 1;
            }
            open_kioku_resolution::ResolutionOutcome::Unresolved { .. } => {
                language_metrics.unresolved += 1;
            }
            open_kioku_resolution::ResolutionOutcome::External { .. } => {
                language_metrics.external += 1;
            }
        }

        let key = relationship_metric_key(edge_type);
        let metrics = self.by_relationship.entry(key).or_default();
        match outcome {
            open_kioku_resolution::ResolutionOutcome::Proven { candidate } => {
                metrics.candidates_considered += candidate.candidates_considered;
                metrics.proven += 1;
                metrics.heuristic_candidates_retained += candidate.heuristic_candidates_retained;
                record_candidate_evidence(metrics, candidate);
            }
            open_kioku_resolution::ResolutionOutcome::Ambiguous {
                candidates,
                candidates_considered,
                ..
            } => {
                metrics.candidates_considered += *candidates_considered;
                metrics.ambiguous += 1;
                metrics.heuristic_candidates_retained += candidates
                    .iter()
                    .filter(|candidate| {
                        candidate.authority(edge_type)
                            != open_kioku_core::RelationshipAuthority::Authoritative
                    })
                    .count();
                for candidate in candidates {
                    record_candidate_evidence(metrics, candidate);
                }
            }
            open_kioku_resolution::ResolutionOutcome::Alternatives { candidates, .. } => {
                metrics.candidates_considered += candidates.len();
                metrics.ambiguous += 1;
                metrics.heuristic_candidates_retained += candidates.len();
                for candidate in candidates {
                    record_candidate_evidence(metrics, candidate);
                }
            }
            open_kioku_resolution::ResolutionOutcome::Unresolved {
                candidates,
                candidates_considered,
                ..
            } => {
                metrics.candidates_considered += *candidates_considered;
                metrics.unresolved += 1;
                metrics.heuristic_candidates_retained += candidates.len();
                for candidate in candidates {
                    record_candidate_evidence(metrics, candidate);
                }
            }
            open_kioku_resolution::ResolutionOutcome::External { .. } => {
                metrics.external += 1;
            }
        }
    }

    fn record_reference_occurrence(&mut self, occurrence: &SymbolOccurrence) {
        if occurrence.is_definition {
            return;
        }
        let metrics = self
            .by_relationship
            .entry(relationship_metric_key(&GraphEdgeType::References))
            .or_default();
        metrics.candidates_considered += 1;
        if occurrence.provenance == EvidenceSourceType::Scip
            && occurrence.confidence == Confidence::Exact
            && occurrence.source_range.is_some()
        {
            metrics.proven += 1;
            *metrics
                .proof_kind_counts
                .entry("exact_occurrence".into())
                .or_default() += 1;
            *metrics
                .resolver_strategy_counts
                .entry("scip_exact_occurrence".into())
                .or_default() += 1;
        } else {
            metrics.unresolved += 1;
            metrics.heuristic_candidates_retained += 1;
        }
    }
}

fn language_metric_key(language: &Language) -> String {
    serde_json::to_value(language)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| format!("{language:?}"))
}

fn elapsed_micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// The candidates of a type relation that become relationships: the proven one, or each file's
/// candidate when configuration selects the file the type is in, whose proofs leave it
/// unproven and name the other files, so no user of any of them is hidden (#625).
fn kept_candidates(
    outcome: open_kioku_resolution::ResolutionOutcome,
) -> impl Iterator<Item = open_kioku_resolution::ResolutionCandidate> {
    match outcome {
        open_kioku_resolution::ResolutionOutcome::Proven { candidate } => vec![candidate],
        open_kioku_resolution::ResolutionOutcome::Alternatives { candidates, .. } => candidates,
        _ => Vec::new(),
    }
    .into_iter()
}

/// Reports the Rust files whose module-path `CALLS` edges are left unresolved because the index
/// cannot tell which crate they belong to. Paths are not named: a skipped root may be a
/// secret-like one.
fn rust_placement_notes(gaps: imports::RustPlacementGaps) -> Vec<QualityNote> {
    let mut notes = Vec::new();
    if gaps.unplaced_packages > 0 {
        notes.push(QualityNote::new(
            QualityNoteKind::RelationshipResolution,
            format!(
                "{} Rust package(s) have indexed source files in a crate module tree with no crate root the index can place (for example a `lib.rs` or `main.rs` that was not indexed, or a `[lib] path` or target `path` outside the module trees the index follows); `crate::`, `self::` and `super::` call paths into other files there are left unresolved",
                gaps.unplaced_packages
            ),
        ));
    }
    if gaps.withheld_files > 0 {
        notes.push(QualityNote::new(
            QualityNoteKind::RelationshipResolution,
            format!(
                "{} Rust crate root(s) in `src/` were not indexed or are in a module tree the index does not follow, so {} source file(s) there that no indexed crate root declares may belong to them; `crate::` call paths in those files are left unresolved",
                gaps.unread_roots, gaps.withheld_files
            ),
        ));
    }
    if gaps.shared_files > 0 {
        let unread_mounts = if gaps.unread_mounts > 0 {
            format!(
                "; {} of the `#[path]` attributes that mark them could not be read (a raw string, a macro, or a directory), so each may mount any file of its package",
                gaps.unread_mounts
            )
        } else {
            String::new()
        };
        let unread_files = if gaps.unread_mounted_files > 0 {
            format!(
                "; {} Rust file(s) a `#[path]` attribute mounts, or that sit below one, were not indexed and their `mod` items could not be read, so each may mount any file of its package",
                gaps.unread_mounted_files
            )
        } else {
            String::new()
        };
        notes.push(QualityNote::new(
            QualityNoteKind::RelationshipResolution,
            format!(
                "{} Rust source file(s) an indexed crate root declares may also be compiled into another crate (a crate root beside them was not indexed, or another crate mounts them or a module above them with `#[path]`); `crate::`, `self::` and `super::` call paths in those files are left unresolved, except a `self::`/`super::` path ending below the file's own module where every crate compiles the file at the same place{unread_mounts}{unread_files}",
                gaps.shared_files
            ),
        ));
    }
    notes
}

fn attach_resolution_quality(quality: &mut IndexQuality, report: Option<ResolutionQualityReport>) {
    if let Some(report) = report.as_ref() {
        if report.candidate_cap_hits > 0 {
            quality.quality_notes.push(QualityNote::new(
                QualityNoteKind::RelationshipResolution,
                format!(
                    "semantic relationship candidate cap ({}) hit for {} occurrence(s); authoritative emission was suppressed for every capped occurrence",
                    open_kioku_resolution::MAX_RESOLUTION_CANDIDATES,
                    report.candidate_cap_hits
                ),
            ));
            quality.quality_notes.sort();
            quality.quality_notes.dedup();
        }
    }
    quality.resolution_quality = report;
}

fn relationship_metric_key(edge_type: &GraphEdgeType) -> String {
    serde_json::to_value(edge_type)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| format!("{edge_type:?}"))
}

fn proof_kind_metric_key(kind: &open_kioku_core::RelationshipProofKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| format!("{kind:?}"))
}

fn record_candidate_evidence(
    metrics: &mut RelationshipResolutionQuality,
    candidate: &open_kioku_resolution::ResolutionCandidate,
) {
    let mut proof_kinds = BTreeSet::new();
    let mut strategies = BTreeSet::new();
    for proof in &candidate.proofs {
        proof_kinds.insert(proof_kind_metric_key(&proof.kind));
        if !proof.resolver_strategy.is_empty() {
            strategies.insert(proof.resolver_strategy.clone());
        }
    }
    for kind in proof_kinds {
        *metrics.proof_kind_counts.entry(kind).or_default() += 1;
    }
    for strategy in strategies {
        *metrics
            .resolver_strategy_counts
            .entry(strategy)
            .or_default() += 1;
    }
}

#[derive(Debug, Clone)]
pub struct IndexProgress {
    pub phase: &'static str,
    pub elapsed_ms: u64,
    pub scanned_files: usize,
    pub indexed_files: usize,
    pub total_files: Option<usize>,
    pub nodes_added: usize,
    pub edges_added: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
}

pub struct Indexer {
    parser: Box<dyn Parser>,
}

impl Default for Indexer {
    fn default() -> Self {
        Self {
            parser: Box::<HeuristicParser>::default(),
        }
    }
}

/// Why one file was dropped from the parse phase. Recorded as a `SkipReason::Error` skip so the
/// omission stays visible in `skip_counts` / `skipped_paths` instead of aborting the index.
struct ParseFailure {
    source: SkipSource,
    message: String,
}

impl Indexer {
    pub fn index_repo(&self, root: impl AsRef<Path>, config: &OkConfig) -> Result<IndexSnapshot> {
        self.index_repo_with_progress(root, config, |_| {})
    }

    /// Reads and parses one discovered file. A file that vanished or lost its permissions between
    /// discovery and parsing is a filesystem skip; a grammar or heuristic that panics on the
    /// file's content is a parser skip. The panic payload is deliberately not recorded: it can
    /// quote the source text around the failing byte, and parser messages stay redacted.
    /// `AssertUnwindSafe` holds because the parser is `Sync` and borrowed immutably; the only
    /// other state the closure touches is the local content buffer, which is dropped either way.
    ///
    /// The flag is true when secret-like values were redacted from the file's content.
    fn parse_file(
        &self,
        root: &Path,
        file: &File,
        build_hint: Option<&str>,
    ) -> std::result::Result<(open_kioku_parse::ParsedFile, bool), ParseFailure> {
        let bytes = fs::read(root.join(&file.path)).map_err(|err| ParseFailure {
            source: SkipSource::Filesystem,
            message: err.to_string(),
        })?;
        let content = String::from_utf8_lossy(&bytes).into_owned();
        // Data, config and prose files are redacted before the parser sees them, so no chunk,
        // symbol, fact or test derived from the text, and nothing stored or searched from
        // those, can carry a secret-like value (#379). Programming-language source is indexed
        // as written, except a private key's PEM body, which no file name can be trusted to
        // keep out (#676).
        let (content, redacted) = match redaction::ContentKind::for_file(&file.path, &file.language)
        {
            None => {
                let redacted = redaction::redact_private_keys(content);
                (redacted.text, redacted.redactions > 0)
            }
            Some(kind) => {
                let redacted = redaction::redact_secret_values(&content, kind);
                (redacted.text, redacted.redactions > 0)
            }
        };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.parser.parse_with_hint(file, &content, build_hint)
        }))
        .map(|parsed| (parsed, redacted))
        .map_err(|_| ParseFailure {
            source: SkipSource::Parser,
            message: "parser panicked on this file; its content was not indexed".to_string(),
        })
    }

    pub fn index_repo_with_history(
        &self,
        root: impl AsRef<Path>,
        config: &OkConfig,
    ) -> Result<(IndexSnapshot, HistorySnapshot)> {
        self.index_repo_with_history_and_progress(root, config, |_| {})
    }

    pub fn index_repo_with_progress<F>(
        &self,
        root: impl AsRef<Path>,
        config: &OkConfig,
        on_progress: F,
    ) -> Result<IndexSnapshot>
    where
        F: Fn(IndexProgress) + Sync,
    {
        self.index_repo_with_history_and_progress(root, config, on_progress)
            .map(|(snapshot, _history)| snapshot)
    }

    pub fn index_repo_with_mode(
        &self,
        root: impl AsRef<Path>,
        config: &OkConfig,
        mode: IndexMode,
    ) -> Result<IndexSnapshot> {
        self.index_repo_with_mode_and_progress(root, config, mode, |_| {})
    }

    pub fn index_repo_with_mode_and_progress<F>(
        &self,
        root: impl AsRef<Path>,
        config: &OkConfig,
        mode: IndexMode,
        on_progress: F,
    ) -> Result<IndexSnapshot>
    where
        F: Fn(IndexProgress) + Sync,
    {
        self.index_repo_with_history_mode_and_progress(root, config, mode, on_progress)
            .map(|(snapshot, _history)| snapshot)
    }

    pub fn index_repo_with_history_and_progress<F>(
        &self,
        root: impl AsRef<Path>,
        config: &OkConfig,
        on_progress: F,
    ) -> Result<(IndexSnapshot, HistorySnapshot)>
    where
        F: Fn(IndexProgress) + Sync,
    {
        self.index_repo_with_history_mode_and_progress(root, config, IndexMode::Full, on_progress)
    }

    pub fn index_repo_with_history_mode_and_progress<F>(
        &self,
        root: impl AsRef<Path>,
        config: &OkConfig,
        mode: IndexMode,
        on_progress: F,
    ) -> Result<(IndexSnapshot, HistorySnapshot)>
    where
        F: Fn(IndexProgress) + Sync,
    {
        let started = Instant::now();
        let mut phase_reports = Vec::new();
        let root = root.as_ref().canonicalize()?;
        let repo_id = RepositoryId::new(stable_id(root.to_string_lossy().as_ref()));
        if mode == IndexMode::CrossProject {
            let repository = Repository {
                id: repo_id,
                name: config.repo.name.clone(),
                root: root.clone(),
                branch: open_kioku_git::branch(&root),
                commit: open_kioku_git::commit(&root),
                indexed_at: Some(Utc::now()),
            };
            emit_progress(
                &on_progress,
                &mut phase_reports,
                started,
                ProgressEvent::new("cross_project")
                    .warning("cross-project mode records repository status without parsing source"),
            );
            let document_event = if config.documents.enabled {
                ProgressEvent::new("document_corpus").warning(
                    "document corpus unavailable in cross-project mode; source was not scanned",
                )
            } else {
                ProgressEvent::new("document_corpus")
                    .warning("document corpus disabled by configuration")
            };
            emit_progress(&on_progress, &mut phase_reports, started, document_event);
            if let Some(report) = phase_reports.last_mut() {
                report.duration_ms = Some(0);
                report.document_files = Some(0);
                report.document_sections = Some(0);
            }
            let quality = index_quality(IndexQualityInput {
                root: &root,
                config,
                scip_report: None,
                test_count: 0,
                // No source was read, so no test was examined: `Some({})` would claim none was
                // excluded.
                excluded_test_targets: None,
                import_count: 0,
                analysis: AnalysisCounts::default(),
                quality_notes: &mode_quality_notes(mode),
                mode,
                phase_reports: &phase_reports,
                skipped_paths: &[],
                coverage: None,
                // Nothing was read, so nothing was stored unredacted.
                redacted_files: Some(0),
            });
            let manifest = IndexManifest {
                analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
                repository,
                file_count: 0,
                symbol_count: 0,
                chunk_count: 0,
                indexed_at: Utc::now(),
                schema_version: open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION,
                index_mode: mode,
                phase_reports: phase_reports.clone(),
                quality,
                snapshot: None,
            };
            return Ok((
                IndexSnapshot {
                    manifest,
                    files: Vec::new(),
                    symbols: Vec::new(),
                    chunks: Vec::new(),
                    document_sections: Vec::new(),
                    tests: Vec::new(),
                    imports: Vec::new(),
                    import_resolutions: Vec::new(),
                    occurrences: Vec::new(),
                    analysis_facts: Vec::new(),
                    scip: None,
                    phase_reports,
                    skipped_paths: Vec::new(),
                    scopes: Vec::new(),
                    bindings: Vec::new(),
                    call_sites: Vec::new(),
                    resolved_relationships: Vec::new(),
                    resolution_diffs: Vec::new(),
                    resolution_quality: None,
                },
                HistorySnapshot::empty(),
            ));
        }
        let build_hint: Option<String> =
            if root.join("build.gradle").exists() || root.join("build.gradle.kts").exists() {
                Some("gradle".to_string())
            } else if root.join("pom.xml").exists() {
                Some("maven".to_string())
            } else if root.join("WORKSPACE").exists()
                || root.join("BUILD.bazel").exists()
                || root.join("BUILD").exists()
            {
                Some("bazel".to_string())
            } else {
                None
            };
        let scan = {
            let mut progress = ProgressRecorder::new(&on_progress, started, &mut phase_reports);
            self.scan_files(&root, config, &repo_id, mode, &mut progress)?
        };
        let files = scan.files;
        let document_sections = scan.document_sections;
        let document_event = if config.documents.enabled {
            ProgressEvent::new("document_corpus")
                .scanned(scan.document_file_count)
                .indexed(scan.document_file_count)
                .total(Some(scan.document_file_count))
        } else {
            ProgressEvent::new("document_corpus")
                .warning("document corpus disabled by configuration")
        };
        emit_progress(&on_progress, &mut phase_reports, started, document_event);
        if let Some(report) = phase_reports.last_mut() {
            report.duration_ms = Some(scan.document_elapsed_ms);
            report.document_files = Some(scan.document_file_count);
            report.document_sections = Some(document_sections.len());
        }
        emit_progress(
            &on_progress,
            &mut phase_reports,
            started,
            ProgressEvent::new("parse")
                .scanned(files.len())
                .total(Some(files.len()))
                .skipped(scan.skipped)
                .warnings(scan.warnings.clone()),
        );
        let parsed_count = AtomicUsize::new(0);
        let outcomes = files
            .par_iter()
            .map(|file| {
                let outcome = self.parse_file(&root, file, build_hint.as_deref());
                let indexed_files = parsed_count.fetch_add(1, Ordering::Relaxed) + 1;
                if should_emit_progress(indexed_files, files.len()) {
                    emit_progress(
                        &on_progress,
                        &mut Vec::new(),
                        started,
                        ProgressEvent::new("parse")
                            .scanned(files.len())
                            .indexed(indexed_files)
                            .total(Some(files.len())),
                    );
                }
                outcome
            })
            .collect::<Vec<_>>();
        // A file that could not be read or parsed is dropped from the index and recorded as a
        // skip; the rest of the repository still indexes. Aborting here left the user with an
        // empty `.ok/` and the same failure on retry (#350).
        let mut skipped_paths = scan.skipped_paths;
        let mut coverage = scan.coverage;
        let mut redacted_files = scan.redacted_files;
        let mut parse_warnings = Vec::new();
        let mut kept_files = Vec::with_capacity(files.len());
        let mut parsed = Vec::with_capacity(files.len());
        // Rust files whose top level invokes a macro (see `RustModuleTree::with_module_macros`).
        let mut rust_module_macros = HashSet::new();
        let mut rust_macro_names = HashMap::new();
        // Rust type items that are not also values, and enum variants (#641, #643).
        let mut rust_type_only_items = Vec::new();
        let mut rust_enum_variants = Vec::new();
        // Rust unit structs, type aliases and impl blocks (#639, #654).
        let mut rust_unit_structs = Vec::new();
        let mut rust_type_aliases = Vec::new();
        let mut rust_impl_blocks = Vec::new();
        for (file, outcome) in files.into_iter().zip(outcomes) {
            match outcome {
                Ok((parsed_file, redacted)) => {
                    redacted_files += usize::from(redacted);
                    if parsed_file.syntax.syntax_errors {
                        let by_pattern = parsed_file.syntax.symbols.iter().any(|symbol| {
                            symbol.provenance == open_kioku_core::EvidenceSourceType::Heuristic
                        });
                        coverage.record_parsed_with_errors(&file.language, by_pattern);
                    }
                    rust_type_only_items
                        .extend(parsed_file.syntax.rust_type_only_items.iter().cloned());
                    rust_enum_variants
                        .extend(parsed_file.syntax.rust_enum_variants.iter().cloned());
                    rust_unit_structs.extend(parsed_file.syntax.rust_unit_structs.iter().cloned());
                    rust_type_aliases.extend(parsed_file.syntax.rust_type_aliases.iter().cloned());
                    rust_impl_blocks.extend(parsed_file.syntax.rust_impl_blocks.iter().cloned());
                    if parsed_file.syntax.invokes_item_macro {
                        rust_module_macros.insert(file.id.clone());
                    }
                    if !parsed_file.syntax.item_macro_names.is_empty() {
                        rust_macro_names.insert(
                            file.id.clone(),
                            parsed_file
                                .syntax
                                .item_macro_names
                                .iter()
                                .cloned()
                                .collect::<HashSet<_>>(),
                        );
                    }
                    kept_files.push(file);
                    parsed.push(parsed_file);
                }
                Err(failure) => {
                    parse_warnings.push(format!(
                        "skipped {}: {}",
                        file.path.display(),
                        failure.message
                    ));
                    // Discovery already counted this file as indexed; move it so coverage keeps
                    // reporting `discovered == indexed + sum(skipped)` per language.
                    if is_supported_code(&file.language) {
                        coverage.record_indexed_dropped(
                            &file.language,
                            file.is_generated,
                            SkipReason::Error,
                        );
                    }
                    skipped_paths.push(SkippedPath {
                        path: file.path,
                        reason: SkipReason::Error,
                        source: failure.source,
                        safe_to_show: true,
                    });
                }
            }
        }
        let files = kept_files;
        // Move parse output into the per-kind collections in one consuming pass, then release
        // the parsed corpus immediately. Cloning field-by-field kept a duplicate of every chunk
        // text, symbol, and occurrence alive for the rest of indexing, which dominated peak
        // memory on large repositories.
        let mut symbols = Vec::new();
        let mut scopes = Vec::new();
        let mut bindings = Vec::new();
        let mut call_sites = Vec::new();
        let mut import_sites = Vec::new();
        let mut module_declarations = Vec::new();
        let mut type_aliases = Vec::new();
        let mut package_declarations = Vec::new();
        let mut export_sites = Vec::new();
        let mut inheritance_sites = Vec::new();
        let mut chunks = Vec::new();
        let mut tests = Vec::new();
        let mut analysis_facts = Vec::new();
        for file in parsed {
            symbols.extend(file.syntax.symbols);
            scopes.extend(file.syntax.scopes);
            bindings.extend(file.syntax.bindings);
            call_sites.extend(file.syntax.calls);
            import_sites.extend(file.syntax.imports);
            module_declarations.extend(file.syntax.module_declarations);
            type_aliases.extend(file.syntax.type_aliases);
            package_declarations.extend(file.syntax.package_declaration);
            export_sites.extend(file.syntax.exports);
            inheritance_sites.extend(file.syntax.inheritance);
            chunks.extend(file.chunks);
            tests.extend(file.tests);
            analysis_facts.extend(file.analysis_facts);
        }
        // A parser reads one file: a C# `partial` type's part that omits its accessibility takes
        // the one another part of the same project declares.
        let mut msbuild_dirs = HashMap::new();
        let csharp_projects = files
            .iter()
            .filter(|file| file.language == Language::CSharp)
            .filter_map(|file| {
                prune::nearest_msbuild_project(&root, &file.path, &mut msbuild_dirs)
                    .map(|project| (file.id.clone(), project))
            })
            .collect::<HashMap<_, _>>();
        open_kioku_languages::csharp::unify_partial_type_visibility(&mut symbols, &csharp_projects);
        // Extraction applies the runners' default discovery rules; where a pytest
        // configuration changes them, its Python test files fall back to the test-path rule.
        let test_discovery_notes =
            test_discovery::widen_configured_python_tests(&root, &files, &mut tests);
        dedupe_symbols(&mut symbols);
        let imports = extract_imports_from_syntax(&import_sites);
        emit_progress(
            &on_progress,
            &mut phase_reports,
            started,
            ProgressEvent::new("extract")
                .scanned(files.len())
                .indexed(files.len())
                .total(Some(files.len()))
                .skipped(skipped_paths.len())
                .warnings(parse_warnings)
                .nodes_added(files.len() + symbols.len() + chunks.len() + tests.len()),
        );

        use crate::project_model::ProjectModelDiscovery;
        // The manifest walks skip what discovery skips, `[index] keep_dirs` included.
        let pruner = prune::DiscoveryPruner::new(&root, config)?;
        let project_model = open_kioku_semantic_model::ProjectModel::discover_in(&pruner);
        analysis_facts.extend(cargo_facts::cargo_manifest_facts(&project_model, &files));
        let mut import_registry = imports::ImportRegistry::default();
        let mut file_map: imports::FileMap = HashMap::new();
        fn register_file_key(map: &mut imports::FileMap, key: String, file_id: &FileId) {
            let candidates = map.entry(key).or_default();
            if !candidates.contains(file_id) {
                candidates.push(file_id.clone());
            }
        }
        for file in &files {
            let mod_path = project_model.module_path_from_file(&file.path, &file.language);
            register_file_key(&mut file_map, mod_path.clone(), &file.id);
            register_file_key(
                &mut file_map,
                file.path.to_string_lossy().to_string(),
                &file.id,
            );
            if let Some(stem) = file.path.file_stem().and_then(|s| s.to_str()) {
                if !mod_path.is_empty() {
                    register_file_key(&mut file_map, format!("{mod_path}.{stem}"), &file.id);
                    register_file_key(&mut file_map, format!("{mod_path}::{stem}"), &file.id);
                }
                register_file_key(&mut file_map, stem.to_string(), &file.id);
            }
        }
        // The module-key map is built without the owning crate, so Rust imports are left to the
        // crate-aware `resolve_rust_imports` below.
        let rust_file_ids = files
            .iter()
            .filter(|file| file.language == Language::Rust)
            .map(|file| file.id.clone())
            .collect::<HashSet<_>>();
        for site in &import_sites {
            if rust_file_ids.contains(&site.file_id) {
                import_registry.insert_unresolved_site(site);
            } else {
                import_registry.resolve_site(site, &file_map);
            }
        }

        let symbol_index = open_kioku_resolution::SymbolIndex::build(symbols.clone());
        import_registry.resolve_symbols_skipping(&symbol_index, &file_map, &rust_file_ids);

        let mut scope_index = open_kioku_resolution::ScopeIndex::build(scopes.clone());
        scope_index.record_module_declarations(&module_declarations);
        scope_index.record_rust_type_items(rust_type_only_items, rust_enum_variants);
        scope_index.record_rust_type_shapes(rust_unit_structs, rust_type_aliases, rust_impl_blocks);
        let rust_modules = imports::RustModuleTree::new(
            &files,
            &project_model,
            &module_declarations,
            &scope_index,
        )
        // A redacted skip names no path. Only secret-like paths are redacted, so a skipped crate
        // root goes unseen here only when its path matches a secret-path pattern
        // (`src/.env.rs`; `src/id_rsa.rs` is source and indexed, #676).
        .with_unindexed_files(
            skipped_paths
                .iter()
                .filter(|skipped| skipped.safe_to_show)
                .map(|skipped| skipped.path.as_path()),
        )
        .with_import_sites(&import_sites)
        // A macro invoked at the top level of a Rust file may expand to items and `use`
        // declarations the parser does not see, so its module's `use` sites settle no name.
        .with_module_macros(rust_module_macros, rust_macro_names);
        let rust_modules = {
            // Only a file skipped for its size has its `mod` lines read; a path policy's
            // exclusion is never read around. A mounted file read this way can mount another
            // skipped file, so reading repeats until no readable file is left unread.
            let mut rust_modules = rust_modules;
            let mut read = HashSet::new();
            loop {
                let wanted = rust_modules.unscanned_module_files();
                let scanned = skipped_paths
                    .iter()
                    .filter(|skipped| {
                        skipped.reason == SkipReason::TooLarge
                            && skipped.source == SkipSource::SizeLimit
                            && skipped.safe_to_show
                            && !open_kioku_core::is_secret_like_path(&skipped.path)
                            && wanted.contains(&skipped.path)
                            && read.insert(skipped.path.as_path())
                    })
                    .map(|skipped| {
                        let names =
                            rust_use_path::read_module_declarations(&root.join(&skipped.path));
                        (skipped.path.as_path(), names)
                    })
                    .collect::<Vec<_>>();
                if scanned.is_empty() {
                    break rust_modules;
                }
                rust_modules = rust_modules.with_scanned_files(scanned);
            }
        };
        scope_index.record_rust_module_placements(rust_modules.module_placements());
        scope_index.record_rust_configured_modules(rust_modules.configured_modules());
        scope_index.record_rust_crate_names(rust_modules.crate_names());
        scope_index.record_rust_external_crates(crate::project_model::rust_external_crate_names(
            &project_model,
            &files,
        ));
        scope_index.record_rust_in_scope_use_paths(
            crate::project_model::rust_in_scope_use_path_files(&project_model, &files),
        );
        // A Rust call path through a module's `use` re-exports (#476) ends in a name some call
        // writes: its callee, read as a value, or a segment of its receiver, read as a type, as
        // in `Engine::new()` (#643).
        let rust_calls = call_sites
            .iter()
            .filter(|call| rust_file_ids.contains(&call.file_id));
        let rust_callee_names = rust_calls
            .clone()
            .map(|call| call.callee_name.as_str())
            .collect::<HashSet<_>>();
        let rust_receiver_names = rust_calls
            .filter_map(|call| call.receiver.as_deref())
            .flat_map(|receiver| receiver.split("::").map(str::trim))
            .collect::<HashSet<_>>();
        let rust_reexports = rust_modules.reexports(
            &symbol_index,
            &scope_index,
            |name, namespace| match namespace {
                open_kioku_resolution::RustNamespace::Value => rust_callee_names.contains(name),
                open_kioku_resolution::RustNamespace::Type => rust_receiver_names.contains(name),
            },
        );
        scope_index.record_rust_reexports(rust_reexports);
        let rust_placement_gaps = rust_modules.placement_gaps();
        import_registry.resolve_rust_imports(&symbol_index, &scope_index, &rust_modules);
        // Import bindings and file-level import edges follow the same declared module tree, and
        // `rust_modules` borrows the project model that moves into `semantic_repo` below.
        let rust_import_targets = imports::rust_import_edge_targets(
            &import_sites,
            &symbol_index,
            &scope_index,
            &rust_modules,
        );
        let resolver_report =
            resolver::resolve_imports(&pruner, &files, &symbols, &imports, &rust_import_targets)?;
        let binding_index = open_kioku_resolution::BindingIndex::build(bindings.clone());
        let mut inheritance_index =
            open_kioku_resolution::InheritanceIndex::build(inheritance_sites.clone());

        let mut semantic_repo = open_kioku_semantic_model::SemanticRepository::new();
        semantic_repo.project = project_model;
        semantic_repo.imports = import_registry.index;
        // Where a trait the index does not know may be in scope, which a method call through a
        // smart pointer must rule out (#639).
        scope_index.record_rust_open_trait_scopes(
            open_kioku_resolution::rust_open_trait_scope_files(
                &semantic_repo,
                &scope_index,
                &rust_file_ids,
            ),
        );
        for exp in &export_sites {
            let mod_id = open_kioku_core::ModuleId::new(format!("{}:module", exp.file_id.0));
            semantic_repo.exports.insert(
                mod_id.clone(),
                open_kioku_semantic_model::ExportBinding {
                    file_id: exp.file_id.clone(),
                    exported_name: exp.exported_name.clone(),
                    origin_symbol: exp.local_name.as_ref().and_then(|local| {
                        let candidates = symbol_index
                            .by_file
                            .get(&exp.file_id)
                            .into_iter()
                            .flatten()
                            .filter(|id| {
                                symbol_index
                                    .get(id)
                                    .map(|s| s.name == *local)
                                    .unwrap_or(false)
                            })
                            .cloned()
                            .collect::<Vec<_>>();
                        match candidates.as_slice() {
                            [candidate] => Some(candidate.clone()),
                            _ => None,
                        }
                    }),
                    source_module: Some(mod_id),
                    is_type_only: false,
                    is_glob: exp.is_glob,
                    evidence: Vec::new(),
                },
            );
        }
        inheritance_index.bind_parents_with_repository(&symbol_index, &semantic_repo);

        let resolution_mode = config.index.resolution_mode;
        let resolver_fact_count = resolver_report.analysis_facts.len();
        analysis_facts.extend(resolver_report.analysis_facts.clone());

        let registry_scope_model = symbol_registry::RegistryScopeModel::new(
            &files,
            &semantic_repo,
            &symbol_index,
            &scope_index,
            &binding_index,
            &inheritance_index,
            &type_aliases,
            &package_declarations,
        );
        let registry_report = symbol_registry::resolve_symbol_edges(
            &chunks,
            &symbols,
            &resolver_report.resolutions,
            config.scip.enabled,
            Some(&registry_scope_model),
        );
        let registry_fact_count = registry_report.analysis_facts.len();

        let mut resolution_diffs = Vec::new();
        let mut quality_report = ResolutionQualityReport::default();
        let mut resolved_relationships = Vec::new();

        let file_lookup: HashMap<FileId, &File> = files.iter().map(|f| (f.id.clone(), f)).collect();

        if resolution_mode == open_kioku_config::ResolutionMode::Shadow
            || resolution_mode == open_kioku_config::ResolutionMode::V2
        {
            quality_report.call_sites = call_sites.len();

            let symbols_by_qualified: HashMap<&str, &Symbol> = symbols
                .iter()
                .map(|s| (s.qualified_name.as_str(), s))
                .collect();
            let symbols_by_name: HashMap<&str, Vec<&Symbol>> = {
                let mut map: HashMap<&str, Vec<&Symbol>> = HashMap::new();
                for s in &symbols {
                    map.entry(s.name.as_str()).or_default().push(s);
                }
                map
            };

            let mut legacy_calls_map: HashMap<(&FileId, u32, &str), Option<SymbolId>> =
                HashMap::new();
            let mut legacy_call_facts_by_file: HashMap<FileId, Vec<&AnalysisFact>> = HashMap::new();
            for fact in &registry_report.analysis_facts {
                if fact.edge_type == GraphEdgeType::Calls {
                    legacy_call_facts_by_file
                        .entry(fact.file_id.clone())
                        .or_default()
                        .push(fact);
                    if let Some(range) = &fact.range {
                        let target_id = symbols_by_qualified
                            .get(fact.target.as_str())
                            .map(|s| s.id.clone())
                            .or_else(|| {
                                let name = fact
                                    .target
                                    .rsplit("::")
                                    .next()
                                    .unwrap_or(fact.target.as_str());
                                symbols_by_name.get(name).and_then(|syms| {
                                    if syms.len() == 1 {
                                        Some(syms[0].id.clone())
                                    } else {
                                        None
                                    }
                                })
                            });
                        let callee_name = fact
                            .target
                            .rsplit("::")
                            .next()
                            .unwrap_or(fact.target.as_str());
                        legacy_calls_map
                            .insert((&fact.file_id, range.start, callee_name), target_id);
                    }
                }
            }

            for call in &call_sites {
                if let Some(file) = file_lookup.get(&call.file_id) {
                    if let Some(semantics) = open_kioku_languages::semantics_for(&file.language) {
                        let ctx = open_kioku_resolution::ResolutionContext::new(
                            &call.file_id,
                            &file.path,
                            None,
                            file.language.clone(),
                            &semantic_repo,
                            &symbol_index,
                            &scope_index,
                            &binding_index,
                            &inheritance_index,
                            semantics,
                        );

                        let enrichment_started = Instant::now();
                        let v2_outcome = open_kioku_resolution::resolve_call_outcome(call, &ctx);
                        quality_report.record_outcome(
                            &file.language,
                            &GraphEdgeType::Calls,
                            &v2_outcome,
                            elapsed_micros(enrichment_started),
                        );
                        let semantic_target = match &v2_outcome {
                            open_kioku_resolution::ResolutionOutcome::Proven { candidate } => {
                                match candidate.confidence {
                                    Confidence::Exact => quality_report.resolved_exact += 1,
                                    Confidence::High => quality_report.resolved_high += 1,
                                    _ => {}
                                }
                                if let Some(caller) = &call.caller_symbol_id {
                                    resolved_relationships.push(
                                        open_kioku_resolution::ResolvedRelationship {
                                            from: caller.clone(),
                                            to: candidate.target_symbol_id.clone(),
                                            edge_type: GraphEdgeType::Calls,
                                            confidence: candidate.confidence,
                                            call_site: Some(call.range.clone()),
                                            evidence: candidate.evidence.clone(),
                                            proofs: candidate.proofs.clone(),
                                        },
                                    );
                                }
                                Some(candidate.target_symbol_id.clone())
                            }
                            open_kioku_resolution::ResolutionOutcome::Ambiguous { .. } => {
                                quality_report.ambiguous += 1;
                                None
                            }
                            // Each file configuration may compile holds a candidate: every one
                            // is kept as a relationship its proofs leave unproven, which names
                            // the other files, so no caller of any of them is hidden (#613).
                            open_kioku_resolution::ResolutionOutcome::Alternatives {
                                candidates,
                                ..
                            } => {
                                quality_report.ambiguous += 1;
                                if let Some(caller) = &call.caller_symbol_id {
                                    resolved_relationships.extend(candidates.iter().map(
                                        |candidate| open_kioku_resolution::ResolvedRelationship {
                                            from: caller.clone(),
                                            to: candidate.target_symbol_id.clone(),
                                            edge_type: GraphEdgeType::Calls,
                                            confidence: candidate.confidence,
                                            call_site: Some(call.range.clone()),
                                            evidence: candidate.evidence.clone(),
                                            proofs: candidate.proofs.clone(),
                                        },
                                    ));
                                }
                                None
                            }
                            open_kioku_resolution::ResolutionOutcome::External { .. } => {
                                quality_report.external += 1;
                                None
                            }
                            open_kioku_resolution::ResolutionOutcome::Unresolved { .. } => {
                                quality_report.unresolved += 1;
                                None
                            }
                        };

                        let legacy_target = legacy_calls_map
                            .get(&(
                                &call.file_id,
                                call.range.start_line,
                                call.callee_name.as_str(),
                            ))
                            .cloned()
                            .flatten()
                            .or_else(|| {
                                legacy_call_facts_by_file
                                    .get(&call.file_id)
                                    .into_iter()
                                    .flatten()
                                    .find(|fact| {
                                        fact.edge_type == GraphEdgeType::Calls
                                            && fact.file_id == call.file_id
                                            && fact
                                                .range
                                                .as_ref()
                                                .map(|r| {
                                                    r.start == call.range.start_line
                                                        || (r.start >= call.range.start_line
                                                            && r.end <= call.range.end_line)
                                                })
                                                .unwrap_or(false)
                                            && (fact.target.ends_with(&call.callee_name)
                                                || fact.target == call.callee_name)
                                    })
                                    .and_then(|fact| {
                                        symbols_by_qualified
                                            .get(fact.target.as_str())
                                            .map(|s| s.id.clone())
                                    })
                            });

                        let agreement = legacy_target == semantic_target;

                        if !agreement {
                            quality_report.disagreement += 1;
                            if legacy_target.is_some() && semantic_target.is_none() {
                                quality_report.legacy_only += 1;
                            } else if legacy_target.is_none() && semantic_target.is_some() {
                                quality_report.semantic_only += 1;
                            }
                        }

                        resolution_diffs.push(ResolutionDiff {
                            call_site_id: call.id.clone(),
                            caller_symbol_id: call.caller_symbol_id.clone(),
                            callee_name: call.callee_name.clone(),
                            legacy_target,
                            semantic_target,
                            agreement,
                        });
                    }
                }
            }
        }

        if resolution_mode == open_kioku_config::ResolutionMode::Shadow
            || resolution_mode == open_kioku_config::ResolutionMode::V2
        {
            for site in &inheritance_sites {
                let Some(child) = symbol_index.get(&site.child_symbol_id) else {
                    continue;
                };
                let Some(file) = file_lookup.get(&child.file_id) else {
                    continue;
                };
                let Some(semantics) = open_kioku_languages::semantics_for(&file.language) else {
                    continue;
                };
                let ctx = open_kioku_resolution::ResolutionContext::new(
                    &child.file_id,
                    &file.path,
                    child.module_id.as_ref(),
                    file.language.clone(),
                    &semantic_repo,
                    &symbol_index,
                    &scope_index,
                    &binding_index,
                    &inheritance_index,
                    semantics,
                );
                let enrichment_started = Instant::now();
                let (edge_type, outcome) =
                    open_kioku_resolution::resolve_inheritance_relationship_outcome(site, &ctx);
                quality_report.record_outcome(
                    &file.language,
                    &edge_type,
                    &outcome,
                    elapsed_micros(enrichment_started),
                );
                resolved_relationships.extend(kept_candidates(outcome).map(|candidate| {
                    open_kioku_resolution::ResolvedRelationship {
                        from: site.child_symbol_id.clone(),
                        to: candidate.target_symbol_id,
                        edge_type: edge_type.clone(),
                        confidence: candidate.confidence,
                        call_site: None,
                        evidence: candidate.evidence,
                        proofs: candidate.proofs,
                    }
                }));
            }

            for binding in &bindings {
                let Some(file) = file_lookup.get(&binding.file_id) else {
                    continue;
                };
                let Some(semantics) = open_kioku_languages::semantics_for(&file.language) else {
                    continue;
                };
                let ctx = open_kioku_resolution::ResolutionContext::new(
                    &binding.file_id,
                    &file.path,
                    None,
                    file.language.clone(),
                    &semantic_repo,
                    &symbol_index,
                    &scope_index,
                    &binding_index,
                    &inheritance_index,
                    semantics,
                );
                let enrichment_started = Instant::now();
                let Some((source, outcome)) =
                    open_kioku_resolution::resolve_declared_type_use_outcome(binding, &ctx)
                else {
                    continue;
                };
                quality_report.record_outcome(
                    &file.language,
                    &GraphEdgeType::UsesType,
                    &outcome,
                    elapsed_micros(enrichment_started),
                );
                resolved_relationships.extend(kept_candidates(outcome).map(|candidate| {
                    open_kioku_resolution::ResolvedRelationship {
                        from: source.clone(),
                        to: candidate.target_symbol_id,
                        edge_type: GraphEdgeType::UsesType,
                        confidence: candidate.confidence,
                        call_site: None,
                        evidence: candidate.evidence,
                        proofs: candidate.proofs,
                    }
                }));
            }
        }

        match resolution_mode {
            open_kioku_config::ResolutionMode::Legacy
            | open_kioku_config::ResolutionMode::Shadow => {
                analysis_facts.extend(registry_report.analysis_facts);
            }
            open_kioku_config::ResolutionMode::V2 => {
                let non_call_facts = registry_report
                    .analysis_facts
                    .into_iter()
                    .filter(|f| f.edge_type != GraphEdgeType::Calls);
                analysis_facts.extend(non_call_facts);
            }
        }
        // After every resolution pass, which read an alias by its syntax: from here on, and in
        // the stored symbol, a placed alias has its target's kind and names its target.
        symbol_registry::apply_type_alias_targets(
            &mut symbols,
            &registry_report.type_alias_targets,
        );
        let relationship_facts = collect_relationship_analysis_facts(
            &files,
            &symbols,
            &chunks,
            &analysis_facts,
            mode,
            &config.semantic,
        );
        let relationship_fact_count = relationship_facts.len();
        analysis_facts.extend(relationship_facts);
        let derived_facts = derived::collect_derived_file_facts(&root, &files);
        let derived_fact_count = derived_facts.len();
        analysis_facts.extend(derived_facts);
        let static_analysis_facts = analysis_facts.len();
        emit_progress(
            &on_progress,
            &mut phase_reports,
            started,
            ProgressEvent::new("analysis")
                .scanned(files.len())
                .indexed(files.len())
                .total(Some(files.len()))
                .edges_added(static_analysis_facts),
        );
        let runtime_facts = collect_runtime_analysis_facts(&root, &files, &symbols)?;
        let runtime_analysis_facts = runtime_facts.len();
        analysis_facts.extend(runtime_facts);
        let validation_facts = collect_validation_analysis_facts(&root, &files, &symbols, &tests)?;
        let validation_analysis_facts = validation_facts.len();
        analysis_facts.extend(validation_facts);
        let git_history = if config.history.enabled {
            collect_git_history(&root, &files, &symbols, config)?
        } else {
            GitHistoryIngest::empty()
        };
        emit_progress(
            &on_progress,
            &mut phase_reports,
            started,
            ProgressEvent::new("history")
                .scanned(files.len())
                .indexed(files.len())
                .total(Some(files.len()))
                .edges_added(git_history.snapshot.cochange_edges.len()),
        );
        let git_history_fact_count = git_history.analysis_facts.len();
        analysis_facts.extend(git_history.analysis_facts);
        let mut occurrences = derive_occurrences(&chunks, &symbols);
        emit_progress(
            &on_progress,
            &mut phase_reports,
            started,
            ProgressEvent::new("occurrences")
                .scanned(files.len())
                .indexed(files.len())
                .total(Some(files.len()))
                .edges_added(occurrences.len()),
        );

        let mut arch_fact_count = 0;
        if let Ok(Some(policy)) = open_kioku_config::load_architecture_policy(&root) {
            if let Ok(resolver) = open_kioku_architecture::PolicyResolver::new(&policy) {
                emit_progress(
                    &on_progress,
                    &mut phase_reports,
                    started,
                    ProgressEvent::new("architecture")
                        .scanned(files.len())
                        .indexed(files.len())
                        .total(Some(files.len())),
                );
                let arch_facts = collect_architecture_facts(&resolver, &files, &symbols);
                arch_fact_count = arch_facts.len();
                analysis_facts.extend(arch_facts);
            }
        }

        let mut scip_report = None;
        if config.scip.enabled {
            // SCIP covers every document it was generated for, discovery's skips included. A
            // document the security policy excludes, or whose path is absolute or leaves the
            // repository, contributes nothing: its symbol strings spell its path and its names
            // are its content. Other unindexed documents (gitignored or
            // generated code) are kept, so references into them still resolve to a symbol.
            let security = path_policy::SecurityPathPolicy::new(config)?;
            let (imported, report) =
                open_kioku_scip::prepare_and_import_scip(&root, &config.scip, &repo_id, &|path| {
                    security.exclusion(path).is_some()
                })?;
            let imported_symbol_count = imported.symbols.len();
            let imported_occurrence_count = imported.occurrences.len();
            symbols.extend(imported.symbols);
            dedupe_symbols(&mut symbols);
            occurrences.extend(imported.occurrences);
            emit_progress(
                &on_progress,
                &mut phase_reports,
                started,
                ProgressEvent::new("scip")
                    .scanned(files.len())
                    .indexed(files.len())
                    .total(Some(files.len()))
                    .nodes_added(imported_symbol_count)
                    .edges_added(imported_occurrence_count),
            );
            scip_report = Some(report);
        }
        for occurrence in &occurrences {
            quality_report.record_reference_occurrence(occurrence);
        }
        let repository = Repository {
            id: repo_id,
            name: config.repo.name.clone(),
            root: root.clone(),
            branch: open_kioku_git::branch(&root),
            commit: open_kioku_git::commit(&root),
            indexed_at: Some(Utc::now()),
        };
        let mut resolver_quality_notes = resolver_report.quality_notes.clone();
        resolver_quality_notes.extend(registry_report.quality_notes);
        resolver_quality_notes.extend(rust_placement_notes(rust_placement_gaps));
        let mut mode_notes = mode_quality_notes(mode);
        mode_notes.extend(resolver_quality_notes);
        mode_notes.extend(git_history.quality_notes.iter().cloned());
        mode_notes.extend(test_discovery_notes);
        let mut quality = index_quality(IndexQualityInput {
            root: &root,
            config,
            scip_report: scip_report.as_ref(),
            // `ok status` reports this as the repository's indexed tests. A disabled test is
            // not validation evidence, so counting it would read as ready while every pack says
            // validation is unavailable.
            test_count: tests
                .iter()
                .filter(|test| test.counts_as_validation_evidence())
                .count(),
            // The disabled targets are counted beside it, so a repository whose tests are all
            // skipped does not read as one that has none.
            excluded_test_targets: Some(excluded_test_targets(&tests)),
            import_count: imports.len(),
            analysis: AnalysisCounts {
                static_facts: static_analysis_facts,
                resolver_facts: resolver_fact_count,
                registry_facts: registry_fact_count,
                relationship_facts: relationship_fact_count,
                derived_facts: derived_fact_count,
                runtime_facts: runtime_analysis_facts,
                validation_facts: validation_analysis_facts,
                git_history_facts: git_history_fact_count,
                architecture_facts: arch_fact_count,
            },
            quality_notes: &mode_notes,
            mode,
            phase_reports: &phase_reports,
            skipped_paths: &skipped_paths,
            coverage: Some(coverage),
            redacted_files: Some(redacted_files),
        });
        let resolution_quality = if resolution_mode == open_kioku_config::ResolutionMode::Legacy {
            None
        } else {
            Some(quality_report)
        };
        attach_resolution_quality(&mut quality, resolution_quality.clone());
        let manifest = IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository,
            file_count: files.len(),
            symbol_count: symbols.len(),
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: open_kioku_core::INDEX_MANIFEST_SCHEMA_VERSION,
            index_mode: mode,
            phase_reports: phase_reports.clone(),
            quality,
            snapshot: None,
        };
        Ok((
            IndexSnapshot {
                manifest,
                files,
                symbols,
                chunks,
                document_sections,
                tests,
                imports,
                import_resolutions: resolver_report.resolutions,
                occurrences,
                analysis_facts,
                scip: scip_report,
                phase_reports,
                skipped_paths,
                scopes,
                bindings,
                call_sites,
                resolved_relationships,
                resolution_diffs,
                resolution_quality,
            },
            git_history.snapshot,
        ))
    }

    fn scan_files(
        &self,
        root: &Path,
        config: &OkConfig,
        repository_id: &RepositoryId,
        mode: IndexMode,
        progress: &mut ProgressRecorder<'_>,
    ) -> Result<ScanResult> {
        let max_size = config.max_file_size_bytes()?;
        let policy = path_policy::IndexPathPolicy::for_scan(root, config)?;
        let document_plain_text = compile_globs(&config.documents.plain_text)?;
        let mut builder = WalkBuilder::new(root);
        builder.hidden(false);
        builder.git_ignore(false).git_exclude(false).parents(false);
        builder.ignore(false);
        builder.follow_links(false);
        // Pruned directories never reach the per-file ledger, so each is recorded here by path
        // and reason: a source directory pruned by mistake must be nameable, not a bare count.
        let pruned = Arc::new(Mutex::new(Vec::<(PathBuf, PruneReason)>::new()));
        builder.filter_entry({
            let pruned = Arc::clone(&pruned);
            let pruner = prune::DiscoveryPruner::new(root, config)?;
            move |entry| {
                let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
                match pruner.classify(entry.path(), is_dir) {
                    prune::DirVerdict::Walk => true,
                    prune::DirVerdict::Tooling => false,
                    prune::DirVerdict::Prune(reason) => {
                        if let Ok(mut pruned) = pruned.lock() {
                            pruned.push((entry.path().to_path_buf(), reason));
                        }
                        false
                    }
                }
            }
        });
        let mut files = Vec::new();
        let mut document_sections = Vec::new();
        let mut document_paths = BTreeSet::<PathBuf>::new();
        let mut document_elapsed_ms = 0u64;
        let mut ledger = ScanLedger::default();
        let mut warnings = Vec::new();
        let mut scanned_files = 0;
        let mut source_like_files = 0;
        let mut redacted_files = 0;
        progress.emit(ProgressEvent::new("scan"));
        for entry in builder.build() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    ledger.skipped_paths.push(SkippedPath {
                        path: PathBuf::from("[walk-error]"),
                        reason: SkipReason::Error,
                        source: SkipSource::Filesystem,
                        safe_to_show: false,
                    });
                    ledger.coverage.walk_errors += 1;
                    warnings.push(format!("discovery walk error: {err}"));
                    continue;
                }
            };
            if !entry
                .file_type()
                .map(|kind| kind.is_file() || kind.is_symlink())
                .unwrap_or(false)
            {
                continue;
            }
            scanned_files += 1;
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
            let language = detect_language(&rel);
            if is_supported_code(&language) {
                source_like_files += 1;
            }
            ledger.discovered(&language);
            if let Some(exclusion) = policy.exclusion(&rel) {
                ledger.skip(
                    root,
                    path,
                    &language,
                    exclusion.reason,
                    exclusion.source,
                    exclusion.safe_to_show,
                );
                if exclusion.source == SkipSource::SecurityPolicy
                    && should_emit_progress(scanned_files, 0)
                {
                    progress.emit_transient(
                        ProgressEvent::new("scan")
                            .scanned(scanned_files)
                            .indexed(files.len())
                            .skipped(ledger.skipped_paths.len()),
                    );
                }
                continue;
            }
            if entry.file_type().is_some_and(|kind| kind.is_symlink()) {
                ledger.skip(
                    root,
                    path,
                    &language,
                    SkipReason::SymlinkPolicy,
                    SkipSource::SymlinkPolicy,
                    true,
                );
                continue;
            }
            if likely_vendor_path(&rel) {
                ledger.skip(
                    root,
                    path,
                    &language,
                    SkipReason::Vendor,
                    SkipSource::Detector,
                    true,
                );
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(err) => {
                    skip_unreadable(
                        root,
                        path,
                        &language,
                        &err.to_string(),
                        &mut ledger,
                        &mut warnings,
                    );
                    continue;
                }
            };
            if metadata.len() > max_size {
                ledger.skip(
                    root,
                    path,
                    &language,
                    SkipReason::TooLarge,
                    SkipSource::SizeLimit,
                    true,
                );
                continue;
            }
            if config.documents.enabled {
                if let Some(document_type) = document_type_for_path(&rel, &document_plain_text) {
                    let document_started = Instant::now();
                    let bytes = match fs::read(path) {
                        Ok(bytes) => bytes,
                        Err(err) => {
                            skip_unreadable(
                                root,
                                path,
                                &language,
                                &err.to_string(),
                                &mut ledger,
                                &mut warnings,
                            );
                            continue;
                        }
                    };
                    if bytes.contains(&0) {
                        document_elapsed_ms = document_elapsed_ms.saturating_add(
                            u64::try_from(document_started.elapsed().as_millis())
                                .unwrap_or(u64::MAX),
                        );
                        ledger.skip(
                            root,
                            path,
                            &language,
                            SkipReason::Binary,
                            SkipSource::Detector,
                            true,
                        );
                        continue;
                    }
                    let content = String::from_utf8_lossy(&bytes).into_owned();
                    if likely_generated(&content) {
                        document_elapsed_ms = document_elapsed_ms.saturating_add(
                            u64::try_from(document_started.elapsed().as_millis())
                                .unwrap_or(u64::MAX),
                        );
                        ledger.skip(
                            root,
                            path,
                            &language,
                            SkipReason::Generated,
                            SkipSource::Detector,
                            true,
                        );
                        continue;
                    }
                    document_paths.insert(rel.clone());
                    ledger.indexed(&language, false);
                    // Documents are prose, redacted like every other non-source file before
                    // anything is derived from them (see `Indexer::parse_file`).
                    // Prose unless the file's name says it holds credentials: the same rule
                    // `parse_file` applies, so a pasted token in `docs/SECRETS.md` is redacted
                    // whichever branch reads the file.
                    let kind = redaction::ContentKind::for_file(&rel, &language)
                        .unwrap_or(redaction::ContentKind::Prose);
                    let redacted = redaction::redact_secret_values(&content, kind);
                    redacted_files += usize::from(redacted.redactions > 0);
                    document_sections.extend(build_document_sections(
                        &rel,
                        &redacted.text,
                        document_type,
                    ));
                    document_elapsed_ms = document_elapsed_ms.saturating_add(
                        u64::try_from(document_started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    );
                    continue;
                }
            }
            if mode == IndexMode::Fast && fast_mode_skip_path(&rel) {
                ledger.skip(
                    root,
                    path,
                    &language,
                    SkipReason::FastMode,
                    SkipSource::FastMode,
                    true,
                );
                continue;
            }
            if !is_supported_code(&language) {
                ledger.skip(
                    root,
                    path,
                    &language,
                    SkipReason::UnsupportedLanguage,
                    SkipSource::LanguageSupport,
                    true,
                );
                continue;
            }
            let bytes = match fs::read(path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    skip_unreadable(
                        root,
                        path,
                        &language,
                        &err.to_string(),
                        &mut ledger,
                        &mut warnings,
                    );
                    continue;
                }
            };
            if bytes.contains(&0) {
                ledger.skip(
                    root,
                    path,
                    &language,
                    SkipReason::Binary,
                    SkipSource::Detector,
                    true,
                );
                continue;
            }
            let content = String::from_utf8_lossy(&bytes);
            // Generated source is indexed and flagged, never dropped: on a Python ML library
            // whose generated implementation files carry a "do not edit" banner, skipping them removed
            // 394 files and a tenth of the files real commits went on to change. Ranking
            // decides what a generated file is worth; the index must still know it exists.
            // .NET build tools name what they write rather than mark it (`*.g.cs`, `*.Designer.cs`).
            let generated_by = if likely_generated(&content) {
                Some(open_kioku_core::GeneratedBy::Banner)
            } else if likely_generated_path(&rel) {
                Some(open_kioku_core::GeneratedBy::BuildToolName)
            } else {
                None
            };
            let is_generated = generated_by.is_some();
            let content_hash = hash_bytes(&bytes);
            ledger.indexed(&language, is_generated);
            files.push(File {
                id: FileId::new(stable_id(&rel.to_string_lossy())),
                repository_id: repository_id.clone(),
                path: rel.clone(),
                language,
                size_bytes: metadata.len(),
                content_hash,
                is_generated,
                is_vendor: false,
                generated_by,
            });
            if should_emit_progress(scanned_files, 0) {
                progress.emit_transient(
                    ProgressEvent::new("scan")
                        .scanned(scanned_files)
                        .indexed(files.len())
                        .skipped(ledger.skipped_paths.len()),
                );
            }
        }
        // The walk is over; the filter holds the only other handle and is never called again.
        let pruned = std::mem::take(&mut *pruned.lock().unwrap_or_else(|err| err.into_inner()));
        ledger.record_pruned_dirs(root, &policy, pruned, &mut warnings);
        let fast_skipped = ledger
            .skipped_paths
            .iter()
            .filter(|path| path.reason == SkipReason::FastMode)
            .count();
        if fast_skipped > 0 {
            warnings.push(format!(
                "fast mode skipped {fast_skipped} code/example/testdata/sample path(s) from code analysis"
            ));
        }
        if files.is_empty() && source_like_files > 0 {
            let git_ignored = ledger
                .skipped_paths
                .iter()
                .filter(|path| path.source == SkipSource::GitIgnore)
                .count();
            warnings.push(format!(
                "discovery indexed no supported files from {source_like_files} source-like path(s) after scanning {scanned_files} file(s); Git ignore accounted for {git_ignored} skip(s); inspect skip counts and repository ignore rules"
            ));
        }
        progress.emit(
            ProgressEvent::new("scan")
                .scanned(scanned_files)
                .indexed(files.len())
                .total(Some(files.len()))
                .skipped(ledger.skipped_paths.len())
                .warnings(warnings.clone()),
        );
        let ScanLedger {
            skipped_paths,
            coverage,
            dependency_trees: _,
        } = ledger;
        let skipped = skipped_paths.len();
        // The walk yields directory entries in filesystem order, which differs between copies
        // of the same tree. Every later collection (chunks, symbols, search documents, storage
        // rows) inherits this order, so it is fixed to repository paths here.
        files.sort_by(|left, right| left.path.cmp(&right.path));
        document_sections.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.line_range.start.cmp(&right.line_range.start))
                .then_with(|| left.line_range.end.cmp(&right.line_range.end))
        });
        Ok(ScanResult {
            files,
            document_sections,
            document_file_count: document_paths.len(),
            document_elapsed_ms,
            skipped,
            warnings,
            skipped_paths,
            coverage,
            redacted_files,
        })
    }
}

#[derive(Debug, Clone)]
struct ScanResult {
    files: Vec<File>,
    document_sections: Vec<DocumentSection>,
    document_file_count: usize,
    document_elapsed_ms: u64,
    skipped: usize,
    warnings: Vec<String>,
    skipped_paths: Vec<SkippedPath>,
    coverage: IndexCoverage,
    /// Document-corpus files whose content had secret-like values redacted.
    redacted_files: usize,
}

/// Discovery's running record of what was left out and why. Coverage counts only
/// recognised languages, so a skipped README or lockfile never dilutes the source ratio.
#[derive(Default)]
struct ScanLedger {
    skipped_paths: Vec<SkippedPath>,
    coverage: IndexCoverage,
    /// Which directories of excluded files hold installed packages, probed once each.
    dependency_trees: dependency_trees::DependencyTrees,
}

impl ScanLedger {
    fn discovered(&mut self, language: &Language) {
        if is_supported_code(language) {
            self.coverage.record_discovered(language);
        }
    }

    fn indexed(&mut self, language: &Language, generated: bool) {
        if is_supported_code(language) {
            self.coverage.record_indexed(language, generated);
        }
    }

    fn skip(
        &mut self,
        root: &Path,
        path: &Path,
        language: &Language,
        reason: SkipReason,
        source: SkipSource,
        safe_to_show: bool,
    ) {
        push_skip(
            root,
            path,
            reason,
            source,
            safe_to_show,
            &mut self.skipped_paths,
        );
        if is_supported_code(language) {
            self.coverage.record_skipped(language, reason);
            if reason.is_policy() {
                // The directory is named only when the path itself may be shown.
                let rel = path.strip_prefix(root).unwrap_or(path);
                let top_dir = safe_to_show.then(|| top_level_dir(rel)).flatten();
                // Only programming-language source can be a gap; a hidden `.github/*.yml` is
                // never probed for.
                let dependency = (top_dir.is_some() && language.is_programming())
                    .then(|| self.dependency_trees.enclosing(root, rel))
                    .flatten();
                self.coverage.record_classified_policy_exclusion(
                    language,
                    source,
                    top_dir.as_deref(),
                    dependency
                        .as_ref()
                        .map(|(dir, evidence)| (dir.as_str(), *evidence)),
                );
            }
        }
    }
}

impl ScanLedger {
    /// Record the directories the walk pruned: each by path in `skipped_paths` and in coverage,
    /// with the Git-tracked programming-language files under it counted. Tracked source under a
    /// `build` or `dist` pruned only because nothing declares it may be a misclassified source
    /// directory, so each such file is discovered and skipped (`pruned`, or the policy that
    /// excludes it anyway). So is tracked source under a directory pruned as a submodule: Git
    /// tracks no file under a real one, so these are this repository's files behind a stray
    /// `.git`. Under a directory pruned on strong evidence (a cache tag, a build
    /// manifest beside it, installed packages) committed files are a published bundle or
    /// vendored packages: counted on the directory, never against the ratio.
    fn record_pruned_dirs(
        &mut self,
        root: &Path,
        policy: &path_policy::IndexPathPolicy,
        mut pruned: Vec<(PathBuf, PruneReason)>,
        warnings: &mut Vec<String>,
    ) {
        if pruned.is_empty() {
            return;
        }
        pruned.sort();
        let tracked = match git_ignore::tracked_files(root) {
            Ok(tracked) => tracked,
            Err(err) => {
                warnings.push(format!(
                    "could not list tracked files under pruned directories, so whether they hold committed source is unknown: {err}"
                ));
                None
            }
        };
        let rel_dirs = pruned
            .iter()
            .map(|(path, _)| path.strip_prefix(root).unwrap_or(path).to_path_buf())
            .collect::<Vec<_>>();
        let index_of = rel_dirs
            .iter()
            .enumerate()
            .map(|(index, rel)| (rel.as_path(), index))
            .collect::<HashMap<_, _>>();
        let mut tracked_counts = vec![0usize; pruned.len()];
        for file in tracked.iter().flatten() {
            // Pruned directories never nest: the walk stopped at the outermost one.
            let Some(index) = file
                .ancestors()
                .skip(1)
                .find_map(|ancestor| index_of.get(ancestor).copied())
            else {
                continue;
            };
            let language = detect_language(file);
            if !(is_supported_code(&language) && language.is_programming()) {
                continue;
            }
            tracked_counts[index] += 1;
            if !pruned[index].1.counts_tracked_source() {
                continue;
            }
            self.discovered(&language);
            let absolute = root.join(file);
            match policy.exclusion(file) {
                Some(exclusion) => self.skip(
                    root,
                    &absolute,
                    &language,
                    exclusion.reason,
                    exclusion.source,
                    exclusion.safe_to_show,
                ),
                None => self.skip(
                    root,
                    &absolute,
                    &language,
                    SkipReason::Pruned,
                    SkipSource::Detector,
                    true,
                ),
            }
        }
        let mut listed = Vec::with_capacity(pruned.len());
        let mut unlisted = 0;
        for ((path, reason), (rel, tracked_source)) in
            pruned.iter().zip(rel_dirs.iter().zip(tracked_counts))
        {
            let safe_to_show = policy
                .security_exclusion(rel)
                .is_none_or(|exclusion| exclusion.safe_to_show);
            push_skip(
                root,
                path,
                SkipReason::Pruned,
                SkipSource::Detector,
                safe_to_show,
                &mut self.skipped_paths,
            );
            if !safe_to_show {
                unlisted += 1;
                continue;
            }
            listed.push(PrunedDir {
                path: rel
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
                reason: *reason,
                tracked_source_files: tracked.is_some().then_some(tracked_source),
            });
        }
        self.coverage.record_pruned_dirs(listed, unlisted);
    }
}

/// The first component of a repository-relative path, or `.` for a file at the root;
/// what a policy-exclusion summary names so `1,485 hidden` reads as `under .claude/`.
fn top_level_dir(rel: &Path) -> Option<String> {
    let mut components = rel.components();
    let first = components.next()?;
    Some(if components.next().is_none() {
        ".".to_owned()
    } else {
        first.as_os_str().to_string_lossy().into_owned()
    })
}

#[derive(Debug, Clone)]
struct ProgressEvent {
    phase: &'static str,
    scanned_files: usize,
    indexed_files: usize,
    total_files: Option<usize>,
    nodes_added: usize,
    edges_added: usize,
    skipped: usize,
    warnings: Vec<String>,
}

impl ProgressEvent {
    fn new(phase: &'static str) -> Self {
        Self {
            phase,
            scanned_files: 0,
            indexed_files: 0,
            total_files: None,
            nodes_added: 0,
            edges_added: 0,
            skipped: 0,
            warnings: Vec::new(),
        }
    }

    fn scanned(mut self, value: usize) -> Self {
        self.scanned_files = value;
        self
    }

    fn indexed(mut self, value: usize) -> Self {
        self.indexed_files = value;
        self
    }

    fn total(mut self, value: Option<usize>) -> Self {
        self.total_files = value;
        self
    }

    fn skipped(mut self, value: usize) -> Self {
        self.skipped = value;
        self
    }

    fn nodes_added(mut self, value: usize) -> Self {
        self.nodes_added = value;
        self
    }

    fn edges_added(mut self, value: usize) -> Self {
        self.edges_added = value;
        self
    }

    fn warnings(mut self, value: Vec<String>) -> Self {
        self.warnings = value;
        self
    }

    fn warning(mut self, value: impl Into<String>) -> Self {
        self.warnings.push(value.into());
        self
    }
}

impl IndexProgress {
    fn from_event(started: Instant, event: ProgressEvent) -> Self {
        Self {
            phase: event.phase,
            elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            scanned_files: event.scanned_files,
            indexed_files: event.indexed_files,
            total_files: event.total_files,
            nodes_added: event.nodes_added,
            edges_added: event.edges_added,
            skipped: event.skipped,
            warnings: event.warnings,
        }
    }

    fn phase_report(&self) -> IndexPhaseReport {
        IndexPhaseReport {
            phase: self.phase.to_string(),
            elapsed_ms: self.elapsed_ms,
            duration_ms: None,
            scanned_files: self.scanned_files,
            indexed_files: self.indexed_files,
            document_files: None,
            document_sections: None,
            nodes_added: self.nodes_added,
            edges_added: self.edges_added,
            skipped: self.skipped,
            warnings: self.warnings.clone(),
        }
    }
}

fn emit_progress(
    on_progress: &dyn Fn(IndexProgress),
    phase_reports: &mut Vec<IndexPhaseReport>,
    started: Instant,
    event: ProgressEvent,
) {
    let progress = IndexProgress::from_event(started, event);
    phase_reports.push(progress.phase_report());
    on_progress(progress);
}

struct ProgressRecorder<'a> {
    on_progress: &'a dyn Fn(IndexProgress),
    started: Instant,
    phase_reports: &'a mut Vec<IndexPhaseReport>,
}

impl<'a> ProgressRecorder<'a> {
    fn new(
        on_progress: &'a dyn Fn(IndexProgress),
        started: Instant,
        phase_reports: &'a mut Vec<IndexPhaseReport>,
    ) -> Self {
        Self {
            on_progress,
            started,
            phase_reports,
        }
    }

    fn emit(&mut self, event: ProgressEvent) {
        emit_progress(self.on_progress, self.phase_reports, self.started, event);
    }

    fn emit_transient(&self, event: ProgressEvent) {
        (self.on_progress)(IndexProgress::from_event(self.started, event));
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct AnalysisCounts {
    static_facts: usize,
    resolver_facts: usize,
    registry_facts: usize,
    relationship_facts: usize,
    derived_facts: usize,
    runtime_facts: usize,
    validation_facts: usize,
    git_history_facts: usize,
    architecture_facts: usize,
}

fn excluded_test_targets(tests: &[TestTarget]) -> BTreeMap<TestExclusionReason, usize> {
    let mut excluded = BTreeMap::new();
    for reason in tests.iter().filter_map(TestTarget::validation_exclusion) {
        *excluded.entry(reason).or_default() += 1;
    }
    excluded
}

struct IndexQualityInput<'a> {
    root: &'a Path,
    config: &'a OkConfig,
    scip_report: Option<&'a ScipIndexReport>,
    test_count: usize,
    excluded_test_targets: Option<BTreeMap<TestExclusionReason, usize>>,
    import_count: usize,
    analysis: AnalysisCounts,
    quality_notes: &'a [QualityNote],
    mode: IndexMode,
    phase_reports: &'a [IndexPhaseReport],
    skipped_paths: &'a [SkippedPath],
    coverage: Option<IndexCoverage>,
    redacted_files: Option<usize>,
}

fn index_quality(input: IndexQualityInput<'_>) -> IndexQuality {
    let mut quality_notes = Vec::new();
    quality_notes.extend(input.quality_notes.iter().cloned());
    if !input.skipped_paths.is_empty() {
        quality_notes.push(QualityNote::new(
            QualityNoteKind::Discovery,
            format!(
                "discovery skipped {} path(s); inspect skip_counts/skipped_paths before treating evidence as complete",
                input.skipped_paths.len()
            ),
        ));
    }
    let root = input.root;
    let config = input.config;
    let analysis = input.analysis;
    let build_systems = detect_build_systems(root);
    let codeql_databases = detect_codeql_databases(root);
    let coverage_reports = count_analysis_artifacts(
        root,
        &[
            "jacoco.xml",
            "coverage.xml",
            "cobertura.xml",
            "lcov.info",
            ".lcov",
        ],
    );
    let junit_reports = count_analysis_artifacts(root, &["test-", "junit"]);
    let mut semantic_provider_notes = Vec::new();
    if !build_systems.is_empty() {
        semantic_provider_notes.push(format!(
            "build systems detected: {}",
            build_systems.join(", ")
        ));
    }
    if codeql_databases > 0 {
        semantic_provider_notes.push(format!(
            "CodeQL database artifacts detected: {codeql_databases}"
        ));
    }
    if coverage_reports > 0 {
        semantic_provider_notes.push(format!("coverage reports detected: {coverage_reports}"));
    }
    if junit_reports > 0 {
        semantic_provider_notes.push(format!("JUnit-style reports detected: {junit_reports}"));
    }
    if analysis.static_facts > 0 {
        semantic_provider_notes.push(format!(
            "language static analysis facts detected: {}",
            analysis.static_facts
        ));
    }
    if analysis.resolver_facts > 0 {
        semantic_provider_notes.push(format!(
            "import resolver facts detected: {}",
            analysis.resolver_facts
        ));
    }
    if analysis.registry_facts > 0 {
        semantic_provider_notes.push(format!(
            "symbol registry facts detected: {}",
            analysis.registry_facts
        ));
    }
    if analysis.relationship_facts > 0 {
        semantic_provider_notes.push(format!(
            "complexity/similarity relationship facts detected: {}",
            analysis.relationship_facts
        ));
    }
    if analysis.derived_facts > 0 {
        semantic_provider_notes.push(format!(
            "derived-file facts detected: {}",
            analysis.derived_facts
        ));
    }
    if analysis.runtime_facts > 0 {
        semantic_provider_notes.push(format!(
            "runtime analysis facts detected: {}",
            analysis.runtime_facts
        ));
    }
    if analysis.validation_facts > 0 {
        semantic_provider_notes.push(format!(
            "validation evidence facts detected: {}",
            analysis.validation_facts
        ));
    }
    if analysis.git_history_facts > 0 {
        semantic_provider_notes.push(format!(
            "git history co-change facts detected: {}",
            analysis.git_history_facts
        ));
    }
    if analysis.architecture_facts > 0 {
        semantic_provider_notes.push(format!(
            "architecture policy resolution facts detected: {}",
            analysis.architecture_facts
        ));
    }
    let scip_mode = format!("{:?}", config.scip.mode).to_ascii_lowercase();
    let mut quality = if let Some(report) = input.scip_report {
        if report.imported_paths.is_empty() {
            quality_notes.push(QualityNote::new(
                QualityNoteKind::Scip,
                "SCIP was enabled but no SCIP index was imported",
            ));
        }
        if report.withheld_documents > 0 {
            quality_notes.push(QualityNote::new(
                QualityNoteKind::Scip,
                format!(
                    "{} SCIP document(s) for paths the security policy excludes (secret-like or \
                     `[paths] deny`), or whose path is not repository-relative, were not \
                     imported; their symbols and references are not in the index",
                    report.withheld_documents
                ),
            ));
        }
        if report.exact_references == 0 {
            quality_notes.push(QualityNote::new(
                QualityNoteKind::ExactReferences,
                "exact reference coverage is unavailable; impact and test selection are heuristic",
            ));
        }
        for attempt in &report.generator_attempts {
            if !matches!(
                attempt.status,
                open_kioku_scip::ScipGeneratorStatus::Generated
                    | open_kioku_scip::ScipGeneratorStatus::Skipped
            ) {
                quality_notes.push(QualityNote::new(
                    QualityNoteKind::Scip,
                    format!(
                        "SCIP {} generation {:?}: {}",
                        attempt.language, attempt.status, attempt.message
                    ),
                ));
            }
        }
        IndexQuality {
            index_mode: input.mode,
            phase_reports: input.phase_reports.to_vec(),
            skip_counts: skip_counts(input.skipped_paths),
            skipped_paths: input.skipped_paths.to_vec(),
            scip_enabled: config.scip.enabled,
            scip_mode,
            scip_indexes_imported: report.imported_paths.len(),
            scip_symbols: report.symbols,
            scip_occurrences: report.occurrences,
            scip_exact_references: report.exact_references,
            test_count: input.test_count,
            excluded_test_targets: input.excluded_test_targets.clone(),
            import_count: input.import_count,
            build_systems,
            codeql_databases,
            coverage_reports,
            junit_reports,
            static_analysis_facts: analysis.static_facts,
            runtime_analysis_facts: analysis.runtime_facts,
            git_history_facts: analysis.git_history_facts,
            architecture_facts: analysis.architecture_facts,
            semantic_provider_notes,
            resolution_quality: None,
            coverage: input.coverage,
            redacted_files: input.redacted_files,
            // Set by the run that publishes this manifest, from the index it replaces.
            pending_pre_redaction_compaction: false,
            pending_deleted_content_clearing: false,
            pending_derived_store_pruning: false,
            quality_notes,
        }
    } else {
        if !config.scip.enabled {
            quality_notes.push(QualityNote::new(
                QualityNoteKind::Scip,
                "SCIP disabled; symbol references use tree-sitter/import heuristics",
            ));
        }
        IndexQuality {
            index_mode: input.mode,
            phase_reports: input.phase_reports.to_vec(),
            skip_counts: skip_counts(input.skipped_paths),
            skipped_paths: input.skipped_paths.to_vec(),
            scip_enabled: config.scip.enabled,
            scip_mode,
            scip_indexes_imported: 0,
            scip_symbols: 0,
            scip_occurrences: 0,
            scip_exact_references: 0,
            test_count: input.test_count,
            excluded_test_targets: input.excluded_test_targets.clone(),
            import_count: input.import_count,
            build_systems,
            codeql_databases,
            coverage_reports,
            junit_reports,
            static_analysis_facts: analysis.static_facts,
            runtime_analysis_facts: analysis.runtime_facts,
            git_history_facts: analysis.git_history_facts,
            architecture_facts: analysis.architecture_facts,
            semantic_provider_notes,
            resolution_quality: None,
            coverage: input.coverage,
            redacted_files: input.redacted_files,
            // Set by the run that publishes this manifest, from the index it replaces.
            pending_pre_redaction_compaction: false,
            pending_deleted_content_clearing: false,
            pending_derived_store_pruning: false,
            quality_notes,
        }
    };
    // Stored order was the discovery walk (readdir order) and rayon completion order.
    // The status sample takes the first entries of each kind and reason, so the lists
    // are sorted once here to read the same on every machine and run.
    quality.quality_notes.sort();
    quality.quality_notes.dedup();
    quality
        .skipped_paths
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.reason.cmp(&b.reason)));
    quality
}

struct GitHistoryIngest {
    snapshot: HistorySnapshot,
    analysis_facts: Vec<AnalysisFact>,
    quality_notes: Vec<QualityNote>,
}

impl GitHistoryIngest {
    fn empty() -> Self {
        Self {
            snapshot: HistorySnapshot::empty(),
            analysis_facts: Vec::new(),
            quality_notes: Vec::new(),
        }
    }
}

/// A commit whose patch could not be read still counts in history and co-change, which come
/// from `--name-status`; only its per-symbol touches are missing, and the index says so.
fn skipped_patch_notes(skipped: &[open_kioku_git::SkippedCommitPatch]) -> Vec<QualityNote> {
    let Some(first) = skipped.first() else {
        return Vec::new();
    };
    vec![QualityNote::new(
        QualityNoteKind::GitHistory,
        format!(
            "git history: the patches of {} commit(s) could not be read and were skipped, so \
             their symbol-level history touches are missing (first: {}: {})",
            skipped.len(),
            first.commit_id.0,
            first.reason
        ),
    )]
}

fn collect_git_history(
    root: &Path,
    files: &[File],
    symbols: &[Symbol],
    config: &OkConfig,
) -> Result<GitHistoryIngest> {
    let history = open_kioku_git::commit_history(root, config.history.max_commits)?;
    let patch_scan = open_kioku_git::commit_patches(root, config.history.max_commits)?;
    // History names every path a commit touched, including files discovery never reads, so it
    // applies the security rules discovery applies first (#525).
    let security = path_policy::SecurityPathPolicy::new(config)?;
    Ok(git_history_ingest(
        files,
        symbols,
        history,
        patch_scan,
        config.history.max_files_per_commit,
        &|path| security.exclusion(path).is_some(),
    ))
}

/// What history ingestion left out because the security policy excludes the path it names.
#[derive(Debug, Default)]
struct WithheldHistory {
    paths: BTreeSet<PathBuf>,
    file_touches: usize,
    /// Unordered pairs, counted before `MAX_HISTORY_COCHANGE_EDGES` caps the stored edges.
    cochange_pairs: usize,
}

impl WithheldHistory {
    /// Counts only: naming a withheld path here would defeat withholding it.
    fn quality_note(&self) -> Option<QualityNote> {
        (!self.paths.is_empty()).then(|| {
            QualityNote::new(
                QualityNoteKind::GitHistory,
                format!(
                    "git history: {} file touch(es) and {} co-change pair(s) (counted before \
                     the co-change edge cap) on {} path(s) the security policy excludes \
                     (secret-like or `[paths] deny`) were not stored",
                    self.file_touches,
                    self.cochange_pairs,
                    self.paths.len()
                ),
            )
        })
    }
}

/// Drop every history row that names a path `excluded` rejects: a file touch on such a path
/// (or renamed from one), its patch, and every co-change pair naming it. `records` are computed
/// before the drop, so a commit too large to count toward co-change stays too large, and the
/// pairs between the other files it touched are unchanged. A skipped patch's reason can quote
/// its entry's path, so an excluded path is masked there too.
fn withhold_excluded_history(
    history: &mut open_kioku_git::CommitHistory,
    patch_scan: &mut open_kioku_git::CommitPatchScan,
    records: &mut Vec<open_kioku_git::CochangeRecord>,
    excluded: &dyn Fn(&Path) -> bool,
) -> WithheldHistory {
    let mut paths = BTreeSet::new();
    let mut names_excluded = |path: &Path| {
        let hit = excluded(path);
        if hit {
            paths.insert(path.to_path_buf());
        }
        hit
    };
    // Both sides of a rename are judged: a touch renamed from `.env` must not keep its old name.
    let mut touch_excluded = |path: &Path, previous_path: Option<&Path>| {
        let path_excluded = names_excluded(path);
        let previous_excluded = previous_path.is_some_and(&mut names_excluded);
        path_excluded || previous_excluded
    };
    let touches_before = history.file_touches.len();
    history
        .file_touches
        .retain(|touch| !touch_excluded(&touch.path, touch.previous_path.as_deref()));
    for commit in &mut patch_scan.commits {
        commit
            .files
            .retain(|file| !touch_excluded(&file.path, file.previous_path.as_deref()));
    }
    // Records hold each pair in both directions; count it once.
    let mut cochange_pairs = 0;
    records.retain(|record| {
        let keep = !excluded(&record.path) && !excluded(&record.cochanged_path);
        if !keep && record.path < record.cochanged_path {
            cochange_pairs += 1;
        }
        keep
    });
    // Longest name first: masking `.env` inside `config/.env.local` first would leave
    // `config/[redacted].local`, which still names the file.
    let mut names = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    for skipped in &mut patch_scan.skipped {
        for name in &names {
            if skipped.reason.contains(name.as_str()) {
                skipped.reason = skipped.reason.replace(name.as_str(), "[redacted]");
            }
        }
    }
    WithheldHistory {
        file_touches: touches_before - history.file_touches.len(),
        cochange_pairs,
        paths,
    }
}

fn git_history_ingest(
    files: &[File],
    symbols: &[Symbol],
    mut history: open_kioku_git::CommitHistory,
    mut patch_scan: open_kioku_git::CommitPatchScan,
    max_files_per_commit: usize,
    excluded: &dyn Fn(&Path) -> bool,
) -> GitHistoryIngest {
    let mut cochange_records =
        open_kioku_git::cochange_records_from_history(&history, max_files_per_commit);
    let withheld = withhold_excluded_history(
        &mut history,
        &mut patch_scan,
        &mut cochange_records,
        excluded,
    );
    let mut quality_notes = skipped_patch_notes(&patch_scan.skipped);
    quality_notes.extend(withheld.quality_note());
    let symbol_touches = map_symbol_touches(files, symbols, &history, &patch_scan.commits);
    let cochange_edges = cochange_records
        .iter()
        .take(MAX_HISTORY_COCHANGE_EDGES)
        .map(|record| GitCochangeEdge {
            id: HistoryRecordId::new(stable_id(&format!(
                "git-cochange:{}:{}",
                record.path.display(),
                record.cochanged_path.display()
            ))),
            path: record.path.clone(),
            cochanged_path: record.cochanged_path.clone(),
            commit_count: record.commit_count,
            recency_weight: record.recency_weight,
            last_changed_at: record
                .commits
                .first()
                .and_then(|commit_id| {
                    history
                        .commits
                        .iter()
                        .find(|commit| commit.id.0 == *commit_id)
                })
                .map(|commit| commit.committed_at),
            sample_commits: record
                .commits
                .iter()
                .map(|commit_id| GitCommitId::new(commit_id.clone()))
                .collect(),
            test_corun: record.test_corun,
        })
        .collect::<Vec<_>>();
    let analysis_facts = git_history_facts(files, &cochange_records);
    GitHistoryIngest {
        snapshot: HistorySnapshot {
            schema_version: HISTORY_SCHEMA_VERSION,
            commits: history.commits,
            file_touches: history.file_touches,
            symbol_touches,
            cochange_edges,
            reviewer_evidence: Vec::new(),
        },
        analysis_facts,
        quality_notes,
    }
}

fn map_symbol_touches(
    files: &[File],
    symbols: &[Symbol],
    history: &open_kioku_git::CommitHistory,
    patches: &[open_kioku_git::CommitPatch],
) -> Vec<GitSymbolTouch> {
    #[derive(Clone)]
    struct MappedTouch {
        commit_id: GitCommitId,
        symbol: Symbol,
        file_path: std::path::PathBuf,
        change_kind: open_kioku_core::GitChangeKind,
        touched_at: chrono::DateTime<Utc>,
        line_ranges: Vec<LineRange>,
        confidence: Confidence,
        uncertainty: Vec<String>,
    }

    let files_by_path = files
        .iter()
        .map(|file| (normalize_history_path(&file.path), file))
        .collect::<HashMap<_, _>>();
    let file_paths_by_id = files
        .iter()
        .map(|file| (file.id.clone(), normalize_history_path(&file.path)))
        .collect::<HashMap<_, _>>();
    let mut canonical_by_path = files_by_path
        .keys()
        .map(|path| (path.clone(), path.clone()))
        .collect::<HashMap<_, _>>();

    for touch in &history.file_touches {
        let path = normalize_history_path(&touch.path);
        let Some(canonical) = canonical_by_path.get(&path).cloned() else {
            continue;
        };
        if let Some(previous_path) = &touch.previous_path {
            canonical_by_path.insert(normalize_history_path(previous_path), canonical);
        }
    }

    let mut symbols_by_path = HashMap::<String, Vec<&Symbol>>::new();
    for symbol in symbols {
        let Some(path) = file_paths_by_id.get(&symbol.file_id) else {
            continue;
        };
        symbols_by_path
            .entry(path.clone())
            .or_default()
            .push(symbol);
    }
    for symbols in symbols_by_path.values_mut() {
        symbols.sort_by(|left, right| {
            left.range
                .as_ref()
                .map(symbol_range_width)
                .cmp(&right.range.as_ref().map(symbol_range_width))
                .then_with(|| left.qualified_name.cmp(&right.qualified_name))
        });
    }

    let commits = history
        .commits
        .iter()
        .enumerate()
        .map(|(index, commit)| (commit.id.0.as_str(), (index, commit)))
        .collect::<HashMap<_, _>>();
    let file_touches = history
        .file_touches
        .iter()
        .map(|touch| {
            (
                (
                    touch.commit_id.0.as_str(),
                    normalize_history_path(&touch.path),
                ),
                touch,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut mapped = HashMap::<(String, String), MappedTouch>::new();

    for commit_patch in patches {
        let Some((commit_index, commit)) = commits.get(commit_patch.commit_id.0.as_str()) else {
            continue;
        };
        for file_patch in &commit_patch.files {
            let observed_path = normalize_history_path(&file_patch.path);
            let Some(canonical_path) = canonical_by_path.get(&observed_path) else {
                continue;
            };
            let Some(path_symbols) = symbols_by_path.get(canonical_path) else {
                continue;
            };
            let change_kind = file_touches
                .get(&(commit_patch.commit_id.0.as_str(), observed_path.clone()))
                .map(|touch| touch.change_kind)
                .unwrap_or(open_kioku_core::GitChangeKind::Unknown);

            for changed_range in &file_patch.line_ranges {
                let mut candidates = path_symbols
                    .iter()
                    .copied()
                    .filter(|symbol| {
                        symbol
                            .range
                            .as_ref()
                            .is_some_and(|range| ranges_overlap(range, changed_range))
                    })
                    .collect::<Vec<_>>();
                let Some(min_width) = candidates
                    .iter()
                    .filter_map(|symbol| symbol.range.as_ref().map(symbol_range_width))
                    .min()
                else {
                    continue;
                };
                candidates.retain(|symbol| {
                    symbol
                        .range
                        .as_ref()
                        .is_some_and(|range| symbol_range_width(range) == min_width)
                });
                candidates.sort_by(|left, right| left.qualified_name.cmp(&right.qualified_name));
                let ambiguous = candidates.len() > 1;

                for symbol in candidates {
                    let symbol_range = symbol
                        .range
                        .as_ref()
                        .expect("mapped symbol candidate has a line range");
                    let mapped_range = LineRange {
                        start: changed_range.start.max(symbol_range.start),
                        end: changed_range.end.min(symbol_range.end),
                    };
                    let mut confidence = if *commit_index == 0 {
                        Confidence::High
                    } else {
                        Confidence::Medium
                    };
                    confidence = lower_confidence(confidence, symbol.confidence);
                    let mut uncertainty = Vec::new();
                    if *commit_index > 0 {
                        uncertainty.push(
                            "historical patch coordinates were mapped onto the current symbol range; later edits may have shifted boundaries"
                                .into(),
                        );
                    }
                    if ambiguous {
                        confidence = Confidence::Low;
                        uncertainty.push(
                            "the changed line range overlaps multiple equally specific current symbols"
                                .into(),
                        );
                    }
                    if observed_path != *canonical_path {
                        uncertainty.push(format!(
                            "the historical path `{observed_path}` was mapped through rename history to `{canonical_path}`"
                        ));
                    }

                    let key = (commit_patch.commit_id.0.clone(), symbol.id.0.clone());
                    let entry = mapped.entry(key).or_insert_with(|| MappedTouch {
                        commit_id: commit_patch.commit_id.clone(),
                        symbol: symbol.clone(),
                        file_path: std::path::PathBuf::from(canonical_path),
                        change_kind,
                        touched_at: commit.committed_at,
                        line_ranges: Vec::new(),
                        confidence,
                        uncertainty: Vec::new(),
                    });
                    entry.line_ranges.push(mapped_range);
                    entry.confidence = lower_confidence(entry.confidence, confidence);
                    entry.uncertainty.extend(uncertainty);
                }
            }
        }
    }

    let commit_order = history
        .commits
        .iter()
        .enumerate()
        .map(|(index, commit)| (commit.id.0.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut touches = mapped
        .into_values()
        .map(|mut touch| {
            touch
                .line_ranges
                .sort_by_key(|range| (range.start, range.end));
            touch.line_ranges.dedup();
            touch.uncertainty.sort();
            touch.uncertainty.dedup();
            GitSymbolTouch {
                id: HistoryRecordId::new(stable_id(&format!(
                    "git-symbol-touch:{}:{}",
                    touch.commit_id.0, touch.symbol.id.0
                ))),
                commit_id: touch.commit_id,
                symbol_id: Some(touch.symbol.id),
                qualified_name: touch.symbol.qualified_name,
                file_path: touch.file_path,
                change_kind: touch.change_kind,
                line_ranges: touch.line_ranges,
                confidence: touch.confidence,
                uncertainty: touch.uncertainty,
                touched_at: touch.touched_at,
            }
        })
        .collect::<Vec<_>>();
    touches.sort_by(|left, right| {
        commit_order
            .get(left.commit_id.0.as_str())
            .cmp(&commit_order.get(right.commit_id.0.as_str()))
            .then_with(|| left.file_path.cmp(&right.file_path))
            .then_with(|| left.qualified_name.cmp(&right.qualified_name))
            // Overloads share a qualified name in one file and commit; without the id they kept
            // the hash order they were collected in.
            .then_with(|| left.symbol_id.cmp(&right.symbol_id))
    });
    touches
}

fn symbol_range_width(range: &LineRange) -> u32 {
    range.end.saturating_sub(range.start)
}

fn ranges_overlap(left: &LineRange, right: &LineRange) -> bool {
    left.start <= right.end && right.start <= left.end
}

fn lower_confidence(left: Confidence, right: Confidence) -> Confidence {
    if confidence_rank(left) <= confidence_rank(right) {
        left
    } else {
        right
    }
}

fn confidence_rank(confidence: Confidence) -> u8 {
    match confidence {
        Confidence::Low => 0,
        Confidence::Medium => 1,
        Confidence::High => 2,
        Confidence::Exact => 3,
    }
}

fn git_history_facts(
    files: &[File],
    records: &[open_kioku_git::CochangeRecord],
) -> Vec<AnalysisFact> {
    let files_by_path = files
        .iter()
        .map(|file| (normalize_history_path(&file.path), file))
        .collect::<HashMap<_, _>>();
    let mut facts = Vec::new();
    for record in records {
        let Some(file) = files_by_path.get(&normalize_history_path(&record.path)) else {
            continue;
        };
        if !files_by_path.contains_key(&normalize_history_path(&record.cochanged_path)) {
            continue;
        }
        let id = stable_id(&format!(
            "git-history:{}:{}",
            record.path.display(),
            record.cochanged_path.display()
        ));
        let mut message = format!(
            "git co-change observed in {} commit(s), recency weight {:.2}",
            record.commit_count, record.recency_weight
        );
        if record.test_corun {
            message.push_str("; includes historical path-to-test co-run");
        }
        let message = compact_message(message);
        facts.push(AnalysisFact {
            id,
            file_id: file.id.clone(),
            symbol_id: None,
            target: normalize_history_path(&record.cochanged_path),
            target_kind: if record.test_corun {
                GraphNodeType::Test
            } else {
                GraphNodeType::File
            },
            target_symbol_id: None,
            ambiguity: Vec::new(),
            edge_type: GraphEdgeType::ChangedBy,
            range: None,
            confidence: Confidence::from_score((0.45 + record.recency_weight / 4.0).min(0.90)),
            source: format!("git-history:{}", record.commits.join(",")).into(),
            source_type: EvidenceSourceType::GitHistory,
            message: message.into(),
        });
        if facts.len() >= 5000 {
            break;
        }
    }
    dedupe_analysis_facts(facts)
}

fn detect_build_systems(root: &Path) -> Vec<String> {
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
        if paths.iter().any(|path| root.join(path).exists()) {
            systems.push(name.to_string());
        }
    }
    systems
}

fn detect_codeql_databases(root: &Path) -> usize {
    [
        ".ok/codeql",
        "codeql-db",
        "codeql-database",
        ".codeql/database",
    ]
    .iter()
    .filter(|path| {
        let path = root.join(path);
        path.is_dir()
            && (path.join("db-java").exists()
                || path.join("codeql-database.yml").exists()
                || path.join("log").exists())
    })
    .count()
}

fn count_analysis_artifacts(root: &Path, names: &[&str]) -> usize {
    let candidates = [
        root.join(".ok/analysis"),
        root.join("build/reports"),
        root.join("target/site"),
        root.join("coverage"),
    ];
    let mut count = 0;
    for candidate in candidates {
        if !candidate.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(candidate)
            .max_depth(5)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if names.iter().any(|needle| file_name.contains(needle)) {
                count += 1;
            }
        }
    }
    count
}

fn normalize_history_path(path: &Path) -> String {
    normalize_path(&path.to_string_lossy())
}

fn normalize_path(value: &str) -> String {
    value.trim_start_matches("./").replace('\\', "/")
}

fn collect_runtime_analysis_facts(
    root: &Path,
    files: &[File],
    symbols: &[Symbol],
) -> Result<Vec<AnalysisFact>> {
    runtime::collect_runtime_analysis_facts(root, files, symbols)
}

fn collect_validation_analysis_facts(
    root: &Path,
    files: &[File],
    symbols: &[Symbol],
    tests: &[TestTarget],
) -> Result<Vec<AnalysisFact>> {
    validation::collect_validation_analysis_facts(root, files, symbols, tests)
}

fn collect_relationship_analysis_facts(
    files: &[File],
    symbols: &[Symbol],
    chunks: &[CodeChunk],
    existing_facts: &[AnalysisFact],
    mode: IndexMode,
    semantic: &open_kioku_config::SemanticConfig,
) -> Vec<AnalysisFact> {
    relationships::collect_relationship_analysis_facts(
        files,
        symbols,
        chunks,
        existing_facts,
        mode,
        semantic,
    )
}

fn dedupe_analysis_facts(mut facts: Vec<AnalysisFact>) -> Vec<AnalysisFact> {
    let mut seen = HashSet::new();
    facts.retain(|fact| seen.insert(fact.id.clone()));
    facts
}

fn should_emit_progress(done: usize, total: usize) -> bool {
    done == total || done % 500 == 0
}

fn mode_quality_notes(mode: IndexMode) -> Vec<QualityNote> {
    let message = match mode {
        IndexMode::Full => return Vec::new(),
        IndexMode::Balanced => {
            "balanced mode: trust-critical passes enabled; expensive optional passes may be skipped when configured"
        }
        IndexMode::Fast => {
            "fast mode: code analysis may skip docs/examples/generated/vendor/testdata/unsupported/oversized paths; allowed documentation is indexed separately in the lightweight document corpus"
        }
        IndexMode::CrossProject => {
            "cross-project mode: source parsing skipped; link already-indexed projects only"
        }
    };
    vec![QualityNote::new(QualityNoteKind::IndexMode, message)]
}

const MAX_DOCUMENT_SECTION_LINES: usize = 120;

fn document_type_for_path(path: &Path, plain_text_paths: &GlobSet) -> Option<DocumentType> {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
    let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    let readme = matches!(
        name.as_str(),
        "readme" | "readme.md" | "readme.mdx" | "readme.txt" | "readme.rst" | "readme.adoc"
    );
    let markdown = matches!(extension.as_deref(), Some("md"));
    let mdx = matches!(extension.as_deref(), Some("mdx"));
    let plain_text =
        matches!(extension.as_deref(), Some("txt")) && (readme || plain_text_paths.is_match(path));
    if !(readme || markdown || mdx || plain_text) {
        return None;
    }

    if readme {
        return Some(DocumentType::Readme);
    }
    if normalized
        .split('/')
        .any(|component| matches!(component, "adr" | "adrs" | "decisions"))
        || name.starts_with("adr-")
        || name.starts_with("adr_")
    {
        return Some(DocumentType::Adr);
    }
    if normalized.contains("architecture")
        || name.contains("architecture")
        || name.contains("design")
    {
        return Some(DocumentType::Architecture);
    }
    if name.starts_with("contributing")
        || name.contains("developer")
        || name.contains("development")
    {
        return Some(DocumentType::Guide);
    }
    if markdown {
        Some(DocumentType::Markdown)
    } else if mdx {
        Some(DocumentType::Mdx)
    } else {
        Some(DocumentType::PlainText)
    }
}

fn build_document_sections(
    path: &Path,
    content: &str,
    document_type: DocumentType,
) -> Vec<DocumentSection> {
    if is_markdown_like_document(path) {
        build_markdown_document_sections(path, content, document_type)
    } else {
        let lines = content.lines().map(str::to_string).collect::<Vec<_>>();
        let mut sections = Vec::new();
        push_bounded_document_sections(&mut sections, path, Vec::new(), 1, &lines, document_type);
        sections
    }
}

fn is_markdown_like_document(path: &Path) -> bool {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);
    matches!(extension.as_deref(), Some("md" | "mdx"))
        || path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("readme"))
}

fn build_markdown_document_sections(
    path: &Path,
    content: &str,
    document_type: DocumentType,
) -> Vec<DocumentSection> {
    let mut sections = Vec::new();
    let mut heading_stack = Vec::<String>::new();
    let mut current_heading_path = Vec::<String>::new();
    let mut current_start = 1u32;
    let mut current_lines = Vec::<String>::new();
    let mut fence: Option<(char, usize)> = None;

    for (index, line) in content.lines().enumerate() {
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        let fence_boundary = document_fence_boundary(line);
        let heading = if fence.is_none() && fence_boundary.is_none() {
            document_markdown_heading(line)
        } else {
            None
        };
        if let Some((level, title)) = heading {
            if !current_lines.is_empty() {
                push_bounded_document_sections(
                    &mut sections,
                    path,
                    current_heading_path.clone(),
                    current_start,
                    &current_lines,
                    document_type,
                );
                current_lines.clear();
            }
            update_document_heading_stack(&mut heading_stack, level, title);
            current_heading_path = heading_stack.clone();
            current_start = line_number;
        } else if current_lines.is_empty() {
            current_heading_path = heading_stack.clone();
            current_start = line_number;
        }
        current_lines.push(line.to_string());
        if let Some((marker, width)) = fence_boundary {
            match fence {
                Some((open_marker, open_width)) if marker == open_marker && width >= open_width => {
                    fence = None;
                }
                None => fence = Some((marker, width)),
                _ => {}
            }
        }
    }

    if !current_lines.is_empty() {
        push_bounded_document_sections(
            &mut sections,
            path,
            current_heading_path,
            current_start,
            &current_lines,
            document_type,
        );
    }
    sections
}

fn push_bounded_document_sections(
    sections: &mut Vec<DocumentSection>,
    path: &Path,
    heading_path: Vec<String>,
    start_line: u32,
    lines: &[String],
    document_type: DocumentType,
) {
    for (index, window) in lines.chunks(MAX_DOCUMENT_SECTION_LINES).enumerate() {
        let text = window.join("\n");
        if text.trim().is_empty() {
            continue;
        }
        let offset =
            u32::try_from(index.saturating_mul(MAX_DOCUMENT_SECTION_LINES)).unwrap_or(u32::MAX);
        let start = start_line.saturating_add(offset);
        let end =
            start.saturating_add(u32::try_from(window.len().saturating_sub(1)).unwrap_or(u32::MAX));
        sections.push(DocumentSection {
            path: path.to_path_buf(),
            heading_path: heading_path.clone(),
            line_range: LineRange { start, end },
            content_hash: hash_bytes(text.as_bytes()),
            content: text,
            document_type,
        });
    }
}

fn document_fence_boundary(line: &str) -> Option<(char, usize)> {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }
    let width = trimmed.chars().take_while(|ch| *ch == marker).count();
    (width >= 3).then_some((marker, width))
}

fn document_markdown_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let remainder = &trimmed[level..];
    if !remainder.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let title = remainder.trim();
    (!title.is_empty()).then_some((level, title))
}

fn update_document_heading_stack(headings: &mut Vec<String>, level: usize, title: &str) {
    let parent_count = level.saturating_sub(1);
    if headings.len() > parent_count {
        headings.truncate(parent_count);
    }
    headings.push(title.to_string());
}

fn fast_mode_skip_path(path: &Path) -> bool {
    path.components().any(|component| {
        let value = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        matches!(
            value.as_str(),
            "docs"
                | "doc"
                | "examples"
                | "example"
                | "testdata"
                | "fixtures"
                | "fixture"
                | "samples"
                | "sample"
                | "generated"
                | "vendor"
                | "third_party"
        ) || value.contains(".generated.")
            || value.ends_with(".generated.rs")
            || value.ends_with(".generated.ts")
            || value.ends_with(".generated.js")
    })
}

#[derive(Debug)]
struct ScopedIgnoreMatcher {
    layers: Vec<(PathBuf, Gitignore)>,
}

impl ScopedIgnoreMatcher {
    fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let mut ignored = false;
        for (scope, matcher) in &self.layers {
            if !path.starts_with(scope) {
                continue;
            }
            let matched = matcher.matched_path_or_any_parents(path, is_dir);
            if matched.is_ignore() {
                ignored = true;
            } else if matched.is_whitelist() {
                ignored = false;
            }
        }
        ignored
    }
}

fn build_ignore_matcher(
    pruner: &prune::DiscoveryPruner,
    file_name: &str,
) -> Result<ScopedIgnoreMatcher> {
    let root = pruner.root();
    let mut ignore_files = Vec::new();
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .ignore(false)
        .follow_links(false)
        .filter_entry({
            let pruner = pruner.clone();
            move |entry| {
                let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
                pruner.classify(entry.path(), is_dir) == prune::DirVerdict::Walk
            }
        })
        .build()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if entry.file_type().is_some_and(|kind| kind.is_file()) && entry.file_name() == file_name {
            ignore_files.push(entry.path().to_path_buf());
        }
    }

    ignore_files.sort_by(|left, right| {
        let left_depth = left
            .parent()
            .map(|path| path.components().count())
            .unwrap_or(0);
        let right_depth = right
            .parent()
            .map(|path| path.components().count())
            .unwrap_or(0);
        left_depth.cmp(&right_depth).then_with(|| left.cmp(right))
    });

    let mut layers = Vec::with_capacity(ignore_files.len());
    for ignore_file in ignore_files {
        let scope = ignore_file.parent().unwrap_or(root).to_path_buf();
        let mut builder = GitignoreBuilder::new(&scope);
        if let Some(err) = builder.add(&ignore_file) {
            return Err(OkError::Config(err.to_string()));
        }
        let matcher = builder
            .build()
            .map_err(|err| OkError::Config(err.to_string()))?;
        layers.push((scope, matcher));
    }
    Ok(ScopedIgnoreMatcher { layers })
}

fn push_skip(
    root: &Path,
    path: &Path,
    reason: SkipReason,
    source: SkipSource,
    safe_to_show: bool,
    skipped_paths: &mut Vec<SkippedPath>,
) {
    let rel = path.strip_prefix(root).unwrap_or(path);
    skipped_paths.push(SkippedPath {
        path: if safe_to_show {
            rel.to_path_buf()
        } else {
            PathBuf::from("[redacted]")
        },
        reason,
        source,
        safe_to_show,
    });
}

/// A file the walker listed but the indexer could not open. The path passed every policy check
/// before we tried to read it, so showing it is safe; the OS message carries no file content.
fn skip_unreadable(
    root: &Path,
    path: &Path,
    language: &Language,
    error: &str,
    ledger: &mut ScanLedger,
    warnings: &mut Vec<String>,
) {
    let rel = path.strip_prefix(root).unwrap_or(path);
    warnings.push(format!("skipped {}: {error}", rel.display()));
    ledger.skip(
        root,
        path,
        language,
        SkipReason::Error,
        SkipSource::Filesystem,
        true,
    );
}

fn skip_counts(skipped_paths: &[SkippedPath]) -> BTreeMap<SkipReason, usize> {
    let mut counts = BTreeMap::new();
    for skipped in skipped_paths {
        *counts.entry(skipped.reason).or_insert(0) += 1;
    }
    counts
}

fn is_hidden_path(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str().to_string_lossy().starts_with('.'))
}

fn compile_globs(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|err| OkError::Config(err.to_string()))?);
    }
    builder
        .build()
        .map_err(|err| OkError::Config(err.to_string()))
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn stable_id(value: &str) -> String {
    hash_bytes(value.as_bytes())
}

fn extract_imports_from_syntax(sites: &[open_kioku_core::ImportSite]) -> Vec<Import> {
    // A stored import row is keyed by file, path and start line. A grouped Rust `use` emits one
    // site per binding, so `use a::{B, B as C};` names one path twice on one line; the bindings
    // stay distinct in the import registry, which reads the sites rather than these rows.
    let mut seen = HashSet::new();
    sites
        .iter()
        .filter(|site| seen.insert((&site.file_id, site.source.as_str(), site.range.start_line)))
        .map(|site| Import {
            file_id: site.file_id.clone(),
            imported: site.source.clone(),
            range: Some(LineRange {
                start: site.range.start_line,
                end: site.range.end_line,
            }),
            confidence: Confidence::High,
        })
        .collect()
}

fn dedupe_symbols(symbols: &mut Vec<Symbol>) {
    let mut seen = HashSet::new();
    symbols.retain(|symbol| seen.insert(symbol.id.clone()));
}

fn derive_occurrences(_chunks: &[CodeChunk], symbols: &[Symbol]) -> Vec<SymbolOccurrence> {
    let mut occurrences = symbols
        .iter()
        .map(|symbol| SymbolOccurrence {
            symbol_id: symbol.id.clone(),
            file_id: symbol.file_id.clone(),
            range: symbol.range.clone(),
            source_range: None,
            is_definition: true,
            confidence: symbol.confidence,
            provenance: symbol.provenance.clone(),
        })
        .collect::<Vec<_>>();
    occurrences.sort_by(|a, b| {
        (
            &a.symbol_id.0,
            &a.file_id.0,
            a.range.as_ref().map(|r| r.start),
            a.is_definition,
        )
            .cmp(&(
                &b.symbol_id.0,
                &b.file_id.0,
                b.range.as_ref().map(|r| r.start),
                b.is_definition,
            ))
    });
    occurrences.dedup_by(|a, b| {
        a.symbol_id == b.symbol_id
            && a.file_id == b.file_id
            && a.range == b.range
            && a.is_definition == b.is_definition
    });
    occurrences
}

fn collect_architecture_facts(
    resolver: &open_kioku_architecture::PolicyResolver,
    files: &[File],
    symbols: &[Symbol],
) -> Vec<AnalysisFact> {
    use open_kioku_core::{Confidence, EvidenceSourceType, GraphEdgeType, GraphNodeType};
    let mut facts = Vec::new();

    // Process files
    for file in files {
        let path = file.path.display().to_string();
        let matches = resolver.resolve_file(&path);
        if matches.is_empty() {
            facts.push(AnalysisFact {
                id: stable_id(&format!("arch:unmapped:file:{}", path)),
                file_id: file.id.clone(),
                symbol_id: None,
                target: "UNMAPPED_ARCHITECTURE".into(),
                target_kind: GraphNodeType::ArchitectureComponent,
                target_symbol_id: None,
                ambiguity: Vec::new(),
                edge_type: GraphEdgeType::BelongsTo,
                range: None,
                confidence: Confidence::Exact,
                source: "policy_resolver".into(),
                source_type: EvidenceSourceType::Heuristic,
                message: "file does not match any architecture policy globs".into(),
            });
        } else {
            for comp_match in matches {
                facts.push(AnalysisFact {
                    id: stable_id(&format!("arch:file:{}:{}", path, comp_match.component_id)),
                    file_id: file.id.clone(),
                    symbol_id: None,
                    target: comp_match.component_id.clone(),
                    target_kind: GraphNodeType::ArchitectureComponent,
                    target_symbol_id: None,
                    ambiguity: Vec::new(),
                    edge_type: GraphEdgeType::BelongsTo,
                    range: None,
                    confidence: Confidence::Exact,
                    source: format!("glob:{}", comp_match.matched_glob).into(),
                    source_type: EvidenceSourceType::Heuristic,
                    message: "file mapped to architecture component via policy".into(),
                });
            }
        }
    }

    // Process symbols
    let mut files_by_id = std::collections::HashMap::new();
    for file in files {
        files_by_id.insert(file.id.clone(), file.path.display().to_string());
    }

    for symbol in symbols {
        if let Some(path) = files_by_id.get(&symbol.file_id) {
            let matches = resolver.resolve_file(path);
            if matches.is_empty() {
                facts.push(AnalysisFact {
                    id: stable_id(&format!("arch:unmapped:symbol:{}", symbol.id.0)),
                    file_id: symbol.file_id.clone(),
                    symbol_id: Some(symbol.id.clone()),
                    target: "UNMAPPED_ARCHITECTURE".into(),
                    target_kind: GraphNodeType::ArchitectureComponent,
                    target_symbol_id: None,
                    ambiguity: Vec::new(),
                    edge_type: GraphEdgeType::BelongsTo,
                    range: symbol.range.clone(),
                    confidence: Confidence::Exact,
                    source: "policy_resolver".into(),
                    source_type: EvidenceSourceType::Heuristic,
                    message: "symbol does not match any architecture policy globs".into(),
                });
            } else {
                for comp_match in matches {
                    facts.push(AnalysisFact {
                        id: stable_id(&format!(
                            "arch:symbol:{}:{}",
                            symbol.id.0, comp_match.component_id
                        )),
                        file_id: symbol.file_id.clone(),
                        symbol_id: Some(symbol.id.clone()),
                        target: comp_match.component_id.clone(),
                        target_kind: GraphNodeType::ArchitectureComponent,
                        target_symbol_id: None,
                        ambiguity: Vec::new(),
                        edge_type: GraphEdgeType::BelongsTo,
                        range: symbol.range.clone(),
                        confidence: Confidence::Exact,
                        source: format!("glob:{}", comp_match.matched_glob).into(),
                        source_type: EvidenceSourceType::Heuristic,
                        message: "symbol mapped to architecture component via policy".into(),
                    });
                }
            }
        }
    }

    dedupe_analysis_facts(facts)
}

#[cfg(test)]
mod tests {
    use super::{
        attach_resolution_quality, derive_occurrences, git_history_ingest, map_symbol_touches,
        Indexer,
    };
    use chrono::{TimeZone, Utc};
    use open_kioku_config::OkConfig;
    use open_kioku_core::{
        CodeChunk, Confidence, EvidenceSourceType, File, FileId, GitChangeKind, GitCommitId,
        GitCommitRecord, GitFileTouch, HistoryRecordId, IndexMode, Language, LineRange, Owner,
        QualityNoteKind, RepositoryId, SkipReason, SkipSource, Symbol, SymbolId, SymbolKind,
    };
    use std::process::Command;

    fn symbol(id: &str, name: &str, line: u32) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("src::index::{name}"),
            kind: SymbolKind::Function,
            file_id: FileId::new(format!("file-{id}")),
            range: Some(LineRange::single(line)),
            language: Language::TypeScript,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
            alias_of: None,
        }
    }

    #[test]
    fn candidate_cap_hits_are_visible_in_index_quality() {
        let mut quality = open_kioku_core::IndexQuality::default();
        let report = open_kioku_core::ResolutionQualityReport {
            candidate_cap_hits: 2,
            ..Default::default()
        };
        attach_resolution_quality(&mut quality, Some(report));

        assert_eq!(
            quality
                .resolution_quality
                .as_ref()
                .map(|report| report.candidate_cap_hits),
            Some(2)
        );
        assert!(quality.quality_notes.iter().any(|note| {
            note.kind == QualityNoteKind::RelationshipResolution
                && note.message.contains("candidate cap")
                && note.message.contains("2 occurrence(s)")
                && note
                    .message
                    .contains("authoritative emission was suppressed")
        }));
    }

    #[test]
    fn derive_occurrences_records_definitions_only_for_heuristic_indexing() {
        let symbols = vec![symbol("retry", "retry", 1), symbol("render", "render", 2)];
        let chunks = vec![CodeChunk {
            id: "chunk".into(),
            file_id: FileId::new("file-chunk"),
            range: LineRange { start: 10, end: 12 },
            language: Language::TypeScript,
            text: "retry(); const retried = true;".into(),
            symbol_id: None,
        }];

        let occurrences = derive_occurrences(&chunks, &symbols);
        let definitions = occurrences
            .iter()
            .filter(|occurrence| occurrence.is_definition)
            .count();
        let references = occurrences
            .iter()
            .filter(|occurrence| !occurrence.is_definition)
            .count();

        assert_eq!(definitions, 2);
        assert_eq!(references, 0);
    }

    #[test]
    fn index_manifest_records_build_and_analysis_provider_signals() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("settings.gradle"), "").unwrap();
        std::fs::create_dir_all(root.join("src/test/java/org/example")).unwrap();
        std::fs::write(
            root.join("src/test/java/org/example/ExampleTests.java"),
            r#"package org.example;
import org.springframework.web.bind.annotation.GetMapping;
class ExampleTests extends BaseTests {
  @GetMapping("/example")
  void works() {
    System.getenv("EXAMPLE_REGION");
    helper();
  }
}
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/test/java/org/example/Util.java"),
            r#"package org.example;
class Util {
  void helper() {}
}
"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".ok/analysis")).unwrap();
        std::fs::write(root.join(".ok/analysis/jacoco.xml"), "<report/>").unwrap();
        std::fs::write(
            root.join(".ok/analysis/TEST-org.example.ExampleTests.xml"),
            "<testsuite/>",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".ok/runtime")).unwrap();
        std::fs::write(
            root.join(".ok/runtime/spans.jsonl"),
            r#"{"file":"src/test/java/org/example/ExampleTests.java","line":4,"attributes":{"http.route":"/example","http.request.method":"GET","db.statement":"select * from example_orders"}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".ok/runtime/incidents.jsonl"),
            r#"{"file":"src/test/java/org/example/ExampleTests.java","line":5,"error.message":"checkout failure after runtime request"}"#,
        )
        .unwrap();

        let mut config = OkConfig::default();
        config.scip.enabled = false;
        let snapshot = Indexer::default().index_repo(root, &config).unwrap();

        assert!(snapshot
            .manifest
            .quality
            .build_systems
            .contains(&"gradle".to_string()));
        assert_eq!(snapshot.manifest.quality.coverage_reports, 1);
        assert_eq!(snapshot.manifest.quality.junit_reports, 1);
        assert!(snapshot.manifest.quality.static_analysis_facts >= 3);
        assert_eq!(snapshot.manifest.quality.runtime_analysis_facts, 6);
        assert!(!snapshot.import_resolutions.is_empty());
        assert!(snapshot
            .analysis_facts
            .iter()
            .any(|fact| fact.source.starts_with("open-kioku-import-resolver/")));
        assert!(snapshot
            .manifest
            .quality
            .semantic_provider_notes
            .iter()
            .any(|note| note.contains("symbol registry facts detected")));
        assert!(snapshot
            .analysis_facts
            .iter()
            .any(|fact| fact.target == "GET /example"));
        assert!(snapshot
            .analysis_facts
            .iter()
            .any(|fact| fact.target == "example_orders"));
        assert!(snapshot
            .analysis_facts
            .iter()
            .any(|fact| fact.target == "checkout failure after runtime request"));
        assert!(snapshot.analysis_facts.iter().any(|fact| {
            fact.target == "GET /example"
                && fact.message.contains("runtime aggregate observed")
                && fact.message.contains("error_rate")
        }));
        assert!(snapshot.analysis_facts.iter().any(|fact| {
            fact.target == "checkout failure after runtime request"
                && fact.message.contains("runtime aggregate observed")
        }));
        assert!(snapshot
            .manifest
            .quality
            .semantic_provider_notes
            .iter()
            .any(|note| note.contains("build systems detected")));
        assert!(snapshot
            .manifest
            .quality
            .semantic_provider_notes
            .iter()
            .any(|note| note.contains("import resolver facts detected")));
    }

    #[test]
    fn index_modes_are_stored_with_phase_reports_and_caveats() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(root.join("examples")).unwrap();
        std::fs::create_dir_all(root.join("testdata")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
        std::fs::write(root.join("docs/guide.rs"), "pub fn docs_only() {}\n").unwrap();
        std::fs::write(root.join("examples/demo.rs"), "pub fn demo() {}\n").unwrap();
        std::fs::write(root.join("testdata/case.rs"), "pub fn fixture() {}\n").unwrap();
        std::fs::write(
            root.join("src/schema.generated.rs"),
            "// @generated\npub fn generated() {}\n",
        )
        .unwrap();

        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;

        let full = Indexer::default().index_repo(root, &config).unwrap();
        assert_eq!(full.manifest.index_mode, IndexMode::Full);
        assert_eq!(full.manifest.quality.index_mode, IndexMode::Full);
        assert!(!full.manifest.phase_reports.is_empty());
        assert!(full
            .manifest
            .phase_reports
            .iter()
            .any(|report| report.phase == "scan"));

        let fast = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Fast)
            .unwrap();
        assert_eq!(fast.manifest.index_mode, IndexMode::Fast);
        assert_eq!(fast.manifest.quality.index_mode, IndexMode::Fast);
        assert_eq!(fast.manifest.file_count, 1);
        assert!(fast
            .manifest
            .quality
            .quality_notes
            .iter()
            .any(|note| note.kind == QualityNoteKind::IndexMode
                && note.message.contains("fast mode")));
        assert!(fast
            .manifest
            .phase_reports
            .iter()
            .any(|report| report.phase == "scan" && report.skipped >= 4));
    }

    /// `ok status` reports `quality.test_count` as the repository's indexed tests. A test the
    /// runner skips is not validation evidence, so a repository of `test.todo` stubs must not
    /// read as ready while every pack reports validation unavailable.
    #[test]
    fn indexed_test_count_excludes_disabled_registration_targets() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/rates.ts"),
            "export function convertCurrency(amount: number, rate: number): number {\n  return Math.round(amount * rate);\n}\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/rates.test.ts"),
            "test(\"rounds half up\", () => {\n  expect(convertCurrency(2, 1.5)).toBe(3);\n});\n\ntest.todo(\"handles negative rates\");\n\nit.skip(\"handles zero\", () => {});\n",
        )
        .unwrap();

        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;

        let snapshot = Indexer::default().index_repo(root, &config).unwrap();
        let names = snapshot
            .tests
            .iter()
            .map(|test| test.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"rounds half up"), "{names:?}");
        assert!(names.contains(&"handles negative rates"), "{names:?}");
        let enabled = snapshot
            .tests
            .iter()
            .filter(|test| test.counts_as_validation_evidence())
            .count();
        assert_eq!(snapshot.manifest.quality.test_count, enabled);
        assert!(
            snapshot.manifest.quality.test_count < snapshot.tests.len(),
            "the disabled stubs must not be counted: {names:?}"
        );
        // They are counted beside it instead, so `ok status` can tell skipped from absent.
        assert_eq!(
            snapshot
                .manifest
                .quality
                .excluded_test_targets
                .as_ref()
                .and_then(|excluded| excluded.get(&open_kioku_core::TestExclusionReason::Disabled))
                .copied(),
            Some(snapshot.tests.len() - enabled),
            "{names:?}"
        );
    }

    #[test]
    fn balanced_mode_keeps_trust_critical_passes_visible() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn issue_token() -> &'static str { \"token\" }\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tests/auth_test.rs"),
            "#[test]\nfn login_returns_valid_token() { assert_eq!(\"token\", \"token\"); }\n",
        )
        .unwrap();

        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;

        let snapshot = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Balanced)
            .unwrap();
        assert_eq!(snapshot.manifest.index_mode, IndexMode::Balanced);
        assert!(snapshot.manifest.quality.test_count > 0);
        assert!(snapshot
            .manifest
            .quality
            .quality_notes
            .iter()
            .any(|note| note.message.contains("balanced mode")));
    }

    #[test]
    fn cross_project_mode_records_status_without_parsing_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn should_not_parse() {}\n").unwrap();

        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;

        let snapshot = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::CrossProject)
            .unwrap();
        assert_eq!(snapshot.manifest.index_mode, IndexMode::CrossProject);
        assert_eq!(snapshot.manifest.file_count, 0);
        assert_eq!(snapshot.manifest.symbol_count, 0);
        assert_eq!(snapshot.manifest.chunk_count, 0);
        assert!(snapshot.files.is_empty());
        assert_eq!(
            snapshot.manifest.quality.excluded_test_targets, None,
            "no test was examined, so the exclusion count is unrecorded, not empty"
        );
        assert!(snapshot
            .manifest
            .quality
            .quality_notes
            .iter()
            .any(|note| note.kind == QualityNoteKind::IndexMode
                && note.message.contains("source parsing skipped")));
    }

    #[test]
    fn a_crate_root_skipped_for_size_shares_only_the_modules_its_mod_lines_declare() {
        // `main.rs` is over `max_file_size` and declares `util`, not `other`: only `util.rs` may
        // be compiled into the binary too. Excluded by `.okignore` instead, it is never read,
        // and both may be (#576).
        let shared_note = |ignore: bool| {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
            )
            .unwrap();
            std::fs::write(
                root.join("src/lib.rs"),
                "mod util;\nmod other;\npub fn helper() {}\n",
            )
            .unwrap();
            std::fs::write(root.join("src/util.rs"), "pub fn u() {}\n").unwrap();
            std::fs::write(root.join("src/other.rs"), "pub fn o() {}\n").unwrap();
            let padding = "// padding\n".repeat(64);
            std::fs::write(
                root.join("src/main.rs"),
                format!("mod util;\n{padding}fn main() {{}}\n"),
            )
            .unwrap();
            if ignore {
                std::fs::write(root.join(".okignore"), "src/main.rs\n").unwrap();
            }
            let mut config = OkConfig::default();
            config.scip.enabled = false;
            config.history.enabled = false;
            config.index.max_file_size = "256b".into();
            let snapshot = Indexer::default()
                .index_repo_with_mode(root, &config, IndexMode::Full)
                .unwrap();
            let expected = if ignore {
                SkipReason::Ignored
            } else {
                SkipReason::TooLarge
            };
            assert!(snapshot.skipped_paths.iter().any(|skipped| skipped.path
                == std::path::Path::new("src/main.rs")
                && skipped.reason == expected));
            snapshot
                .manifest
                .quality
                .quality_notes
                .iter()
                .find(|note| {
                    note.message
                        .contains("may also be compiled into another crate")
                })
                .map(|note| note.message.clone())
        };
        let sized = shared_note(false).expect("util.rs is shared");
        assert!(sized.starts_with("1 Rust source file(s)"), "{sized}");
        let ignored = shared_note(true).expect("both modules are shared");
        assert!(ignored.starts_with("2 Rust source file(s)"), "{ignored}");
    }

    #[test]
    fn a_mounted_file_skipped_for_size_shares_the_module_files_its_mod_lines_declare() {
        // `tests/it.rs` mounts `src/m.rs` with `#[path]`, and `m.rs` is over `max_file_size`.
        // Read from `src/`, its `mod b;` is the library's `src/b.rs`, which the test crate then
        // compiles with its own `helper`: `crate::helper()` there must prove no edge into the
        // library's (#610). A comment or a `//` in a string beside the item hides nothing.
        let exact_calls_from_b = |declaring: &str| {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::create_dir_all(root.join("tests")).unwrap();
            std::fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
            )
            .unwrap();
            std::fs::write(root.join("src/lib.rs"), "mod b;\npub fn helper() {}\n").unwrap();
            std::fs::write(
                root.join("src/b.rs"),
                "pub fn u() {\n    crate::helper();\n}\n",
            )
            .unwrap();
            let padding = "// padding\n".repeat(64);
            std::fs::write(root.join("src/m.rs"), format!("{declaring}\n{padding}")).unwrap();
            std::fs::write(
                root.join("tests/it.rs"),
                "#[path = \"../src/m.rs\"]\nmod m;\nfn helper() {}\n#[test]\nfn t() {}\n",
            )
            .unwrap();
            let mut config = OkConfig::default();
            config.scip.enabled = false;
            config.history.enabled = false;
            config.index.max_file_size = "256b".into();
            let snapshot = Indexer::default()
                .index_repo_with_mode(root, &config, IndexMode::Full)
                .unwrap();
            assert!(snapshot
                .skipped_paths
                .iter()
                .any(|skipped| skipped.path == std::path::Path::new("src/m.rs")
                    && skipped.reason == SkipReason::TooLarge));
            let symbol = |path: &str, name: &str| {
                let file = snapshot
                    .files
                    .iter()
                    .find(|file| file.path == std::path::Path::new(path))
                    .map(|file| file.id.clone())
                    .expect("the file is indexed");
                snapshot
                    .symbols
                    .iter()
                    .find(|symbol| symbol.file_id == file && symbol.name == name)
                    .map(|symbol| symbol.id.clone())
                    .expect("the symbol is indexed")
            };
            let (caller, helper) = (symbol("src/b.rs", "u"), symbol("src/lib.rs", "helper"));
            snapshot
                .resolved_relationships
                .iter()
                .filter(|edge| {
                    edge.from == caller
                        && edge.to == helper
                        && edge.edge_type == open_kioku_core::GraphEdgeType::Calls
                        && edge.confidence == Confidence::Exact
                })
                .count()
        };
        // Declaring no `b`, the mounted file leaves `b.rs` to the library alone.
        assert_eq!(exact_calls_from_b("mod z;"), 1);
        for declaring in [
            "mod b;",
            "mod b /* c */;",
            "const U: &str = \"http://example\"; mod b;",
            "const R: &[u8] = br#\"//\"#; mod b;",
        ] {
            assert_eq!(exact_calls_from_b(declaring), 0, "{declaring}");
        }
    }

    #[test]
    fn a_cfg_attr_path_module_below_a_mounted_file_is_shared_at_its_default_location_too() {
        // `tests/it.rs` mounts `src/sys/mod.rs`, which declares `mod imp;` with a `path` that
        // is only set on Windows: elsewhere the test crate compiles `src/sys/imp.rs` with its
        // own `helper`, so `crate::helper()` there proves no edge into the library's (#608).
        // Exact `CALLS` into the library's `helper` from `u` in `sys/imp.rs`, `common.rs` and
        // `other.rs`, with `sys/mod.rs` parsed or skipped for size.
        let exact_calls = |declaring: &str, skipped: bool| {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            std::fs::create_dir_all(root.join("src/sys")).unwrap();
            std::fs::create_dir_all(root.join("tests")).unwrap();
            std::fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
            )
            .unwrap();
            std::fs::write(
                root.join("src/lib.rs"),
                "mod sys;\nmod common;\nmod other;\npub fn helper() {}\n",
            )
            .unwrap();
            for file in ["src/sys/imp.rs", "src/common.rs", "src/other.rs"] {
                std::fs::write(root.join(file), "pub fn u() {\n    crate::helper();\n}\n").unwrap();
            }
            let padding = if skipped {
                "// padding\n".repeat(64)
            } else {
                String::new()
            };
            std::fs::write(
                root.join("src/sys/mod.rs"),
                format!("{declaring}\nmod imp;\n{padding}"),
            )
            .unwrap();
            std::fs::write(
                root.join("tests/it.rs"),
                "#[path = \"../src/sys/mod.rs\"]\nmod sys;\nfn helper() {}\n#[test]\nfn t() {}\n",
            )
            .unwrap();
            let mut config = OkConfig::default();
            config.scip.enabled = false;
            config.history.enabled = false;
            config.index.max_file_size = "256b".into();
            let snapshot = Indexer::default()
                .index_repo_with_mode(root, &config, IndexMode::Full)
                .unwrap();
            assert_eq!(
                snapshot.skipped_paths.iter().any(|skipped| skipped.path
                    == std::path::Path::new("src/sys/mod.rs")
                    && skipped.reason == SkipReason::TooLarge),
                skipped
            );
            let symbol = |path: &str, name: &str| {
                let file = snapshot
                    .files
                    .iter()
                    .find(|file| file.path == std::path::Path::new(path))
                    .map(|file| file.id.clone())
                    .expect("the file is indexed");
                snapshot
                    .symbols
                    .iter()
                    .find(|symbol| symbol.file_id == file && symbol.name == name)
                    .map(|symbol| symbol.id.clone())
                    .expect("the symbol is indexed")
            };
            let helper = symbol("src/lib.rs", "helper");
            ["src/sys/imp.rs", "src/common.rs", "src/other.rs"].map(|file| {
                let caller = symbol(file, "u");
                snapshot
                    .resolved_relationships
                    .iter()
                    .filter(|edge| {
                        edge.from == caller
                            && edge.to == helper
                            && edge.edge_type == open_kioku_core::GraphEdgeType::Calls
                            && edge.confidence == Confidence::Exact
                    })
                    .count()
            })
        };
        let conditional = "#[cfg_attr(windows, path = \"../common.rs\")]";
        let unconditional = "#[path = \"../common.rs\"]";
        assert_eq!(exact_calls(conditional, false), [0, 0, 1]);
        // Read off the skipped file's lines, only the files it may compile are shared.
        assert_eq!(exact_calls(conditional, true), [0, 0, 1]);
        // Control: a `path` that always applies leaves `sys/imp.rs` to the library.
        assert_eq!(exact_calls(unconditional, false), [1, 0, 1]);
        // Unchanged: the scan does not read such a `path`, so the skipped file may mount any
        // file of the package.
        assert_eq!(exact_calls(unconditional, true), [0, 0, 0]);
        // Conditions that cover every build never compile `imp` from its default location, parsed
        // or read off the skipped file's lines: `sys/imp.rs` is a leftover file of the library
        // alone, and only the two paths are compiled into the test crate (#613).
        let complementary =
            "#[cfg_attr(unix, path = \"../common.rs\")]\n#[cfg_attr(not( unix ), path = \"../other.rs\")]";
        assert_eq!(exact_calls(complementary, false), [1, 0, 0]);
        assert_eq!(exact_calls(complementary, true), [1, 0, 0]);
        let always = "#[cfg_attr(all(), path = \"../common.rs\")]";
        assert_eq!(exact_calls(always, false), [1, 0, 1]);
        assert_eq!(exact_calls(always, true), [1, 0, 1]);
    }

    #[test]
    fn a_path_into_a_module_whose_file_configuration_selects_reaches_every_file_unproven() {
        // `go` calls `crate::sys::imp::f()`, and `sys/mod.rs` gives `imp` its file with
        // `declaring`. Each `CALLS` edge from `go`, by the file of its target and whether it is
        // authoritative (#613).
        let calls_from_go = |declaring: &str| {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path();
            std::fs::create_dir_all(root.join("src/sys")).unwrap();
            std::fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
            )
            .unwrap();
            std::fs::write(
                root.join("src/lib.rs"),
                "mod sys;\npub fn go() {\n    crate::sys::imp::f();\n}\n",
            )
            .unwrap();
            for file in ["imp", "win", "u", "o", "x"] {
                std::fs::write(root.join(format!("src/sys/{file}.rs")), "pub fn f() {}\n").unwrap();
            }
            std::fs::write(root.join("src/sys/mod.rs"), declaring).unwrap();
            let mut config = OkConfig::default();
            config.scip.enabled = false;
            config.history.enabled = false;
            let snapshot = Indexer::default()
                .index_repo_with_mode(root, &config, IndexMode::Full)
                .unwrap();
            let go = snapshot
                .symbols
                .iter()
                .find(|symbol| symbol.name == "go")
                .map(|symbol| symbol.id.clone())
                .expect("`go` is indexed");
            let file_of = |id: &SymbolId| {
                let symbol = snapshot.symbols.iter().find(|symbol| symbol.id == *id)?;
                snapshot
                    .files
                    .iter()
                    .find(|file| file.id == symbol.file_id)
                    .map(|file| file.path.to_string_lossy().replace('\\', "/"))
            };
            let mut edges = snapshot
                .resolved_relationships
                .iter()
                .filter(|edge| {
                    edge.from == go && edge.edge_type == open_kioku_core::GraphEdgeType::Calls
                })
                .map(|edge| {
                    let authoritative =
                        open_kioku_core::relationship_authority(&edge.edge_type, &edge.proofs)
                            == open_kioku_core::RelationshipAuthority::Authoritative;
                    (file_of(&edge.to).unwrap_or_default(), authoritative)
                })
                .collect::<Vec<_>>();
            edges.sort();
            edges
        };
        let unproven = |files: &[&str]| {
            files
                .iter()
                .map(|file| (file.to_string(), false))
                .collect::<Vec<_>>()
        };
        // `imp` is `sys/imp.rs` unless the condition holds, and `sys/win.rs` when it does.
        assert_eq!(
            calls_from_go("#[cfg_attr(windows, path = \"win.rs\")]\npub mod imp;\n"),
            unproven(&["src/sys/imp.rs", "src/sys/win.rs"])
        );
        assert_eq!(
            calls_from_go(
                "#[cfg(not(windows))]\npub mod imp;\n#[cfg(windows)]\n#[path = \"win.rs\"]\npub mod imp;\n"
            ),
            unproven(&["src/sys/imp.rs", "src/sys/win.rs"])
        );
        // `unix` beside `not(unix)` never leaves `imp` at `sys/imp.rs`.
        assert_eq!(
            calls_from_go(
                "#[cfg_attr(unix, path = \"u.rs\")]\n#[cfg_attr(not(unix), path = \"o.rs\")]\npub mod imp;\n"
            ),
            unproven(&["src/sys/o.rs", "src/sys/u.rs"])
        );
        // `all()` always moves `imp` to `sys/x.rs`, which, like any `#[path]` file, a path
        // does not end in; `sys/imp.rs` is never compiled.
        assert_eq!(
            calls_from_go("#[cfg_attr(all(), path = \"x.rs\")]\npub mod imp;\n"),
            Vec::new()
        );
        // Control: with no choice the placed file is proven.
        assert_eq!(
            calls_from_go("pub mod imp;\n"),
            vec![("src/sys/imp.rs".to_string(), true)]
        );
    }

    /// Each `CALLS` edge from `caller` in a package `fx` of `files`, by the file of its target
    /// and whether it is authoritative, with the files its proofs name as ambiguity.
    fn rust_calls_from(files: &[(&str, &str)], caller: &str) -> Vec<(String, bool, Vec<String>)> {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        for (path, source) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;
        let snapshot = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Full)
            .unwrap();
        let caller = snapshot
            .symbols
            .iter()
            .find(|symbol| symbol.name == caller)
            .map(|symbol| symbol.id.clone())
            .expect("the caller is indexed");
        let file_of = |id: &SymbolId| {
            let symbol = snapshot.symbols.iter().find(|symbol| symbol.id == *id)?;
            snapshot
                .files
                .iter()
                .find(|file| file.id == symbol.file_id)
                .map(|file| file.path.to_string_lossy().replace('\\', "/"))
        };
        let mut edges = snapshot
            .resolved_relationships
            .iter()
            .filter(|edge| {
                edge.from == caller && edge.edge_type == open_kioku_core::GraphEdgeType::Calls
            })
            .map(|edge| {
                let authoritative =
                    open_kioku_core::relationship_authority(&edge.edge_type, &edge.proofs)
                        == open_kioku_core::RelationshipAuthority::Authoritative;
                let mut ambiguity = edge
                    .proofs
                    .iter()
                    .flat_map(|proof| proof.ambiguity.iter().cloned())
                    .collect::<Vec<_>>();
                ambiguity.sort();
                ambiguity.dedup();
                (
                    file_of(&edge.to).unwrap_or_default(),
                    authoritative,
                    ambiguity,
                )
            })
            .collect::<Vec<_>>();
        edges.sort();
        edges
    }

    /// Each `edge_type` relationship in a package `fx` of `files`, as (file and name of its
    /// source, file of its target, whether it is authoritative, the files its proofs name as
    /// ambiguity), sorted.
    fn rust_relations(
        files: &[(&str, &str)],
        edge_type: open_kioku_core::GraphEdgeType,
    ) -> Vec<(String, String, bool, Vec<String>)> {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        for (path, source) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;
        let snapshot = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Full)
            .unwrap();
        let symbol = |id: &SymbolId| snapshot.symbols.iter().find(|symbol| symbol.id == *id);
        let file_of = |id: &SymbolId| {
            let symbol = symbol(id)?;
            snapshot
                .files
                .iter()
                .find(|file| file.id == symbol.file_id)
                .map(|file| file.path.to_string_lossy().replace('\\', "/"))
        };
        let mut edges = snapshot
            .resolved_relationships
            .iter()
            .filter(|edge| edge.edge_type == edge_type)
            .map(|edge| {
                let authoritative =
                    open_kioku_core::relationship_authority(&edge.edge_type, &edge.proofs)
                        == open_kioku_core::RelationshipAuthority::Authoritative;
                let mut ambiguity = edge
                    .proofs
                    .iter()
                    .flat_map(|proof| proof.ambiguity.iter().cloned())
                    .collect::<Vec<_>>();
                ambiguity.sort();
                ambiguity.dedup();
                (
                    format!(
                        "{}::{}",
                        file_of(&edge.from).unwrap_or_default(),
                        symbol(&edge.from)
                            .map(|symbol| symbol.name.as_str())
                            .unwrap_or("")
                    ),
                    file_of(&edge.to).unwrap_or_default(),
                    authoritative,
                    ambiguity,
                )
            })
            .collect::<Vec<_>>();
        edges.sort();
        edges.dedup();
        edges
    }

    /// The `CALLS` edges of a Rust crate made of `files`, each as its caller (`file::name`), its
    /// target (`file:line`) and whether it is authoritative.
    fn rust_call_targets(files: &[(&str, &str)]) -> Vec<(String, String, bool)> {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        for (path, source) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, source).unwrap();
        }
        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;
        let snapshot = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Full)
            .unwrap();
        let symbol = |id: &SymbolId| snapshot.symbols.iter().find(|symbol| symbol.id == *id);
        let file_of = |symbol: &Symbol| {
            snapshot
                .files
                .iter()
                .find(|file| file.id == symbol.file_id)
                .map(|file| file.path.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default()
        };
        let mut edges = snapshot
            .resolved_relationships
            .iter()
            .filter(|edge| edge.edge_type == open_kioku_core::GraphEdgeType::Calls)
            .filter_map(|edge| {
                let (from, to) = (symbol(&edge.from)?, symbol(&edge.to)?);
                Some((
                    format!("{}::{}", file_of(from), from.name),
                    format!("{}:{}", file_of(to), to.range.as_ref()?.start),
                    open_kioku_core::relationship_authority(&edge.edge_type, &edge.proofs)
                        == open_kioku_core::RelationshipAuthority::Authoritative,
                ))
            })
            .collect::<Vec<_>>();
        edges.sort();
        edges.dedup();
        edges
    }

    #[test]
    fn a_method_call_through_an_aliased_or_dereferenced_field_reaches_the_type_it_names() {
        // `Disk` in `src/model.rs`, with an alias of it and one of an `Arc` of it. `persist`
        // is inherent and a `Saver` method, `take_it` inherent by reference and a `Taker`
        // method by value, and `clone`, `strong_count` and `lock` share their names with
        // methods of `Rc`, `Arc` and `Mutex` (#639).
        let model = "use std::sync::Arc;\n\
            pub struct Disk;\n\
            impl Disk {\n\
            \x20   pub fn save(&self) {}\n\
            \x20   pub fn persist(&self) {}\n\
            \x20   pub fn take_it(&self) {}\n\
            \x20   pub fn clone(&self) -> u8 {\n        0\n    }\n\
            \x20   pub fn strong_count(&self) -> u8 {\n        0\n    }\n\
            \x20   pub fn lock(&self) -> u8 {\n        0\n    }\n\
            }\n\
            pub type Store = Disk;\n\
            pub type Shared = Arc<Disk>;\n\
            pub trait Saver {\n    fn persist(&self);\n}\n\
            impl Saver for Disk {\n    fn persist(&self) {}\n}\n\
            pub trait Taker {\n    fn take_it(self);\n}\n\
            impl Taker for Disk {\n    fn take_it(self) {}\n}\n";
        let app = "use crate::model::{Disk, Shared, Store};\n\
            use std::rc::Rc;\n\
            use std::sync::{Arc, Mutex};\n\
            pub struct App {\n\
            \x20   s: Store,\n    h: Shared,\n    d: Disk,\n    shared: &'static Disk,\n\
            \x20   r: Rc<Disk>,\n    a: Arc<Disk>,\n    b: Box<Disk>,\n    n: Arc<Box<Disk>>,\n\
            \x20   m: Mutex<Disk>,\n\
            }\n\
            impl App {\n\
            \x20   pub fn aliased(&self) {\n        self.s.save();\n    }\n\
            \x20   pub fn aliased_arc(&self) {\n        self.h.save();\n    }\n\
            \x20   pub fn inherent(&self) {\n        self.d.persist();\n    }\n\
            \x20   pub fn inherent_shared(&self) {\n        self.shared.persist();\n    }\n\
            \x20   pub fn by_value_trait(&self) {\n        self.d.take_it();\n    }\n\
            \x20   pub fn rc(&self) {\n        self.r.save();\n    }\n\
            \x20   pub fn arc(&self) {\n        self.a.save();\n    }\n\
            \x20   pub fn boxed(&self) {\n        self.b.save();\n    }\n\
            \x20   pub fn nested(&self) {\n        self.n.save();\n    }\n\
            \x20   pub fn pointer_methods(&self) {\n\
            \x20       let _ = self.r.clone();\n        let _ = Rc::clone(&self.r);\n\
            \x20       let _ = self.a.strong_count();\n        let _ = Arc::strong_count(&self.a);\n\
            \x20       let _ = self.a.persist();\n\
            \x20   }\n\
            \x20   pub fn mutex(&self) {\n        let _ = self.m.lock();\n        self.m.lock().unwrap().save();\n    }\n\
            }\n";
        // A trait the index does not know may be in scope here, and the `Box` is this file's.
        let open = "use crate::model::Disk;\n\
            use outside::Helper;\n\
            use std::sync::Arc;\n\
            pub struct Open {\n    a: Arc<Disk>,\n}\n\
            impl Open {\n    pub fn opened(&self) {\n        let _ = Helper;\n        self.a.save();\n    }\n}\n";
        let other_box = "use crate::model::Disk;\n\
            pub struct Box<T>(T);\n\
            pub struct Other {\n    b: Box<Disk>,\n}\n\
            impl Other {\n    pub fn foreign(&self) {\n        self.b.save();\n    }\n}\n";
        let files = [
            (
                "src/lib.rs",
                "mod app;\nmod model;\nmod open;\nmod other_box;\n",
            ),
            ("src/model.rs", model),
            ("src/app.rs", app),
            ("src/open.rs", open),
            ("src/other_box.rs", other_box),
        ];
        let calls = rust_call_targets(&files)
            .into_iter()
            .filter(|(from, _, authoritative)| *authoritative && !from.starts_with("src/model.rs"))
            .collect::<Vec<_>>();
        let edge = |from: &str, line: u32| {
            (
                format!("src/app.rs::{from}"),
                format!("src/model.rs:{line}"),
                true,
            )
        };
        // `save` is on line 4 and the inherent `persist` on line 5. `take_it` by value through
        // `Taker` comes first in the probe, `persist` through a pointer is a method `Saver`
        // declares, and a pointer's own method names reach nothing.
        assert_eq!(
            calls,
            vec![
                edge("aliased", 4),
                edge("aliased_arc", 4),
                edge("arc", 4),
                edge("boxed", 4),
                edge("inherent", 5),
                edge("inherent_shared", 5),
                edge("nested", 4),
                edge("rc", 4),
            ]
        );
    }

    #[test]
    fn a_local_built_by_a_struct_constructor_and_a_path_through_an_enum_alias_reach_the_type() {
        // `P` is a tuple struct and `U` a unit struct of `src/a.rs`, each with `m` on line 3 and
        // 7; `make` is a function, and `Shape` an enum aliased as `Alias` with `k` on line 16
        // (#654).
        let a = "pub struct P(pub u8);\nimpl P {\n    pub fn m(&self) {}\n}\npub struct U;\nimpl U {\n    pub fn m(&self) {}\n}\npub fn make(_: u8) -> P {\n    P(0)\n}\npub enum Shape {\n    Circle(u8),\n}\nimpl Shape {\n    pub fn k() {}\n}\npub type Alias = Shape;\n";
        let lib = "mod a;\nuse crate::a::{make, Alias, P, U};\n\
            pub fn tuple() {\n    let p = P(1);\n    p.m();\n}\n\
            pub fn unit() {\n    let u = U;\n    u.m();\n}\n\
            pub fn unit_path() {\n    let u = crate::a::U;\n    u.m();\n}\n\
            pub fn tuple_path() {\n    let p = crate::a::P(1);\n    p.m();\n}\n\
            pub fn function() {\n    let p = make(1);\n    p.m();\n}\n\
            pub fn constructor_value() {\n    let p = P;\n    let _ = p(2);\n}\n\
            pub fn through_alias() {\n    Alias::k();\n}\n";
        let calls = rust_call_targets(&[("src/lib.rs", lib), ("src/a.rs", a)])
            .into_iter()
            .filter(|(_, to, authoritative)| *authoritative && to.starts_with("src/a.rs"))
            .collect::<Vec<_>>();
        let edge = |from: &str, line: u32| {
            (
                format!("src/lib.rs::{from}"),
                format!("src/a.rs:{line}"),
                true,
            )
        };
        // A function's result is not read as a `P`, and `P` as a value is its constructor. The
        // calls to `make` and, through a path, to `P`'s constructor are edges of their own.
        assert_eq!(
            calls,
            vec![
                edge("function", 9),
                edge("through_alias", 16),
                edge("tuple", 3),
                edge("tuple_path", 1),
                edge("tuple_path", 3),
                edge("unit", 7),
                edge("unit_path", 7),
            ]
        );
    }

    #[test]
    fn globs_bringing_in_one_name_in_different_namespaces_settle_each_namespace() {
        // `c` globs `a`, which holds a module `f`, and `b`, which holds a function `f`: the value
        // `c::f` is `b::f`, and the module `c::f` is `a::f` (#654). `d` globs `a` and `e`, both
        // holding a module `f`, so `d::f` names neither. `g` globs `h`, whose `f` is a module
        // file, and `b`.
        let files = [
            (
                "src/lib.rs",
                "mod a;\nmod b;\nmod c;\nmod d;\nmod e;\nmod g;\nmod h;\n\
                 pub fn value() {\n    crate::c::f();\n}\n\
                 pub fn module() {\n    crate::c::f::h();\n}\n\
                 pub fn clash() {\n    crate::d::f::h();\n}\n\
                 pub fn module_file() {\n    crate::g::f::h();\n}\n",
            ),
            ("src/a.rs", "pub mod f {\n    pub fn h() {}\n}\n"),
            ("src/b.rs", "pub fn f() {}\n"),
            ("src/c.rs", "pub use crate::a::*;\npub use crate::b::*;\n"),
            ("src/d.rs", "pub use crate::a::*;\npub use crate::e::*;\n"),
            ("src/e.rs", "pub mod f {\n    pub fn h() {}\n}\n"),
            ("src/g.rs", "pub use crate::b::*;\npub use crate::h::*;\n"),
            ("src/h.rs", "pub mod f;\n"),
            ("src/h/f.rs", "pub fn h() {}\n"),
        ];
        let calls = rust_call_targets(&files)
            .into_iter()
            .filter(|(from, ..)| from.starts_with("src/lib.rs"))
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            vec![
                ("src/lib.rs::module".into(), "src/a.rs:2".into(), true),
                (
                    "src/lib.rs::module_file".into(),
                    "src/h/f.rs:1".into(),
                    true
                ),
                ("src/lib.rs::value".into(), "src/b.rs:1".into(), true),
            ]
        );
    }

    #[test]
    fn a_path_or_import_written_inside_one_alternative_reaches_that_alternative_alone() {
        // Each alternative of `imp` declares `util` and calls its `g` through a path (`h`) and
        // through an import (`f`); `lib.rs` calls it from outside every alternative (#624).
        let inside = "pub mod util;\nuse crate::sys::imp::util::g;\npub fn f() {\n    g();\n}\npub fn h() {\n    crate::sys::imp::util::g();\n}\n";
        let top = "mod sys;\npub fn top() {\n    crate::sys::imp::util::g();\n}\n";
        let g = "pub fn g() {}\n";
        let calls = |files: &[(&str, &str)]| {
            rust_relations(files, open_kioku_core::GraphEdgeType::Calls)
                .into_iter()
                .map(|(from, to, authoritative, ambiguity)| {
                    (from, to, authoritative, ambiguity.len())
                })
                .collect::<Vec<_>>()
        };
        let edge = |from: &str, to: &str, authoritative: bool, files: usize| {
            (from.to_string(), to.to_string(), authoritative, files)
        };
        // Both alternatives are files a `path` attribute mounts, which the tree does not place.
        assert_eq!(
            calls(&[
                ("src/lib.rs", top),
                (
                    "src/sys/mod.rs",
                    "#[cfg_attr(unix, path = \"unix/mod.rs\")]\n#[cfg_attr(not(unix), path = \"other/mod.rs\")]\npub mod imp;\n",
                ),
                ("src/sys/unix/mod.rs", inside),
                ("src/sys/unix/util.rs", g),
                ("src/sys/other/mod.rs", inside),
                ("src/sys/other/util.rs", g),
            ]),
            vec![
                edge("src/lib.rs::top", "src/sys/other/util.rs", false, 2),
                edge("src/lib.rs::top", "src/sys/unix/util.rs", false, 2),
                edge("src/sys/other/mod.rs::f", "src/sys/other/util.rs", true, 0),
                edge("src/sys/other/mod.rs::h", "src/sys/other/util.rs", true, 0),
                edge("src/sys/unix/mod.rs::f", "src/sys/unix/util.rs", true, 0),
                edge("src/sys/unix/mod.rs::h", "src/sys/unix/util.rs", true, 0),
            ]
        );
        // The placed default location beside one mounted file: each reads its own `util`.
        assert_eq!(
            calls(&[
                ("src/lib.rs", top),
                (
                    "src/sys/mod.rs",
                    "#[cfg_attr(windows, path = \"other/mod.rs\")]\npub mod imp;\n",
                ),
                ("src/sys/imp/mod.rs", inside),
                ("src/sys/imp/util.rs", g),
                ("src/sys/other/mod.rs", inside),
                ("src/sys/other/util.rs", g),
            ]),
            vec![
                edge("src/lib.rs::top", "src/sys/imp/util.rs", false, 2),
                edge("src/lib.rs::top", "src/sys/other/util.rs", false, 2),
                edge("src/sys/imp/mod.rs::f", "src/sys/imp/util.rs", true, 0),
                edge("src/sys/imp/mod.rs::h", "src/sys/imp/util.rs", true, 0),
                edge("src/sys/other/mod.rs::f", "src/sys/other/util.rs", true, 0),
                edge("src/sys/other/mod.rs::h", "src/sys/other/util.rs", true, 0),
            ]
        );
        // A choice nested in one alternative leaves that alternative's two files, unproven.
        assert_eq!(
            calls(&[
                ("src/lib.rs", "mod sys;\n"),
                (
                    "src/sys/mod.rs",
                    "#[cfg_attr(windows, path = \"win/mod.rs\")]\npub mod imp;\n",
                ),
                ("src/sys/imp/mod.rs", inside),
                ("src/sys/imp/util.rs", g),
                (
                    "src/sys/win/mod.rs",
                    &format!("#[cfg_attr(target_arch = \"x86\", path = \"util32.rs\")]\n{inside}"),
                ),
                ("src/sys/win/util.rs", g),
                ("src/sys/win/util32.rs", g),
            ]),
            vec![
                edge("src/sys/imp/mod.rs::f", "src/sys/imp/util.rs", true, 0),
                edge("src/sys/imp/mod.rs::h", "src/sys/imp/util.rs", true, 0),
                edge("src/sys/win/mod.rs::f", "src/sys/win/util.rs", false, 2),
                edge("src/sys/win/mod.rs::f", "src/sys/win/util32.rs", false, 2),
                edge("src/sys/win/mod.rs::h", "src/sys/win/util.rs", false, 2),
                edge("src/sys/win/mod.rs::h", "src/sys/win/util32.rs", false, 2),
            ]
        );
        // `#[cfg]`-gated definitions of `g` in the one file the choice leaves stay candidates,
        // unproven, rather than losing the edge.
        assert_eq!(
            calls(&[
                ("src/lib.rs", "mod sys;\n"),
                (
                    "src/sys/mod.rs",
                    "#[cfg_attr(unix, path = \"unix/mod.rs\")]\n#[cfg_attr(not(unix), path = \"other/mod.rs\")]\npub mod imp;\n",
                ),
                ("src/sys/unix/mod.rs", inside),
                (
                    "src/sys/unix/util.rs",
                    "#[cfg(target_os = \"linux\")]\npub fn g() {}\n#[cfg(not(target_os = \"linux\"))]\npub fn g() {}\n",
                ),
                ("src/sys/other/mod.rs", "pub mod util;\n"),
                ("src/sys/other/util.rs", g),
            ]),
            vec![
                edge("src/sys/unix/mod.rs::f", "src/sys/unix/util.rs", false, 1),
                edge("src/sys/unix/mod.rs::h", "src/sys/unix/util.rs", false, 1),
            ]
        );
        // A file both alternatives mount is compiled with either, so it reaches both.
        let common = "use crate::sys::imp::util::g;\npub fn f() {\n    g();\n}\npub fn h() {\n    crate::sys::imp::util::g();\n}\n";
        let mounts = "#[path = \"../common.rs\"]\npub mod common;\npub mod util;\n";
        assert_eq!(
            calls(&[
                ("src/lib.rs", "mod sys;\n"),
                (
                    "src/sys/mod.rs",
                    "#[cfg_attr(unix, path = \"unix/mod.rs\")]\n#[cfg_attr(not(unix), path = \"other/mod.rs\")]\npub mod imp;\n",
                ),
                ("src/sys/common.rs", common),
                ("src/sys/unix/mod.rs", mounts),
                ("src/sys/unix/util.rs", g),
                ("src/sys/other/mod.rs", mounts),
                ("src/sys/other/util.rs", g),
            ]),
            vec![
                edge("src/sys/common.rs::f", "src/sys/other/util.rs", false, 2),
                edge("src/sys/common.rs::f", "src/sys/unix/util.rs", false, 2),
                edge("src/sys/common.rs::h", "src/sys/other/util.rs", false, 2),
                edge("src/sys/common.rs::h", "src/sys/unix/util.rs", false, 2),
            ]
        );
    }

    #[test]
    fn a_file_below_a_choice_that_a_path_outside_it_also_mounts_reaches_every_alternative() {
        // The crate root also mounts `util.rs` of one alternative with `#[path]`, so that file is
        // compiled on every build, whichever file `imp` is: its `crate::sys::imp::h` is each
        // alternative's `h`, unproven, though the choice would place it in one alternative.
        let util = "use crate::sys::imp::h;\npub fn g() {\n    h();\n}\npub fn k() {\n    crate::sys::imp::h();\n}\n";
        let backend = "pub mod util;\npub fn h() {}\n";
        let calls = |files: &[(&str, &str)]| {
            rust_relations(files, open_kioku_core::GraphEdgeType::Calls)
                .into_iter()
                .filter(|(from, ..)| from.contains("util.rs::"))
                .map(|(from, to, authoritative, ambiguity)| {
                    (from, to, authoritative, ambiguity.len())
                })
                .collect::<Vec<_>>()
        };
        let edge = |from: &str, to: &str, authoritative: bool, files: usize| {
            (from.to_string(), to.to_string(), authoritative, files)
        };
        let both = |from: &str, left: &str, right: &str| {
            vec![
                edge(&format!("{from}::g"), left, false, 2),
                edge(&format!("{from}::g"), right, false, 2),
                edge(&format!("{from}::k"), left, false, 2),
                edge(&format!("{from}::k"), right, false, 2),
            ]
        };
        // Both alternatives are mounted; the root mounts `unix/util.rs` a second time.
        let (unix, other) = ("src/sys/unix/mod.rs", "src/sys/other/mod.rs");
        let mounted = |root: &'static str| {
            calls(&[
                ("src/lib.rs", root),
                (
                    "src/sys/mod.rs",
                    "#[cfg_attr(unix, path = \"unix/mod.rs\")]\n#[cfg_attr(not(unix), path = \"other/mod.rs\")]\npub mod imp;\n",
                ),
                (unix, backend),
                ("src/sys/unix/util.rs", util),
                (other, backend),
                ("src/sys/other/util.rs", util),
            ])
        };
        let mut expected = both("src/sys/unix/util.rs", other, unix);
        expected.extend([
            edge("src/sys/other/util.rs::g", other, true, 0),
            edge("src/sys/other/util.rs::k", other, true, 0),
        ]);
        expected.sort();
        assert_eq!(
            mounted("mod sys;\n#[path = \"sys/unix/util.rs\"]\nmod uu;\n"),
            expected
        );
        // Control: without the second mount each file reads its own alternative, proven.
        assert_eq!(
            mounted("mod sys;\n"),
            vec![
                edge("src/sys/other/util.rs::g", other, true, 0),
                edge("src/sys/other/util.rs::k", other, true, 0),
                edge("src/sys/unix/util.rs::g", unix, true, 0),
                edge("src/sys/unix/util.rs::k", unix, true, 0),
            ]
        );
        // The placed default alternative's `util.rs`, mounted again by the root, is not proven
        // through the placed tree either.
        let (imp, win) = ("src/sys/imp/mod.rs", "src/sys/win/mod.rs");
        let mut expected = both("src/sys/imp/util.rs", imp, win);
        expected.extend([
            edge("src/sys/win/util.rs::g", win, true, 0),
            edge("src/sys/win/util.rs::k", win, true, 0),
        ]);
        expected.sort();
        assert_eq!(
            calls(&[
                (
                    "src/lib.rs",
                    "mod sys;\n#[path = \"sys/imp/util.rs\"]\nmod uu;\n"
                ),
                (
                    "src/sys/mod.rs",
                    "#[cfg_attr(windows, path = \"win/mod.rs\")]\npub mod imp;\n",
                ),
                (imp, backend),
                ("src/sys/imp/util.rs", util),
                (win, backend),
                ("src/sys/win/util.rs", util),
            ]),
            expected
        );
    }

    #[test]
    fn a_type_written_as_a_crate_self_or_super_path_is_read_for_uses_type_and_receivers() {
        // `Entry` with `save` in `src/model.rs`, named by a `crate::`, `self::` and `super::`
        // path from three files (#637).
        let model = "pub mod inner;\npub struct Entry;\nimpl Entry {\n    pub fn new() -> Self {\n        Entry\n    }\n    pub fn save(&self) {}\n}\npub fn here(e: &self::Entry) {\n    e.save();\n}\n";
        let files = [
            (
                "src/lib.rs",
                "pub mod model;\npub fn go(e: crate::model::Entry) {\n    e.save();\n}\npub fn made() {\n    let e = crate::model::Entry::new();\n    e.save();\n}\npub fn stray(e: crate::missing::Entry) {\n    e.save();\n}\n",
            ),
            ("src/model.rs", model),
            (
                "src/model/inner.rs",
                "pub fn up(e: &mut super::Entry) {\n    e.save();\n}\npub fn wrong(e: super::super::Entry) {\n    e.save();\n}\n",
            ),
        ];
        let relations = |edge_type| {
            rust_relations(&files, edge_type)
                .into_iter()
                .map(|(from, to, authoritative, ambiguity)| {
                    (from, to, authoritative, ambiguity.len())
                })
                .collect::<Vec<_>>()
        };
        let edge = |from: &str, authoritative: bool, files: usize| {
            (
                from.to_string(),
                "src/model.rs".to_string(),
                authoritative,
                files,
            )
        };
        let calls = relations(open_kioku_core::GraphEdgeType::Calls)
            .into_iter()
            .filter(|(_, to, ..)| to == "src/model.rs")
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            vec![
                edge("src/lib.rs::go", true, 0),
                edge("src/lib.rs::made", true, 0),
                edge("src/model.rs::here", true, 0),
                edge("src/model/inner.rs::up", true, 0),
            ]
        );
        assert_eq!(
            relations(open_kioku_core::GraphEdgeType::UsesType),
            vec![
                edge("src/lib.rs::go", true, 0),
                edge("src/model.rs::here", true, 0),
                edge("src/model/inner.rs::up", true, 0),
            ]
        );

        // Through a module whose file configuration selects, each file's type is a candidate,
        // unproven, from outside the choice; a path written inside one alternative reads its own.
        let types = "pub mod user;\npub struct S;\nimpl S {\n    pub fn m(&self) {}\n}\n";
        let user =
            |name: &str, path: &str| format!("pub fn {name}(s: {path}) {{\n    s.m();\n}}\n");
        let (imp_user, win_user) = (user("u", "crate::sys::imp::S"), user("w", "super::S"));
        let files = [
            (
                "src/lib.rs",
                "mod sys;\npub fn go(s: &crate::sys::imp::S) {\n    s.m();\n}\n",
            ),
            (
                "src/sys/mod.rs",
                "#[cfg_attr(windows, path = \"win/mod.rs\")]\npub mod imp;\n",
            ),
            ("src/sys/imp/mod.rs", types),
            ("src/sys/imp/user.rs", imp_user.as_str()),
            ("src/sys/win/mod.rs", types),
            ("src/sys/win/user.rs", win_user.as_str()),
        ];
        let (imp, win) = ("src/sys/imp/mod.rs", "src/sys/win/mod.rs");
        let edge = |from: &str, to: &str, authoritative: bool, files: usize| {
            (from.to_string(), to.to_string(), authoritative, files)
        };
        let expected = vec![
            edge("src/lib.rs::go", imp, false, 2),
            edge("src/lib.rs::go", win, false, 2),
            edge("src/sys/imp/user.rs::u", imp, true, 0),
            edge("src/sys/win/user.rs::w", win, true, 0),
        ];
        for edge_type in [
            open_kioku_core::GraphEdgeType::Calls,
            open_kioku_core::GraphEdgeType::UsesType,
        ] {
            let found = rust_relations(&files, edge_type.clone())
                .into_iter()
                .map(|(from, to, authoritative, ambiguity)| {
                    (from, to, authoritative, ambiguity.len())
                })
                .collect::<Vec<_>>();
            assert_eq!(found, expected, "{edge_type:?}");
        }
    }

    #[test]
    fn a_method_call_through_a_struct_field_is_read_through_the_fields_type() {
        // `Store::save` in `src/store.rs`; `src/other.rs` declares another `Store` with `save`.
        // `src/app.rs` imports the first, and its structs hold fields of it (#630).
        let store = "pub struct Store;\nimpl Store {\n    pub fn save(&self) {}\n}\n";
        let app = "use crate::store::Store;\n\
            pub struct Inner {\n    pub store: Store,\n    pub missing: Missing,\n}\n\
            pub struct App<T> {\n    inner: Inner,\n    store: &'static Store,\n    boxed: Box<Store>,\n    generic: T,\n    #[cfg(unix)]\n    gated: Store,\n    #[cfg(not(unix))]\n    gated: Store,\n}\n\
            impl<T> App<T> {\n\
            \x20   pub fn direct(&self) {\n        self.store.save();\n    }\n\
            \x20   pub fn chained(&self) {\n        self.inner.store.save();\n    }\n\
            \x20   pub fn boxed(&self) {\n        self.boxed.save();\n    }\n\
            \x20   pub fn generic(&self) {\n        self.generic.save();\n    }\n\
            \x20   pub fn gated(&self) {\n        self.gated.save();\n    }\n\
            \x20   pub fn unknown(&self) {\n        self.inner.missing.save();\n    }\n\
            }\n\
            pub fn through(inner: &Inner) {\n    inner.store.save();\n}\n\
            pub fn nested(app: &App<u8>) {\n    app.inner.store.save();\n}\n";
        let other = "pub struct Store;\nimpl Store {\n    pub fn save(&self) {}\n}\npub fn elsewhere(inner: &crate::app::Inner) {\n    inner.store.save();\n}\n";
        let files = [
            ("src/lib.rs", "mod store;\nmod app;\nmod other;\n"),
            ("src/store.rs", store),
            ("src/app.rs", app),
            ("src/other.rs", other),
        ];
        let calls = rust_relations(&files, open_kioku_core::GraphEdgeType::Calls)
            .into_iter()
            .filter(|(_, to, ..)| to != "src/app.rs")
            .map(|(from, to, authoritative, ambiguity)| (from, to, authoritative, ambiguity.len()))
            .collect::<Vec<_>>();
        let edge = |from: &str| (from.to_string(), "src/store.rs".to_string(), true, 0usize);
        // The field's type is read where the struct is declared: `other.rs`'s own `Store` is
        // not the field's. A generic, `#[cfg]`-duplicated or unresolved field reaches nothing;
        // a boxed one reaches what the `Box` holds (#639).
        assert_eq!(
            calls,
            vec![
                edge("src/app.rs::boxed"),
                edge("src/app.rs::chained"),
                edge("src/app.rs::direct"),
                edge("src/app.rs::nested"),
                edge("src/app.rs::through"),
                edge("src/other.rs::elsewhere"),
            ]
        );
        // A field's type is not read as a type the struct uses.
        assert!(
            rust_relations(&files, open_kioku_core::GraphEdgeType::UsesType)
                .iter()
                .all(|(from, ..)| !from.ends_with("::Inner") && !from.ends_with("::App"))
        );
    }

    #[test]
    fn a_type_imported_through_a_module_whose_file_configuration_selects_is_every_files_type() {
        // `use crate::sys::imp::{S, T};` where `imp` is `imp/mod.rs` or `win/mod.rs`, each
        // declaring `S` with `new` and `m`, and a trait `T`. `win/user.rs` is below the mounted
        // alternative, `imp/user.rs` below the placed one (#625).
        let types = "pub mod user;\npub struct S;\nimpl S {\n    pub fn new() -> Self {\n        S\n    }\n    pub fn m(&self) {}\n}\npub trait T {\n    fn t(&self);\n}\n";
        let user = |name: &str| {
            format!("use crate::sys::imp::S;\npub fn {name}(s: &S) {{\n    s.m();\n}}\n")
        };
        let (imp_user, win_user) = (user("u"), user("w"));
        let files = |declaring: &'static str| {
            vec![
                (
                    "src/lib.rs",
                    "mod sys;\nuse crate::sys::imp::{S, T};\npub struct Mine;\nimpl T for Mine {\n    fn t(&self) {}\n}\npub fn go(s: &S) {\n    s.m();\n}\npub fn mk() {\n    let s = S::new();\n    s.m();\n}\n",
                ),
                ("src/sys/mod.rs", declaring),
                ("src/sys/imp/mod.rs", types),
                ("src/sys/imp/user.rs", imp_user.as_str()),
                ("src/sys/win/mod.rs", types),
                ("src/sys/win/user.rs", win_user.as_str()),
            ]
        };
        let relations = |declaring: &'static str, edge_type| {
            rust_relations(&files(declaring), edge_type)
                .into_iter()
                .map(|(from, to, authoritative, ambiguity)| {
                    (from, to, authoritative, ambiguity.len())
                })
                .collect::<Vec<_>>()
        };
        let edge = |from: &str, to: &str, authoritative: bool, files: usize| {
            (from.to_string(), to.to_string(), authoritative, files)
        };
        let configured = "#[cfg_attr(windows, path = \"win/mod.rs\")]\npub mod imp;\n";
        let (imp, win) = ("src/sys/imp/mod.rs", "src/sys/win/mod.rs");
        let both = |from: &str| vec![edge(from, imp, false, 2), edge(from, win, false, 2)];
        // Every method call through the imported type, and `S::new()`, reaches both files'
        // method, unproven; one written below either alternative reaches its own.
        let mut calls = both("src/lib.rs::go");
        calls.extend(both("src/lib.rs::mk"));
        calls.push(edge("src/sys/imp/user.rs::u", imp, true, 0));
        calls.push(edge("src/sys/win/user.rs::w", win, true, 0));
        calls.sort();
        assert_eq!(
            relations(configured, open_kioku_core::GraphEdgeType::Calls),
            calls
        );
        // The declared type and the implemented trait are each file's too, never authoritative.
        let uses = relations(configured, open_kioku_core::GraphEdgeType::UsesType);
        assert_eq!(
            uses.iter()
                .filter(|(from, ..)| from == "src/lib.rs::go")
                .cloned()
                .collect::<Vec<_>>(),
            both("src/lib.rs::go")
        );
        assert!(
            uses.contains(&edge("src/sys/win/user.rs::w", win, true, 0)),
            "{uses:?}"
        );
        assert_eq!(
            relations(configured, open_kioku_core::GraphEdgeType::Implements),
            both("src/lib.rs::Mine")
        );
        // Control: with no choice the placed file is proven throughout.
        let placed = "pub mod imp;\n";
        assert!(relations(placed, open_kioku_core::GraphEdgeType::Calls)
            .iter()
            .filter(|(from, ..)| from.starts_with("src/lib.rs::"))
            .all(|(_, to, authoritative, files)| to == imp && *authoritative && *files == 0));
        assert_eq!(
            relations(placed, open_kioku_core::GraphEdgeType::Implements),
            vec![edge("src/lib.rs::Mine", imp, true, 0)]
        );
    }

    #[test]
    fn an_import_through_a_module_whose_file_configuration_selects_reaches_every_file_unproven() {
        // `sys/mod.rs` gives `imp` its file with `declaring`; `lib.rs` is `root`. Each file of
        // `sys` declares `pub fn f() {}` (#615).
        let calls = |declaring: &str, root: &str, caller: &str| {
            rust_calls_from(
                &[
                    ("src/lib.rs", root),
                    ("src/sys/mod.rs", declaring),
                    ("src/sys/imp.rs", "pub fn f() {}\n"),
                    ("src/sys/win.rs", "pub fn f() {}\n"),
                ],
                caller,
            )
        };
        let cfg_attr = "#[cfg_attr(windows, path = \"win.rs\")]\npub mod imp;\n";
        let both = |files: &[&str]| {
            let named = files
                .iter()
                .map(|file| file.to_string())
                .collect::<Vec<_>>();
            named
                .iter()
                .map(|file| (file.clone(), false, named.clone()))
                .collect::<Vec<_>>()
        };
        let imp_or_win = both(&["src/sys/imp.rs", "src/sys/win.rs"]);
        let item = "mod sys;\nuse crate::sys::imp::f;\npub fn go() {\n    f();\n}\n";
        // An item import, and the `#[cfg]` spelling of the same choice.
        assert_eq!(calls(cfg_attr, item, "go"), imp_or_win);
        assert_eq!(
            calls(
                "#[cfg(not(windows))]\npub mod imp;\n#[cfg(windows)]\n#[path = \"win.rs\"]\npub mod imp;\n",
                item,
                "go"
            ),
            imp_or_win
        );
        // A module import, called through.
        assert_eq!(
            calls(
                cfg_attr,
                "mod sys;\nuse crate::sys::imp;\npub fn go() {\n    imp::f();\n}\n",
                "go"
            ),
            imp_or_win
        );
        // An import written in the declaring module makes no choice either.
        assert_eq!(
            calls(
                &format!("{cfg_attr}use self::imp::f;\npub fn run() {{\n    f();\n}}\n"),
                "mod sys;\n",
                "run"
            ),
            imp_or_win
        );
        // A glob binds no call, with or without the choice: it does not reach the default file
        // alone.
        let reexporting = format!("{cfg_attr}pub use imp::f;\n");
        assert_eq!(
            calls(
                &reexporting,
                "mod sys;\nuse crate::sys::imp::*;\npub fn go() {\n    f();\n}\n",
                "go"
            ),
            Vec::new()
        );
        // An in-crate path through the `pub use`, which makes no choice, reaches every file
        // (#476).
        assert_eq!(
            calls(
                &reexporting,
                "mod sys;\nuse crate::sys::f;\npub fn go() {\n    f();\n}\n",
                "go"
            ),
            imp_or_win
        );
        // Control: with no choice the import proves the placed file.
        assert_eq!(
            calls("pub mod imp;\n", item, "go"),
            vec![("src/sys/imp.rs".to_string(), true, Vec::new())]
        );
    }

    #[test]
    fn an_import_through_a_crate_name_into_a_module_whose_file_configuration_selects_is_unproven() {
        // A binary imports `f` from the library, directly and through `sys`'s `pub use imp::f;`,
        // which a crate-name path follows; the library compiles `imp` from one of two files.
        let calls = |import: &str| {
            rust_calls_from(
                &[
                    ("src/lib.rs", "pub mod sys;\n"),
                    (
                        "src/sys/mod.rs",
                        "#[cfg_attr(windows, path = \"win.rs\")]\npub mod imp;\npub use imp::f;\n",
                    ),
                    ("src/sys/imp.rs", "pub fn f() {}\n"),
                    ("src/sys/win.rs", "pub fn f() {}\n"),
                    (
                        "src/bin/tool.rs",
                        &format!("use {import};\nfn main() {{\n    f();\n}}\n"),
                    ),
                ],
                "main",
            )
        };
        let files = vec!["src/sys/imp.rs".to_string(), "src/sys/win.rs".to_string()];
        let imp_or_win = files
            .iter()
            .map(|file| (file.clone(), false, files.clone()))
            .collect::<Vec<_>>();
        assert_eq!(calls("fx::sys::imp::f"), imp_or_win);
        assert_eq!(calls("fx::sys::f"), imp_or_win);
    }

    #[test]
    fn an_import_below_a_module_whose_file_configuration_selects_is_proven_from_inside_it() {
        // `sys/imp.rs` is compiled only when `imp` is that file, so its `use self::inner::g;` is
        // `sys/imp/inner.rs` alone, though `sys/win.rs` declares an `inner` of its own (#615).
        assert_eq!(
            rust_calls_from(
                &[
                    ("src/lib.rs", "mod sys;\n"),
                    (
                        "src/sys/mod.rs",
                        "#[cfg_attr(windows, path = \"win.rs\")]\npub mod imp;\n"
                    ),
                    (
                        "src/sys/imp.rs",
                        "mod inner;\nuse self::inner::g;\npub fn f() {\n    g();\n}\n"
                    ),
                    ("src/sys/imp/inner.rs", "pub fn g() {}\n"),
                    ("src/sys/win.rs", "mod inner;\n"),
                    ("src/sys/inner.rs", "pub fn g() {}\n"),
                ],
                "f",
            ),
            vec![("src/sys/imp/inner.rs".to_string(), true, Vec::new())]
        );
    }

    #[test]
    fn a_configuration_selected_file_another_module_also_declares_stays_a_candidate() {
        // `imp` is `src/other.rs` on unix, the file `crate::other` also names, and
        // `src/sys/imp.rs` elsewhere (#616). Every spelling of a call into `imp` reaches both, and
        // one into `crate::other` is `src/other.rs` alone.
        let calls = |root: &str| {
            rust_calls_from(
                &[
                    ("src/lib.rs", root),
                    (
                        "src/sys/mod.rs",
                        "#[cfg_attr(unix, path = \"../other.rs\")]\npub mod imp;\n",
                    ),
                    ("src/sys/imp.rs", "pub fn f() {}\n"),
                    ("src/other.rs", "pub fn f() {}\n"),
                ],
                "go",
            )
        };
        let files = vec!["src/other.rs".to_string(), "src/sys/imp.rs".to_string()];
        let other_or_imp = files
            .iter()
            .map(|file| (file.clone(), false, files.clone()))
            .collect::<Vec<_>>();
        for root in [
            "mod other;\nmod sys;\npub fn go() {\n    crate::sys::imp::f();\n}\n",
            "mod other;\nmod sys;\nuse crate::sys::imp::f;\npub fn go() {\n    f();\n}\n",
        ] {
            assert_eq!(calls(root), other_or_imp, "{root}");
        }
        let direct =
            "mod other;\nmod sys;\nuse crate::other::f;\npub fn go() {\n    f();\n    crate::other::f();\n}\n";
        assert_eq!(
            calls(direct),
            vec![
                ("src/other.rs".to_string(), true, Vec::new()),
                ("src/other.rs".to_string(), true, Vec::new()),
            ]
        );
    }

    /// The `CALLS` edges from `go`, whose body is `body`, in a package where `b` holds a glob
    /// of `a` beside items of the names `a` defines, in other namespaces (#643).
    fn calls_into_namespaces(body: &str) -> Vec<(String, bool, Vec<String>)> {
        // `a`: a `fn S`, a `fn T`, a unit `struct U` with `new`, a `fn K` and a `fn Q`. `b`: the
        // glob, a braced `struct S`, a tuple `struct T`, a `fn U`, an `enum K` and a `trait Q`.
        rust_calls_from(
            &[
                ("src/lib.rs", &format!("mod a;\nmod b;\n\npub fn go() {{\n    {body}\n}}\n")),
                (
                    "src/a.rs",
                    "#![allow(non_snake_case)]\npub fn S() {}\npub fn T(_: u8) {}\npub struct U;\nimpl U {\n    pub fn new() -> Self {\n        U\n    }\n}\npub fn K() {}\npub fn Q() {}\n",
                ),
                (
                    "src/b.rs",
                    "#![allow(non_snake_case)]\npub use crate::a::*;\npub struct S {\n    pub x: u8,\n}\npub struct T(pub u8);\npub fn U() {}\npub enum K {\n    X,\n}\npub trait Q {}\n",
                ),
            ],
            "go",
        )
    }

    #[test]
    fn a_rust_path_reads_a_name_in_the_namespace_it_uses_it_in() {
        let exact = |file: &str| vec![(file.to_string(), true, Vec::new())];
        for (body, file, why) in [
            (
                "crate::b::S();",
                "src/a.rs",
                "a call names the glob's function: the braced struct is a type alone",
            ),
            ("crate::b::K();", "src/a.rs", "an enum is a type alone"),
            ("crate::b::Q();", "src/a.rs", "a trait is a type alone"),
            (
                "let _ = crate::b::U::new();",
                "src/a.rs",
                "a type path names the glob's struct: the local function is a value alone",
            ),
            (
                "let _ = crate::b::T(1);",
                "src/b.rs",
                "a tuple struct is a value too, and shadows the glob's function",
            ),
            (
                "crate::b::U();",
                "src/b.rs",
                "the local function is the value",
            ),
        ] {
            assert_eq!(calls_into_namespaces(body), exact(file), "{why}: {body}");
        }
    }

    #[test]
    fn a_rust_path_follows_enum_globs_renamed_modules_and_inline_blocks() {
        // `shapes` re-exports its enum's variants and `tokens`' items; `facade` renames `m`;
        // the inline `n` re-exports `m::g` and `n::all` globs `m` (#641).
        let calls = |body: &str| {
            rust_calls_from(
                &[
                    (
                        "src/lib.rs",
                        &format!(
                            "mod m;\nmod shapes;\nmod tokens;\nuse m as facade;\npub mod n {{\n    pub use super::m::g;\n    pub mod all {{\n        pub use crate::m::*;\n    }}\n}}\n\npub fn go() {{\n    {body}\n}}\n"
                        ),
                    ),
                    ("src/m.rs", "pub fn g() {}\n"),
                    (
                        "src/shapes.rs",
                        "pub enum Shape {\n    Circle(u8),\n}\npub use Shape::*;\npub use crate::tokens::*;\n",
                    ),
                    ("src/tokens.rs", "pub fn target_fn() {}\n"),
                ],
                "go",
            )
        };
        let exact = |file: &str| vec![(file.to_string(), true, Vec::new())];
        assert_eq!(
            calls("crate::shapes::target_fn();"),
            exact("src/tokens.rs"),
            "a glob of an enum brings in its variants alone"
        );
        assert_eq!(calls("crate::facade::g();"), exact("src/m.rs"));
        assert_eq!(calls("crate::n::g();"), exact("src/m.rs"));
        assert_eq!(
            calls("crate::n::all::g();"),
            Vec::new(),
            "a glob of an inline block is not followed"
        );
        assert_eq!(
            calls("let _ = crate::shapes::Circle(1);"),
            Vec::new(),
            "a variant is no item the index records"
        );
    }

    #[test]
    fn discovery_reports_typed_skipped_paths_without_reading_secret_content() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("blocked")).unwrap();
        std::fs::create_dir_all(root.join("vendor")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join(".gitignore"), "git_ignored.rs\n").unwrap();
        std::fs::write(root.join(".okignore"), "ok_ignored.rs\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
        std::fs::write(root.join("blocked/deny.rs"), "pub fn blocked() {}\n").unwrap();
        std::fs::write(root.join(".hidden.rs"), "pub fn hidden() {}\n").unwrap();
        std::fs::write(root.join("git_ignored.rs"), "pub fn git_ignored() {}\n").unwrap();
        std::fs::write(root.join("ok_ignored.rs"), "pub fn ok_ignored() {}\n").unwrap();
        std::fs::write(root.join("large.rs"), "pub fn too_large() {}\n".repeat(8)).unwrap();
        std::fs::write(root.join("binary.rs"), b"pub fn binary() {}\0").unwrap();
        std::fs::write(
            root.join("generated.rs"),
            "// @generated\npub fn generated() {}\n",
        )
        .unwrap();
        std::fs::write(root.join("vendor/lib.rs"), "pub fn vendored() {}\n").unwrap();
        std::fs::write(root.join("docs/guide.rs"), "pub fn docs() {}\n").unwrap();
        std::fs::write(root.join(".env"), "OPEN_KIOKU_SECRET=do-not-read\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("src/lib.rs"), root.join("linked.rs")).unwrap();

        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;
        config.index.max_file_size = "64b".into();
        config.paths.deny = vec!["blocked/**".into()];

        let snapshot = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Fast)
            .unwrap();
        let quality = &snapshot.manifest.quality;
        // src/lib.rs plus generated.rs: generated source is indexed and flagged, not skipped.
        assert_eq!(snapshot.manifest.file_count, 2);
        let generated = snapshot
            .files
            .iter()
            .find(|file| file.path.ends_with("generated.rs"))
            .expect("generated source is indexed rather than skipped");
        assert!(generated.is_generated);
        assert_skip(quality, SkipReason::Denied, SkipSource::SecurityPolicy);
        assert_skip(quality, SkipReason::Hidden, SkipSource::HiddenPolicy);
        assert_skip(quality, SkipReason::Ignored, SkipSource::GitIgnore);
        assert_skip(quality, SkipReason::Ignored, SkipSource::OkIgnore);
        assert_skip(quality, SkipReason::TooLarge, SkipSource::SizeLimit);
        assert_skip(quality, SkipReason::Binary, SkipSource::Detector);
        assert_skip(quality, SkipReason::Vendor, SkipSource::Detector);
        assert_skip(quality, SkipReason::FastMode, SkipSource::FastMode);
        #[cfg(unix)]
        assert_skip(
            quality,
            SkipReason::SymlinkPolicy,
            SkipSource::SymlinkPolicy,
        );

        let secret = quality
            .skipped_paths
            .iter()
            .find(|path| path.reason == SkipReason::SecretPolicy)
            .expect("secret-like path should be skipped by secret policy");
        assert!(!secret.safe_to_show);
        assert_eq!(secret.path.display().to_string(), "[redacted]");
        assert!(quality
            .quality_notes
            .iter()
            .any(|note| note.kind == QualityNoteKind::Discovery
                && note.message.contains("discovery skipped")));
    }

    fn assert_skip(
        quality: &open_kioku_core::IndexQuality,
        reason: SkipReason,
        source: SkipSource,
    ) {
        assert!(
            quality
                .skipped_paths
                .iter()
                .any(|path| path.reason == reason && path.source == source),
            "missing skip reason={reason:?} source={source:?}: {:?}",
            quality.skipped_paths
        );
        assert!(quality.skip_counts.get(&reason).copied().unwrap_or(0) > 0);
    }

    #[test]
    fn index_git_history_facts_can_be_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "user.name", "Test User"]);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("src/auth.rs"), "pub fn login() {}\n").unwrap();
        std::fs::write(
            root.join("tests/auth_test.rs"),
            "#[test] fn login_test() {}\n",
        )
        .unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "auth with tests"]);

        let mut enabled = OkConfig::default();
        enabled.scip.enabled = false;
        let (snapshot, history) = Indexer::default()
            .index_repo_with_history(root, &enabled)
            .unwrap();
        assert!(snapshot.manifest.quality.git_history_facts > 0);
        assert!(snapshot
            .analysis_facts
            .iter()
            .any(|fact| fact.source_type == EvidenceSourceType::GitHistory
                && fact.target == "tests/auth_test.rs"));
        assert_eq!(history.commits.len(), 1);
        assert_eq!(history.file_touches.len(), 2);
        assert_eq!(history.symbol_touches.len(), 2);
        assert!(history
            .symbol_touches
            .iter()
            .all(|touch| touch.confidence == Confidence::High && !touch.line_ranges.is_empty()));
        assert!(!history.cochange_edges.is_empty());

        let mut disabled = enabled;
        disabled.history.enabled = false;
        let (snapshot, history) = Indexer::default()
            .index_repo_with_history(root, &disabled)
            .unwrap();
        assert_eq!(snapshot.manifest.quality.git_history_facts, 0);
        assert!(!snapshot
            .analysis_facts
            .iter()
            .any(|fact| fact.source_type == EvidenceSourceType::GitHistory));
        assert!(history.commits.is_empty());
        assert!(history.file_touches.is_empty());
    }

    /// Discovery never reads a secret-like or denied path, so history must not record one
    /// either (#525): no touch, rename, co-change pair or fact may name it, and the history of
    /// the files committed beside it is what it would have been without it.
    #[test]
    fn history_withholds_paths_the_security_policy_excludes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "user.name", "Test User"]);
        for dir in ["src", "tests", "config", "blocked"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("src/auth.rs"), "pub fn login() {}\n").unwrap();
        std::fs::write(
            root.join("tests/auth_test.rs"),
            "#[test] fn login_test() {}\n",
        )
        .unwrap();
        std::fs::write(root.join(".env"), "TOKEN=abc\n").unwrap();
        std::fs::write(root.join(".env.sample"), "TOKEN=\n").unwrap();
        std::fs::write(root.join("config/server.key"), "key\n").unwrap();
        std::fs::write(root.join("blocked/notes.rs"), "pub fn hidden() {}\n").unwrap();
        // Named for credentials but not secret-like: discovery indexes it (redacted), so its
        // history is kept.
        std::fs::write(root.join("config/credentials.json"), "{}\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-m", "auth with config"]);
        // A rename away from a secret-like name must not keep the old name either.
        git(root, &["mv", ".env.sample", "config/env.sample"]);
        std::fs::write(root.join("src/auth.rs"), "pub fn login() { let _ = 1; }\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-m", "move sample env"]);

        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.paths.deny = vec!["blocked/**".into()];
        let (snapshot, history) = Indexer::default()
            .index_repo_with_history(root, &config)
            .unwrap();

        let recorded = serde_json::to_string(&history).unwrap();
        let facts = serde_json::to_string(&snapshot.analysis_facts).unwrap();
        let notes = serde_json::to_string(&snapshot.manifest.quality.quality_notes).unwrap();
        for withheld in [".env", "server.key", "blocked/notes.rs"] {
            assert!(!recorded.contains(withheld), "{withheld} in {recorded}");
            assert!(!facts.contains(withheld), "{withheld} in {facts}");
            assert!(!notes.contains(withheld), "{withheld} in {notes}");
        }

        // The files committed beside them keep their history.
        let touched = |path: &str| {
            history
                .file_touches
                .iter()
                .filter(|touch| touch.path == std::path::Path::new(path))
                .count()
        };
        assert_eq!(touched("src/auth.rs"), 2);
        assert_eq!(touched("tests/auth_test.rs"), 1);
        assert_eq!(touched("config/credentials.json"), 1);
        assert!(history.cochange_edges.iter().any(|edge| {
            edge.path == std::path::Path::new("src/auth.rs")
                && edge.cochanged_path == std::path::Path::new("tests/auth_test.rs")
                && edge.commit_count == 1
        }));
        assert!(history.cochange_edges.iter().any(|edge| {
            edge.path == std::path::Path::new("src/auth.rs")
                && edge.cochanged_path == std::path::Path::new("config/credentials.json")
        }));
        assert!(history
            .symbol_touches
            .iter()
            .any(|touch| touch.file_path == std::path::Path::new("src/auth.rs")));

        // What was withheld is counted, never named.
        let note = snapshot
            .manifest
            .quality
            .quality_notes
            .iter()
            .find(|note| note.message.contains("security policy excludes"))
            .expect("withheld history is reported");
        assert_eq!(note.kind, QualityNoteKind::GitHistory);
        assert!(note.message.contains("on 4 path(s)"), "{}", note.message);
        // Seven files in the first commit, three of them kept: 21 - 3 unordered pairs name a
        // withheld path. The rename commit pairs only kept names.
        assert!(
            note.message.contains("and 18 co-change pair(s)"),
            "{}",
            note.message
        );
    }

    #[test]
    fn a_skipped_patch_reason_does_not_name_a_withheld_path() {
        let history = open_kioku_git::CommitHistory {
            commits: Vec::new(),
            file_touches: vec![GitFileTouch {
                id: HistoryRecordId::new("touch"),
                commit_id: GitCommitId::new("aaaa"),
                path: ".env".into(),
                previous_path: None,
                change_kind: GitChangeKind::Modified,
                additions: None,
                deletions: None,
                touched_at: Utc::now(),
            }],
        };
        let ingest = git_history_ingest(
            &[],
            &[],
            history,
            open_kioku_git::CommitPatchScan {
                commits: Vec::new(),
                skipped: vec![open_kioku_git::SkippedCommitPatch {
                    commit_id: GitCommitId::new("aaaa"),
                    reason: "git diff output is malformed in the entry for `.env` at line 3".into(),
                }],
            },
            10,
            &|path| open_kioku_core::is_secret_like_path(path),
        );
        assert!(ingest.snapshot.file_touches.is_empty());
        let notes = serde_json::to_string(&ingest.quality_notes).unwrap();
        assert!(!notes.contains(".env"), "{notes}");
        assert!(notes.contains("entry for `[redacted]`"), "{notes}");
    }

    /// A withheld name inside a longer withheld name must not be masked first: `.env` inside
    /// `config/.env.local` would leave `config/[redacted].local`.
    #[test]
    fn a_skipped_patch_reason_masks_the_longest_withheld_name_first() {
        let touch = |id: &str, path: &str| GitFileTouch {
            id: HistoryRecordId::new(id),
            commit_id: GitCommitId::new("aaaa"),
            path: path.into(),
            previous_path: None,
            change_kind: GitChangeKind::Modified,
            additions: None,
            deletions: None,
            touched_at: Utc::now(),
        };
        let history = open_kioku_git::CommitHistory {
            commits: Vec::new(),
            file_touches: vec![touch("a", ".env"), touch("b", "config/.env.local")],
        };
        let ingest = git_history_ingest(
            &[],
            &[],
            history,
            open_kioku_git::CommitPatchScan {
                commits: Vec::new(),
                skipped: vec![open_kioku_git::SkippedCommitPatch {
                    commit_id: GitCommitId::new("aaaa"),
                    reason: "git diff output is malformed in the entry for `config/.env.local` \
                             after `.env`"
                        .into(),
                }],
            },
            10,
            &|path| open_kioku_core::is_secret_like_path(path),
        );
        let notes = serde_json::to_string(&ingest.quality_notes).unwrap();
        assert!(!notes.contains("config/"), "{notes}");
        assert!(!notes.contains(".local"), "{notes}");
        assert!(
            notes.contains("entry for `[redacted]` after `[redacted]`"),
            "{notes}"
        );
    }

    #[test]
    fn a_skipped_commit_patch_is_reported_as_a_history_quality_note() {
        let skipped = |id: &str| open_kioku_git::SkippedCommitPatch {
            commit_id: GitCommitId::new(id),
            reason: "git diff output is malformed in the entry for `a.rs` at line 7".into(),
        };
        let clean = git_history_ingest(
            &[],
            &[],
            open_kioku_git::CommitHistory::empty(),
            open_kioku_git::CommitPatchScan::default(),
            10,
            &|_| false,
        );
        assert!(clean.quality_notes.is_empty());

        let ingest = git_history_ingest(
            &[],
            &[],
            open_kioku_git::CommitHistory::empty(),
            open_kioku_git::CommitPatchScan {
                commits: Vec::new(),
                skipped: vec![skipped("aaaa"), skipped("bbbb")],
            },
            10,
            &|_| false,
        );

        assert_eq!(ingest.quality_notes.len(), 1);
        let note = &ingest.quality_notes[0];
        assert_eq!(note.kind, QualityNoteKind::GitHistory);
        assert!(note.message.contains("2 commit(s)"), "{}", note.message);
        assert!(note.message.contains("first: aaaa:"), "{}", note.message);
        assert!(
            note.message.contains("entry for `a.rs`"),
            "{}",
            note.message
        );
    }

    #[test]
    fn symbol_mapping_marks_ambiguous_historical_ranges_as_uncertain() {
        let file = File {
            id: FileId::new("file"),
            repository_id: RepositoryId::new("repo"),
            path: "src/new.rs".into(),
            language: Language::Rust,
            size_bytes: 10,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
            generated_by: None,
        };
        let symbols = vec![
            Symbol {
                id: SymbolId::new("outer"),
                name: "Outer".into(),
                qualified_name: "crate::Outer".into(),
                kind: SymbolKind::Class,
                file_id: file.id.clone(),
                range: Some(LineRange { start: 1, end: 20 }),
                language: Language::Rust,
                confidence: Confidence::High,
                provenance: EvidenceSourceType::TreeSitter,
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility: open_kioku_core::Visibility::Unknown,
                alias_of: None,
            },
            Symbol {
                id: SymbolId::new("left"),
                name: "left".into(),
                qualified_name: "crate::Outer::left".into(),
                kind: SymbolKind::Method,
                file_id: file.id.clone(),
                range: Some(LineRange { start: 5, end: 10 }),
                language: Language::Rust,
                confidence: Confidence::High,
                provenance: EvidenceSourceType::TreeSitter,
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility: open_kioku_core::Visibility::Unknown,
                alias_of: None,
            },
            Symbol {
                id: SymbolId::new("right"),
                name: "right".into(),
                qualified_name: "crate::Outer::right".into(),
                kind: SymbolKind::Method,
                file_id: file.id.clone(),
                range: Some(LineRange { start: 5, end: 10 }),
                language: Language::Rust,
                confidence: Confidence::High,
                provenance: EvidenceSourceType::TreeSitter,
                module_id: None,
                parent_symbol_id: None,
                scope_id: None,
                signature: None,
                visibility: open_kioku_core::Visibility::Unknown,
                alias_of: None,
            },
        ];
        let newer_at = Utc.with_ymd_and_hms(2026, 6, 2, 12, 0, 0).unwrap();
        let older_at = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
        let newer = history_commit("newer", newer_at);
        let older = history_commit("older", older_at);
        let history = open_kioku_git::CommitHistory {
            commits: vec![newer.clone(), older.clone()],
            file_touches: vec![
                GitFileTouch {
                    id: HistoryRecordId::new("rename"),
                    commit_id: newer.id.clone(),
                    path: "src/new.rs".into(),
                    previous_path: Some("src/old.rs".into()),
                    change_kind: GitChangeKind::Renamed,
                    additions: None,
                    deletions: None,
                    touched_at: newer_at,
                },
                GitFileTouch {
                    id: HistoryRecordId::new("older-touch"),
                    commit_id: older.id.clone(),
                    path: "src/old.rs".into(),
                    previous_path: None,
                    change_kind: GitChangeKind::Modified,
                    additions: None,
                    deletions: None,
                    touched_at: older_at,
                },
            ],
        };
        let patches = vec![open_kioku_git::CommitPatch {
            commit_id: older.id,
            files: vec![open_kioku_git::FilePatch {
                path: "src/old.rs".into(),
                previous_path: None,
                line_ranges: vec![LineRange { start: 3, end: 12 }],
            }],
        }];

        let touches = map_symbol_touches(&[file], &symbols, &history, &patches);

        assert_eq!(touches.len(), 2);
        assert!(touches
            .iter()
            .all(|touch| touch.confidence == Confidence::Low));
        assert!(touches.iter().all(|touch| touch
            .uncertainty
            .iter()
            .any(|note| note.contains("multiple equally specific"))));
        assert!(touches.iter().all(|touch| touch
            .uncertainty
            .iter()
            .any(|note| note.contains("mapped through rename history"))));
        assert!(touches
            .iter()
            .all(|touch| touch.symbol_id.as_ref() != Some(&SymbolId::new("outer"))));
        assert!(touches
            .iter()
            .all(|touch| touch.line_ranges == vec![LineRange { start: 5, end: 10 }]));
    }

    fn history_commit(id: &str, at: chrono::DateTime<Utc>) -> GitCommitRecord {
        GitCommitRecord {
            id: GitCommitId::new(id),
            parent_ids: Vec::new(),
            author: Owner {
                name: "Test User".into(),
                email: Some("test@example.com".into()),
            },
            committer: None,
            authored_at: at,
            committed_at: at,
            summary: id.into(),
            message: id.into(),
            file_count: 1,
        }
    }

    fn git(root: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }
}

#[cfg(test)]
mod document_corpus_tests {
    use super::*;
    use std::fs;

    #[test]
    fn document_classifier_requires_plain_text_opt_in_and_rejects_readme_code() {
        let none = compile_globs(&[]).unwrap();
        let configured = compile_globs(&["docs/*.txt".into(), "docs/**/*.txt".into()]).unwrap();
        assert_eq!(
            document_type_for_path(Path::new("docs/guide.md"), &none),
            Some(DocumentType::Markdown)
        );
        assert!(document_type_for_path(Path::new("docs/notes.txt"), &none).is_none());
        assert_eq!(
            document_type_for_path(Path::new("docs/notes.txt"), &configured),
            Some(DocumentType::PlainText)
        );
        assert_eq!(
            document_type_for_path(Path::new("README"), &none),
            Some(DocumentType::Readme)
        );
        assert!(document_type_for_path(Path::new("README.rs"), &none).is_none());
        assert!(document_type_for_path(Path::new("docs/examples/client.rs"), &none).is_none());
    }

    #[test]
    fn markdown_sections_preserve_heading_paths_and_are_bounded() {
        let mut content = String::from("# Root\nintro\n## Rotation\n");
        for index in 0..250 {
            content.push_str(&format!("rotation line {index}\n"));
        }
        let sections =
            build_document_sections(Path::new("docs/guide.md"), &content, DocumentType::Markdown);
        let rotation = sections
            .iter()
            .filter(|section| section.heading_path == ["Root", "Rotation"])
            .collect::<Vec<_>>();
        assert_eq!(rotation.len(), 3);
        assert!(rotation
            .iter()
            .all(|section| section.line_range.end - section.line_range.start < 120));
        assert_eq!(rotation[0].line_range.start, 3);
        assert!(!rotation[0].content_hash.is_empty());
    }

    #[test]
    fn fast_mode_document_corpus_uses_common_security_and_ignore_policy() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn live() {}\n").unwrap();
        fs::write(root.join("docs/visible.md"), "# Visible\nquasar protocol\n").unwrap();
        fs::write(root.join("docs/blocked.md"), "# Blocked\nsecret design\n").unwrap();
        let mut config = OkConfig::default();
        config.history.enabled = false;
        config.paths.deny = vec!["docs/blocked.md".into()];

        let snapshot = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Fast)
            .unwrap();
        assert!(snapshot
            .document_sections
            .iter()
            .any(|section| section.path == Path::new("docs/visible.md")));
        assert!(!snapshot
            .document_sections
            .iter()
            .any(|section| section.path == Path::new("docs/blocked.md")));
        assert!(!snapshot
            .files
            .iter()
            .any(|file| file.path == Path::new("docs/visible.md")));
        let report = snapshot
            .phase_reports
            .iter()
            .find(|report| report.phase == "document_corpus")
            .unwrap();
        assert_eq!(report.document_files, Some(1));
        assert!(report.document_sections.is_some_and(|count| count >= 1));
        assert!(report.duration_ms.is_some());
    }
}

#[cfg(test)]
mod document_corpus_acceptance_hardening_tests {
    use super::*;
    use std::fs;

    #[test]
    fn plain_text_is_opt_in_and_readme_code_is_not_a_document() {
        let none = compile_globs(&[]).unwrap();
        let configured = compile_globs(&["docs/*.txt".into(), "docs/**/*.txt".into()]).unwrap();
        assert_eq!(
            document_type_for_path(Path::new("docs/guide.md"), &none),
            Some(DocumentType::Markdown)
        );
        assert!(document_type_for_path(Path::new("docs/notes.txt"), &none).is_none());
        assert_eq!(
            document_type_for_path(Path::new("docs/notes.txt"), &configured),
            Some(DocumentType::PlainText)
        );
        assert_eq!(
            document_type_for_path(Path::new("README.txt"), &none),
            Some(DocumentType::Readme)
        );
        assert!(document_type_for_path(Path::new("README.rs"), &none).is_none());
        assert!(document_type_for_path(Path::new("docs/examples/client.rs"), &none).is_none());
    }

    #[test]
    fn fenced_markdown_heading_is_not_document_structure() {
        let content =
            "# Root\nintro\n```text\n## Fake heading\nbody\n```\n## Real heading\nreal body\n";
        let sections =
            build_document_sections(Path::new("docs/guide.md"), content, DocumentType::Markdown);
        assert!(!sections.iter().any(|section| {
            section
                .heading_path
                .iter()
                .any(|heading| heading == "Fake heading")
        }));
        assert!(sections.iter().any(|section| {
            section.heading_path == ["Root", "Real heading"]
                && section.line_range == LineRange { start: 7, end: 8 }
        }));
    }

    #[test]
    fn corpus_inherits_okignore_and_can_be_disabled() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("docs/visible.md"), "# Visible\nkept\n").unwrap();
        fs::write(root.join("docs/ignored.md"), "# Ignored\nsecret\n").unwrap();
        fs::write(root.join(".okignore"), "docs/ignored.md\n").unwrap();

        let mut config = OkConfig::default();
        config.history.enabled = false;
        let enabled = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Fast)
            .unwrap();
        assert!(enabled
            .document_sections
            .iter()
            .any(|section| section.path == Path::new("docs/visible.md")));
        assert!(!enabled
            .document_sections
            .iter()
            .any(|section| section.path == Path::new("docs/ignored.md")));

        config.documents.enabled = false;
        let disabled = Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Fast)
            .unwrap();
        assert!(disabled.document_sections.is_empty());
        let report = disabled
            .phase_reports
            .iter()
            .find(|report| report.phase == "document_corpus")
            .unwrap();
        assert!(report
            .warnings
            .iter()
            .any(|warning| warning.contains("disabled by configuration")));
    }
}

#[cfg(test)]
mod per_file_failure_tests {
    use super::*;
    use open_kioku_parse::ParsedFile;

    /// Stands in for a grammar that crashes on one file's content (#347 was a slice landing
    /// inside a multi-byte character); every other file parses as empty.
    struct PanicsOnMarker;

    impl Parser for PanicsOnMarker {
        fn parse_with_hint(&self, _file: &File, content: &str, _hint: Option<&str>) -> ParsedFile {
            assert!(!content.contains("boom"), "simulated parser crash");
            ParsedFile {
                syntax: Default::default(),
                chunks: Vec::new(),
                analysis_facts: Vec::new(),
                tests: Vec::new(),
            }
        }
    }

    fn quiet_config() -> OkConfig {
        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;
        config.documents.enabled = false;
        config
    }

    #[test]
    fn file_removed_between_discovery_and_parse_is_a_filesystem_skip() {
        let dir = tempfile::tempdir().unwrap();
        let file = File {
            id: FileId::new("gone"),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from("src/gone.rs"),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: String::new(),
            is_generated: false,
            is_vendor: false,
            generated_by: None,
        };
        let failure = Indexer::default()
            .parse_file(dir.path(), &file, None)
            .expect_err("a missing file must not parse");
        assert_eq!(failure.source, SkipSource::Filesystem);
        assert!(!failure.message.is_empty());
    }

    #[test]
    fn parser_panic_on_one_file_is_recorded_and_the_index_completes() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/fine.rs"), "pub fn fine() {}\n").unwrap();
        fs::write(dir.path().join("src/bad.rs"), "pub fn boom() {}\n").unwrap();
        let indexer = Indexer {
            parser: Box::new(PanicsOnMarker),
        };
        let snapshot = indexer
            .index_repo(dir.path(), &quiet_config())
            .expect("one crashing file must not abort the index");

        let paths: Vec<_> = snapshot
            .files
            .iter()
            .map(|file| file.path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(paths, vec!["src/fine.rs"]);
        assert_eq!(snapshot.manifest.file_count, 1);
        let skipped: Vec<_> = snapshot
            .skipped_paths
            .iter()
            .filter(|skip| skip.reason == SkipReason::Error)
            .collect();
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].path, PathBuf::from("src/bad.rs"));
        assert_eq!(skipped[0].source, SkipSource::Parser);
        assert_eq!(
            snapshot
                .manifest
                .quality
                .skip_counts
                .get(&SkipReason::Error),
            Some(&1)
        );
        assert!(snapshot
            .phase_reports
            .iter()
            .any(|report| report.warnings.iter().any(|w| w.contains("src/bad.rs"))));

        // The dropped file must move from indexed to skipped in coverage, or the ratio would
        // claim a file the index does not hold.
        let coverage = snapshot
            .manifest
            .quality
            .coverage
            .as_ref()
            .expect("a full index records coverage");
        assert_eq!(coverage.discovered, 2);
        assert_eq!(coverage.indexed, 1);
        assert_eq!(coverage.skipped.get(&SkipReason::Error), Some(&1));
        let rust = coverage
            .by_language
            .get(Language::Rust.key())
            .expect("rust coverage is recorded");
        assert_eq!(
            rust.discovered,
            rust.indexed + rust.skipped.values().sum::<usize>()
        );
    }
}
