use chrono::Utc;
use open_kioku_contract::{
    ChangeContractV1, ConfidenceAssessment, ConfidenceLevel, ContractFile, ContractId,
    ContractTimestamps, ContractVersion, EvidenceRef, ImpactedSymbol, RequiredTest, RiskAssessment,
    RiskLevel, SourcePlanRef, ValidationCommand,
};
use open_kioku_core::PlanReport;
use std::collections::BTreeMap;

pub struct PlanReportAdapter;

impl PlanReportAdapter {
    pub fn adapt(plan: &PlanReport) -> ChangeContractV1 {
        // compute digest for source_plan_ref
        let plan_json = serde_json::to_string(plan).unwrap_or_default();
        use sha2::{Digest, Sha256};
        let digest = format!("{:x}", Sha256::digest(plan_json.as_bytes()));

        let id = ContractId::new(format!(
            "contract-{}",
            hex::encode(&digest.as_bytes()[0..8])
        ));

        let mut evidence_refs = Vec::new();
        for ev in &plan.evidence {
            evidence_refs.push(EvidenceRef::new(ev.id.0.clone()));
        }

        let primary_files = plan
            .recommended_change_boundary
            .allowed_files
            .iter()
            .map(ContractFile::new)
            .collect();
        let secondary_files = plan
            .recommended_change_boundary
            .caution_files
            .iter()
            .map(ContractFile::new)
            .collect();
        let forbidden_files = plan
            .recommended_change_boundary
            .forbidden_rules
            .iter()
            .map(|r| ContractFile::new(&r.pattern))
            .collect();

        let impacted_symbols = plan
            .relevant_symbols
            .iter()
            .map(|s| ImpactedSymbol::new(s.qualified_name.clone()))
            .collect();

        let required_tests = plan
            .validation
            .iter()
            .map(|test| RequiredTest {
                target: test.name.clone(),
                reason: "Included in plan validation".to_string(),
                evidence_refs: test
                    .evidence_refs
                    .iter()
                    .map(|e| EvidenceRef::new(e.clone()))
                    .collect(),
            })
            .collect();

        let architecture_constraints = Vec::new();

        let validation_commands = plan
            .validation
            .iter()
            .filter_map(|t| {
                t.command.clone().map(|cmd| ValidationCommand {
                    command: cmd,
                    reason: format!("Validation target {}", t.name),
                })
            })
            .collect();

        let confidence_score = plan.confidence_breakdown.overall_score;

        ChangeContractV1 {
            id,
            version: ContractVersion::V1,
            task: plan.task.clone(),
            evidence_refs,
            primary_files,
            secondary_files,
            forbidden_files,
            impacted_symbols,
            required_tests,
            architecture_constraints,
            validation_commands,
            risk: RiskAssessment {
                level: RiskLevel::from_score(plan.risk.score as f64),
                score: plan.risk.score as f64,
                reasons: plan.risk.reasons.clone(),
            },
            confidence: ConfidenceAssessment {
                level: ConfidenceLevel::from_score(confidence_score as f64),
                score: confidence_score as f64,
                basis: vec![plan.confidence_summary.clone()],
                uncertainty: vec![],
            },
            timestamps: ContractTimestamps {
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            source_plan_ref: SourcePlanRef {
                id: plan.task.clone(),
                digest,
            },
            extensions: BTreeMap::new(),
        }
    }
}
