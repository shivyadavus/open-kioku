use chrono::Utc;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use open_kioku_config::OkConfig;
use open_kioku_core::{
    CodeChunk, File, FileId, Import, IndexManifest, IndexQuality, Repository, RepositoryId, Symbol,
    SymbolOccurrence, TestTarget,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_languages::{
    detect_language, is_supported_code, likely_generated, likely_vendor_path,
};
use open_kioku_parse::{HeuristicParser, Parser};
use open_kioku_scip::ScipIndexReport;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Clone)]
pub struct IndexSnapshot {
    pub manifest: IndexManifest,
    pub files: Vec<File>,
    pub symbols: Vec<Symbol>,
    pub chunks: Vec<CodeChunk>,
    pub tests: Vec<TestTarget>,
    pub imports: Vec<Import>,
    pub occurrences: Vec<SymbolOccurrence>,
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

    pub fn index_repo_with_progress<F>(
        &self,
        root: impl AsRef<Path>,
        config: &OkConfig,
        on_progress: F,
    ) -> Result<IndexSnapshot>
    where
        F: Fn(IndexProgress) + Sync,
    {
        let root = root.as_ref().canonicalize()?;
        let repo_id = RepositoryId::new(stable_id(root.to_string_lossy().as_ref()));
        let build_hint: Option<String> = if root.join("build.gradle").exists() || root.join("build.gradle.kts").exists() {
            Some("gradle".to_string())
        } else if root.join("pom.xml").exists() {
            Some("maven".to_string())
        } else if root.join("WORKSPACE").exists() || root.join("BUILD.bazel").exists() || root.join("BUILD").exists() {
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
                let parsed = self.parser.parse_with_hint(file, &content, build_hint.as_deref());
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
        on_progress(IndexProgress {
            phase: "occurrences",
            scanned_files: files.len(),
            indexed_files: files.len(),
            total_files: Some(files.len()),
        });
        let mut occurrences = derive_occurrences(&chunks, &symbols);
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
        let quality = index_quality(config, scip_report.as_ref(), tests.len(), imports.len());
        let manifest = IndexManifest {
            repository,
            file_count: files.len(),
            symbol_count: symbols.len(),
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: 1,
            quality,
        };
        Ok(IndexSnapshot {
            manifest,
            files,
            symbols,
            chunks,
            tests,
            imports,
            occurrences,
            scip: scip_report,
        })
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

fn index_quality(
    config: &OkConfig,
    scip_report: Option<&ScipIndexReport>,
    test_count: usize,
    import_count: usize,
) -> IndexQuality {
    let mut quality_notes = Vec::new();
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
            quality_notes,
        }
    }
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
    use super::derive_occurrences;
    use open_kioku_core::{
        CodeChunk, Confidence, EvidenceSourceType, FileId, Language, LineRange, Symbol, SymbolId,
        SymbolKind,
    };

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
}
