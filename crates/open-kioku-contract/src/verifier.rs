use crate::ChangeContractV1;
use std::collections::BTreeSet;
use std::path::PathBuf;

pub struct ContractVerifier;

#[derive(Debug, Clone, Default)]
pub struct VerifyContractInput {
    pub changed_files: Vec<PathBuf>,
    pub changed_symbols: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ContractVerificationReport {
    pub passes: bool,
    pub violations: Vec<String>,
}

impl ContractVerifier {
    pub fn verify(
        contract: &ChangeContractV1,
        input: VerifyContractInput,
    ) -> ContractVerificationReport {
        let mut violations = Vec::new();

        let allowed_files: BTreeSet<_> = contract
            .primary_files
            .iter()
            .chain(contract.secondary_files.iter())
            .map(|f| f.as_path().to_path_buf())
            .collect();

        // Check file bounds
        for changed_file in &input.changed_files {
            let is_allowed = allowed_files.contains(changed_file);
            if !is_allowed {
                violations.push(format!(
                    "File {} modified but not allowed by contract",
                    changed_file.display()
                ));
            }
        }

        // Check forbidden files
        let forbidden_files: BTreeSet<_> = contract
            .forbidden_files
            .iter()
            .map(|f| f.as_path().to_path_buf())
            .collect();
        for changed_file in &input.changed_files {
            if forbidden_files.contains(changed_file) {
                violations.push(format!(
                    "File {} modified but forbidden by contract",
                    changed_file.display()
                ));
            }
        }

        // Check symbols bounds
        let allowed_symbols: BTreeSet<_> = contract
            .impacted_symbols
            .iter()
            .map(|s| s.0.clone())
            .collect();
        for changed_symbol in &input.changed_symbols {
            if !allowed_symbols.contains(changed_symbol) {
                violations.push(format!(
                    "Symbol {} modified but not tracked in contract",
                    changed_symbol
                ));
            }
        }

        ContractVerificationReport {
            passes: violations.is_empty(),
            violations,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConfidenceAssessment, ConfidenceLevel, ContractId, ContractTimestamps, ContractVersion,
        RiskAssessment, RiskLevel, SourcePlanRef,
    };
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn test_contract() -> ChangeContractV1 {
        ChangeContractV1 {
            id: ContractId::new("test"),
            version: ContractVersion::V1,
            task: "test task".into(),
            evidence_refs: vec![],
            primary_files: vec![crate::ContractFile::new("src/main.rs")],
            secondary_files: vec![crate::ContractFile::new("src/lib.rs")],
            forbidden_files: vec![crate::ContractFile::new("vendor/ignored.rs")],
            impacted_symbols: vec![crate::ImpactedSymbol::new("main")],
            required_tests: vec![],
            architecture_constraints: vec![],
            validation_commands: vec![],
            risk: RiskAssessment {
                level: RiskLevel::Low,
                score: 0.1,
                reasons: vec![],
            },
            confidence: ConfidenceAssessment {
                level: ConfidenceLevel::High,
                score: 0.9,
                basis: vec![],
                uncertainty: vec![],
            },
            timestamps: ContractTimestamps {
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            source_plan_ref: SourcePlanRef {
                id: "plan".into(),
                digest: "digest".into(),
            },
            extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn test_verify_passes() {
        let contract = test_contract();
        let input = VerifyContractInput {
            changed_files: vec![PathBuf::from("src/main.rs")],
            changed_symbols: vec!["main".to_string()],
        };

        let report = ContractVerifier::verify(&contract, input);
        assert!(report.passes);
        assert!(report.violations.is_empty());
    }

    #[test]
    fn test_verify_fails_unallowed_file() {
        let contract = test_contract();
        let input = VerifyContractInput {
            changed_files: vec![PathBuf::from("src/other.rs")],
            changed_symbols: vec!["main".to_string()],
        };

        let report = ContractVerifier::verify(&contract, input);
        assert!(!report.passes);
        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].contains("not allowed by contract"));
    }

    #[test]
    fn test_verify_fails_forbidden_file() {
        let contract = test_contract();
        let input = VerifyContractInput {
            changed_files: vec![PathBuf::from("vendor/ignored.rs")],
            changed_symbols: vec!["main".to_string()],
        };

        let report = ContractVerifier::verify(&contract, input);
        assert!(!report.passes);
        // It's both unallowed and forbidden in this logic, but wait, forbidden is also explicitly checked.
        assert!(report
            .violations
            .iter()
            .any(|v| v.contains("forbidden by contract")));
    }

    #[test]
    fn test_verify_fails_unallowed_symbol() {
        let contract = test_contract();
        let input = VerifyContractInput {
            changed_files: vec![PathBuf::from("src/main.rs")],
            changed_symbols: vec!["untracked_function".to_string()],
        };

        let report = ContractVerifier::verify(&contract, input);
        assert!(!report.passes);
        assert_eq!(report.violations.len(), 1);
        assert!(report.violations[0].contains("not tracked in contract"));
    }
}
