use chrono::Utc;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use open_kioku_config::OkConfig;
use open_kioku_core::{
    AnalysisFact, CodeChunk, Confidence, EvidenceSourceType, File, FileId, GitCochangeEdge,
    GitCommitId, GraphEdgeType, GraphNodeType, HistoryRecordId, HistorySnapshot, Import,
    IndexManifest, IndexQuality, LineRange, Repository, RepositoryId, Symbol, SymbolOccurrence,
    TestTarget, HISTORY_SCHEMA_VERSION,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_languages::{
    detect_language, is_supported_code, likely_generated, likely_vendor_path,
};
use open_kioku_parse::{HeuristicParser, Parser};
use open_kioku_scip::ScipIndexReport;
use rayon::prelude::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

const MAX_HISTORY_COCHANGE_EDGES: usize = 5000;

#[derive(Debug, Clone)]
pub struct IndexSnapshot {
    pub manifest: IndexManifest,
    pub files: Vec<File>,
    pub symbols: Vec<Symbol>,
    pub chunks: Vec<CodeChunk>,
    pub tests: Vec<TestTarget>,
    pub imports: Vec<Import>,
    pub occurrences: Vec<SymbolOccurrence>,
    pub analysis_facts: Vec<AnalysisFact>,
    pub scip: Option<ScipIndexReport>,
}

#[derive(Debug, Clone)]
pub struct IndexProgress {
    pub phase: &'static str,
    pub scanned_files: usize,
    pub indexed_files: usize,
    pub total_files: Option<usize>,
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

    pub fn index_repo_with_history_and_progress<F>(
        &self,
        root: impl AsRef<Path>,
        config: &OkConfig,
        on_progress: F,
    ) -> Result<(IndexSnapshot, HistorySnapshot)>
    where
        F: Fn(IndexProgress) + Sync,
    {
        let root = root.as_ref().canonicalize()?;
        let repo_id = RepositoryId::new(stable_id(root.to_string_lossy().as_ref()));
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
        let files = self.scan_files(&root, config, &repo_id, &on_progress)?;
        on_progress(IndexProgress {
            phase: "parse",
            scanned_files: files.len(),
            indexed_files: 0,
            total_files: Some(files.len()),
        });
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
                    on_progress(IndexProgress {
                        phase: "parse",
                        scanned_files: files.len(),
                        indexed_files,
                        total_files: Some(files.len()),
                    });
                }
                Ok(parsed)
            })
            .collect::<Result<Vec<_>>>()?;
        on_progress(IndexProgress {
            phase: "extract",
            scanned_files: files.len(),
            indexed_files: files.len(),
            total_files: Some(files.len()),
        });

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
        let static_analysis_facts = analysis_facts.len();
        on_progress(IndexProgress {
            phase: "analysis",
            scanned_files: files.len(),
            indexed_files: files.len(),
            total_files: Some(files.len()),
        });
        let runtime_facts = collect_runtime_analysis_facts(&root, &files)?;
        let runtime_analysis_facts = runtime_facts.len();
        analysis_facts.extend(runtime_facts);
        on_progress(IndexProgress {
            phase: "history",
            scanned_files: files.len(),
            indexed_files: files.len(),
            total_files: Some(files.len()),
        });
        let git_history = if config.history.enabled {
            collect_git_history(
                &root,
                &files,
                config.history.max_commits,
                config.history.max_files_per_commit,
            )?
        } else {
            GitHistoryIngest::empty()
        };
        let git_history_fact_count = git_history.analysis_facts.len();
        analysis_facts.extend(git_history.analysis_facts);
        on_progress(IndexProgress {
            phase: "occurrences",
            scanned_files: files.len(),
            indexed_files: files.len(),
            total_files: Some(files.len()),
        });
        let mut occurrences = derive_occurrences(&chunks, &symbols);

        let mut arch_fact_count = 0;
        if let Ok(Some(policy)) = open_kioku_config::load_architecture_policy(&root) {
            if let Ok(resolver) = open_kioku_architecture::PolicyResolver::new(&policy) {
                on_progress(IndexProgress {
                    phase: "architecture",
                    scanned_files: files.len(),
                    indexed_files: files.len(),
                    total_files: Some(files.len()),
                });
                let arch_facts = collect_architecture_facts(&resolver, &files, &symbols);
                arch_fact_count = arch_facts.len();
                analysis_facts.extend(arch_facts);
            }
        }

        let mut scip_report = None;
        if config.scip.enabled {
            on_progress(IndexProgress {
                phase: "scip",
                scanned_files: files.len(),
                indexed_files: files.len(),
                total_files: Some(files.len()),
            });
            let (imported, report) =
                open_kioku_scip::prepare_and_import_scip(&root, &config.scip, &repo_id)?;
            symbols.extend(imported.symbols);
            dedupe_symbols(&mut symbols);
            occurrences.extend(imported.occurrences);
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
        let quality = index_quality(
            &root,
            config,
            scip_report.as_ref(),
            tests.len(),
            imports.len(),
            AnalysisCounts {
                static_facts: static_analysis_facts,
                runtime_facts: runtime_analysis_facts,
                git_history_facts: git_history_fact_count,
                architecture_facts: arch_fact_count,
            },
        );
        let manifest = IndexManifest {
            repository,
            file_count: files.len(),
            symbol_count: symbols.len(),
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: 1,
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
                occurrences,
                analysis_facts,
                scip: scip_report,
            },
            git_history.snapshot,
        ))
    }

    fn scan_files(
        &self,
        root: &Path,
        config: &OkConfig,
        repository_id: &RepositoryId,
        on_progress: &dyn Fn(IndexProgress),
    ) -> Result<Vec<File>> {
        let max_size = config.max_file_size_bytes()?;
        let excludes = compile_globs(&config.index.exclude)?;
        let denied = compile_globs(&config.paths.deny)?;
        let mut builder = WalkBuilder::new(root);
        builder.hidden(!config.security.allow_hidden_files);
        builder.git_ignore(true).git_exclude(true).parents(true);
        let mut files = Vec::new();
        let mut scanned_files = 0;
        on_progress(IndexProgress {
            phase: "scan",
            scanned_files,
            indexed_files: files.len(),
            total_files: None,
        });
        for entry in builder.build() {
            let entry = entry.map_err(|err| OkError::Index(err.to_string()))?;
            if !entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            scanned_files += 1;
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
            if excludes.is_match(&rel) || denied.is_match(&rel) {
                if should_emit_progress(scanned_files, 0) {
                    on_progress(IndexProgress {
                        phase: "scan",
                        scanned_files,
                        indexed_files: files.len(),
                        total_files: None,
                    });
                }
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|err| OkError::Index(err.to_string()))?;
            if metadata.len() > max_size {
                continue;
            }
            let language = detect_language(&rel);
            if !is_supported_code(&language) {
                continue;
            }
            let bytes = fs::read(path)?;
            if bytes.contains(&0) {
                continue;
            }
            let content = String::from_utf8_lossy(&bytes);
            let content_hash = hash_bytes(&bytes);
            files.push(File {
                id: FileId::new(stable_id(&rel.to_string_lossy())),
                repository_id: repository_id.clone(),
                path: rel.clone(),
                language,
                size_bytes: metadata.len(),
                content_hash,
                is_generated: likely_generated(&content),
                is_vendor: likely_vendor_path(&rel),
            });
            if should_emit_progress(scanned_files, 0) {
                on_progress(IndexProgress {
                    phase: "scan",
                    scanned_files,
                    indexed_files: files.len(),
                    total_files: None,
                });
            }
        }
        on_progress(IndexProgress {
            phase: "scan",
            scanned_files,
            indexed_files: files.len(),
            total_files: Some(files.len()),
        });
        Ok(files)
    }
}

#[derive(Debug, Clone, Copy)]
struct AnalysisCounts {
    static_facts: usize,
    runtime_facts: usize,
    git_history_facts: usize,
    architecture_facts: usize,
}

fn index_quality(
    root: &Path,
    config: &OkConfig,
    scip_report: Option<&ScipIndexReport>,
    test_count: usize,
    import_count: usize,
    analysis: AnalysisCounts,
) -> IndexQuality {
    let mut quality_notes = Vec::new();
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
    if let Some(report) = scip_report {
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
            scip_enabled: config.scip.enabled,
            scip_mode,
            scip_indexes_imported: report.imported_paths.len(),
            scip_symbols: report.symbols,
            scip_occurrences: report.occurrences,
            scip_exact_references: report.exact_references,
            test_count,
            import_count,
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
            scip_enabled: config.scip.enabled,
            scip_mode,
            scip_indexes_imported: 0,
            scip_symbols: 0,
            scip_occurrences: 0,
            scip_exact_references: 0,
            test_count,
            import_count,
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
    max_commits: usize,
    max_files_per_commit: usize,
) -> Result<GitHistoryIngest> {
    let history = open_kioku_git::commit_history(root, max_commits)?;
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
            symbol_touches: Vec::new(),
            cochange_edges,
            reviewer_evidence: Vec::new(),
        },
        analysis_facts,
    })
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

fn collect_runtime_analysis_facts(root: &Path, files: &[File]) -> Result<Vec<AnalysisFact>> {
    let files_by_path = files
        .iter()
        .map(|file| (normalize_path(&file.path.to_string_lossy()), file))
        .collect::<HashMap<_, _>>();
    let mut facts = Vec::new();
    for runtime_root in [
        root.join(".ok/runtime"),
        root.join(".ok/analysis/runtime"),
        root.join(".ok/analysis"),
    ] {
        if !runtime_root.is_dir() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&runtime_root)
            .max_depth(3)
            .into_iter()
            .filter_map(|entry| entry.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let lower_name = file_name.to_ascii_lowercase();
            if !lower_name.ends_with(".jsonl")
                || !(lower_name.contains("span")
                    || lower_name.contains("trace")
                    || lower_name.contains("runtime")
                    || lower_name.contains("otel")
                    || lower_name.contains("log")
                    || lower_name.contains("incident")
                    || lower_name.contains("error")
                    || lower_name.contains("failure"))
            {
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|err| OkError::Index(err.to_string()))?;
            if metadata.len() > 5 * 1024 * 1024 {
                continue;
            }
            let content = fs::read_to_string(path)?;
            for (idx, line) in content.lines().enumerate() {
                if facts.len() >= 10_000 {
                    return Ok(dedupe_analysis_facts(facts));
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                    continue;
                };
                let Some(source_file) =
                    json_string(&value, &["file", "code.filepath", "source.file"])
                else {
                    continue;
                };
                let normalized = normalize_runtime_file(root, &source_file);
                let Some(file) = files_by_path.get(&normalized).copied() else {
                    continue;
                };
                let line_number = json_u32(&value, &["line", "code.lineno", "source.line"]);
                if let Some(fact) = runtime_endpoint_fact(file, &value, line_number, path, idx + 1)
                {
                    facts.push(fact);
                }
                if let Some(fact) = runtime_table_fact(file, &value, line_number, path, idx + 1) {
                    facts.push(fact);
                }
                if let Some(fact) = runtime_incident_fact(file, &value, line_number, path, idx + 1)
                {
                    facts.push(fact);
                }
            }
        }
    }
    Ok(dedupe_analysis_facts(facts))
}

fn runtime_endpoint_fact(
    file: &File,
    value: &Value,
    line_number: Option<u32>,
    artifact: &Path,
    artifact_line: usize,
) -> Option<AnalysisFact> {
    let route = json_string(
        value,
        &[
            "http.route",
            "http.target",
            "url.path",
            "route",
            "name",
            "span.name",
        ],
    )?;
    if !route.contains('/') {
        return None;
    }
    let method = json_string(
        value,
        &[
            "http.request.method",
            "http.method",
            "method",
            "request.method",
        ],
    )
    .unwrap_or_else(|| "HTTP".into())
    .to_ascii_uppercase();
    Some(runtime_fact(
        file,
        GraphEdgeType::ExposesEndpoint,
        GraphNodeType::Endpoint,
        format!("{method} {route}"),
        line_number,
        RuntimeFactSource {
            artifact,
            artifact_line,
            message: "runtime endpoint observed in local trace artifact",
        },
    ))
}

fn runtime_table_fact(
    file: &File,
    value: &Value,
    line_number: Option<u32>,
    artifact: &Path,
    artifact_line: usize,
) -> Option<AnalysisFact> {
    let statement = json_string(value, &["db.statement", "sql", "database.statement"])?;
    let table = extract_sql_table(&statement)?;
    Some(runtime_fact(
        file,
        GraphEdgeType::ReadsTable,
        GraphNodeType::DatabaseTable,
        table,
        line_number,
        RuntimeFactSource {
            artifact,
            artifact_line,
            message: "runtime database access observed in local trace artifact",
        },
    ))
}

fn runtime_incident_fact(
    file: &File,
    value: &Value,
    line_number: Option<u32>,
    artifact: &Path,
    artifact_line: usize,
) -> Option<AnalysisFact> {
    let message = json_string(
        value,
        &[
            "error.message",
            "exception.message",
            "log.message",
            "message",
            "event.message",
            "span.status.message",
            "name",
            "span.name",
        ],
    )?;
    let signal = compact_runtime_message(&message)?;
    Some(runtime_fact(
        file,
        GraphEdgeType::FailedIn,
        GraphNodeType::RuntimeError,
        signal,
        line_number,
        RuntimeFactSource {
            artifact,
            artifact_line,
            message: "runtime incident observed in local log or failure artifact",
        },
    ))
}

struct RuntimeFactSource<'a> {
    artifact: &'a Path,
    artifact_line: usize,
    message: &'static str,
}

fn runtime_fact(
    file: &File,
    edge_type: GraphEdgeType,
    target_kind: GraphNodeType,
    target: String,
    line_number: Option<u32>,
    source: RuntimeFactSource<'_>,
) -> AnalysisFact {
    AnalysisFact {
        id: stable_id(&format!(
            "runtime:{}:{:?}:{}:{}",
            file.path.display(),
            edge_type,
            target,
            source.artifact_line
        )),
        file_id: file.id.clone(),
        symbol_id: None,
        target,
        target_kind,
        edge_type,
        range: line_number.map(LineRange::single),
        confidence: Confidence::High,
        source: format!("open-kioku-runtime:{}", source.artifact.display()),
        source_type: EvidenceSourceType::Runtime,
        message: source.message.into(),
    }
}

fn json_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = nested_json_value(value, key).and_then(Value::as_str) {
            return Some(value.to_string());
        }
        if let Some(value) = value
            .get("attributes")
            .and_then(|attributes| nested_json_value(attributes, key))
            .and_then(Value::as_str)
        {
            return Some(value.to_string());
        }
        if let Some(value) = value
            .get("resource")
            .and_then(|resource| resource.get("attributes"))
            .and_then(|attributes| nested_json_value(attributes, key))
            .and_then(Value::as_str)
        {
            return Some(value.to_string());
        }
    }
    None
}

fn json_u32(value: &Value, keys: &[&str]) -> Option<u32> {
    for key in keys {
        if let Some(value) = nested_json_value(value, key)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
        {
            return Some(value);
        }
        if let Some(value) = value
            .get("attributes")
            .and_then(|attributes| nested_json_value(attributes, key))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
        {
            return Some(value);
        }
    }
    None
}

fn nested_json_value<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    if let Some(exact) = value.get(key) {
        return Some(exact);
    }
    let mut current = value;
    for segment in key.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn normalize_runtime_file(root: &Path, value: &str) -> String {
    let path = Path::new(value);
    let rel = if path.is_absolute() {
        path.strip_prefix(root).unwrap_or(path)
    } else {
        path
    };
    normalize_path(&rel.to_string_lossy())
}

fn normalize_path(value: &str) -> String {
    value.trim_start_matches("./").replace('\\', "/")
}

fn normalize_history_path(path: &Path) -> String {
    normalize_path(&path.to_string_lossy())
}

fn extract_sql_table(statement: &str) -> Option<String> {
    let lower = statement.to_ascii_lowercase();
    for keyword in [" from ", " join ", " update ", " into "] {
        if let Some(index) = lower.find(keyword) {
            let start = index + keyword.len();
            let table = statement[start..]
                .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '.')
                .find(|part| !part.is_empty())?;
            return Some(table.to_string());
        }
    }
    None
}

fn compact_runtime_message(message: &str) -> Option<String> {
    let value = message.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(160).collect())
}

fn dedupe_analysis_facts(mut facts: Vec<AnalysisFact>) -> Vec<AnalysisFact> {
    let mut seen = HashSet::new();
    facts.retain(|fact| seen.insert(fact.id.clone()));
    facts
}

fn should_emit_progress(done: usize, total: usize) -> bool {
    done == total || done % 500 == 0
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

#[cfg(test)]
mod tests {
    use super::{derive_occurrences, Indexer};
    use open_kioku_config::OkConfig;
    use open_kioku_core::{
        CodeChunk, Confidence, EvidenceSourceType, FileId, Language, LineRange, Symbol, SymbolId,
        SymbolKind,
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
  }
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
        assert_eq!(snapshot.manifest.quality.runtime_analysis_facts, 3);
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
        assert!(snapshot
            .manifest
            .quality
            .semantic_provider_notes
            .iter()
            .any(|note| note.contains("build systems detected")));
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
            // Note: we could emit an unmapped target fact here, but the issue says "Mark unmapped files or symbols explicitly".
            // We can emit an AnalysisFact indicating Unmapped to surface it in the graph.
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
                source_type: EvidenceSourceType::Heuristic, // or Policy
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
