use open_kioku_errors::{OkError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use usearch::{new_index, Index, IndexOptions, MetricKind, ScalarKind};

pub const PRODUCTION_HNSW_PROFILE: &str = "usearch-2.21.1-hnsw-meta3-c32-a256-s1024";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VectorId(pub u64);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorRecord {
    pub id: VectorId,
    pub target_id: String,
    pub target_kind: String,
    pub vector: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorHit {
    pub id: VectorId,
    pub target_id: String,
    pub target_kind: String,
    pub score: f32,
}

#[derive(Debug, Clone, Default)]
pub struct VectorSearchOptions {
    pub limit: usize,
    pub allowlist: Option<HashSet<VectorId>>,
    pub target_kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorIndexStats {
    pub backend: String,
    pub dimensions: usize,
    pub vector_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExactFlatVectorIndex {
    dimensions: usize,
    records: BTreeMap<VectorId, VectorRecord>,
}

impl ExactFlatVectorIndex {
    pub fn new(dimensions: usize) -> Result<Self> {
        if dimensions == 0 {
            return Err(OkError::Unsupported(
                "exact-flat vector index requires dimensions > 0".into(),
            ));
        }
        Ok(Self {
            dimensions,
            records: BTreeMap::new(),
        })
    }

    pub fn add(&mut self, record: VectorRecord) -> Result<()> {
        validate_vector_dimensions(&record, self.dimensions)?;
        if self
            .records
            .get(&record.id)
            .is_some_and(|value| value.target_id != record.target_id)
        {
            return Err(vector_id_collision(record.id));
        }
        self.records.insert(record.id, record);
        Ok(())
    }

    pub fn remove(&mut self, id: VectorId) -> Option<VectorRecord> {
        self.records.remove(&id)
    }

    pub fn search(&self, query: &[f32], options: VectorSearchOptions) -> Result<Vec<VectorHit>> {
        validate_query(query, self.dimensions)?;
        let mut hits = self
            .records
            .values()
            .filter(|record| matches_record_filters(record.id, &record.target_kind, &options))
            .filter_map(|record| {
                let score = dot(query, &record.vector);
                (score > 0.0).then(|| VectorHit {
                    id: record.id,
                    target_id: record.target_id.clone(),
                    target_kind: record.target_kind.clone(),
                    score,
                })
            })
            .collect::<Vec<_>>();
        sort_and_truncate(&mut hits, options.limit);
        Ok(hits)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read(path)?;
        Ok(serde_json::from_slice(&raw)?)
    }

    pub fn stats(&self) -> VectorIndexStats {
        VectorIndexStats {
            backend: "exact-flat".into(),
            dimensions: self.dimensions,
            vector_count: self.records.len(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnScalarKind {
    F32,
    Bf16,
}

impl AnnScalarKind {
    fn usearch(self) -> ScalarKind {
        match self {
            Self::F32 => ScalarKind::F32,
            Self::Bf16 => ScalarKind::BF16,
        }
    }
}

/// HNSW knobs that affect the persisted graph or query behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HnswParameters {
    pub connectivity: usize,
    pub expansion_add: usize,
    pub expansion_search: usize,
}

pub const PRODUCTION_HNSW_PARAMETERS: HnswParameters = HnswParameters {
    connectivity: 32,
    expansion_add: 256,
    expansion_search: 1_024,
};

impl Default for HnswParameters {
    fn default() -> Self {
        PRODUCTION_HNSW_PARAMETERS
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnnTargetMetadata {
    target_id: String,
    target_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsearchMetadata {
    schema_version: u32,
    dimensions: usize,
    scalar_kind: AnnScalarKind,
    parameters: HnswParameters,
    records: BTreeMap<VectorId, AnnTargetMetadata>,
}

pub struct UsearchHnswVectorIndex {
    dimensions: usize,
    scalar_kind: AnnScalarKind,
    parameters: HnswParameters,
    records: BTreeMap<VectorId, AnnTargetMetadata>,
    index: Index,
}

impl UsearchHnswVectorIndex {
    const METADATA_SCHEMA_VERSION: u32 = 3;

    pub fn new(dimensions: usize, scalar_kind: AnnScalarKind, capacity: usize) -> Result<Self> {
        Self::with_parameters(dimensions, scalar_kind, capacity, HnswParameters::default())
    }

    /// Calibration constructor for varying only query expansion while keeping
    /// the measured production construction profile.
    pub fn with_search_expansion(
        dimensions: usize,
        scalar_kind: AnnScalarKind,
        capacity: usize,
        search_expansion: usize,
    ) -> Result<Self> {
        Self::with_parameters(
            dimensions,
            scalar_kind,
            capacity,
            HnswParameters {
                expansion_search: search_expansion,
                ..HnswParameters::default()
            },
        )
    }

    pub fn with_parameters(
        dimensions: usize,
        scalar_kind: AnnScalarKind,
        capacity: usize,
        parameters: HnswParameters,
    ) -> Result<Self> {
        if dimensions == 0 {
            return Err(OkError::Unsupported(
                "HNSW vector index requires dimensions > 0".into(),
            ));
        }
        if parameters.connectivity == 0
            || parameters.expansion_add == 0
            || parameters.expansion_search == 0
        {
            return Err(OkError::Unsupported(
                "HNSW connectivity and expansion parameters must be greater than zero".into(),
            ));
        }
        let index = build_usearch_index(dimensions, scalar_kind, parameters)?;
        index.reserve(capacity.max(1)).map_err(usearch_error)?;
        Ok(Self {
            dimensions,
            scalar_kind,
            parameters,
            records: BTreeMap::new(),
            index,
        })
    }

    pub fn add(&mut self, record: VectorRecord) -> Result<()> {
        validate_vector_dimensions(&record, self.dimensions)?;
        if self
            .records
            .get(&record.id)
            .is_some_and(|value| value.target_id != record.target_id)
        {
            return Err(vector_id_collision(record.id));
        }
        if self.records.len() >= self.index.capacity() {
            let next = self
                .index
                .capacity()
                .max(1)
                .saturating_mul(2)
                .max(self.records.len().saturating_add(1));
            self.index.reserve(next).map_err(usearch_error)?;
        }
        self.index
            .add(record.id.0, &record.vector)
            .map_err(usearch_error)?;
        self.records.insert(
            record.id,
            AnnTargetMetadata {
                target_id: record.target_id,
                target_kind: record.target_kind,
            },
        );
        Ok(())
    }

    pub fn search(&self, query: &[f32], options: VectorSearchOptions) -> Result<Vec<VectorHit>> {
        validate_query(query, self.dimensions)?;
        let limit = options.limit.max(1);
        let matches = if options.allowlist.is_some() || options.target_kind.is_some() {
            self.index
                .filtered_search(query, limit, |key| {
                    self.records.get(&VectorId(key)).is_some_and(|record| {
                        matches_record_filters(VectorId(key), &record.target_kind, &options)
                    })
                })
                .map_err(usearch_error)?
        } else {
            self.index.search(query, limit).map_err(usearch_error)?
        };
        let mut hits = matches
            .keys
            .into_iter()
            .zip(matches.distances)
            .filter_map(|(key, distance)| {
                let id = VectorId(key);
                let record = self.records.get(&id)?;
                let score = 1.0 - distance;
                (score > 0.0).then(|| VectorHit {
                    id,
                    target_id: record.target_id.clone(),
                    target_kind: record.target_kind.clone(),
                    score,
                })
            })
            .collect::<Vec<_>>();
        sort_and_truncate(&mut hits, options.limit);
        Ok(hits)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.index.save(path_to_str(path)?).map_err(usearch_error)?;
        let metadata = UsearchMetadata {
            schema_version: Self::METADATA_SCHEMA_VERSION,
            dimensions: self.dimensions,
            scalar_kind: self.scalar_kind,
            parameters: self.parameters,
            records: self.records.clone(),
        };
        fs::write(
            usearch_metadata_path(path),
            serde_json::to_vec_pretty(&metadata)?,
        )?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let metadata: UsearchMetadata =
            serde_json::from_slice(&fs::read(usearch_metadata_path(path))?)?;
        if metadata.schema_version != Self::METADATA_SCHEMA_VERSION {
            return Err(OkError::Storage(format!(
                "unsupported HNSW metadata schema {}; expected {}",
                metadata.schema_version,
                Self::METADATA_SCHEMA_VERSION
            )));
        }
        let index = build_usearch_index(
            metadata.dimensions,
            metadata.scalar_kind,
            metadata.parameters,
        )?;
        index.load(path_to_str(path)?).map_err(usearch_error)?;
        if index.size() != metadata.records.len() {
            return Err(OkError::Storage(format!(
                "HNSW index/metadata vector-count mismatch: index={}, metadata={}",
                index.size(),
                metadata.records.len()
            )));
        }
        Ok(Self {
            dimensions: metadata.dimensions,
            scalar_kind: metadata.scalar_kind,
            parameters: metadata.parameters,
            records: metadata.records,
            index,
        })
    }

    pub fn stats(&self) -> VectorIndexStats {
        VectorIndexStats {
            backend: match self.scalar_kind {
                AnnScalarKind::F32 => "usearch-hnsw-f32",
                AnnScalarKind::Bf16 => "usearch-hnsw-bf16",
            }
            .into(),
            dimensions: self.dimensions,
            vector_count: self.records.len(),
        }
    }

    pub fn memory_usage_bytes(&self) -> usize {
        self.index.memory_usage()
    }

    pub fn connectivity(&self) -> usize {
        self.index.connectivity()
    }

    pub fn parameters(&self) -> HnswParameters {
        self.parameters
    }
}

fn build_usearch_index(
    dimensions: usize,
    scalar_kind: AnnScalarKind,
    parameters: HnswParameters,
) -> Result<Index> {
    new_index(&IndexOptions {
        dimensions,
        metric: MetricKind::Cos,
        quantization: scalar_kind.usearch(),
        connectivity: parameters.connectivity,
        expansion_add: parameters.expansion_add,
        expansion_search: parameters.expansion_search,
        multi: false,
    })
    .map_err(usearch_error)
}

fn validate_vector_dimensions(record: &VectorRecord, dimensions: usize) -> Result<()> {
    if record.vector.len() != dimensions {
        return Err(OkError::Storage(format!(
            "vector {} has {} dimensions, expected {}",
            record.id.0,
            record.vector.len(),
            dimensions
        )));
    }
    Ok(())
}

fn vector_id_collision(id: VectorId) -> OkError {
    OkError::Storage(format!("vector id collision for {}", id.0))
}

fn validate_query(query: &[f32], dimensions: usize) -> Result<()> {
    if query.len() != dimensions {
        return Err(OkError::Storage(format!(
            "query vector has {} dimensions, expected {}",
            query.len(),
            dimensions
        )));
    }
    Ok(())
}

fn matches_record_filters(id: VectorId, target_kind: &str, options: &VectorSearchOptions) -> bool {
    !options
        .allowlist
        .as_ref()
        .is_some_and(|allowlist| !allowlist.contains(&id))
        && !options
            .target_kind
            .as_ref()
            .is_some_and(|kind| kind != target_kind)
}

fn sort_and_truncate(hits: &mut Vec<VectorHit>, limit: usize) {
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.target_id.cmp(&right.target_id))
    });
    hits.truncate(limit.max(1));
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

fn path_to_str(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| OkError::Storage(format!("non-UTF-8 vector index path: {}", path.display())))
}

fn usearch_metadata_path(path: &Path) -> PathBuf {
    path.with_extension("meta.json")
}

fn usearch_error(error: impl std::fmt::Display) -> OkError {
    OkError::Storage(format!("USearch HNSW error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u64, target: &str, kind: &str, vector: &[f32]) -> VectorRecord {
        VectorRecord {
            id: VectorId(id),
            target_id: target.into(),
            target_kind: kind.into(),
            vector: vector.to_vec(),
        }
    }

    #[test]
    fn exact_backend_searches_with_allowlist() {
        let mut index = ExactFlatVectorIndex::new(2).unwrap();
        index.add(record(1, "a", "chunk", &[1.0, 0.0])).unwrap();
        index.add(record(2, "b", "chunk", &[0.0, 1.0])).unwrap();
        let hits = index
            .search(
                &[1.0, 0.0],
                VectorSearchOptions {
                    limit: 5,
                    allowlist: Some(HashSet::from([VectorId(1)])),
                    target_kind: None,
                },
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target_id, "a");
    }

    #[test]
    fn hnsw_matches_exact_order_on_small_separated_fixture() {
        let records = [
            record(1, "exact", "chunk", &[1.0, 0.0, 0.0]),
            record(2, "near", "chunk", &[0.8, 0.6, 0.0]),
            record(3, "other", "symbol", &[0.0, 1.0, 0.0]),
            record(4, "far", "chunk", &[0.0, 0.0, 1.0]),
        ];
        let mut exact = ExactFlatVectorIndex::new(3).unwrap();
        let mut hnsw = UsearchHnswVectorIndex::new(3, AnnScalarKind::F32, records.len()).unwrap();
        for value in records {
            exact.add(value.clone()).unwrap();
            hnsw.add(value).unwrap();
        }
        let options = VectorSearchOptions {
            limit: 4,
            allowlist: None,
            target_kind: None,
        };
        let exact_hits = exact.search(&[1.0, 0.0, 0.0], options.clone()).unwrap();
        let hnsw_hits = hnsw.search(&[1.0, 0.0, 0.0], options).unwrap();
        assert_eq!(
            exact_hits
                .iter()
                .map(|hit| &hit.target_id)
                .collect::<Vec<_>>(),
            hnsw_hits
                .iter()
                .map(|hit| &hit.target_id)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn hnsw_preserves_filters_parameters_and_persistence_without_duplicate_vectors() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("index.usearch");
        let parameters = HnswParameters {
            connectivity: 16,
            expansion_add: 128,
            expansion_search: 256,
        };
        let mut index =
            UsearchHnswVectorIndex::with_parameters(2, AnnScalarKind::Bf16, 4, parameters).unwrap();
        index.add(record(1, "a", "chunk", &[1.0, 0.0])).unwrap();
        index.add(record(2, "b", "symbol", &[0.9, 0.1])).unwrap();
        index.add(record(3, "c", "chunk", &[0.0, 1.0])).unwrap();
        index.save(&path).unwrap();

        let metadata = fs::read_to_string(usearch_metadata_path(&path)).unwrap();
        assert!(!metadata.contains("\"vector\""));
        assert!(metadata.contains("\"connectivity\": 16"));
        assert!(metadata.contains("\"expansion_add\": 128"));
        assert!(metadata.contains("\"expansion_search\": 256"));

        let loaded = UsearchHnswVectorIndex::load(&path).unwrap();
        assert_eq!(loaded.parameters(), parameters);
        let hits = loaded
            .search(
                &[1.0, 0.0],
                VectorSearchOptions {
                    limit: 5,
                    allowlist: Some(HashSet::from([VectorId(1), VectorId(2)])),
                    target_kind: Some("chunk".into()),
                },
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].target_id, "a");
        assert!(loaded.memory_usage_bytes() > 0);
    }

    #[test]
    fn default_hnsw_profile_matches_measured_production_profile() {
        assert_eq!(HnswParameters::default(), PRODUCTION_HNSW_PARAMETERS);
        assert_eq!(PRODUCTION_HNSW_PARAMETERS.connectivity, 32);
        assert_eq!(PRODUCTION_HNSW_PARAMETERS.expansion_add, 256);
        assert_eq!(PRODUCTION_HNSW_PARAMETERS.expansion_search, 1_024);
    }

    #[test]
    fn rejects_zero_calibration_expansion() {
        let err = UsearchHnswVectorIndex::with_search_expansion(2, AnnScalarKind::F32, 2, 0)
            .err()
            .expect("zero expansion should fail");
        assert!(err.to_string().contains("greater than zero"));
    }

    #[test]
    fn detects_vector_id_collision() {
        let mut index = ExactFlatVectorIndex::new(1).unwrap();
        index.add(record(1, "a", "chunk", &[1.0])).unwrap();
        let err = index.add(record(1, "b", "chunk", &[1.0])).unwrap_err();
        assert!(err.to_string().contains("collision"));
    }
}
