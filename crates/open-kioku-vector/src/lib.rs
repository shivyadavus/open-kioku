use open_kioku_errors::{OkError, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

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
        if record.vector.len() != self.dimensions {
            return Err(OkError::Storage(format!(
                "vector {} has {} dimensions, expected {}",
                record.id.0,
                record.vector.len(),
                self.dimensions
            )));
        }
        if self
            .records
            .get(&record.id)
            .is_some_and(|existing| existing.target_id != record.target_id)
        {
            return Err(OkError::Storage(format!(
                "vector id collision for {}",
                record.id.0
            )));
        }
        self.records.insert(record.id, record);
        Ok(())
    }

    pub fn remove(&mut self, id: VectorId) -> Option<VectorRecord> {
        self.records.remove(&id)
    }

    pub fn search(&self, query: &[f32], options: VectorSearchOptions) -> Result<Vec<VectorHit>> {
        if query.len() != self.dimensions {
            return Err(OkError::Storage(format!(
                "query vector has {} dimensions, expected {}",
                query.len(),
                self.dimensions
            )));
        }
        let mut hits = Vec::new();
        for record in self.records.values() {
            if options
                .allowlist
                .as_ref()
                .is_some_and(|allowlist| !allowlist.contains(&record.id))
            {
                continue;
            }
            if options
                .target_kind
                .as_ref()
                .is_some_and(|kind| kind != &record.target_kind)
            {
                continue;
            }
            let score = dot(query, &record.vector);
            if score <= 0.0 {
                continue;
            }
            hits.push(VectorHit {
                id: record.id,
                target_id: record.target_id.clone(),
                target_kind: record.target_kind.clone(),
                score,
            });
        }
        hits.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| left.target_id.cmp(&right.target_id))
        });
        hits.truncate(options.limit.max(1));
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

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_backend_searches_with_allowlist() {
        let mut index = ExactFlatVectorIndex::new(2).unwrap();
        index
            .add(VectorRecord {
                id: VectorId(1),
                target_id: "a".into(),
                target_kind: "chunk".into(),
                vector: vec![1.0, 0.0],
            })
            .unwrap();
        index
            .add(VectorRecord {
                id: VectorId(2),
                target_id: "b".into(),
                target_kind: "chunk".into(),
                vector: vec![0.0, 1.0],
            })
            .unwrap();

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

        let removed = index.remove(VectorId(1)).unwrap();
        assert_eq!(removed.target_id, "a");
        let hits_after_remove = index
            .search(
                &[1.0, 0.0],
                VectorSearchOptions {
                    limit: 5,
                    allowlist: None,
                    target_kind: None,
                },
            )
            .unwrap();
        assert!(hits_after_remove.is_empty());
    }

    #[test]
    fn detects_vector_id_collision() {
        let mut index = ExactFlatVectorIndex::new(1).unwrap();
        index
            .add(VectorRecord {
                id: VectorId(1),
                target_id: "a".into(),
                target_kind: "chunk".into(),
                vector: vec![1.0],
            })
            .unwrap();

        let err = index
            .add(VectorRecord {
                id: VectorId(1),
                target_id: "b".into(),
                target_kind: "chunk".into(),
                vector: vec![1.0],
            })
            .unwrap_err();
        assert!(err.to_string().contains("collision"));
    }
}
