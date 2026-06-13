use chrono::Utc;
use open_kioku_contract::{
    ChangeContractV1, ConfidenceAssessment, ConfidenceLevel, ContractFile, ContractId,
    ContractTimestamps, ContractVersion, EvidenceRef, ImpactedSymbol, RequiredTest, RiskAssessment,
    RiskLevel, SourcePlanRef,
};
use open_kioku_core::PlanReport;
use open_kioku_errors::Result;
use std::collections::BTreeMap;
use uuid::Uuid;

pub struct ContractBuilder;

impl ContractBuilder {
    pub fn build(task: &str, _limit: usize) -> Result<ChangeContractV1> {
        // Build an empty ChangeContractV1 stub for now
        Ok(ChangeContractV1 {
            id: ContractId::new(Uuid::new_v4().to_string()),
            version: ContractVersion::V1,
            task: task.to_string(),
            evidence_refs: vec![],
            primary_files: vec![],
            secondary_files: vec![],
            forbidden_files: vec![],
            impacted_symbols: vec![],
            required_tests: vec![],
            architecture_constraints: vec![],
            validation_commands: vec![],
            risk: RiskAssessment {
                level: RiskLevel::Low,
                score: 0.0,
                reasons: vec![],
            },
            confidence: ConfidenceAssessment {
                level: ConfidenceLevel::Low,
                score: 0.0,
                basis: vec![],
                uncertainty: vec![],
            },
            timestamps: ContractTimestamps {
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            source_plan_ref: SourcePlanRef {
                id: "stub".into(),
                digest: "stub".into(),
            },
            extensions: BTreeMap::new(),
        })
    }

    pub fn from_plan(plan: &PlanReport) -> Result<ChangeContractV1> {
        let mut evidence_refs = vec![];
        for ev in &plan.evidence {
            evidence_refs.push(EvidenceRef::new(&ev.id.0));
        }

        let mut primary_files = vec![];
        for ctx in &plan.primary_context {
            primary_files.push(ContractFile::new(&ctx.path));
        }

        let mut impacted_symbols = vec![];
        for sym in &plan.relevant_symbols {
            impacted_symbols.push(ImpactedSymbol::new(&sym.name));
        }

        let mut required_tests = vec![];
        for test in &plan.validation {
            required_tests.push(RequiredTest {
                target: test.file_id.0.clone(),
                reason: test.reason.clone(),
                evidence_refs: vec![],
            });
        }

        let mut forbidden_files = vec![];
        for pb in &plan.recommended_change_boundary.forbidden_files {
            forbidden_files.push(ContractFile::new(pb));
        }

        let risk = RiskAssessment {
            level: RiskLevel::from_score(plan.risk.score as f64),
            score: plan.risk.score as f64,
            reasons: plan.risk.reasons.clone(),
        };

        let confidence = ConfidenceAssessment {
            level: ConfidenceLevel::from_score(plan.confidence_breakdown.overall_score as f64),
            score: plan.confidence_breakdown.overall_score as f64,
            basis: vec![plan.confidence_summary.clone()],
            uncertainty: plan.confidence_breakdown.caveats.clone(),
        };

        let timestamps = ContractTimestamps {
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        Ok(ChangeContractV1 {
            id: ContractId::new(Uuid::new_v4().to_string()),
            version: ContractVersion::V1,
            task: plan.task.clone(),
            evidence_refs,
            primary_files,
            secondary_files: vec![],
            forbidden_files,
            impacted_symbols,
            required_tests,
            architecture_constraints: vec![],
            validation_commands: vec![],
            risk,
            confidence,
            timestamps,
            source_plan_ref: SourcePlanRef {
                id: "derived-from-plan".into(),
                digest: "derived-from-plan".into(),
            },
            extensions: BTreeMap::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{ConfidenceBreakdown, RiskReport};

    #[test]
    fn test_from_empty_plan() {
        let plan = PlanReport {
            task: "test task".into(),
            summary: "summary".into(),
            primary_context: vec![],
            relevant_symbols: vec![],
            impact: open_kioku_core::ImpactReport {
                target: "target".into(),
                direct_impacts: vec![],
                indirect_impacts: vec![],
                risk_report: RiskReport {
                    score: 0.0,
                    level: "low".into(),
                    reasons: vec![],
                },
                evidence: vec![],
                score_breakdown: vec![],
            },
            validation: vec![],
            risk: RiskReport {
                score: 0.5,
                level: "medium".into(),
                reasons: vec!["some risk".into()],
            },
            recommended_change_boundary: open_kioku_core::ChangeBoundary::default(),
            recommended_next_steps: vec![],
            tool_calls: vec![],
            memory_facts: vec![],
            runtime_signals: vec![],
            evidence: vec![],
            evidence_by_section: BTreeMap::new(),
            negative_evidence: vec![],
            confidence_summary: "confident".into(),
            confidence_breakdown: ConfidenceBreakdown::default(),
            score_breakdown: vec![],
        };

        let contract = ContractBuilder::from_plan(&plan).expect("builds contract");
        assert_eq!(contract.task, "test task");
        assert_eq!(contract.risk.score, 0.5);
        assert_eq!(contract.risk.reasons, vec!["some risk"]);
    }
}
