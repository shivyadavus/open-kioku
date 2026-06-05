use chrono::Utc;
use open_kioku_config::SemanticConfig;
use open_kioku_core::{
    search_result_evidence_ids, CodeChunk, File, LineRange, ScoreComponent, SearchResult, Symbol,
};
use open_kioku_embeddings::{EmbeddingProvider, LocalHashEmbeddingProvider};
use open_kioku_errors::{OkError, Result};
use open_kioku_storage::MetadataStore;
use open_kioku_vector::{ExactFlatVectorIndex, VectorId, VectorRecord, VectorSearchOptions};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: u32 = 1;
const CHUNKER_VERSION: &str = "open-kioku-chunks-v1";
const INDEX_VERSION: &str = "exact-flat-json-v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticManifest {
    pub schema_version: u32,
    pub backend: String,
    pub embedding_provider: String,
    pub embedding_model: String,
    pub dimensions: usize,
    pub distance_metric: String,
    pub chunker_version: String,
    pub index_version: String,
    pub source_commit: Option<String>,
    pub created_at: String,
    pub vector_count: usize,
    pub target_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticStats {
    pub vector_count: usize,
    pub indexed_count: usize,
    pub stale_count: usize,
    pub failed_count: usize,
    pub cache_hits: usize,
    pub cache_misses: usize,
    pub disk_usage_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticStatus {
    pub state: String,
    pub ready: bool,
    pub stale: bool,
    pub corrupt: bool,
    pub provider: String,
    pub backend: String,
    pub model: String,
    pub dimensions: usize,
    pub distance: String,
    pub vector_count: usize,
    pub indexed_count: usize,
    pub stale_count: usize,
    pub failed_count: usize,
    pub disk_usage_bytes: u64,
    pub current_dir: PathBuf,
    pub manifest: Option<SemanticManifest>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticIndexReport {
    pub status: SemanticStatus,
    pub indexed_count: usize,
    pub reused_embeddings: usize,
    pub embedded_count: usize,
    pub removed_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SemanticTarget {
    stable_id: String,
    kind: String,
    file_id: String,
    path: PathBuf,
    line_range: Option<LineRange>,
    symbol_id: Option<String>,
    text: String,
    content_hash: String,
    vector_id: VectorId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmbeddingCacheEntry {
    target_id: String,
    content_hash: String,
    model: String,
    dimensions: usize,
    vector: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EmbeddingCache {
    entries: BTreeMap<String, EmbeddingCacheEntry>,
}

pub struct SemanticIndexManager<'a> {
    repo: PathBuf,
    store: &'a dyn MetadataStore,
    config: SemanticConfig,
}

impl<'a> SemanticIndexManager<'a> {
    pub fn new(
        repo: impl AsRef<Path>,
        store: &'a dyn MetadataStore,
        config: &SemanticConfig,
    ) -> Self {
        Self {
            repo: repo.as_ref().to_path_buf(),
            store,
            config: config.clone(),
        }
    }

    pub fn status(&self) -> SemanticStatus {
        let current = self.current_dir();
        let manifest_path = current.join("manifest.json");
        let stats_path = current.join("stats.json");
        let mut notes = Vec::new();
        if !self.config.enabled {
            notes.push("semantic search is disabled in ok.toml; `ok semantic index` is explicit local opt-in".into());
        }
        if !current.exists() {
            return SemanticStatus {
                state: if self.config.enabled {
                    "missing"
                } else {
                    "disabled"
                }
                .into(),
                ready: false,
                stale: false,
                corrupt: false,
                provider: self.config.provider.clone(),
                backend: self.config.backend.clone(),
                model: self.config.model.clone(),
                dimensions: self.config.dimensions,
                distance: self.config.distance.clone(),
                vector_count: 0,
                indexed_count: 0,
                stale_count: 0,
                failed_count: 0,
                disk_usage_bytes: 0,
                current_dir: current,
                manifest: None,
                notes,
            };
        }

        let manifest = read_json::<SemanticManifest>(&manifest_path);
        let stats = read_json::<SemanticStats>(&stats_path).unwrap_or(SemanticStats {
            vector_count: 0,
            indexed_count: 0,
            stale_count: 0,
            failed_count: 0,
            cache_hits: 0,
            cache_misses: 0,
            disk_usage_bytes: dir_size(&current),
        });
        let corrupt = manifest.is_none() || !current.join("index.json").exists();
        let stale = manifest
            .as_ref()
            .map(|manifest| !self.compatible(manifest))
            .unwrap_or(false);
        if stale {
            notes.push("semantic index manifest is stale for the current semantic config".into());
        }
        if corrupt {
            notes.push("semantic index is corrupt or incomplete".into());
        }
        let ready = !corrupt && !stale;
        SemanticStatus {
            state: if corrupt {
                "corrupt"
            } else if stale {
                "stale"
            } else {
                "ready"
            }
            .into(),
            ready,
            stale,
            corrupt,
            provider: self.config.provider.clone(),
            backend: self.config.backend.clone(),
            model: self.config.model.clone(),
            dimensions: self.config.dimensions,
            distance: self.config.distance.clone(),
            vector_count: stats.vector_count,
            indexed_count: stats.indexed_count,
            stale_count: stats.stale_count,
            failed_count: stats.failed_count,
            disk_usage_bytes: dir_size(&current),
            current_dir: current,
            manifest,
            notes,
        }
    }

    pub fn index(&self) -> Result<SemanticIndexReport> {
        self.build_and_promote()
    }

    pub fn rebuild(&self) -> Result<SemanticIndexReport> {
        let _ = fs::remove_dir_all(self.builds_dir());
        self.build_and_promote()
    }

    pub fn clean(&self, include_cache: bool) -> Result<()> {
        let vectors = self.vectors_dir();
        if include_cache {
            let _ = fs::remove_dir_all(&vectors);
        } else {
            let _ = fs::remove_dir_all(self.current_dir());
            let _ = fs::remove_dir_all(self.builds_dir());
        }
        Ok(())
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.search_with_allowlist(query, limit, None)
    }

    pub fn search_with_allowlist(
        &self,
        query: &str,
        limit: usize,
        allowlist: Option<HashSet<VectorId>>,
    ) -> Result<Vec<SearchResult>> {
        let status = self.status();
        if !status.ready {
            return Err(OkError::Unsupported(format!(
                "semantic index is {}; run `ok semantic index` first",
                status.state
            )));
        }
        let provider = provider_for_config(&self.config)?;
        let index = ExactFlatVectorIndex::load(&self.current_dir().join("index.json"))?;
        let targets = read_targets(&self.current_dir().join("ids.json"))?;
        let query_vector = provider.embed(query)?;
        let hits = index.search(
            &query_vector,
            VectorSearchOptions {
                limit,
                allowlist,
                target_kind: None,
            },
        )?;
        hydrate_hits(self.store, &targets, hits)
    }

    fn build_and_promote(&self) -> Result<SemanticIndexReport> {
        if self.config.backend != "exact-flat" {
            return Err(OkError::Unsupported(format!(
                "semantic backend `{}` is not supported; use exact-flat",
                self.config.backend
            )));
        }
        let provider = provider_for_config(&self.config)?;
        let targets = collect_targets(self.store, &self.config)?;
        let current_cache =
            read_json::<EmbeddingCache>(&self.current_dir().join("embeddings.cache"))
                .unwrap_or_default();
        let build_dir = self
            .builds_dir()
            .join(format!("build-{}", Utc::now().timestamp_millis()));
        fs::create_dir_all(&build_dir)?;

        let mut cache = EmbeddingCache::default();
        let mut index = ExactFlatVectorIndex::new(self.config.dimensions)?;
        let mut cache_hits = 0usize;
        let mut cache_misses = 0usize;
        let mut failed = 0usize;
        let mut counts = BTreeMap::<String, usize>::new();

        for target in &targets {
            *counts.entry(target.kind.clone()).or_default() += 1;
            let cache_key = cache_key(target, &self.config);
            let vector = if let Some(entry) = current_cache.entries.get(&cache_key) {
                if entry.content_hash == target.content_hash
                    && entry.model == self.config.model
                    && entry.dimensions == self.config.dimensions
                {
                    cache_hits += 1;
                    entry.vector.clone()
                } else {
                    cache_misses += 1;
                    provider.embed(&target.text)?
                }
            } else {
                cache_misses += 1;
                provider.embed(&target.text)?
            };
            if vector.len() != self.config.dimensions {
                failed += 1;
                continue;
            }
            index.add(VectorRecord {
                id: target.vector_id,
                target_id: target.stable_id.clone(),
                target_kind: target.kind.clone(),
                vector: vector.clone(),
            })?;
            cache.entries.insert(
                cache_key,
                EmbeddingCacheEntry {
                    target_id: target.stable_id.clone(),
                    content_hash: target.content_hash.clone(),
                    model: self.config.model.clone(),
                    dimensions: self.config.dimensions,
                    vector,
                },
            );
        }

        let manifest = SemanticManifest {
            schema_version: SCHEMA_VERSION,
            backend: self.config.backend.clone(),
            embedding_provider: self.config.provider.clone(),
            embedding_model: self.config.model.clone(),
            dimensions: self.config.dimensions,
            distance_metric: self.config.distance.clone(),
            chunker_version: CHUNKER_VERSION.into(),
            index_version: INDEX_VERSION.into(),
            source_commit: self
                .store
                .manifest()
                .ok()
                .flatten()
                .and_then(|manifest| manifest.repository.commit),
            created_at: Utc::now().to_rfc3339(),
            vector_count: index.stats().vector_count,
            target_counts: counts,
        };
        let stats = SemanticStats {
            vector_count: manifest.vector_count,
            indexed_count: targets.len().saturating_sub(failed),
            stale_count: 0,
            failed_count: failed,
            cache_hits,
            cache_misses,
            disk_usage_bytes: 0,
        };

        write_json(&build_dir.join("manifest.json"), &manifest)?;
        write_json(&build_dir.join("ids.json"), &targets)?;
        write_json(&build_dir.join("embeddings.cache"), &cache)?;
        index.save(&build_dir.join("index.json"))?;
        let mut stats = stats;
        stats.disk_usage_bytes = dir_size(&build_dir);
        write_json(&build_dir.join("stats.json"), &stats)?;

        let current = self.current_dir();
        let previous = self.vectors_dir().join("previous");
        let _ = fs::remove_dir_all(&previous);
        if current.exists() {
            fs::rename(&current, &previous)?;
        }
        if let Err(err) = fs::rename(&build_dir, &current) {
            if previous.exists() {
                let _ = fs::rename(&previous, &current);
            }
            return Err(err.into());
        }
        let _ = fs::remove_dir_all(&previous);

        Ok(SemanticIndexReport {
            status: self.status(),
            indexed_count: targets.len().saturating_sub(failed),
            reused_embeddings: cache_hits,
            embedded_count: cache_misses,
            removed_count: removed_count(&current_cache, &cache),
        })
    }

    fn compatible(&self, manifest: &SemanticManifest) -> bool {
        manifest.schema_version == SCHEMA_VERSION
            && manifest.backend == self.config.backend
            && manifest.embedding_provider == self.config.provider
            && manifest.embedding_model == self.config.model
            && manifest.dimensions == self.config.dimensions
            && manifest.distance_metric == self.config.distance
            && manifest.chunker_version == CHUNKER_VERSION
            && manifest.index_version == INDEX_VERSION
    }

    fn vectors_dir(&self) -> PathBuf {
        self.repo.join(".ok/vectors")
    }

    fn current_dir(&self) -> PathBuf {
        self.vectors_dir().join("current")
    }

    fn builds_dir(&self) -> PathBuf {
        self.vectors_dir().join("builds")
    }
}

pub struct SemanticSearchEngine<'a> {
    manager: SemanticIndexManager<'a>,
}

impl<'a> SemanticSearchEngine<'a> {
    pub fn new(
        repo: impl AsRef<Path>,
        store: &'a dyn MetadataStore,
        config: &SemanticConfig,
    ) -> Self {
        Self {
            manager: SemanticIndexManager::new(repo, store, config),
        }
    }

    pub fn from_config(
        repo: impl AsRef<Path>,
        store: &'a dyn MetadataStore,
        config: &SemanticConfig,
    ) -> Result<Option<Self>> {
        if !config.enabled {
            return Ok(None);
        }
        let engine = Self::new(repo, store, config);
        if !engine.manager.status().ready {
            return Ok(None);
        }
        Ok(Some(engine))
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        self.manager.search(query, limit)
    }
}

pub fn provider_from_config(config: &SemanticConfig) -> Result<Option<Box<dyn EmbeddingProvider>>> {
    if !config.enabled {
        return Ok(None);
    }
    Ok(Some(provider_for_config(config)?))
}

pub fn ensure_enabled(config: &SemanticConfig) -> Result<()> {
    provider_from_config(config).and_then(|provider| {
        provider
            .map(|_| ())
            .ok_or_else(|| OkError::Unsupported("semantic search is disabled in ok.toml".into()))
    })
}

fn provider_for_config(config: &SemanticConfig) -> Result<Box<dyn EmbeddingProvider>> {
    match config.provider.as_str() {
        "local" | "local-hash" | "hash" => Ok(Box::new(LocalHashEmbeddingProvider::new(
            config.dimensions,
        )?)),
        "disabled" => Err(OkError::Unsupported(
            "semantic embedding provider is disabled".into(),
        )),
        "external" if !config.external_provider_allowed => Err(OkError::Unsupported(
            "external semantic providers require explicit opt-in".into(),
        )),
        other => Err(OkError::Unsupported(format!(
            "semantic provider `{other}` is not available; supported offline provider: local"
        ))),
    }
}

fn collect_targets(
    store: &dyn MetadataStore,
    config: &SemanticConfig,
) -> Result<Vec<SemanticTarget>> {
    let files = store.list_files(usize::MAX, 0)?;
    let file_by_id = files
        .iter()
        .map(|file| (file.id.0.clone(), file.clone()))
        .collect::<HashMap<_, _>>();
    let symbols = store.list_symbols(None, usize::MAX, 0)?;
    let symbol_by_id = symbols
        .into_iter()
        .map(|symbol| (symbol.id.0.clone(), symbol))
        .collect::<HashMap<_, _>>();
    let mut targets = Vec::new();
    if config.index_chunks {
        for chunk in store.all_chunks()? {
            let Some(file) = file_by_id.get(&chunk.file_id.0) else {
                continue;
            };
            if excluded_path(file) {
                continue;
            }
            let symbol = chunk
                .symbol_id
                .as_ref()
                .and_then(|id| symbol_by_id.get(&id.0));
            targets.push(target_for_chunk(file, &chunk, symbol, config));
        }
    }
    if config.index_symbols {
        for symbol in symbol_by_id.values() {
            let Some(file) = file_by_id.get(&symbol.file_id.0) else {
                continue;
            };
            if excluded_path(file) {
                continue;
            }
            let text = format!(
                "path: {}\nsymbol: {}\nqualified_name: {}\nkind: {:?}\nlanguage: {:?}",
                file.path.display(),
                symbol.name,
                symbol.qualified_name,
                symbol.kind,
                symbol.language
            );
            targets.push(new_target(
                format!("symbol:{}", symbol.id.0),
                "symbol",
                file,
                symbol.range.clone(),
                Some(symbol.id.0.clone()),
                text,
                config,
            ));
        }
    }
    targets.sort_by(|left, right| left.stable_id.cmp(&right.stable_id));
    Ok(targets)
}

fn target_for_chunk(
    file: &File,
    chunk: &CodeChunk,
    symbol: Option<&Symbol>,
    config: &SemanticConfig,
) -> SemanticTarget {
    let text = format!(
        "path: {}\nlanguage: {:?}\nsymbol: {}\nkind: chunk\n{}",
        file.path.display(),
        chunk.language,
        symbol
            .map(|symbol| symbol.qualified_name.as_str())
            .unwrap_or(""),
        chunk.text
    );
    new_target(
        format!("chunk:{}", chunk.id),
        "chunk",
        file,
        Some(chunk.range.clone()),
        chunk.symbol_id.as_ref().map(|id| id.0.clone()),
        text,
        config,
    )
}

fn new_target(
    stable_id: String,
    kind: &str,
    file: &File,
    line_range: Option<LineRange>,
    symbol_id: Option<String>,
    text: String,
    config: &SemanticConfig,
) -> SemanticTarget {
    let content_hash = stable_hex_hash(&text);
    let vector_id = VectorId(stable_hash(&format!(
        "{}:{}:{}:{}",
        stable_id, kind, config.model, config.dimensions
    )));
    SemanticTarget {
        stable_id,
        kind: kind.into(),
        file_id: file.id.0.clone(),
        path: file.path.clone(),
        line_range,
        symbol_id,
        text,
        content_hash,
        vector_id,
    }
}

fn hydrate_hits(
    store: &dyn MetadataStore,
    targets: &HashMap<String, SemanticTarget>,
    hits: Vec<open_kioku_vector::VectorHit>,
) -> Result<Vec<SearchResult>> {
    let symbols = store
        .list_symbols(None, usize::MAX, 0)?
        .into_iter()
        .map(|symbol| (symbol.id.0.clone(), symbol))
        .collect::<HashMap<_, _>>();
    let mut results = Vec::new();
    for hit in hits {
        let Some(target) = targets.get(&hit.target_id) else {
            continue;
        };
        let evidence = vec![
            "semantic vector similarity from local exact-flat index".into(),
            "embedding provider mode: local; repository source stayed on this machine".into(),
        ];
        let evidence_refs =
            search_result_evidence_ids(&target.path, &target.line_range, evidence.len());
        results.push(SearchResult {
            path: target.path.clone(),
            line_range: target.line_range.clone(),
            snippet: snippet(&target.text),
            symbol: target
                .symbol_id
                .as_ref()
                .and_then(|id| symbols.get(id))
                .cloned(),
            score: hit.score,
            match_reason: "semantic vector similarity".into(),
            evidence,
            evidence_refs: evidence_refs.clone(),
            confidence: hit.score.clamp(0.0, 1.0),
            score_breakdown: vec![ScoreComponent::single(
                "semantic_similarity",
                hit.score,
                evidence_refs,
                "cosine similarity from local exact-flat semantic vector index",
            )],
        });
    }
    Ok(results)
}

fn read_targets(path: &Path) -> Result<HashMap<String, SemanticTarget>> {
    let raw = fs::read(path)?;
    let targets = serde_json::from_slice::<Vec<SemanticTarget>>(&raw)?;
    Ok(targets
        .into_iter()
        .map(|target| (target.stable_id.clone(), target))
        .collect())
}

fn excluded_path(file: &File) -> bool {
    let path = file.path.to_string_lossy().to_ascii_lowercase();
    file.is_vendor
        || file.is_generated
        || path.contains("/vendor/")
        || path.contains("node_modules")
        || path.contains("/target/")
        || path.ends_with("lock")
        || path.ends_with(".lock")
        || path.contains(".env")
        || path.contains("secret")
}

fn cache_key(target: &SemanticTarget, config: &SemanticConfig) -> String {
    format!(
        "{}:{}:{}:{}",
        target.stable_id, target.content_hash, config.model, config.dimensions
    )
}

fn removed_count(old: &EmbeddingCache, new: &EmbeddingCache) -> usize {
    old.entries
        .keys()
        .filter(|key| !new.entries.contains_key(*key))
        .count()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice(&raw).ok())
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| {
            entry
                .metadata()
                .map(|meta| {
                    if meta.is_dir() {
                        dir_size(&entry.path())
                    } else {
                        meta.len()
                    }
                })
                .unwrap_or(0)
        })
        .sum()
}

fn snippet(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with("path:"))
        .unwrap_or_default()
        .trim()
        .chars()
        .take(240)
        .collect()
}

fn stable_hex_hash(value: &str) -> String {
    format!("{:016x}", stable_hash(value))
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, EvidenceSourceType, FileId, Language, RepositoryId, SymbolId, SymbolKind,
    };
    use open_kioku_storage::{IndexData, MetadataStore};
    use open_kioku_storage_sqlite::SqliteStore;

    #[test]
    fn disabled_config_returns_no_provider() {
        let config = SemanticConfig {
            enabled: false,
            ..semantic_config()
        };

        assert!(provider_from_config(&config).unwrap().is_none());
    }

    #[test]
    fn unsupported_provider_is_explicit() {
        let config = SemanticConfig {
            enabled: true,
            provider: "remote-api".into(),
            ..semantic_config()
        };

        let err = match provider_from_config(&config) {
            Ok(_) => panic!("unsupported provider should fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("not available"));
    }

    #[test]
    fn builds_persisted_semantic_index_and_reuses_cache() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(temp.path().join(".ok/index.sqlite")).unwrap();
        let manifest = open_kioku_core::IndexManifest {
            repository: open_kioku_core::Repository {
                id: RepositoryId("repo".into()),
                name: "repo".into(),
                root: temp.path().to_path_buf(),
                branch: Some("main".into()),
                commit: Some("abc".into()),
                indexed_at: Some(Utc::now()),
            },
            file_count: 1,
            symbol_count: 1,
            chunk_count: 1,
            indexed_at: Utc::now(),
            schema_version: 1,
            quality: Default::default(),
        };
        let files = vec![file("file_auth", "src/auth.rs")];
        let symbols = vec![symbol("symbol_issue_token", "issue_token", "file_auth")];
        let chunks = vec![chunk(
            "chunk_auth",
            "file_auth",
            "pub fn issue_token() { create session token }",
            Some("symbol_issue_token"),
        )];
        store
            .replace_index(IndexData {
                manifest: &manifest,
                files: &files,
                symbols: &symbols,
                chunks: &chunks,
                tests: &[],
                imports: &[],
                occurrences: &[],
                analysis_facts: &[],
            })
            .unwrap();

        let config = semantic_config();
        let manager = SemanticIndexManager::new(temp.path(), &store, &config);
        let first = manager.index().unwrap();
        let second = manager.index().unwrap();
        let results = manager.search("issue token", 5).unwrap();

        assert!(temp
            .path()
            .join(".ok/vectors/current/manifest.json")
            .exists());
        assert!(first.indexed_count >= 2);
        assert!(second.reused_embeddings >= first.indexed_count);
        assert_eq!(results[0].path, PathBuf::from("src/auth.rs"));
        assert!(results[0]
            .score_breakdown
            .iter()
            .any(|component| component.signal == "semantic_similarity"));
    }

    fn semantic_config() -> SemanticConfig {
        SemanticConfig {
            enabled: true,
            backend: "exact-flat".into(),
            provider: "local".into(),
            model: "local-hash".into(),
            dimensions: 64,
            distance: "cosine".into(),
            batch_size: 64,
            index_symbols: true,
            index_chunks: true,
            index_docs: true,
            index_memory: true,
            external_provider_allowed: false,
        }
    }

    fn file(id: &str, path: &str) -> File {
        File {
            id: FileId(id.into()),
            repository_id: RepositoryId("repo".into()),
            path: PathBuf::from(path),
            language: Language::Rust,
            size_bytes: 0,
            content_hash: String::new(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn symbol(id: &str, name: &str, file_id: &str) -> Symbol {
        Symbol {
            id: SymbolId(id.into()),
            name: name.into(),
            qualified_name: name.into(),
            kind: SymbolKind::Function,
            file_id: FileId(file_id.into()),
            range: Some(LineRange::single(1)),
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
        }
    }

    fn chunk(id: &str, file_id: &str, text: &str, symbol_id: Option<&str>) -> CodeChunk {
        CodeChunk {
            id: id.into(),
            file_id: FileId(file_id.into()),
            range: LineRange::single(1),
            language: Language::Rust,
            text: text.into(),
            symbol_id: symbol_id.map(|id| SymbolId(id.into())),
        }
    }
}
