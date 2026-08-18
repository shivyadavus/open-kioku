use chrono::Utc;
use open_kioku_config::SemanticConfig;
use open_kioku_core::{
    search_result_evidence_ids, CodeChunk, File, LineRange, ScoreComponent, SearchResult, Symbol,
};
use open_kioku_embeddings::{
    neural_model_cache_dir, EmbeddingProvider, FastEmbedEmbeddingProvider,
    LocalHashEmbeddingProvider, LocalNeuralModel, QWEN3_MAX_LENGTH,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_storage::MetadataStore;
use open_kioku_vector::{
    AnnScalarKind, ExactFlatVectorIndex, UsearchHnswVectorIndex, VectorId, VectorRecord,
    VectorSearchOptions, PRODUCTION_HNSW_PARAMETERS, PRODUCTION_HNSW_PROFILE,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: u32 = 2;
const CHUNKER_VERSION: &str = "open-kioku-chunks-v1";
const EXACT_INDEX_VERSION: &str = "exact-flat-json-v1";
const HNSW_INDEX_VERSION: &str = PRODUCTION_HNSW_PROFILE;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticManifest {
    pub schema_version: u32,
    pub backend: String,
    pub embedding_provider: String,
    pub embedding_model: String,
    #[serde(default)]
    pub embedding_implementation: String,
    #[serde(default)]
    pub model_artifact_sha256: Option<String>,
    pub dimensions: usize,
    pub distance_metric: String,
    pub chunker_version: String,
    pub index_version: String,
    pub source_commit: Option<String>,
    #[serde(default)]
    pub source_index_fingerprint: Option<String>,
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
    pub ann_active: bool,
    pub ann_profile: Option<String>,
    pub model: String,
    pub embedding_implementation: String,
    pub model_artifact_sha256: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticSearchRouting {
    pub selected_backend: String,
    pub total_vector_count: usize,
    pub eligible_candidate_count: usize,
    pub filter_selectivity: String,
    /// Whether any supported semantic candidate filter was enforced before top-k selection.
    /// This is broader than `path_scope_enforced`: callers may supply a precomputed vector
    /// allowlist for language, project, symbol, module, or another repository-validated scope.
    #[serde(default)]
    pub filter_scope_enforced: bool,
    pub path_scope_enforced: bool,
    pub routing_reason: String,
    pub ann_profile: Option<String>,
    pub caveats: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SemanticSearchReport {
    pub results: Vec<SearchResult>,
    pub routing: SemanticSearchRouting,
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
    #[serde(default)]
    embedding_implementation: String,
    #[serde(default)]
    model_artifact_sha256: Option<String>,
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
                ann_active: false,
                ann_profile: None,
                model: self.config.model.clone(),
                embedding_implementation: expected_embedding_implementation(&self.config),
                model_artifact_sha256: None,
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
        let corrupt = manifest
            .as_ref()
            .map(|manifest| !index_artifacts_present(&current, &manifest.backend))
            .unwrap_or(true);
        let source_stale = manifest
            .as_ref()
            .is_some_and(|manifest| !self.source_generation_compatible(manifest));
        let stale = manifest
            .as_ref()
            .map(|manifest| !self.compatible(manifest))
            .unwrap_or(false);
        if source_stale {
            notes.push(
                "semantic index is stale for the current authoritative index generation; rebuild semantic index"
                    .into(),
            );
        } else if stale {
            notes.push("semantic index manifest is stale for the current semantic config".into());
        }
        if corrupt {
            notes.push("semantic index is corrupt or incomplete".into());
        }
        let ready = !corrupt && !stale;
        let resolved_backend = manifest
            .as_ref()
            .map(|value| value.backend.clone())
            .unwrap_or_else(|| self.config.backend.clone());
        let ann_active = backend_is_ann(&resolved_backend) && ready;
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
            backend: resolved_backend,
            ann_active,
            ann_profile: ann_active.then(|| PRODUCTION_HNSW_PROFILE.to_string()),
            model: self.config.model.clone(),
            embedding_implementation: manifest
                .as_ref()
                .map(|value| value.embedding_implementation.clone())
                .unwrap_or_else(|| expected_embedding_implementation(&self.config)),
            model_artifact_sha256: manifest
                .as_ref()
                .and_then(|value| value.model_artifact_sha256.clone()),
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
        self.build_and_promote(false)
    }

    pub fn index_with_model_download(&self) -> Result<SemanticIndexReport> {
        self.build_and_promote(true)
    }

    pub fn rebuild(&self) -> Result<SemanticIndexReport> {
        let _ = fs::remove_dir_all(self.builds_dir());
        self.build_and_promote(false)
    }

    pub fn rebuild_with_model_download(&self) -> Result<SemanticIndexReport> {
        let _ = fs::remove_dir_all(self.builds_dir());
        self.build_and_promote(true)
    }

    pub fn clean(&self, include_cache: bool) -> Result<()> {
        let vectors = self.vectors_dir();
        if include_cache {
            let _ = fs::remove_dir_all(&vectors);
            let _ = fs::remove_dir_all(self.models_dir());
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
        Ok(self
            .search_with_allowlist_report(query, limit, allowlist)?
            .results)
    }

    /// Search a repository-validated semantic candidate set and return routing diagnostics.
    ///
    /// The allowlist is intentionally generic: higher layers can derive it from language,
    /// project/workspace, symbol/module, path, or another scope already represented in repository
    /// metadata. The semantic layer treats the set as an enforced eligibility boundary and never
    /// widens it while choosing between exact-flat and ANN.
    pub fn search_with_allowlist_report(
        &self,
        query: &str,
        limit: usize,
        allowlist: Option<HashSet<VectorId>>,
    ) -> Result<SemanticSearchReport> {
        self.search_routed(query, limit, allowlist, &[])
    }

    pub fn search_with_path_prefixes(
        &self,
        query: &str,
        limit: usize,
        path_prefixes: &[String],
    ) -> Result<SemanticSearchReport> {
        self.search_routed(query, limit, None, path_prefixes)
    }

    fn search_routed(
        &self,
        query: &str,
        limit: usize,
        caller_allowlist: Option<HashSet<VectorId>>,
        path_prefixes: &[String],
    ) -> Result<SemanticSearchReport> {
        let status = self.status();
        if !status.ready {
            return Err(OkError::Unsupported(format!(
                "semantic index is {}; run `ok semantic index` first",
                status.state
            )));
        }
        let provider = provider_for_config(&self.config, false, &self.models_dir())?;
        let targets = read_targets(&self.current_dir().join("ids.json"))?;
        let total_vector_count = targets.len();
        let normalized_prefixes = normalize_path_prefixes(path_prefixes)?;
        let path_scope_enforced = !normalized_prefixes.is_empty();
        let scoped_ids = if path_scope_enforced {
            Some(
                targets
                    .values()
                    .filter(|target| target_matches_path_scope(target, &normalized_prefixes))
                    .map(|target| target.vector_id)
                    .collect::<HashSet<_>>(),
            )
        } else {
            None
        };
        let allowlist = intersect_allowlists(caller_allowlist, scoped_ids);
        // Any concrete allowlist is an enforced eligibility boundary, regardless of which
        // higher-level filter produced it. Backend routing must depend on the effective candidate
        // population, not on whether the filter happened to be a path prefix.
        let filter_scope_enforced = allowlist.is_some();
        let eligible_candidate_count = allowlist
            .as_ref()
            .map(|ids| {
                targets
                    .values()
                    .filter(|target| ids.contains(&target.vector_id))
                    .count()
            })
            .unwrap_or(total_vector_count);
        let filter_selectivity = semantic_filter_selectivity(
            total_vector_count,
            eligible_candidate_count,
            filter_scope_enforced,
        );
        let manifest_backend = status
            .manifest
            .as_ref()
            .map(|manifest| manifest.backend.as_str())
            .ok_or_else(|| OkError::Storage("semantic manifest missing for ready index".into()))?;
        let mut selected_backend = manifest_backend.to_string();
        let mut routing_reason = if path_scope_enforced {
            format!(
                "validated path scope leaves {eligible_candidate_count} of {total_vector_count} semantic candidates"
            )
        } else if filter_scope_enforced {
            format!(
                "validated semantic candidate filter leaves {eligible_candidate_count} of {total_vector_count} candidates"
            )
        } else {
            format!("semantic query uses the persisted {manifest_backend} backend")
        };
        let mut caveats = Vec::new();

        let should_use_scoped_exact = filter_scope_enforced
            && should_route_scoped_exact(&self.config, manifest_backend, eligible_candidate_count);
        let query_vector = provider.embed_query(query)?;
        let options = VectorSearchOptions {
            limit,
            allowlist: allowlist.clone(),
            target_kind: None,
        };

        let hits = if should_use_scoped_exact {
            match build_scoped_exact_index(
                &self.current_dir(),
                &targets,
                allowlist.as_ref().ok_or_else(|| {
                  OkError::Storage(
                      "validated semantic candidate filter lost its allowlist; refusing to widen retrieval"
                          .into(),
                  )
              })?,
                &self.config,
            )? {
                Some(index) => {
                    selected_backend = "exact-flat".into();
                    routing_reason = format!(
                        "auto backend selected exact-flat because the enforced filter has {eligible_candidate_count} eligible candidates below ann_min_rows={} (total vectors {total_vector_count})",
                        self.config.ann_min_rows
                    );
                    index.search(
                        &query_vector,
                        VectorSearchOptions {
                            limit,
                            allowlist: None,
                            target_kind: None,
                        },
                    )?
                }
                None => {
                    caveats.push(
                        "filtered exact-flat routing could not reconstruct a complete exact subset from the local embedding cache; retained pre-filtered ANN search without widening scope"
                            .into(),
                    );
                    routing_reason = format!(
                        "auto backend retained {manifest_backend} because the exact subset cache was incomplete; the enforced candidate filter still applies before ANN top-k"
                    );
                    search_persisted_backend(
                        &self.current_dir(),
                        manifest_backend,
                        &query_vector,
                        options,
                    )?
                }
            }
        } else {
            if filter_scope_enforced && backend_is_ann(manifest_backend) {
                routing_reason = if self.config.backend == "auto" {
                    format!(
                        "auto backend retained {manifest_backend} because the enforced filter has {eligible_candidate_count} eligible candidates at or above ann_min_rows={} (total vectors {total_vector_count})",
                        self.config.ann_min_rows
                    )
                } else {
                    format!(
                        "explicit backend `{}` retained {manifest_backend}; the enforced candidate filter applies before ANN top-k",
                        self.config.backend
                    )
                };
            }
            search_persisted_backend(
                &self.current_dir(),
                manifest_backend,
                &query_vector,
                options,
            )?
        };
        let results = hydrate_hits(self.store, &targets, hits)?;
        Ok(SemanticSearchReport {
            results,
            routing: SemanticSearchRouting {
                selected_backend: selected_backend.clone(),
                total_vector_count,
                eligible_candidate_count,
                filter_selectivity,
                filter_scope_enforced,
                path_scope_enforced,
                routing_reason,
                ann_profile: backend_is_ann(&selected_backend)
                    .then(|| PRODUCTION_HNSW_PROFILE.to_string()),
                caveats,
            },
        })
    }

    fn build_and_promote(&self, allow_model_download: bool) -> Result<SemanticIndexReport> {
        let provider = provider_for_config(&self.config, allow_model_download, &self.models_dir())?;
        let descriptor = provider.descriptor();
        if descriptor.dimensions != self.config.dimensions {
            return Err(OkError::Config(format!(
                "semantic model {} requires {} dimensions, but ok.toml configures {}",
                descriptor.model, descriptor.dimensions, self.config.dimensions
            )));
        }
        let model_artifact_sha256 = model_artifact_digest(&self.config, &self.models_dir())?;
        let targets = collect_targets(self.store, &self.config)?;
        let mut resolved_backend = resolve_semantic_backend(&self.config, targets.len())?;
        let current_cache =
            read_json::<EmbeddingCache>(&self.current_dir().join("embeddings.cache"))
                .unwrap_or_default();
        let build_dir = self
            .builds_dir()
            .join(format!("build-{}", Utc::now().timestamp_millis()));
        fs::create_dir_all(&build_dir)?;

        let mut cache = EmbeddingCache::default();
        let mut exact_index = if resolved_backend == ResolvedSemanticBackend::ExactFlat {
            Some(ExactFlatVectorIndex::new(self.config.dimensions)?)
        } else {
            None
        };
        let mut ann_index = match resolved_backend {
            ResolvedSemanticBackend::ExactFlat => None,
            ResolvedSemanticBackend::HnswF32 => Some(UsearchHnswVectorIndex::with_parameters(
                self.config.dimensions,
                AnnScalarKind::F32,
                targets.len(),
                PRODUCTION_HNSW_PARAMETERS,
            )?),
            ResolvedSemanticBackend::HnswBf16 => Some(UsearchHnswVectorIndex::with_parameters(
                self.config.dimensions,
                AnnScalarKind::Bf16,
                targets.len(),
                PRODUCTION_HNSW_PARAMETERS,
            )?),
        };
        let mut cache_hits = 0usize;
        let mut cache_misses = 0usize;
        let mut failed = 0usize;
        let mut counts = BTreeMap::<String, usize>::new();
        let mut vectors = vec![None::<Vec<f32>>; targets.len()];
        let mut missing_indexes = Vec::new();

        for (index, target) in targets.iter().enumerate() {
            *counts.entry(target.kind.clone()).or_default() += 1;
            let key = cache_key(target, &self.config);
            if let Some(entry) = current_cache.entries.get(&key) {
                if entry.content_hash == target.content_hash
                    && entry.model == descriptor.model
                    && entry.embedding_implementation == descriptor.implementation
                    && entry.model_artifact_sha256 == model_artifact_sha256
                    && entry.dimensions == self.config.dimensions
                    && entry.vector.len() == self.config.dimensions
                {
                    cache_hits += 1;
                    vectors[index] = Some(entry.vector.clone());
                    continue;
                }
            }
            cache_misses += 1;
            missing_indexes.push(index);
        }

        if !missing_indexes.is_empty() {
            let missing_texts = missing_indexes
                .iter()
                .map(|index| targets[*index].text.clone())
                .collect::<Vec<_>>();
            let embedded = provider.embed_document_batch(&missing_texts, self.config.batch_size)?;
            if embedded.len() != missing_indexes.len() {
                return Err(OkError::Storage(format!(
                    "embedding provider returned {} vectors for {} inputs",
                    embedded.len(),
                    missing_indexes.len()
                )));
            }
            for (index, vector) in missing_indexes.into_iter().zip(embedded) {
                vectors[index] = Some(vector);
            }
        }

        for (target, vector) in targets.iter().zip(vectors) {
            let Some(vector) = vector else {
                failed += 1;
                continue;
            };
            if vector.len() != self.config.dimensions {
                failed += 1;
                continue;
            }
            let record = VectorRecord {
                id: target.vector_id,
                target_id: target.stable_id.clone(),
                target_kind: target.kind.clone(),
                vector: vector.clone(),
            };
            if let Some(index) = exact_index.as_mut() {
                index.add(record)?;
            } else if let Some(index) = ann_index.as_mut() {
                index.add(record)?;
            } else {
                return Err(OkError::Storage(
                    "semantic vector backend was not initialized".into(),
                ));
            }
            cache.entries.insert(
                cache_key(target, &self.config),
                EmbeddingCacheEntry {
                    target_id: target.stable_id.clone(),
                    content_hash: target.content_hash.clone(),
                    model: descriptor.model.clone(),
                    embedding_implementation: descriptor.implementation.clone(),
                    model_artifact_sha256: model_artifact_sha256.clone(),
                    dimensions: self.config.dimensions,
                    vector,
                },
            );
        }

        let successful_vector_count = exact_index
            .as_ref()
            .map(|index| index.stats().vector_count)
            .or_else(|| ann_index.as_ref().map(|index| index.stats().vector_count))
            .unwrap_or(0);
        if auto_backend_needs_exact_fallback(
            &self.config,
            resolved_backend,
            successful_vector_count,
        ) {
            let mut fallback = ExactFlatVectorIndex::new(self.config.dimensions)?;
            for target in &targets {
                let Some(entry) = cache.entries.get(&cache_key(target, &self.config)) else {
                    continue;
                };
                fallback.add(VectorRecord {
                    id: target.vector_id,
                    target_id: target.stable_id.clone(),
                    target_kind: target.kind.clone(),
                    vector: entry.vector.clone(),
                })?;
            }
            exact_index = Some(fallback);
            ann_index = None;
            resolved_backend = ResolvedSemanticBackend::ExactFlat;
        }

        let manifest = SemanticManifest {
            schema_version: SCHEMA_VERSION,
            backend: resolved_backend_name(resolved_backend).into(),
            embedding_provider: self.config.provider.clone(),
            embedding_model: descriptor.model.clone(),
            embedding_implementation: descriptor.implementation.clone(),
            model_artifact_sha256: model_artifact_sha256.clone(),
            dimensions: self.config.dimensions,
            distance_metric: self.config.distance.clone(),
            chunker_version: CHUNKER_VERSION.into(),
            index_version: index_version_for_backend(resolved_backend).into(),
            source_commit: self
                .store
                .manifest()
                .ok()
                .flatten()
                .and_then(|manifest| manifest.repository.commit),
            source_index_fingerprint: Some(source_index_fingerprint(self.store)?),
            created_at: Utc::now().to_rfc3339(),
            vector_count: exact_index
                .as_ref()
                .map(|index| index.stats().vector_count)
                .or_else(|| ann_index.as_ref().map(|index| index.stats().vector_count))
                .unwrap_or(0),
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
        if let Some(index) = exact_index.as_ref() {
            index.save(&build_dir.join("index.json"))?;
        } else if let Some(index) = ann_index.as_ref() {
            index.save(&build_dir.join("index.usearch"))?;
        }
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
            && self.source_generation_compatible(manifest)
            && resolve_semantic_backend(&self.config, manifest.vector_count)
                .is_ok_and(|backend| manifest.backend == resolved_backend_name(backend))
            && manifest.embedding_provider == self.config.provider
            && manifest.embedding_model == canonical_model_name(&self.config)
            && manifest.embedding_implementation == expected_embedding_implementation(&self.config)
            && manifest.model_artifact_sha256
                == model_artifact_digest(&self.config, &self.models_dir())
                    .ok()
                    .flatten()
            && manifest.dimensions == self.config.dimensions
            && manifest.distance_metric == self.config.distance
            && manifest.chunker_version == CHUNKER_VERSION
            && resolved_backend_from_name(&manifest.backend)
                .is_ok_and(|backend| manifest.index_version == index_version_for_backend(backend))
    }

    fn source_generation_compatible(&self, manifest: &SemanticManifest) -> bool {
        source_index_fingerprint(self.store).is_ok_and(|fingerprint| {
            manifest.source_index_fingerprint.as_deref() == Some(fingerprint.as_str())
        })
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

    fn models_dir(&self) -> PathBuf {
        self.repo.join(".ok/models/fastembed")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedSemanticBackend {
    ExactFlat,
    HnswF32,
    HnswBf16,
}

fn resolve_semantic_backend(
    config: &SemanticConfig,
    vector_count: usize,
) -> Result<ResolvedSemanticBackend> {
    match config.backend.as_str() {
        "exact-flat" => Ok(ResolvedSemanticBackend::ExactFlat),
        "auto" => {
            if vector_count >= config.ann_min_rows {
                Ok(ResolvedSemanticBackend::HnswF32)
            } else {
                Ok(ResolvedSemanticBackend::ExactFlat)
            }
        }
        "usearch-hnsw-f32" => Ok(ResolvedSemanticBackend::HnswF32),
        "usearch-hnsw-bf16" => Ok(ResolvedSemanticBackend::HnswBf16),
        other => Err(OkError::Unsupported(format!(
            "semantic backend `{other}` is not supported; use exact-flat, auto, usearch-hnsw-f32, or usearch-hnsw-bf16"
        ))),
    }
}

fn auto_backend_needs_exact_fallback(
    config: &SemanticConfig,
    resolved_backend: ResolvedSemanticBackend,
    successful_vector_count: usize,
) -> bool {
    config.backend == "auto"
        && resolved_backend != ResolvedSemanticBackend::ExactFlat
        && successful_vector_count < config.ann_min_rows
}

fn resolved_backend_from_name(name: &str) -> Result<ResolvedSemanticBackend> {
    match name {
        "exact-flat" => Ok(ResolvedSemanticBackend::ExactFlat),
        "usearch-hnsw-f32" => Ok(ResolvedSemanticBackend::HnswF32),
        "usearch-hnsw-bf16" => Ok(ResolvedSemanticBackend::HnswBf16),
        other => Err(OkError::Storage(format!(
            "semantic manifest contains unsupported vector backend `{other}`"
        ))),
    }
}

fn resolved_backend_name(backend: ResolvedSemanticBackend) -> &'static str {
    match backend {
        ResolvedSemanticBackend::ExactFlat => "exact-flat",
        ResolvedSemanticBackend::HnswF32 => "usearch-hnsw-f32",
        ResolvedSemanticBackend::HnswBf16 => "usearch-hnsw-bf16",
    }
}

fn backend_is_ann(backend: &str) -> bool {
    matches!(backend, "usearch-hnsw-f32" | "usearch-hnsw-bf16")
}

fn index_version_for_backend(backend: ResolvedSemanticBackend) -> &'static str {
    match backend {
        ResolvedSemanticBackend::ExactFlat => EXACT_INDEX_VERSION,
        ResolvedSemanticBackend::HnswF32 | ResolvedSemanticBackend::HnswBf16 => HNSW_INDEX_VERSION,
    }
}

fn index_artifacts_present(current: &Path, backend: &str) -> bool {
    match resolved_backend_from_name(backend) {
        Ok(ResolvedSemanticBackend::ExactFlat) => current.join("index.json").is_file(),
        Ok(ResolvedSemanticBackend::HnswF32 | ResolvedSemanticBackend::HnswBf16) => {
            current.join("index.usearch").is_file() && current.join("index.meta.json").is_file()
        }
        Err(_) => false,
    }
}

fn normalize_path_prefixes(path_prefixes: &[String]) -> Result<Vec<String>> {
    let mut normalized = Vec::new();
    for prefix in path_prefixes {
        let value = prefix
            .replace('\\', "/")
            .trim_start_matches("./")
            .trim_matches('/')
            .to_string();
        if value.is_empty()
            || value
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
        {
            return Err(OkError::Unsupported(format!(
                "semantic path scope `{prefix}` is not a safe repository-relative prefix"
            )));
        }
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    normalized.sort();
    Ok(normalized)
}

fn target_matches_path_scope(target: &SemanticTarget, prefixes: &[String]) -> bool {
    let path = target.path.to_string_lossy().replace('\\', "/");
    prefixes
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

fn intersect_allowlists(
    left: Option<HashSet<VectorId>>,
    right: Option<HashSet<VectorId>>,
) -> Option<HashSet<VectorId>> {
    match (left, right) {
        (None, None) => None,
        (Some(ids), None) | (None, Some(ids)) => Some(ids),
        (Some(left), Some(right)) => Some(left.intersection(&right).copied().collect()),
    }
}

fn should_route_scoped_exact(
    config: &SemanticConfig,
    persisted_backend: &str,
    eligible_candidate_count: usize,
) -> bool {
    config.backend == "auto"
        && backend_is_ann(persisted_backend)
        && eligible_candidate_count < config.ann_min_rows
}

fn semantic_filter_selectivity(total: usize, eligible: usize, filtered: bool) -> String {
    if !filtered {
        return "unfiltered".into();
    }
    if total == 0 || eligible.saturating_mul(10) <= total {
        "highly-selective".into()
    } else if eligible.saturating_mul(2) <= total {
        "medium-selectivity".into()
    } else {
        "broad".into()
    }
}

fn build_scoped_exact_index(
    current: &Path,
    targets: &HashMap<String, SemanticTarget>,
    allowlist: &HashSet<VectorId>,
    config: &SemanticConfig,
) -> Result<Option<ExactFlatVectorIndex>> {
    let Some(cache) = read_json::<EmbeddingCache>(&current.join("embeddings.cache")) else {
        return Ok(None);
    };
    let mut exact = ExactFlatVectorIndex::new(config.dimensions)?;
    for target in targets
        .values()
        .filter(|target| allowlist.contains(&target.vector_id))
    {
        let Some(entry) = cache.entries.get(&cache_key(target, config)) else {
            return Ok(None);
        };
        if entry.target_id != target.stable_id
            || entry.dimensions != config.dimensions
            || entry.vector.len() != config.dimensions
        {
            return Ok(None);
        }
        exact.add(VectorRecord {
            id: target.vector_id,
            target_id: target.stable_id.clone(),
            target_kind: target.kind.clone(),
            vector: entry.vector.clone(),
        })?;
    }
    Ok(Some(exact))
}

fn search_persisted_backend(
    current: &Path,
    backend: &str,
    query_vector: &[f32],
    options: VectorSearchOptions,
) -> Result<Vec<open_kioku_vector::VectorHit>> {
    match backend {
        "exact-flat" => {
            ExactFlatVectorIndex::load(&current.join("index.json"))?.search(query_vector, options)
        }
        "usearch-hnsw-f32" | "usearch-hnsw-bf16" => {
            let index = UsearchHnswVectorIndex::load(&current.join("index.usearch"))?;
            if index.parameters() != PRODUCTION_HNSW_PARAMETERS {
                return Err(OkError::Storage(format!(
                    "semantic HNSW artifact uses {:?}, expected measured profile {} ({:?}); rebuild the semantic index",
                    index.parameters(),
                    PRODUCTION_HNSW_PROFILE,
                    PRODUCTION_HNSW_PARAMETERS
                )));
            }
            index.search(query_vector, options)
        }
        other => Err(OkError::Storage(format!(
            "semantic manifest contains unsupported vector backend `{other}`"
        ))),
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
    Ok(Some(provider_for_config(
        config,
        false,
        Path::new(".ok/models/fastembed"),
    )?))
}

pub fn ensure_enabled(config: &SemanticConfig) -> Result<()> {
    provider_from_config(config).and_then(|provider| {
        provider
            .map(|_| ())
            .ok_or_else(|| OkError::Unsupported("semantic search is disabled in ok.toml".into()))
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelReadyFile {
    path: PathBuf,
    size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ModelReadyMarker {
    model: String,
    implementation: String,
    artifact_sha256: String,
    files: Vec<ModelReadyFile>,
}

fn provider_for_config(
    config: &SemanticConfig,
    allow_model_download: bool,
    models_root: &Path,
) -> Result<Box<dyn EmbeddingProvider>> {
    match config.provider.as_str() {
        "local" | "local-hash" | "hash" => Ok(Box::new(LocalHashEmbeddingProvider::new(
            config.dimensions,
        )?)),
        "fastembed" | "local-neural" => {
            let model = LocalNeuralModel::parse(&config.model)?;
            let cache_dir = neural_model_cache_dir(models_root, model);
            if !allow_model_download {
                validate_model_ready_marker(config, &cache_dir)?;
            }
            fs::create_dir_all(&cache_dir)?;
            let provider = FastEmbedEmbeddingProvider::new(
                model,
                config.dimensions,
                config.batch_size,
                &cache_dir,
            )?;
            if allow_model_download {
                write_model_ready_marker(&cache_dir, &provider.descriptor())?;
            }
            Ok(Box::new(provider))
        }
        "disabled" => Err(OkError::Unsupported(
            "semantic embedding provider is disabled".into(),
        )),
        "external" if !config.external_provider_allowed => Err(OkError::Unsupported(
            "external semantic providers require explicit opt-in".into(),
        )),
        other => Err(OkError::Unsupported(format!(
            "semantic provider `{other}` is not available; supported local providers: local, fastembed"
        ))),
    }
}

fn canonical_model_name(config: &SemanticConfig) -> String {
    if matches!(config.provider.as_str(), "fastembed" | "local-neural") {
        LocalNeuralModel::parse(&config.model)
            .map(|model| model.canonical_name().to_string())
            .unwrap_or_else(|_| config.model.clone())
    } else {
        config.model.clone()
    }
}

fn expected_embedding_implementation(config: &SemanticConfig) -> String {
    if matches!(config.provider.as_str(), "fastembed" | "local-neural") {
        let suffix = match LocalNeuralModel::parse(&config.model) {
            Ok(LocalNeuralModel::JinaEmbeddingsV2BaseCode) => "onnx".to_string(),
            Ok(_) => format!("qwen3-candle:maxlen-{QWEN3_MAX_LENGTH}"),
            Err(_) => "unknown".to_string(),
        };
        format!(
            "{}:{suffix}",
            open_kioku_embeddings::FASTEMBED_PROVIDER_VERSION
        )
    } else {
        "open-kioku-local-hash-v1".into()
    }
}

fn model_ready_marker_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(".open-kioku-model-ready.json")
}

fn validate_model_ready_marker(config: &SemanticConfig, cache_dir: &Path) -> Result<()> {
    let marker_path = model_ready_marker_path(cache_dir);
    let marker = read_json::<ModelReadyMarker>(&marker_path).ok_or_else(|| {
        OkError::Unsupported(format!(
            "local neural model {} is not installed; run `ok semantic index --allow-model-download` to explicitly download it",
            canonical_model_name(config)
        ))
    })?;
    if marker.model != canonical_model_name(config)
        || marker.implementation != expected_embedding_implementation(config)
    {
        return Err(OkError::Unsupported(
            "local neural model cache provenance does not match the configured model; rerun `ok semantic index --allow-model-download`".into(),
        ));
    }
    for file in &marker.files {
        let path = cache_dir.join(&file.path);
        let size = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if size != file.size || !path.is_file() {
            return Err(OkError::Unsupported(format!(
                "local neural model cache is incomplete at {}; rerun `ok semantic index --allow-model-download`",
                file.path.display()
            )));
        }
    }
    Ok(())
}

fn write_model_ready_marker(
    cache_dir: &Path,
    descriptor: &open_kioku_embeddings::EmbeddingProviderDescriptor,
) -> Result<()> {
    let (artifact_sha256, files) = fingerprint_model_cache(cache_dir)?;
    let marker = ModelReadyMarker {
        model: descriptor.model.clone(),
        implementation: descriptor.implementation.clone(),
        artifact_sha256,
        files,
    };
    write_json(&model_ready_marker_path(cache_dir), &marker)
}

fn model_artifact_digest(config: &SemanticConfig, models_root: &Path) -> Result<Option<String>> {
    if !matches!(config.provider.as_str(), "fastembed" | "local-neural") {
        return Ok(None);
    }
    let model = LocalNeuralModel::parse(&config.model)?;
    let cache_dir = neural_model_cache_dir(models_root, model);
    let marker = read_json::<ModelReadyMarker>(&model_ready_marker_path(&cache_dir));
    Ok(marker
        .filter(|marker| {
            marker.model == canonical_model_name(config)
                && marker.implementation == expected_embedding_implementation(config)
        })
        .map(|marker| marker.artifact_sha256))
}

fn fingerprint_model_cache(cache_dir: &Path) -> Result<(String, Vec<ModelReadyFile>)> {
    let mut paths = Vec::new();
    collect_model_files(cache_dir, cache_dir, &mut paths)?;
    paths.sort();
    let mut files = Vec::new();
    let mut hasher = Sha256::new();
    hasher.update(b"open-kioku-local-neural-model-v2\0");
    for relative in paths {
        if ignored_model_cache_file(&relative) {
            continue;
        }
        let absolute = cache_dir.join(&relative);
        let metadata = fs::metadata(&absolute)?;
        hasher.update(relative.to_string_lossy().replace('\\', "/").as_bytes());
        hasher.update(b"\0");
        hasher.update(fs::read(&absolute)?);
        hasher.update(b"\0");
        files.push(ModelReadyFile {
            path: relative,
            size: metadata.len(),
        });
    }
    if files.is_empty() {
        return Err(OkError::Unsupported(
            "local neural model initialization completed without cache artifacts".into(),
        ));
    }
    Ok((format!("sha256:{:x}", hasher.finalize()), files))
}

fn ignored_model_cache_file(path: &Path) -> bool {
    let normalized = path.to_string_lossy().replace('\\', "/");
    normalized == ".open-kioku-model-ready.json"
        || normalized.contains("/.locks/")
        || normalized.starts_with(".locks/")
        || normalized.ends_with(".lock")
        || normalized.ends_with(".tmp")
        || normalized.ends_with(".part")
        || normalized.ends_with(".incomplete")
}

fn collect_model_files(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_model_files(root, &path, files)?;
        } else if path.is_file() {
            files.push(path.strip_prefix(root).unwrap_or(&path).to_path_buf());
        }
    }
    Ok(())
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
            "semantic vector similarity from local semantic index".into(),
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
                "cosine similarity from local semantic vector index",
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

fn source_index_fingerprint(store: &dyn MetadataStore) -> Result<String> {
    let manifest = store.manifest()?.ok_or_else(|| {
        OkError::Storage(
            "authoritative index manifest is missing; run `ok index .` before semantic indexing"
                .into(),
        )
    })?;
    let raw = serde_json::to_vec(&manifest)?;
    let mut hasher = Sha256::new();
    hasher.update(b"open-kioku-semantic-source-index-v1\0");
    hasher.update(raw);
    Ok(format!("{:x}", hasher.finalize()))
}

fn cache_key(target: &SemanticTarget, config: &SemanticConfig) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        target.stable_id,
        target.content_hash,
        config.model,
        config.dimensions,
        expected_embedding_implementation(config)
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
    fn neural_provider_requires_explicit_download_before_model_initialization() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(temp.path().join(".ok/index.sqlite")).unwrap();
        let mut config = semantic_config();
        config.provider = "fastembed".into();
        config.model = "Qwen/Qwen3-Embedding-0.6B".into();
        config.dimensions = 1_024;
        let manager = SemanticIndexManager::new(temp.path(), &store, &config);
        let err = manager.index().unwrap_err();
        let message = err.to_string();
        assert!(message.contains("not installed"));
        assert!(message.contains("--allow-model-download"));
        assert!(!temp.path().join(".ok/models/fastembed").exists());
    }

    #[test]
    fn auto_backend_respects_vector_count_gate() {
        let mut config = SemanticConfig {
            enabled: true,
            backend: "auto".into(),
            provider: "local".into(),
            model: "local-hash".into(),
            dimensions: 8,
            distance: "cosine".into(),
            batch_size: 4,
            ann_min_rows: 100,
            index_symbols: true,
            index_chunks: true,
            index_docs: true,
            index_memory: true,
            external_provider_allowed: false,
        };
        assert_eq!(
            resolve_semantic_backend(&config, 99).unwrap(),
            ResolvedSemanticBackend::ExactFlat
        );
        assert_eq!(
            resolve_semantic_backend(&config, 100).unwrap(),
            ResolvedSemanticBackend::HnswF32
        );
        assert!(auto_backend_needs_exact_fallback(
            &config,
            ResolvedSemanticBackend::HnswF32,
            99
        ));
        assert!(!auto_backend_needs_exact_fallback(
            &config,
            ResolvedSemanticBackend::HnswF32,
            100
        ));
        config.backend = "usearch-hnsw-bf16".into();
        assert_eq!(
            resolve_semantic_backend(&config, 1).unwrap(),
            ResolvedSemanticBackend::HnswBf16
        );
    }

    #[test]
    fn builds_persisted_semantic_index_and_reuses_cache() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(temp.path().join(".ok/index.sqlite")).unwrap();
        let manifest = open_kioku_core::IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
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
            index_mode: Default::default(),
            phase_reports: Vec::new(),
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
                scopes: &[],
                bindings: &[],
                call_sites: &[],
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

    #[test]
    fn auto_ann_routes_highly_selective_path_scope_to_exact_flat() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(temp.path().join(".ok/index.sqlite")).unwrap();
        let manifest = open_kioku_core::IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository: open_kioku_core::Repository {
                id: RepositoryId("repo".into()),
                name: "repo".into(),
                root: temp.path().to_path_buf(),
                branch: Some("main".into()),
                commit: Some("abc".into()),
                indexed_at: Some(Utc::now()),
            },
            file_count: 3,
            symbol_count: 3,
            chunk_count: 3,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: Default::default(),
        };
        let files = vec![
            file("file_auth", "src/auth.rs"),
            file("file_billing", "src/billing.rs"),
            file("file_profile", "src/profile.rs"),
        ];
        let symbols = vec![
            symbol("symbol_auth", "issue_token", "file_auth"),
            symbol("symbol_billing", "issue_invoice", "file_billing"),
            symbol("symbol_profile", "load_profile", "file_profile"),
        ];
        let chunks = vec![
            chunk(
                "chunk_auth",
                "file_auth",
                "pub fn issue_token() { create session token }",
                Some("symbol_auth"),
            ),
            chunk(
                "chunk_billing",
                "file_billing",
                "pub fn issue_invoice() { billing invoice }",
                Some("symbol_billing"),
            ),
            chunk(
                "chunk_profile",
                "file_profile",
                "pub fn load_profile() { user profile }",
                Some("symbol_profile"),
            ),
        ];
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
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let mut config = semantic_config();
        config.backend = "auto".into();
        config.ann_min_rows = 5;
        let manager = SemanticIndexManager::new(temp.path(), &store, &config);
        let indexed = manager.index().unwrap();
        assert_eq!(indexed.status.backend, "usearch-hnsw-f32");

        let report = manager
            .search_with_path_prefixes("issue token", 5, &["src/auth.rs".to_string()])
            .unwrap();
        assert_eq!(report.routing.selected_backend, "exact-flat");
        assert!(report.routing.path_scope_enforced);
        assert_eq!(report.routing.total_vector_count, 6);
        assert_eq!(report.routing.eligible_candidate_count, 2);
        assert!(report
            .results
            .iter()
            .all(|result| result.path.as_path() == std::path::Path::new("src/auth.rs")));
    }

    #[test]
    fn auto_ann_routes_selective_precomputed_allowlist_to_exact_flat() {
        let temp = tempfile::tempdir().unwrap();
        let store = SqliteStore::open(temp.path().join(".ok/index.sqlite")).unwrap();
        let manifest = open_kioku_core::IndexManifest {
            analysis_semantics: Some(open_kioku_core::AnalysisSemanticsState::current()),
            repository: open_kioku_core::Repository {
                id: RepositoryId("repo".into()),
                name: "repo".into(),
                root: temp.path().to_path_buf(),
                branch: Some("main".into()),
                commit: Some("abc".into()),
                indexed_at: Some(Utc::now()),
            },
            file_count: 3,
            symbol_count: 3,
            chunk_count: 3,
            indexed_at: Utc::now(),
            schema_version: 1,
            index_mode: Default::default(),
            phase_reports: Vec::new(),
            quality: Default::default(),
        };
        let files = vec![
            file("file_auth", "src/auth.rs"),
            file("file_billing", "src/billing.rs"),
            file("file_profile", "src/profile.rs"),
        ];
        let symbols = vec![
            symbol("symbol_auth", "issue_token", "file_auth"),
            symbol("symbol_billing", "issue_invoice", "file_billing"),
            symbol("symbol_profile", "load_profile", "file_profile"),
        ];
        let chunks = vec![
            chunk(
                "chunk_auth",
                "file_auth",
                "pub fn issue_token() { create session token }",
                Some("symbol_auth"),
            ),
            chunk(
                "chunk_billing",
                "file_billing",
                "pub fn issue_invoice() { billing invoice }",
                Some("symbol_billing"),
            ),
            chunk(
                "chunk_profile",
                "file_profile",
                "pub fn load_profile() { user profile }",
                Some("symbol_profile"),
            ),
        ];
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
                scopes: &[],
                bindings: &[],
                call_sites: &[],
            })
            .unwrap();

        let mut config = semantic_config();
        config.backend = "auto".into();
        config.ann_min_rows = 5;
        let manager = SemanticIndexManager::new(temp.path(), &store, &config);
        let indexed = manager.index().unwrap();
        assert_eq!(indexed.status.backend, "usearch-hnsw-f32");

        let targets = read_targets(&manager.current_dir().join("ids.json")).unwrap();
        let auth_ids = targets
            .values()
            .filter(|target| target.path == std::path::Path::new("src/auth.rs"))
            .map(|target| target.vector_id)
            .collect::<HashSet<_>>();
        assert_eq!(auth_ids.len(), 2);

        let report = manager
            .search_with_allowlist_report("issue token", 5, Some(auth_ids))
            .unwrap();
        assert_eq!(report.routing.selected_backend, "exact-flat");
        assert!(report.routing.filter_scope_enforced);
        assert!(!report.routing.path_scope_enforced);
        assert_eq!(report.routing.total_vector_count, 6);
        assert_eq!(report.routing.eligible_candidate_count, 2);
        assert_eq!(report.routing.filter_selectivity, "medium-selectivity");
        assert!(report
            .results
            .iter()
            .all(|result| result.path.as_path() == std::path::Path::new("src/auth.rs")));
    }

    #[test]
    fn explicit_ann_is_not_overridden_by_selective_scope() {
        let mut config = semantic_config();
        config.backend = "usearch-hnsw-f32".into();
        config.ann_min_rows = 10_000;
        assert!(!should_route_scoped_exact(&config, "usearch-hnsw-f32", 1));
    }

    #[test]
    fn unsafe_semantic_path_scope_fails_closed() {
        let err = normalize_path_prefixes(&["../outside".to_string()]).unwrap_err();
        assert!(err.to_string().contains("safe repository-relative"));
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
            ann_min_rows: 10_000,
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
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Unknown,
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
