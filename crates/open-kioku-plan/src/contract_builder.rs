use chrono::Utc;
use open_kioku_contract::{
    validate_traceability, ArchitectureConstraint, ChangeContractV1, ConfidenceAssessment,
    ConfidenceLevel, ConstraintSeverity, ContractEvidenceTrace, ContractFile, ContractId,
    ContractTimestamps, ContractVersion, EvidenceRef, ExpansionApprovalRequirement, ImpactedSymbol,
    RequiredTest, RiskAssessment, RiskLevel, SourcePlanRef, ValidationCommand,
    ValidationRequirement,
};
use open_kioku_core::{
    PlanReport, PolicyCheckReport, PolicySignalSummary, PolicyViolation, PolicyViolationEvidenceRef,
};
use open_kioku_errors::{OkError, Result};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use uuid::Uuid;

pub struct ContractBuilder;

pub fn summarize_policy_for_contract(plan: &PlanReport) -> Option<PolicySignalSummary> {
    let report = architecture_policy_report(plan)?;
    Some(policy_signal_summary_from_report(report))
}

impl ContractBuilder {
    pub fn build(task: &str, _limit: usize) -> Result<ChangeContractV1> {
        Err(OkError::Unsupported(format!(
            "contract generation for `{task}` requires a PlanReport; use ContractBuilder::from_plan"
        )))
    }

    pub fn from_plan(plan: &PlanReport) -> Result<ChangeContractV1> {
        let evidence_refs = stable_evidence_refs(plan);
        if evidence_refs.is_empty() {
            return Err(OkError::Config(
                "contract generation requires at least one plan evidence reference".into(),
            ));
        }
        let fallback_refs = limited_refs(&evidence_refs, 3);
        let boundary_refs = boundary_evidence_refs(plan, &fallback_refs);
        let primary_refs = primary_context_evidence_refs(plan, &fallback_refs);
        let test_refs = validation_evidence_refs(plan, &fallback_refs);
        let risk_refs = section_evidence_refs(plan, "risk", &fallback_refs);
        let confidence_refs = section_evidence_refs(plan, "confidence", &fallback_refs);
        let history_refs = section_evidence_refs(plan, "history", &[]);

        let mut primary_files = Vec::new();
        let mut seen_files = BTreeSet::new();
        for ctx in &plan.primary_context {
            push_contract_file(&mut primary_files, &mut seen_files, &ctx.path);
        }
        for path in &plan.recommended_change_boundary.allowed_files {
            push_contract_file(&mut primary_files, &mut seen_files, path);
        }
        if primary_files.is_empty() {
            return Err(OkError::Config(
                "contract generation requires primary context or allowed boundary files".into(),
            ));
        }

        let mut impacted_symbols = Vec::new();
        let mut seen_symbols = BTreeSet::new();
        for sym in &plan.relevant_symbols {
            let symbol = if sym.qualified_name.trim().is_empty() {
                sym.name.trim()
            } else {
                sym.qualified_name.trim()
            };
            if !symbol.is_empty() && seen_symbols.insert(symbol.to_string()) {
                impacted_symbols.push(ImpactedSymbol::new(symbol));
            }
        }
        if impacted_symbols.is_empty() {
            impacted_symbols.push(ImpactedSymbol::new(format!(
                "file:{}",
                primary_files[0].as_str()
            )));
        }

        let mut required_tests = Vec::new();
        for test in &plan.validation {
            required_tests.push(RequiredTest {
                target: test.file_id.0.clone(),
                reason: non_empty_or(
                    &test.reason,
                    "Validation target selected by the source plan",
                ),
                evidence_refs: refs_or_fallback(&test.evidence_refs, &fallback_refs),
            });
        }
        if required_tests.is_empty() {
            required_tests.push(RequiredTest {
                target: "manual-validation".into(),
                reason: "No validation target was selected by the plan; manual validation is required before accepting the contract".into(),
                evidence_refs: fallback_refs.clone(),
            });
        }

        let mut secondary_files = Vec::new();
        let mut seen_secondary = seen_files.clone();
        for path in &plan.recommended_change_boundary.caution_files {
            push_contract_file(&mut secondary_files, &mut seen_secondary, path);
        }

        let mut forbidden_files = Vec::new();
        let mut seen_forbidden = BTreeSet::new();
        for pb in &plan.recommended_change_boundary.forbidden_files {
            push_contract_file(&mut forbidden_files, &mut seen_forbidden, pb);
        }

        let mut architecture_constraints = vec![ArchitectureConstraint {
            rule: "recommended-change-boundary".into(),
            severity: ConstraintSeverity::Required,
            reason: "Edits must stay within the recommended boundary unless explicit expansion evidence is supplied".into(),
            evidence_refs: boundary_refs.clone(),
        }];
        for rule in &plan.recommended_change_boundary.forbidden_rules {
            architecture_constraints.push(ArchitectureConstraint {
                rule: format!("forbidden-boundary:{}", rule.pattern),
                severity: ConstraintSeverity::Forbidden,
                reason: non_empty_or(&rule.reason, "Forbidden boundary rule from the source plan"),
                evidence_refs: refs_or_fallback(&rule.evidence_refs, &boundary_refs),
            });
        }
        let policy_refs = architecture_policy_evidence_refs(plan);
        architecture_constraints.extend(architecture_policy_constraints(plan, &policy_refs));
        let architecture_refs = merged_evidence_refs(&boundary_refs, &policy_refs);

        let mut validation_commands = Vec::new();
        let mut seen_commands = BTreeSet::new();
        for test in &plan.validation {
            let command = test
                .command
                .as_deref()
                .filter(|command| !command.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("manual validation: {}", test.name));
            if seen_commands.insert(command.clone()) {
                validation_commands.push(ValidationCommand {
                    command,
                    reason: non_empty_or(
                        &test.reason,
                        "Validation target selected by the source plan",
                    ),
                });
            }
        }
        if validation_commands.is_empty() {
            validation_commands.push(ValidationCommand {
                command: "manual validation: review plan evidence".into(),
                reason: "No executable validation command was selected by the plan".into(),
            });
        }
        let validation_requirements = validation_commands
            .iter()
            .map(|command| ValidationRequirement {
                command: command.command.clone(),
                cwd: None,
                reason: command.reason.clone(),
                evidence_refs: test_refs.clone(),
            })
            .collect::<Vec<_>>();

        let risk = RiskAssessment {
            level: RiskLevel::from_score(plan.risk.score as f64),
            score: plan.risk.score as f64,
            reasons: if plan.risk.reasons.is_empty() {
                vec!["No explicit risk reasons were produced by the plan".into()]
            } else {
                plan.risk.reasons.clone()
            },
        };

        let confidence_level =
            ConfidenceLevel::from_score(plan.confidence_breakdown.overall_score as f64);
        let confidence = ConfidenceAssessment {
            level: confidence_level,
            score: plan.confidence_breakdown.overall_score as f64,
            basis: if plan.confidence_summary.trim().is_empty() {
                vec!["No confidence summary was produced by the plan".into()]
            } else {
                vec![plan.confidence_summary.clone()]
            },
            uncertainty: if confidence_level == ConfidenceLevel::Exact
                || !plan.confidence_breakdown.caveats.is_empty()
            {
                plan.confidence_breakdown.caveats.clone()
            } else {
                vec!["No explicit confidence caveats were produced by the plan".into()]
            },
        };

        let timestamps = ContractTimestamps {
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let traceability = traceability_entries(
            TraceabilityRefs {
                primary: &primary_refs,
                tests: &test_refs,
                boundary: &architecture_refs,
                risk: &risk_refs,
                confidence: &confidence_refs,
                history: &history_refs,
            },
            !secondary_files.is_empty(),
            !forbidden_files.is_empty(),
        );
        let expansion_approval_requirements = expansion_approval_requirements(plan, &boundary_refs);
        let mut extensions = BTreeMap::new();
        if let Some(history_summary) = contract_history_signal_summary(plan, &history_refs) {
            extensions.insert("history_signal_summary".into(), history_summary);
        }

        let contract = ChangeContractV1 {
            id: ContractId::new(Uuid::new_v4().to_string()),
            version: ContractVersion::V1,
            task: plan.task.clone(),
            evidence_refs,
            primary_files,
            secondary_files,
            forbidden_files,
            impacted_symbols,
            required_tests,
            architecture_constraints,
            api_surface_constraints: Vec::new(),
            dependency_delta_constraints: Vec::new(),
            traceability,
            expansion_approval_requirements,
            validation_commands,
            validation_requirements,
            risk,
            confidence,
            timestamps,
            source_plan_ref: SourcePlanRef {
                id: "derived-from-plan".into(),
                digest: "derived-from-plan".into(),
            },
            extensions,
        };
        contract.validate().map_err(|err| {
            OkError::Config(format!("generated change contract is invalid: {err}"))
        })?;
        validate_traceability(&contract).map_err(|err| {
            OkError::Config(format!(
                "generated change contract is missing traceability: {err}"
            ))
        })?;
        Ok(contract)
    }
}

fn stable_evidence_refs(plan: &PlanReport) -> Vec<EvidenceRef> {
    let mut refs = BTreeSet::new();
    for evidence in &plan.evidence {
        push_ref(&mut refs, &evidence.id.0);
    }
    for refs_for_section in plan.evidence_by_section.values() {
        push_refs(&mut refs, refs_for_section);
    }
    for ctx in &plan.primary_context {
        for evidence_ref in ctx.derived_evidence_ids() {
            push_ref(&mut refs, &evidence_ref);
        }
        for component in &ctx.score_breakdown {
            push_refs(&mut refs, &component.evidence_ids);
        }
    }
    for evidence in &plan.impact.evidence {
        push_ref(&mut refs, &evidence.id.0);
    }
    for component in &plan.impact.score_breakdown {
        push_refs(&mut refs, &component.evidence_ids);
    }
    push_refs(&mut refs, &plan.recommended_change_boundary.evidence_refs);
    for rule in &plan.recommended_change_boundary.allowed_rules {
        push_refs(&mut refs, &rule.evidence_refs);
    }
    for rule in &plan.recommended_change_boundary.caution_rules {
        push_refs(&mut refs, &rule.evidence_refs);
    }
    for rule in &plan.recommended_change_boundary.forbidden_rules {
        push_refs(&mut refs, &rule.evidence_refs);
    }
    for requirement in &plan.recommended_change_boundary.expansion_requirements {
        push_refs(&mut refs, &requirement.required_evidence_refs);
    }
    for test in &plan.validation {
        push_refs(&mut refs, &test.evidence_refs);
        for component in &test.score_breakdown {
            push_refs(&mut refs, &component.evidence_ids);
        }
    }
    for component in &plan.score_breakdown {
        push_refs(&mut refs, &component.evidence_ids);
    }
    push_refs(&mut refs, &architecture_policy_evidence_ref_strings(plan));
    refs.into_iter().map(EvidenceRef::new).collect()
}

fn push_ref(refs: &mut BTreeSet<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        refs.insert(value.to_string());
    }
}

fn push_refs(refs: &mut BTreeSet<String>, values: &[String]) {
    for value in values {
        push_ref(refs, value);
    }
}

fn limited_refs(refs: &[EvidenceRef], limit: usize) -> Vec<EvidenceRef> {
    refs.iter().take(limit).cloned().collect()
}

fn merged_evidence_refs(left: &[EvidenceRef], right: &[EvidenceRef]) -> Vec<EvidenceRef> {
    let mut refs = BTreeSet::new();
    for evidence_ref in left.iter().chain(right.iter()) {
        push_ref(&mut refs, &evidence_ref.0);
    }
    refs.into_iter().map(EvidenceRef::new).collect()
}

fn refs_or_fallback(values: &[String], fallback: &[EvidenceRef]) -> Vec<EvidenceRef> {
    let mut refs = BTreeSet::new();
    push_refs(&mut refs, values);
    let refs = refs.into_iter().map(EvidenceRef::new).collect::<Vec<_>>();
    if refs.is_empty() {
        fallback.to_vec()
    } else {
        refs
    }
}

fn section_evidence_refs(
    plan: &PlanReport,
    section: &str,
    fallback: &[EvidenceRef],
) -> Vec<EvidenceRef> {
    let refs = plan
        .evidence_by_section
        .get(section)
        .map(|values| refs_or_fallback(values, fallback))
        .unwrap_or_default();
    if refs.is_empty() {
        fallback.to_vec()
    } else {
        refs
    }
}

fn primary_context_evidence_refs(plan: &PlanReport, fallback: &[EvidenceRef]) -> Vec<EvidenceRef> {
    let mut refs = BTreeSet::new();
    for ctx in &plan.primary_context {
        for evidence_ref in ctx.derived_evidence_ids() {
            push_ref(&mut refs, &evidence_ref);
        }
    }
    let refs = refs.into_iter().map(EvidenceRef::new).collect::<Vec<_>>();
    if refs.is_empty() {
        fallback.to_vec()
    } else {
        refs
    }
}

fn validation_evidence_refs(plan: &PlanReport, fallback: &[EvidenceRef]) -> Vec<EvidenceRef> {
    let mut refs = BTreeSet::new();
    for test in &plan.validation {
        push_refs(&mut refs, &test.evidence_refs);
    }
    let refs = refs.into_iter().map(EvidenceRef::new).collect::<Vec<_>>();
    if refs.is_empty() {
        fallback.to_vec()
    } else {
        refs
    }
}

fn boundary_evidence_refs(plan: &PlanReport, fallback: &[EvidenceRef]) -> Vec<EvidenceRef> {
    let mut refs = BTreeSet::new();
    push_refs(&mut refs, &plan.recommended_change_boundary.evidence_refs);
    for rule in &plan.recommended_change_boundary.allowed_rules {
        push_refs(&mut refs, &rule.evidence_refs);
    }
    for rule in &plan.recommended_change_boundary.caution_rules {
        push_refs(&mut refs, &rule.evidence_refs);
    }
    for rule in &plan.recommended_change_boundary.forbidden_rules {
        push_refs(&mut refs, &rule.evidence_refs);
    }
    for requirement in &plan.recommended_change_boundary.expansion_requirements {
        push_refs(&mut refs, &requirement.required_evidence_refs);
    }
    let refs = refs.into_iter().map(EvidenceRef::new).collect::<Vec<_>>();
    if refs.is_empty() {
        fallback.to_vec()
    } else {
        refs
    }
}

fn architecture_policy_report(plan: &PlanReport) -> Option<&PolicyCheckReport> {
    plan.architecture_policy
        .as_ref()
        .or(plan.impact.architecture_policy.as_ref())
        .filter(|report| report.configured)
}

fn architecture_policy_evidence_ref_strings(plan: &PlanReport) -> Vec<String> {
    summarize_policy_for_contract(plan)
        .map(|summary| summary.evidence_refs)
        .unwrap_or_default()
}

fn architecture_policy_evidence_refs(plan: &PlanReport) -> Vec<EvidenceRef> {
    architecture_policy_evidence_ref_strings(plan)
        .into_iter()
        .map(EvidenceRef::new)
        .collect()
}

fn architecture_policy_constraints(
    plan: &PlanReport,
    policy_refs: &[EvidenceRef],
) -> Vec<ArchitectureConstraint> {
    let Some(report) = architecture_policy_report(plan) else {
        return Vec::new();
    };
    let summary = policy_signal_summary_from_report(report);
    let summary_refs = policy_refs
        .iter()
        .filter(|evidence_ref| evidence_ref.0 == "architecture-policy:summary")
        .cloned()
        .collect::<Vec<_>>();
    let summary_refs = if summary_refs.is_empty() {
        policy_refs.to_vec()
    } else {
        summary_refs
    };
    let mut constraints = vec![ArchitectureConstraint {
        rule: "architecture-policy-summary".into(),
        severity: if summary.violation_count > 0 {
            ConstraintSeverity::Forbidden
        } else if summary.unknown_edge_count > 0 {
            ConstraintSeverity::Required
        } else {
            ConstraintSeverity::Advisory
        },
        reason: format!(
            "Architecture policy evaluated {} edge(s): {} allowed, {} violation(s), {} unknown.",
            summary.evaluated_edge_count,
            summary.allowed_edges,
            summary.violation_count,
            summary.unknown_edge_count
        ),
        evidence_refs: summary_refs,
    }];
    constraints.extend(
        report
            .violations
            .iter()
            .take(25)
            .enumerate()
            .map(|(index, violation)| ArchitectureConstraint {
                rule: format!(
                    "architecture-policy-violation:{}",
                    stable_constraint_slug(&violation.rule_id, index)
                ),
                severity: policy_constraint_severity(&violation.severity),
                reason: format!(
                    "{}: {} -> {} via {:?}: {}",
                    violation.rule_id,
                    violation.source_path.display(),
                    violation.target_path.display(),
                    violation.edge_type,
                    violation.message
                ),
                evidence_refs: vec![EvidenceRef::new(architecture_policy_violation_ref(
                    index, violation,
                ))],
            }),
    );
    constraints
}

fn policy_signal_summary_from_report(report: &PolicyCheckReport) -> PolicySignalSummary {
    let violation_refs = report
        .violations
        .iter()
        .take(25)
        .enumerate()
        .map(|(index, violation)| PolicyViolationEvidenceRef {
            id: architecture_policy_violation_ref(index, violation),
            rule_id: violation.rule_id.clone(),
            severity: violation.severity.clone(),
            source_path: violation.source_path.clone(),
            target_path: violation.target_path.clone(),
            edge_type: violation.edge_type,
        })
        .collect::<Vec<_>>();
    let mut evidence_refs = vec!["architecture-policy:summary".into()];
    evidence_refs.extend(
        violation_refs
            .iter()
            .map(|evidence_ref| evidence_ref.id.clone()),
    );
    PolicySignalSummary {
        configured: report.configured,
        evaluated_edge_count: report.evaluated_edge_count,
        allowed_edges: report.allowed_edges,
        violation_count: report.violation_count,
        public_api_violation_count: report.public_api_violation_count,
        exempted_violation_count: report.exempted_violation_count,
        unknown_edge_count: report.unknown_edge_count,
        evidence_refs,
        violation_refs,
        uncertainty: report.uncertainty.clone(),
    }
}

fn architecture_policy_violation_ref(index: usize, violation: &PolicyViolation) -> String {
    format!(
        "architecture-policy:violation:{index}:{}",
        stable_constraint_slug(&violation.rule_id, index)
    )
}

fn stable_constraint_slug(value: &str, fallback_index: usize) -> String {
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        format!("rule-{fallback_index}")
    } else {
        slug
    }
}

fn policy_constraint_severity(severity: &str) -> ConstraintSeverity {
    match severity.trim().to_ascii_lowercase().as_str() {
        "error" | "deny" | "forbidden" | "block" | "blocked" => ConstraintSeverity::Forbidden,
        "warning" | "warn" | "required" => ConstraintSeverity::Required,
        _ => ConstraintSeverity::Advisory,
    }
}

struct TraceabilityRefs<'a> {
    primary: &'a [EvidenceRef],
    tests: &'a [EvidenceRef],
    boundary: &'a [EvidenceRef],
    risk: &'a [EvidenceRef],
    confidence: &'a [EvidenceRef],
    history: &'a [EvidenceRef],
}

fn traceability_entries(
    refs: TraceabilityRefs<'_>,
    has_secondary_files: bool,
    has_forbidden_files: bool,
) -> Vec<ContractEvidenceTrace> {
    let mut entries = vec![
        trace(
            "task",
            "Task text is constrained by the source plan and its evidence set",
            refs.primary,
        ),
        trace(
            "primary_files",
            "Primary files come from matched plan context and allowed boundary files",
            refs.primary,
        ),
        trace(
            "impacted_symbols",
            "Impacted symbols come from plan symbol evidence or file-level fallback scope",
            refs.primary,
        ),
        trace(
            "required_tests",
            "Required tests come from the plan validation section",
            refs.tests,
        ),
        trace(
            "architecture_constraints",
            "Architecture constraints come from the recommended change boundary and architecture policy report",
            refs.boundary,
        ),
        trace(
            "validation_commands",
            "Validation commands come from test commands or explicit manual validation targets",
            refs.tests,
        ),
        trace(
            "validation_requirements",
            "Validation requirements bind commands to attestation evidence for post-edit verification",
            refs.tests,
        ),
        trace(
            "risk",
            "Risk assessment is constrained by the plan risk section",
            refs.risk,
        ),
        trace(
            "confidence",
            "Confidence assessment is constrained by the plan confidence section",
            refs.confidence,
        ),
    ];
    if has_secondary_files {
        entries.push(trace(
            "secondary_files",
            "Secondary files come from caution boundary files",
            refs.boundary,
        ));
    }
    if has_forbidden_files {
        entries.push(trace(
            "forbidden_files",
            "Forbidden files come from explicit plan boundary exclusions",
            refs.boundary,
        ));
    }
    if !refs.history.is_empty() {
        entries.push(trace(
            "history_signals",
            "History signals come from bounded churn, ownership, similar-change, and reviewer evidence in the source plan",
            refs.history,
        ));
    }
    entries
}

fn contract_history_signal_summary(
    plan: &PlanReport,
    history_refs: &[EvidenceRef],
) -> Option<serde_json::Value> {
    let mut signals = BTreeSet::new();
    for component in &plan.score_breakdown {
        if is_history_signal(&component.signal) {
            signals.insert(component.signal.clone());
        }
    }
    if signals.is_empty() && history_refs.is_empty() {
        return None;
    }
    Some(json!({
        "signals": signals.into_iter().collect::<Vec<_>>(),
        "evidence_refs": history_refs.iter().map(|value| value.0.clone()).collect::<Vec<_>>(),
        "bounded": true,
        "dominance_rule": "exact code references, direct symbol coverage, and explicit boundary evidence remain authoritative over historical heuristics"
    }))
}

fn is_history_signal(signal: &str) -> bool {
    matches!(
        signal,
        "history_churn" | "ownership_risk" | "similar_change_overlap" | "reviewer_affinity"
    )
}

fn trace(field: &str, rationale: &str, evidence_refs: &[EvidenceRef]) -> ContractEvidenceTrace {
    ContractEvidenceTrace {
        field: field.into(),
        rationale: rationale.into(),
        evidence_refs: evidence_refs.to_vec(),
        unspecified_rationale: evidence_refs
            .is_empty()
            .then(|| "The source plan did not provide a field-specific evidence reference".into()),
    }
}

fn expansion_approval_requirements(
    plan: &PlanReport,
    fallback: &[EvidenceRef],
) -> Vec<ExpansionApprovalRequirement> {
    let requirements = plan
        .recommended_change_boundary
        .expansion_requirements
        .iter()
        .enumerate()
        .map(|(index, requirement)| ExpansionApprovalRequirement {
            scope: format!("outside_recommended_change_boundary[{index}]"),
            reason: non_empty_or(
                &requirement.reason,
                "Boundary expansion requires explicit saved-plan evidence",
            ),
            required_evidence_refs: refs_or_fallback(&requirement.required_evidence_refs, fallback),
        })
        .collect::<Vec<_>>();
    if requirements.is_empty() {
        vec![ExpansionApprovalRequirement {
            scope: "outside_recommended_change_boundary".into(),
            reason: "Changing files outside the recommended boundary requires explicit evidence from the saved plan".into(),
            required_evidence_refs: fallback.to_vec(),
        }]
    } else {
        requirements
    }
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.into()
    } else {
        value.into()
    }
}

fn push_contract_file(
    files: &mut Vec<ContractFile>,
    seen: &mut BTreeSet<String>,
    path: impl AsRef<Path>,
) {
    let file = ContractFile::new(path);
    if seen.insert(file.0.clone()) {
        files.push(file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, ConfidenceBreakdown, EnforcedEdgeType, FileId, Language, PolicyCheckReport,
        PolicyMatchEvidence, PolicyViolation, RiskReport, ScoreComponent, SearchResult, Symbol,
        SymbolId, SymbolKind, TestTarget,
    };
    use std::path::PathBuf;

    #[test]
    fn from_plan_rejects_plans_without_evidence() {
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
                architecture_policy: None,
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
            architecture_policy: None,
            evidence: vec![],
            evidence_by_section: BTreeMap::new(),
            negative_evidence: vec![],
            confidence_summary: "confident".into(),
            confidence_breakdown: ConfidenceBreakdown::default(),
            score_breakdown: vec![],
        };

        let err = ContractBuilder::from_plan(&plan).expect_err("empty plans are not authoritative");
        assert!(err
            .to_string()
            .contains("requires at least one plan evidence reference"));
    }

    #[test]
    fn from_plan_generates_strict_traceable_contracts() {
        let mut evidence_by_section = BTreeMap::new();
        evidence_by_section.insert("risk".into(), vec!["risk:surface".into()]);
        evidence_by_section.insert("confidence".into(), vec!["ctx:lib".into()]);
        evidence_by_section.insert("history".into(), vec!["history-similar:abc".into()]);

        let plan = PlanReport {
            task: "change handler".into(),
            summary: "summary".into(),
            primary_context: vec![SearchResult {
                path: PathBuf::from("src/lib.rs"),
                line_range: None,
                snippet: "fn handler() {}".into(),
                symbol: None,
                score: 0.9,
                match_reason: "lexical match".into(),
                evidence: vec![],
                evidence_refs: vec!["ctx:lib".into()],
                confidence: 0.9,
                score_breakdown: vec![],
            }],
            relevant_symbols: vec![Symbol {
                id: SymbolId::new("sym-handler"),
                name: "handler".into(),
                qualified_name: "crate::handler".into(),
                kind: SymbolKind::Function,
                file_id: FileId::new("src/lib.rs"),
                range: None,
                language: Language::Rust,
                confidence: Confidence::High,
                provenance: open_kioku_core::EvidenceSourceType::TreeSitter,
            }],
            impact: open_kioku_core::ImpactReport {
                target: "target".into(),
                direct_impacts: vec![],
                indirect_impacts: vec![],
                risk_report: RiskReport {
                    score: 0.2,
                    level: "low".into(),
                    reasons: vec!["low impact".into()],
                },
                evidence: vec![],
                architecture_policy: None,
                score_breakdown: vec![],
            },
            validation: vec![TestTarget {
                id: "unit-handler".into(),
                name: "unit handler".into(),
                file_id: FileId::new("tests/handler.rs"),
                range: None,
                command: Some("cargo test -p app handler".into()),
                confidence: Confidence::High,
                reason: "handler behavior is covered by unit tests".into(),
                evidence_refs: vec!["test:unit-handler".into()],
                score_breakdown: vec![],
            }],
            risk: RiskReport {
                score: 0.5,
                level: "medium".into(),
                reasons: vec!["some risk".into()],
            },
            recommended_change_boundary: open_kioku_core::ChangeBoundary {
                allowed_files: vec![PathBuf::from("src/lib.rs")],
                caution_files: vec![PathBuf::from("src/config.rs")],
                forbidden_files: vec![PathBuf::from("secrets.env")],
                evidence_refs: vec!["boundary:allowed".into()],
                expansion_requirements: vec![open_kioku_core::BoundaryExpansionRequirement {
                    reason: "Expansion requires boundary evidence".into(),
                    required_evidence_refs: vec!["boundary:allowed".into()],
                }],
                ..Default::default()
            },
            recommended_next_steps: vec![],
            tool_calls: vec![],
            memory_facts: vec![],
            runtime_signals: vec![],
            architecture_policy: Some(policy_report_with_violation()),
            evidence: vec![],
            evidence_by_section,
            negative_evidence: vec![],
            confidence_summary: "confident".into(),
            confidence_breakdown: ConfidenceBreakdown {
                overall_enum: Confidence::Medium,
                overall_score: 0.7,
                components: vec![],
                blockers: vec![],
                caveats: vec!["runtime corroboration is absent".into()],
            },
            score_breakdown: vec![ScoreComponent::adjustment(
                "similar_change_overlap",
                0.12,
                vec!["history-similar:abc".into()],
                "bounded similar-change overlap from persisted local history",
            )],
        };

        let contract = ContractBuilder::from_plan(&plan).expect("builds contract");
        let policy_summary =
            summarize_policy_for_contract(&plan).expect("policy summary is available");
        assert_eq!(policy_summary.violation_count, 1);
        assert_eq!(
            policy_summary.violation_refs[0].id,
            "architecture-policy:violation:0:domain-api"
        );
        assert_eq!(contract.task, "change handler");
        assert_eq!(contract.risk.score, 0.5);
        assert_eq!(contract.risk.reasons, vec!["some risk"]);
        validate_traceability(&contract).expect("generated contract is traceable");
        assert!(contract
            .traceability
            .iter()
            .any(|trace| trace.field == "forbidden_files"));
        assert_eq!(
            contract.expansion_approval_requirements[0].required_evidence_refs,
            vec![EvidenceRef::new("boundary:allowed")]
        );
        assert!(contract
            .evidence_refs
            .iter()
            .any(|evidence_ref| evidence_ref.0 == "architecture-policy:violation:0:domain-api"));
        let policy_constraint = contract
            .architecture_constraints
            .iter()
            .find(|constraint| constraint.rule == "architecture-policy-violation:domain-api")
            .expect("policy violation constraint");
        assert_eq!(policy_constraint.severity, ConstraintSeverity::Forbidden);
        assert_eq!(
            policy_constraint.evidence_refs,
            vec![EvidenceRef::new(
                "architecture-policy:violation:0:domain-api"
            )]
        );
        assert!(contract
            .traceability
            .iter()
            .any(|trace| trace.field == "history_signals"));
        assert!(contract
            .extensions
            .get("history_signal_summary")
            .and_then(|value| value.get("signals"))
            .and_then(|value| value.as_array())
            .is_some_and(|signals| signals
                .iter()
                .any(|signal| signal == "similar_change_overlap")));
    }

    fn policy_report_with_violation() -> PolicyCheckReport {
        PolicyCheckReport {
            configured: true,
            evaluated_edge_count: 1,
            allowed_edges: 0,
            violation_count: 1,
            violations: vec![PolicyViolation {
                rule_id: "domain-api".into(),
                severity: "error".into(),
                source_component: "domain".into(),
                target_component: "api".into(),
                source_path: PathBuf::from("src/domain.rs"),
                target_path: PathBuf::from("src/api.rs"),
                edge_type: EnforcedEdgeType::Imports,
                evidence: PolicyMatchEvidence {
                    edge_id: "edge-domain-api".into(),
                    edge_type: EnforcedEdgeType::Imports,
                    source_node: "src/domain.rs".into(),
                    target_node: "src/api.rs".into(),
                    source_path: PathBuf::from("src/domain.rs"),
                    target_path: PathBuf::from("src/api.rs"),
                    confidence: Confidence::High,
                    message: "domain imported api".into(),
                },
                message: "domain must not import api".into(),
            }],
            ..PolicyCheckReport::default()
        }
    }
}
