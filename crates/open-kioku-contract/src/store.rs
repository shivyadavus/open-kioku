use crate::{ChangeContractV1, ContractId};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Contract already exists and cannot be silently overwritten")]
    AlreadyExists,
    #[error("Contract not found: {0}")]
    NotFound(String),
    #[error("Corrupt contract JSON detected: {0}")]
    CorruptJson(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StoredContractRecord {
    pub contract: ChangeContractV1,
    pub stored_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContractVerificationRecord {
    pub verified_at: DateTime<Utc>,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContractStoreIndexEntry {
    pub id: ContractId,
    pub task: String,
    pub stored_at: DateTime<Utc>,
}

pub trait ContractStore {
    fn save(&self, contract: &ChangeContractV1) -> Result<(), StoreError>;
    fn load(&self, id: &ContractId) -> Result<ChangeContractV1, StoreError>;
    fn list(&self) -> Result<Vec<ContractStoreIndexEntry>, StoreError>;
    fn append_verification(
        &self,
        id: &ContractId,
        record: &ContractVerificationRecord,
    ) -> Result<(), StoreError>;
}

pub struct FsContractStore {
    base_dir: PathBuf,
}

impl FsContractStore {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    fn contract_path(&self, id: &ContractId) -> PathBuf {
        self.base_dir.join(format!("{}.json", id.0))
    }

    fn verification_path(&self, id: &ContractId) -> PathBuf {
        self.base_dir.join(format!("{}.verify.jsonl", id.0))
    }
}

impl ContractStore for FsContractStore {
    fn save(&self, contract: &ChangeContractV1) -> Result<(), StoreError> {
        if !self.base_dir.exists() {
            fs::create_dir_all(&self.base_dir)?;
        }

        let path = self.contract_path(&contract.id);
        if path.exists() {
            return Err(StoreError::AlreadyExists);
        }

        let record = StoredContractRecord {
            contract: contract.clone(),
            stored_at: Utc::now(),
        };

        let json = serde_json::to_string_pretty(&record)?;
        fs::write(path, json)?;

        Ok(())
    }

    fn load(&self, id: &ContractId) -> Result<ChangeContractV1, StoreError> {
        let path = self.contract_path(id);
        if !path.exists() {
            return Err(StoreError::NotFound(id.0.clone()));
        }

        let content = fs::read_to_string(&path)?;
        let record: StoredContractRecord =
            serde_json::from_str(&content).map_err(|e| StoreError::CorruptJson(e.to_string()))?;

        Ok(record.contract)
    }

    fn list(&self) -> Result<Vec<ContractStoreIndexEntry>, StoreError> {
        if !self.base_dir.exists() {
            return Ok(vec![]);
        }

        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
                let content = match fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                if let Ok(record) = serde_json::from_str::<StoredContractRecord>(&content) {
                    entries.push(ContractStoreIndexEntry {
                        id: record.contract.id.clone(),
                        task: record.contract.task.clone(),
                        stored_at: record.stored_at,
                    });
                }
            }
        }

        // Sort in reverse chronological order
        entries.sort_by_key(|b| std::cmp::Reverse(b.stored_at));
        Ok(entries)
    }

    fn append_verification(
        &self,
        id: &ContractId,
        record: &ContractVerificationRecord,
    ) -> Result<(), StoreError> {
        let contract_path = self.contract_path(id);
        if !contract_path.exists() {
            return Err(StoreError::NotFound(id.0.clone()));
        }

        let path = self.verification_path(id);
        let mut jsonl = serde_json::to_string(record)?;
        jsonl.push('\n');

        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file.write_all(jsonl.as_bytes())?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConfidenceAssessment, ConfidenceLevel, ContractFile, ContractTimestamps, ContractVersion,
        ImpactedSymbol, RequiredTest, RiskAssessment, RiskLevel, SourcePlanRef,
    };
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn dummy_contract(id: &str) -> ChangeContractV1 {
        ChangeContractV1 {
            id: ContractId::new(id),
            version: ContractVersion::V1,
            task: "dummy task".into(),
            evidence_refs: vec![crate::EvidenceRef::new("ev-1")],
            primary_files: vec![ContractFile::new("src/main.rs")],
            secondary_files: vec![],
            forbidden_files: vec![],
            impacted_symbols: vec![ImpactedSymbol::new("main")],
            required_tests: vec![RequiredTest {
                target: "src/main.rs".into(),
                reason: "test".into(),
                evidence_refs: vec![crate::EvidenceRef::new("ev-1")],
            }],
            architecture_constraints: vec![crate::ArchitectureConstraint {
                rule: "some rule".into(),
                severity: crate::ConstraintSeverity::Advisory,
                reason: "some reason".into(),
                evidence_refs: vec![crate::EvidenceRef::new("ev-1")],
            }],
            traceability: vec![],
            expansion_approval_requirements: vec![],
            validation_commands: vec![crate::ValidationCommand {
                command: "cargo test".into(),
                reason: "validate".into(),
            }],
            risk: RiskAssessment {
                level: RiskLevel::Low,
                score: 0.1,
                reasons: vec!["some risk".into()],
            },
            confidence: ConfidenceAssessment {
                level: ConfidenceLevel::High,
                score: 0.9,
                basis: vec!["some basis".into()],
                uncertainty: vec!["some uncertainty".into()],
            },
            timestamps: ContractTimestamps {
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            source_plan_ref: SourcePlanRef {
                id: "plan-id".into(),
                digest: "plan-digest".into(),
            },
            extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn test_save_load() {
        let dir = tempdir().unwrap();
        let store = FsContractStore::new(dir.path());
        let contract = dummy_contract("c-123");

        store.save(&contract).expect("save succeeds");
        let loaded = store
            .load(&ContractId::new("c-123"))
            .expect("load succeeds");
        assert_eq!(contract.id, loaded.id);
        assert_eq!(contract.task, loaded.task);
    }

    #[test]
    fn test_no_silent_overwrite() {
        let dir = tempdir().unwrap();
        let store = FsContractStore::new(dir.path());
        let contract = dummy_contract("c-123");

        store.save(&contract).expect("save succeeds");
        let err = store.save(&contract).expect_err("overwrite should fail");
        assert!(matches!(err, StoreError::AlreadyExists));
    }

    #[test]
    fn test_list_reverse_chronological() {
        let dir = tempdir().unwrap();
        let store = FsContractStore::new(dir.path());

        let contract1 = dummy_contract("c-1");
        store.save(&contract1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let contract2 = dummy_contract("c-2");
        store.save(&contract2).unwrap();

        let entries = store.list().expect("list succeeds");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id.0, "c-2");
        assert_eq!(entries[1].id.0, "c-1");
    }

    #[test]
    fn test_append_verification() {
        let dir = tempdir().unwrap();
        let store = FsContractStore::new(dir.path());
        let contract = dummy_contract("c-123");

        store.save(&contract).unwrap();

        let record = ContractVerificationRecord {
            verified_at: Utc::now(),
            success: true,
            stdout: "passed".into(),
            stderr: "".into(),
            report: None,
        };

        store
            .append_verification(&contract.id, &record)
            .expect("append succeeds");

        let verify_path = store.verification_path(&contract.id);
        let content = fs::read_to_string(&verify_path).expect("verify file exists");
        assert!(content.contains("passed"));
    }

    #[test]
    fn test_corrupt_json() {
        let dir = tempdir().unwrap();
        let store = FsContractStore::new(dir.path());
        let id = ContractId::new("c-123");

        fs::create_dir_all(dir.path()).unwrap();
        fs::write(store.contract_path(&id), "{ invalid json").unwrap();

        let err = store.load(&id).expect_err("should fail with corrupt json");
        assert!(matches!(err, StoreError::CorruptJson(_)));
    }

    #[test]
    fn test_missing_contract() {
        let dir = tempdir().unwrap();
        let store = FsContractStore::new(dir.path());
        let err = store
            .load(&ContractId::new("missing"))
            .expect_err("should fail");
        assert!(matches!(err, StoreError::NotFound(_)));
    }
}
