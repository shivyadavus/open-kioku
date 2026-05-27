use chrono::Utc;
use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use open_kioku_config::OkConfig;
use open_kioku_core::{
    CodeChunk, Confidence, EvidenceSourceType, File, FileId, Import, IndexManifest, Repository,
    RepositoryId, Symbol, SymbolOccurrence, TestTarget,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_languages::{
    detect_language, is_supported_code, likely_generated, likely_vendor_path,
};
use open_kioku_parse::{HeuristicParser, Parser};
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct IndexSnapshot {
    pub manifest: IndexManifest,
    pub files: Vec<File>,
    pub symbols: Vec<Symbol>,
    pub chunks: Vec<CodeChunk>,
    pub tests: Vec<TestTarget>,
    pub imports: Vec<Import>,
    pub occurrences: Vec<SymbolOccurrence>,
}

#[derive(Debug, Clone)]
pub struct IndexProgress {
    pub scanned_files: usize,
    pub indexed_files: usize,
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
        let root = root.as_ref().canonicalize()?;
        let repo_id = RepositoryId::new(stable_id(root.to_string_lossy().as_ref()));
        let files = self.scan_files(&root, config, &repo_id)?;
        let parsed = files
            .par_iter()
            .map(|file| -> Result<_> {
                let bytes = fs::read(root.join(&file.path))?;
                let content = String::from_utf8_lossy(&bytes).into_owned();
                Ok(self.parser.parse(file, &content))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut symbols = parsed
            .iter()
            .flat_map(|file| file.symbols.clone())
            .collect::<Vec<_>>();
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
        let mut occurrences = derive_occurrences(&chunks, &symbols);
        if config.scip.enabled {
            let imported =
                open_kioku_scip::import_configured_scip_files(&root, &config.scip.paths, &repo_id)?;
            symbols.extend(imported.symbols);
            occurrences.extend(imported.occurrences);
        }
        let repository = Repository {
            id: repo_id,
            name: config.repo.name.clone(),
            root: root.clone(),
            branch: open_kioku_git::branch(&root),
            commit: open_kioku_git::commit(&root),
            indexed_at: Some(Utc::now()),
        };
        let manifest = IndexManifest {
            repository,
            file_count: files.len(),
            symbol_count: symbols.len(),
            chunk_count: chunks.len(),
            indexed_at: Utc::now(),
            schema_version: 1,
        };
        Ok(IndexSnapshot {
            manifest,
            files,
            symbols,
            chunks,
            tests,
            imports,
            occurrences,
        })
    }

    fn scan_files(
        &self,
        root: &Path,
        config: &OkConfig,
        repository_id: &RepositoryId,
    ) -> Result<Vec<File>> {
        let max_size = config.max_file_size_bytes()?;
        let excludes = compile_globs(&config.index.exclude)?;
        let denied = compile_globs(&config.paths.deny)?;
        let mut builder = WalkBuilder::new(root);
        builder.hidden(!config.security.allow_hidden_files);
        builder.git_ignore(true).git_exclude(true).parents(true);
        let mut files = Vec::new();
        for entry in builder.build() {
            let entry = entry.map_err(|err| OkError::Index(err.to_string()))?;
            if !entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
            if excludes.is_match(&rel) || denied.is_match(&rel) {
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
        }
        Ok(files)
    }
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

fn derive_occurrences(chunks: &[CodeChunk], symbols: &[Symbol]) -> Vec<SymbolOccurrence> {
    const MIN_HEURISTIC_NAME_LEN: usize = 4;
    let is_word_match = |text: &str, name: &str| -> bool {
        let mut start = 0;
        while let Some(pos) = text[start..].find(name) {
            let abs = start + pos;
            let before_ok = abs == 0
                || !text.as_bytes()[abs - 1].is_ascii_alphanumeric()
                    && text.as_bytes()[abs - 1] != b'_';
            let after = abs + name.len();
            let after_ok = after >= text.len()
                || !text.as_bytes()[after].is_ascii_alphanumeric()
                    && text.as_bytes()[after] != b'_';
            if before_ok && after_ok {
                return true;
            }
            start = abs + 1;
        }
        false
    };

    let mut occurrences = Vec::new();
    for symbol in symbols {
        occurrences.push(SymbolOccurrence {
            symbol_id: symbol.id.clone(),
            file_id: symbol.file_id.clone(),
            range: symbol.range.clone(),
            is_definition: true,
            confidence: symbol.confidence,
            provenance: symbol.provenance.clone(),
        });
    }
    for chunk in chunks {
        for symbol in symbols {
            if symbol.name.len() < MIN_HEURISTIC_NAME_LEN {
                continue;
            }
            if chunk.symbol_id.as_ref() == Some(&symbol.id) {
                continue;
            }
            if is_word_match(&chunk.text, &symbol.name) {
                occurrences.push(SymbolOccurrence {
                    symbol_id: symbol.id.clone(),
                    file_id: chunk.file_id.clone(),
                    range: Some(chunk.range.clone()),
                    is_definition: false,
                    confidence: Confidence::Low,
                    provenance: EvidenceSourceType::Heuristic,
                });
            }
        }
    }
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
