use chrono::Utc;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use open_kioku_config::OkConfig;
use open_kioku_core::{
    AnalysisFact, CodeChunk, Confidence, EvidenceSourceType, File, FileId, GitCochangeEdge,
    GitCommitId, GitSymbolTouch, GraphEdgeType, GraphNodeType, HistoryRecordId, HistorySnapshot,
    Import, IndexManifest, IndexMode, IndexPhaseReport, IndexQuality, LineRange, Repository,
    RepositoryId, SkipReason, SkipSource, SkippedPath, Symbol, SymbolOccurrence, TestTarget,
    HISTORY_SCHEMA_VERSION,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_languages::{
    detect_language, is_supported_code, likely_generated, likely_vendor_path,
};
use open_kioku_parse::{HeuristicParser, Parser};
use open_kioku_scip::ScipIndexReport;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

pub mod resolver;
pub mod runtime;
pub mod symbol_registry;

const MAX_HISTORY_COCHANGE_EDGES: usize = 5000;

#[derive(Debug, Clone)]
pub struct IndexSnapshot {
    pub manifest: IndexManifest,
    pub files: Vec<File>,
    pub symbols: Vec<Symbol>,
    pub chunks: Vec<CodeChunk>,
    pub tests: Vec<TestTarget>,
    pub imports: Vec<Import>,
    pub import_resolutions: Vec<open_kioku_core::ImportResolution>,
    pub occurrences: Vec<SymbolOccurrence>,
    pub analysis_facts: Vec<AnalysisFact>,
    pub scip: Option<ScipIndexReport>,
    pub phase_reports: Vec<IndexPhaseReport>,
    pub skipped_paths: Vec<SkippedPath>,
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

impl Indexer {
    pub fn index_repo(&self, root: impl AsRef<Path>, config: &OkConfig) -> Result<IndexSnapshot> {
        self.index_repo_with_progress(root, config, |_| {})
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
            let quality = index_quality(IndexQualityInput {
                root: &root,
                config,
                scip_report: None,
                test_count: 0,
                import_count: 0,
                analysis: AnalysisCounts::default(),
                quality_notes: &[
                    "cross-project mode: source parsing skipped; link already-indexed projects only"
                        .into(),
                ],
                mode,
                phase_reports: &phase_reports,
                skipped_paths: &[],
            });
            let manifest = IndexManifest {
                repository,
                file_count: 0,
                symbol_count: 0,
                chunk_count: 0,
                indexed_at: Utc::now(),
                schema_version: 1,
                index_mode: mode,
                phase_reports: phase_reports.clone(),
                quality,
            };
            return Ok((
                IndexSnapshot {
                    manifest,
                    files: Vec::new(),
                    symbols: Vec::new(),
                    chunks: Vec::new(),
                    tests: Vec::new(),
                    imports: Vec::new(),
                    import_resolutions: Vec::new(),
                    occurrences: Vec::new(),
                    analysis_facts: Vec::new(),
                    scip: None,
                    phase_reports,
                    skipped_paths: Vec::new(),
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
        let parsed = files
            .par_iter()
            .map(|file| -> Result<_> {
                let bytes = fs::read(root.join(&file.path))?;
                let content = String::from_utf8_lossy(&bytes).into_owned();
                let parsed = self
                    .parser
                    .parse_with_hint(file, &content, build_hint.as_deref());
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
                Ok(parsed)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut symbols = parsed
            .iter()
            .flat_map(|file| file.symbols.clone())
            .collect::<Vec<_>>();
        dedupe_symbols(&mut symbols);
        let chunks = parsed
            .iter()
            .flat_map(|file| file.chunks.clone())
            .collect::<Vec<_>>();
        let tests = parsed
            .iter()
            .flat_map(|file| file.tests.clone())
            .collect::<Vec<_>>();
        let imports = parsed
            .iter()
            .flat_map(|file| file.imports.clone())
            .collect::<Vec<_>>();
        let mut analysis_facts = parsed
            .iter()
            .flat_map(|file| file.analysis_facts.clone())
            .collect::<Vec<_>>();
        emit_progress(
            &on_progress,
            &mut phase_reports,
            started,
            ProgressEvent::new("extract")
                .scanned(files.len())
                .indexed(files.len())
                .total(Some(files.len()))
                .nodes_added(files.len() + symbols.len() + chunks.len() + tests.len()),
        );
        let resolver_report = resolver::resolve_imports(&root, &files, &symbols, &imports)?;
        let resolver_fact_count = resolver_report.analysis_facts.len();
        analysis_facts.extend(resolver_report.analysis_facts.clone());
        let registry_report = symbol_registry::resolve_symbol_edges(
            &chunks,
            &symbols,
            &resolver_report.resolutions,
            config.scip.enabled,
        );
        let registry_fact_count = registry_report.analysis_facts.len();
        analysis_facts.extend(registry_report.analysis_facts);
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
        let git_history = if config.history.enabled {
            collect_git_history(
                &root,
                &files,
                &symbols,
                config.history.max_commits,
                config.history.max_files_per_commit,
            )?
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
            let (imported, report) =
                open_kioku_scip::prepare_and_import_scip(&root, &config.scip, &repo_id)?;
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
        let mut mode_notes = mode_quality_notes(mode);
        mode_notes.extend(resolver_quality_notes);
        let quality = index_quality(IndexQualityInput {
            root: &root,
            config,
            scip_report: scip_report.as_ref(),
            test_count: tests.len(),
            import_count: imports.len(),
            analysis: AnalysisCounts {
                static_facts: static_analysis_facts,
                resolver_facts: resolver_fact_count,
                registry_facts: registry_fact_count,
                runtime_facts: runtime_analysis_facts,
                git_history_facts: git_history_fact_count,
                architecture_facts: arch_fact_count,
            },
            quality_notes: &mode_notes,
            mode,
            phase_reports: &phase_reports,
            skipped_paths: &scan.skipped_paths,
        });
        let manifest = IndexManifest {
            repository,
            file_count: files.len(),
            symbol_count: symbols.len(),
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: mode,
            phase_reports: phase_reports.clone(),
            quality,
        };
        Ok((
            IndexSnapshot {
                manifest,
                files,
                symbols,
                chunks,
                tests,
                imports,
                import_resolutions: resolver_report.resolutions,
                occurrences,
                analysis_facts,
                scip: scip_report,
                phase_reports,
                skipped_paths: scan.skipped_paths,
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
        let excludes = compile_globs(&config.index.exclude)?;
        let denied = compile_globs(&config.paths.deny)?;
        let git_ignores = build_ignore_matcher(root, ".gitignore")?;
        let ok_ignores = build_ignore_matcher(root, ".okignore")?;
        let mut builder = WalkBuilder::new(root);
        builder.hidden(false);
        builder.git_ignore(false).git_exclude(false).parents(false);
        builder.ignore(false);
        builder.follow_links(false);
        builder.filter_entry(|entry| !is_heavy_discovery_dir(entry.path()));
        let mut files = Vec::new();
        let mut skipped_paths = Vec::new();
        let mut warnings = Vec::new();
        let mut scanned_files = 0;
        progress.emit(ProgressEvent::new("scan"));
        for entry in builder.build() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    skipped_paths.push(SkippedPath {
                        path: PathBuf::from("[walk-error]"),
                        reason: SkipReason::Error,
                        source: SkipSource::Filesystem,
                        safe_to_show: false,
                    });
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
            let secret_policy = is_secret_like_path(&rel);
            if secret_policy || denied.is_match(&rel) {
                let safe_to_show = !secret_policy || !config.security.redact_secrets;
                let reason = if secret_policy {
                    SkipReason::SecretPolicy
                } else {
                    SkipReason::Denied
                };
                push_skip(
                    root,
                    path,
                    reason,
                    SkipSource::SecurityPolicy,
                    safe_to_show,
                    &mut skipped_paths,
                );
                if should_emit_progress(scanned_files, 0) {
                    progress.emit_transient(
                        ProgressEvent::new("scan")
                            .scanned(scanned_files)
                            .indexed(files.len())
                            .skipped(skipped_paths.len()),
                    );
                }
                continue;
            }
            if !config.security.allow_hidden_files && is_hidden_path(&rel) {
                push_skip(
                    root,
                    path,
                    SkipReason::Hidden,
                    SkipSource::HiddenPolicy,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            if excludes.is_match(&rel) {
                push_skip(
                    root,
                    path,
                    SkipReason::Ignored,
                    SkipSource::ConfigExclude,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            if git_ignores
                .matched_path_or_any_parents(path, false)
                .is_ignore()
            {
                push_skip(
                    root,
                    path,
                    SkipReason::Ignored,
                    SkipSource::GitIgnore,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            if ok_ignores
                .matched_path_or_any_parents(path, false)
                .is_ignore()
            {
                push_skip(
                    root,
                    path,
                    SkipReason::Ignored,
                    SkipSource::OkIgnore,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            if entry.file_type().is_some_and(|kind| kind.is_symlink()) {
                push_skip(
                    root,
                    path,
                    SkipReason::SymlinkPolicy,
                    SkipSource::SymlinkPolicy,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            if likely_vendor_path(&rel) {
                push_skip(
                    root,
                    path,
                    SkipReason::Vendor,
                    SkipSource::Detector,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            if mode == IndexMode::Fast && fast_mode_skip_path(&rel) {
                push_skip(
                    root,
                    path,
                    SkipReason::FastMode,
                    SkipSource::FastMode,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|err| OkError::Index(err.to_string()))?;
            if metadata.len() > max_size {
                push_skip(
                    root,
                    path,
                    SkipReason::TooLarge,
                    SkipSource::SizeLimit,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            let language = detect_language(&rel);
            if !is_supported_code(&language) {
                push_skip(
                    root,
                    path,
                    SkipReason::UnsupportedLanguage,
                    SkipSource::LanguageSupport,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            let bytes = fs::read(path)?;
            if bytes.contains(&0) {
                push_skip(
                    root,
                    path,
                    SkipReason::Binary,
                    SkipSource::Detector,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            let content = String::from_utf8_lossy(&bytes);
            let is_generated = likely_generated(&content);
            if is_generated {
                push_skip(
                    root,
                    path,
                    SkipReason::Generated,
                    SkipSource::Detector,
                    true,
                    &mut skipped_paths,
                );
                continue;
            }
            let content_hash = hash_bytes(&bytes);
            files.push(File {
                id: FileId::new(stable_id(&rel.to_string_lossy())),
                repository_id: repository_id.clone(),
                path: rel.clone(),
                language,
                size_bytes: metadata.len(),
                content_hash,
                is_generated,
                is_vendor: false,
            });
            if should_emit_progress(scanned_files, 0) {
                progress.emit_transient(
                    ProgressEvent::new("scan")
                        .scanned(scanned_files)
                        .indexed(files.len())
                        .skipped(skipped_paths.len()),
                );
            }
        }
        let fast_skipped = skipped_paths
            .iter()
            .filter(|path| path.reason == SkipReason::FastMode)
            .count();
        if fast_skipped > 0 {
            warnings.push(format!(
                "fast mode skipped {fast_skipped} docs/examples/testdata/sample path(s)"
            ));
        }
        progress.emit(
            ProgressEvent::new("scan")
                .scanned(scanned_files)
                .indexed(files.len())
                .total(Some(files.len()))
                .skipped(skipped_paths.len())
                .warnings(warnings.clone()),
        );
        let skipped = skipped_paths.len();
        Ok(ScanResult {
            files,
            skipped,
            warnings,
            skipped_paths,
        })
    }
}

#[derive(Debug, Clone)]
struct ScanResult {
    files: Vec<File>,
    skipped: usize,
    warnings: Vec<String>,
    skipped_paths: Vec<SkippedPath>,
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
            scanned_files: self.scanned_files,
            indexed_files: self.indexed_files,
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
    runtime_facts: usize,
    git_history_facts: usize,
    architecture_facts: usize,
}

struct IndexQualityInput<'a> {
    root: &'a Path,
    config: &'a OkConfig,
    scip_report: Option<&'a ScipIndexReport>,
    test_count: usize,
    import_count: usize,
    analysis: AnalysisCounts,
    quality_notes: &'a [String],
    mode: IndexMode,
    phase_reports: &'a [IndexPhaseReport],
    skipped_paths: &'a [SkippedPath],
}

fn index_quality(input: IndexQualityInput<'_>) -> IndexQuality {
    let mut quality_notes = Vec::new();
    quality_notes.extend(input.quality_notes.iter().cloned());
    if !input.skipped_paths.is_empty() {
        quality_notes.push(format!(
            "discovery skipped {} path(s); inspect skip_counts/skipped_paths before treating evidence as complete",
            input.skipped_paths.len()
        ));
    }
    let root = input.root;
    let config = input.config;
    let analysis = input.analysis;
    let build_systems = detect_build_systems(root);
    let codeql_databases = detect_codeql_databases(root);
    let coverage_reports = count_analysis_artifacts(root, &["jacoco.xml", "coverage.xml"]);
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
    if analysis.runtime_facts > 0 {
        semantic_provider_notes.push(format!(
            "runtime analysis facts detected: {}",
            analysis.runtime_facts
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
    if let Some(report) = input.scip_report {
        if report.imported_paths.is_empty() {
            quality_notes.push("SCIP was enabled but no SCIP index was imported".into());
        }
        if report.exact_references == 0 {
            quality_notes.push(
                "exact reference coverage is unavailable; impact and test selection are heuristic"
                    .into(),
            );
        }
        for attempt in &report.generator_attempts {
            if !matches!(
                attempt.status,
                open_kioku_scip::ScipGeneratorStatus::Generated
                    | open_kioku_scip::ScipGeneratorStatus::Skipped
            ) {
                quality_notes.push(format!(
                    "SCIP {} generation {:?}: {}",
                    attempt.language, attempt.status, attempt.message
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
            quality_notes,
        }
    } else {
        if !config.scip.enabled {
            quality_notes
                .push("SCIP disabled; symbol references use tree-sitter/import heuristics".into());
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
            quality_notes,
        }
    }
}

struct GitHistoryIngest {
    snapshot: HistorySnapshot,
    analysis_facts: Vec<AnalysisFact>,
}

impl GitHistoryIngest {
    fn empty() -> Self {
        Self {
            snapshot: HistorySnapshot::empty(),
            analysis_facts: Vec::new(),
        }
    }
}

fn collect_git_history(
    root: &Path,
    files: &[File],
    symbols: &[Symbol],
    max_commits: usize,
    max_files_per_commit: usize,
) -> Result<GitHistoryIngest> {
    let history = open_kioku_git::commit_history(root, max_commits)?;
    let patches = open_kioku_git::commit_patches(root, max_commits)?;
    let symbol_touches = map_symbol_touches(files, symbols, &history, &patches);
    let cochange_records =
        open_kioku_git::cochange_records_from_history(&history, max_files_per_commit);
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
    Ok(GitHistoryIngest {
        snapshot: HistorySnapshot {
            schema_version: HISTORY_SCHEMA_VERSION,
            commits: history.commits,
            file_touches: history.file_touches,
            symbol_touches,
            cochange_edges,
            reviewer_evidence: Vec::new(),
        },
        analysis_facts,
    })
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
            edge_type: GraphEdgeType::ChangedBy,
            range: None,
            confidence: Confidence::from_score((0.45 + record.recency_weight / 4.0).min(0.90)),
            source: format!("git-history:{}", record.commits.join(",")),
            source_type: EvidenceSourceType::GitHistory,
            message,
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

fn dedupe_analysis_facts(mut facts: Vec<AnalysisFact>) -> Vec<AnalysisFact> {
    let mut seen = HashSet::new();
    facts.retain(|fact| seen.insert(fact.id.clone()));
    facts
}

fn should_emit_progress(done: usize, total: usize) -> bool {
    done == total || done % 500 == 0
}

fn mode_quality_notes(mode: IndexMode) -> Vec<String> {
    match mode {
        IndexMode::Full => Vec::new(),
        IndexMode::Balanced => vec![
            "balanced mode: trust-critical passes enabled; expensive optional passes may be skipped when configured"
                .into(),
        ],
        IndexMode::Fast => vec![
            "fast mode: docs, examples, generated files, vendor paths, testdata, unsupported files, and oversized files may be skipped"
                .into(),
        ],
        IndexMode::CrossProject => vec![
            "cross-project mode: source parsing skipped; link already-indexed projects only".into(),
        ],
    }
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

fn build_ignore_matcher(root: &Path, file_name: &str) -> Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    let direct = root.join(file_name);
    if direct.exists() {
        builder.add(&direct);
    }
    if file_name == ".gitignore" {
        let git_exclude = root.join(".git/info/exclude");
        if git_exclude.exists() {
            builder.add(git_exclude);
        }
    }
    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .parents(false)
        .ignore(false)
        .follow_links(false)
        .filter_entry(|entry| !is_heavy_discovery_dir(entry.path()))
        .build()
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if entry.file_name() == file_name {
            builder.add(entry.path());
        }
    }
    builder
        .build()
        .map_err(|err| OkError::Config(err.to_string()))
}

fn is_heavy_discovery_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git" | ".ok" | "target" | "node_modules" | "dist" | "build" | ".venv"
    )
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

fn is_secret_like_path(path: &Path) -> bool {
    path.components().any(|component| {
        let value = component.as_os_str().to_string_lossy().to_ascii_lowercase();
        value == ".env"
            || value.starts_with(".env.")
            || matches!(value.as_str(), ".aws" | ".ssh" | "secrets" | "secret")
            || value.contains("secret")
            || value.contains("credential")
            || value.ends_with("_key")
            || value.ends_with(".pem")
            || value.ends_with(".key")
    })
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
                    edge_type: GraphEdgeType::BelongsTo,
                    range: None,
                    confidence: Confidence::Exact,
                    source: format!("glob:{}", comp_match.matched_glob),
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
                        edge_type: GraphEdgeType::BelongsTo,
                        range: symbol.range.clone(),
                        confidence: Confidence::Exact,
                        source: format!("glob:{}", comp_match.matched_glob),
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
    use super::{derive_occurrences, map_symbol_touches, Indexer};
    use chrono::{TimeZone, Utc};
    use open_kioku_config::OkConfig;
    use open_kioku_core::{
        CodeChunk, Confidence, EvidenceSourceType, File, FileId, GitChangeKind, GitCommitId,
        GitCommitRecord, GitFileTouch, HistoryRecordId, IndexMode, Language, LineRange, Owner,
        RepositoryId, SkipReason, SkipSource, Symbol, SymbolId, SymbolKind,
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
        }
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
            .any(|note| note.contains("fast mode")));
        assert!(fast
            .manifest
            .phase_reports
            .iter()
            .any(|report| report.phase == "scan" && report.skipped >= 4));
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
            .any(|note| note.contains("balanced mode")));
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
        assert!(snapshot
            .manifest
            .quality
            .quality_notes
            .iter()
            .any(|note| note.contains("source parsing skipped")));
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
        assert_eq!(snapshot.manifest.file_count, 1);
        assert_skip(quality, SkipReason::Denied, SkipSource::SecurityPolicy);
        assert_skip(quality, SkipReason::Hidden, SkipSource::HiddenPolicy);
        assert_skip(quality, SkipReason::Ignored, SkipSource::GitIgnore);
        assert_skip(quality, SkipReason::Ignored, SkipSource::OkIgnore);
        assert_skip(quality, SkipReason::TooLarge, SkipSource::SizeLimit);
        assert_skip(quality, SkipReason::Binary, SkipSource::Detector);
        assert_skip(quality, SkipReason::Generated, SkipSource::Detector);
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
            .any(|note| note.contains("discovery skipped")));
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
