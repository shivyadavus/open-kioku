use open_kioku_actions::{ActionKind, PolicyGate};
use open_kioku_config::OkConfig;
use open_kioku_context::ContextPackBuilder;
use open_kioku_contract::{
    validate_traceability, ChangeContractV1, ConstraintSeverity, ContractStore,
    ContractVerificationRecord, EvidenceRef,
};
use open_kioku_core::{
    AnalysisFact, BoundaryExpansionRequirement, BoundaryForbiddenRule, ChangeBoundary, Confidence,
    ConfidenceBreakdown, EvidenceSourceType, FileId, ImpactReport, PatchId, PatchPlan, PlanReport,
    RiskReport, SearchResult, TestTarget,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_impact::ImpactEngine;
use open_kioku_plan::ContractBuilder;
use open_kioku_storage::{MetadataStore, OkStore, SearchIndex};
use open_kioku_tests::TestSelector;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

const HIGH_RUNTIME_ERROR_RATE: f32 = 0.20;

pub struct PatchPlanner<'a> {
    config: &'a OkConfig,
    store: &'a dyn OkStore,
}

impl<'a> PatchPlanner<'a> {
    pub fn new(config: &'a OkConfig, store: &'a dyn OkStore) -> Self {
        Self { config, store }
    }

    pub fn plan(&self, task: &str) -> Result<PatchPlan> {
        let context = ContextPackBuilder::new(self.store).build(task, 12)?;
        Ok(PatchPlan {
            id: PatchId::new(stable_id(task)),
            task: task.into(),
            allowed_files: context.recommended_change_boundary.allowed_files,
            caution_files: context.recommended_change_boundary.caution_files,
            forbidden_files: context.recommended_change_boundary.forbidden_files,
            change_steps: vec![
                "Inspect primary symbols and definitions from the context pack".into(),
                "Constrain edits to allowed files unless evidence justifies expansion".into(),
                "Run the recommended validation plan after approval".into(),
            ],
            risks: context.risk_report.reasons,
            assumptions: vec![
                "Generated and vendor files remain out of scope".into(),
                "Patch application requires explicit write mode and approval".into(),
            ],
            tests: context.test_candidates,
            rollback_notes: vec!["Revert the unified diff if validation fails".into()],
            unified_diff: None,
            requires_approval: self.config.security.approval_required,
            evidence: context.evidence,
        })
    }

    pub fn apply(&self, _patch: &PatchPlan, approved: bool) -> Result<()> {
        PolicyGate::new(self.config).ensure_allowed(ActionKind::ApplyPatch)?;
        if self.config.security.approval_required && !approved {
            return Err(OkError::PolicyDenied(
                "patch application requires explicit approval".into(),
            ));
        }
        Err(OkError::Unsupported(
            "patch application is intentionally not implemented without a diff applicator".into(),
        ))
    }
}

pub struct ChangeVerifier<'a> {
    store: &'a dyn OkStore,
    search_index: Option<&'a dyn SearchIndex>,
}

pub struct ContractVerifier<'a> {
    store: &'a dyn OkStore,
    search_index: Option<&'a dyn SearchIndex>,
    contract_store: Option<&'a dyn ContractStore>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VerifyChangeInput {
    #[serde(default)]
    pub changed_files: Vec<PathBuf>,
    #[serde(default)]
    pub unified_diff: Option<String>,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub run_commands: bool,
    #[serde(default)]
    pub traceability_strict: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeVerificationReport {
    pub verdict: VerificationVerdict,
    pub changed_files: Vec<PathBuf>,
    pub changed_symbols: Vec<String>,
    #[serde(default)]
    pub traceability: Vec<VerificationTrace>,
    pub boundary_violations: Vec<VerificationFinding>,
    pub warnings: Vec<VerificationFinding>,
    pub missing_tests: Vec<VerificationFinding>,
    pub changed_impact: Vec<VerificationFinding>,
    pub recommended_tests: Vec<TestTarget>,
    pub command_results: Vec<ValidationCommandResult>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationTrace {
    pub field: String,
    pub rationale: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationVerdict {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationDecision {
    Pass,
    Warn,
    Fail,
}

impl From<VerificationVerdict> for VerificationDecision {
    fn from(value: VerificationVerdict) -> Self {
        match value {
            VerificationVerdict::Pass => Self::Pass,
            VerificationVerdict::Warn => Self::Warn,
            VerificationVerdict::Fail => Self::Fail,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationPolicySnapshot {
    pub contract_version: String,
    pub traceability_strict: bool,
    pub primary_files: Vec<PathBuf>,
    pub secondary_files: Vec<PathBuf>,
    pub forbidden_files: Vec<PathBuf>,
    pub architecture_constraints: Vec<String>,
    pub expansion_requirements: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractVerificationReport {
    pub contract_id: String,
    pub decision: VerificationDecision,
    pub policy_snapshot: VerificationPolicySnapshot,
    pub change_report: ChangeVerificationReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationFinding {
    pub path: Option<PathBuf>,
    pub kind: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationCommandResult {
    pub command: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl<'a> ChangeVerifier<'a> {
    pub fn new(store: &'a dyn OkStore) -> Self {
        Self {
            store,
            search_index: None,
        }
    }

    pub fn with_search_index(mut self, search_index: Option<&'a dyn SearchIndex>) -> Self {
        self.search_index = search_index;
        self
    }

    pub fn verify(
        &self,
        repo: &Path,
        plan: &PlanReport,
        input: VerifyChangeInput,
    ) -> Result<ChangeVerificationReport> {
        if let Ok(contract) = ContractBuilder::from_plan(plan) {
            return ContractVerifier::new(self.store)
                .with_search_index(self.search_index)
                .verify_plan_adapter(repo, &contract, plan, input)
                .map(|report| report.change_report);
        }
        self.verify_plan_direct(repo, plan, input)
    }

    fn verify_plan_direct(
        &self,
        repo: &Path,
        plan: &PlanReport,
        input: VerifyChangeInput,
    ) -> Result<ChangeVerificationReport> {
        let changed_files = changed_files_from_input(&input);
        if changed_files.is_empty() {
            return Err(OkError::Config(
                "verify requires at least one changed file or a non-empty unified diff".into(),
            ));
        }

        let mut boundary_violations =
            boundary_violations(plan, &changed_files, &input.evidence_refs);
        if input.traceability_strict {
            boundary_violations.extend(unknown_evidence_ref_violations(plan, &input.evidence_refs));
        }
        let changed_symbols = changed_symbols(self.store, &changed_files)?;
        let recommended_tests = recommended_tests(self.store, &changed_files)?;
        let missing_tests = missing_tests(plan, &recommended_tests);
        let changed_impact = changed_impact(self.store, self.search_index, plan, &changed_files)?;
        let command_results = if input.run_commands {
            run_validation_commands(repo, plan)
        } else {
            Vec::new()
        };
        let command_failures = command_results
            .iter()
            .filter(|result| result.status == "fail")
            .map(|result| VerificationFinding {
                path: None,
                kind: "command_failed".into(),
                reason: format!(
                    "validation command `{}` exited with {:?}",
                    result.command, result.exit_code
                ),
                evidence_refs: Vec::new(),
            })
            .collect::<Vec<_>>();

        let mut warnings = Vec::new();
        warnings.extend(caution_warnings(plan, &changed_files));
        warnings.extend(expansion_warnings(
            plan,
            &changed_files,
            &input.evidence_refs,
        ));
        warnings.extend(runtime_warnings(self.store, &changed_files)?);
        let traceability = verification_traceability(plan, &input);

        let verdict = if !boundary_violations.is_empty() || !command_failures.is_empty() {
            VerificationVerdict::Fail
        } else if !warnings.is_empty() || !missing_tests.is_empty() || !changed_impact.is_empty() {
            VerificationVerdict::Warn
        } else {
            VerificationVerdict::Pass
        };

        let mut all_boundary_violations = boundary_violations;
        all_boundary_violations.extend(command_failures);

        Ok(ChangeVerificationReport {
            verdict,
            changed_files,
            changed_symbols,
            traceability,
            boundary_violations: all_boundary_violations,
            warnings,
            missing_tests,
            changed_impact,
            recommended_tests,
            command_results,
            evidence_refs: input.evidence_refs,
        })
    }
}

impl<'a> ContractVerifier<'a> {
    pub fn new(store: &'a dyn OkStore) -> Self {
        Self {
            store,
            search_index: None,
            contract_store: None,
        }
    }

    pub fn with_search_index(mut self, search_index: Option<&'a dyn SearchIndex>) -> Self {
        self.search_index = search_index;
        self
    }

    pub fn with_contract_store(mut self, contract_store: Option<&'a dyn ContractStore>) -> Self {
        self.contract_store = contract_store;
        self
    }

    pub fn verify(
        &self,
        repo: &Path,
        contract: &ChangeContractV1,
        input: VerifyChangeInput,
    ) -> Result<ContractVerificationReport> {
        let plan = contract_to_plan_report(contract);
        self.verify_with_plan(repo, contract, &plan, input)
    }

    pub fn verify_plan_adapter(
        &self,
        repo: &Path,
        contract: &ChangeContractV1,
        plan: &PlanReport,
        input: VerifyChangeInput,
    ) -> Result<ContractVerificationReport> {
        self.verify_with_plan(repo, contract, plan, input)
    }

    fn verify_with_plan(
        &self,
        repo: &Path,
        contract: &ChangeContractV1,
        plan: &PlanReport,
        input: VerifyChangeInput,
    ) -> Result<ContractVerificationReport> {
        contract.validate().map_err(|err| {
            OkError::Config(format!("contract verification input is invalid: {err}"))
        })?;
        if input.traceability_strict {
            validate_traceability(contract).map_err(|err| {
                OkError::Config(format!(
                    "contract verification input is missing traceability: {err}"
                ))
            })?;
        }

        let traceability_strict = input.traceability_strict;
        let change_report = ChangeVerifier {
            store: self.store,
            search_index: self.search_index,
        }
        .verify_plan_direct(repo, plan, input)?;
        let decision = VerificationDecision::from(change_report.verdict);
        let report = ContractVerificationReport {
            contract_id: contract.id.0.clone(),
            decision,
            policy_snapshot: policy_snapshot(contract, traceability_strict),
            change_report,
        };
        if let Some(store) = self.contract_store {
            let report_value = serde_json::to_value(&report).map_err(OkError::Json)?;
            let record = ContractVerificationRecord {
                verified_at: chrono::Utc::now(),
                success: decision != VerificationDecision::Fail,
                stdout: serde_json::to_string(&report).map_err(OkError::Json)?,
                stderr: String::new(),
                report: Some(report_value),
            };
            store
                .append_verification(&contract.id, &record)
                .map_err(|err| OkError::Storage(err.to_string()))?;
        }
        Ok(report)
    }
}

fn contract_to_plan_report(contract: &ChangeContractV1) -> PlanReport {
    let evidence_refs = evidence_ref_strings(&contract.evidence_refs);
    let mut evidence_by_section = BTreeMap::new();
    evidence_by_section.insert("contract".into(), evidence_refs.clone());
    for trace in &contract.traceability {
        evidence_by_section.insert(
            trace.field.clone(),
            evidence_ref_strings(&trace.evidence_refs),
        );
    }

    PlanReport {
        task: contract.task.clone(),
        summary: format!("verification adapter for contract {}", contract.id),
        primary_context: Vec::new(),
        relevant_symbols: Vec::new(),
        impact: ImpactReport {
            target: contract.task.clone(),
            direct_impacts: Vec::new(),
            indirect_impacts: Vec::new(),
            risk_report: contract_risk_report(contract),
            evidence: Vec::new(),
            score_breakdown: Vec::new(),
        },
        validation: contract_validation_targets(contract),
        risk: contract_risk_report(contract),
        recommended_change_boundary: contract_change_boundary(contract),
        recommended_next_steps: Vec::new(),
        tool_calls: Vec::new(),
        memory_facts: Vec::new(),
        runtime_signals: Vec::new(),
        evidence: Vec::new(),
        evidence_by_section,
        negative_evidence: Vec::new(),
        confidence_summary: contract.confidence.basis.join("; "),
        confidence_breakdown: ConfidenceBreakdown {
            overall_enum: contract_confidence(contract),
            overall_score: contract.confidence.score as f32,
            components: Vec::new(),
            blockers: Vec::new(),
            caveats: contract.confidence.uncertainty.clone(),
        },
        score_breakdown: Vec::new(),
    }
}

fn contract_change_boundary(contract: &ChangeContractV1) -> ChangeBoundary {
    let evidence_refs = evidence_ref_strings(&contract.evidence_refs);
    let mut forbidden_rules = contract
        .forbidden_files
        .iter()
        .map(|file| BoundaryForbiddenRule {
            pattern: file.as_str().into(),
            reason: "forbidden by contract boundary".into(),
            evidence_refs: evidence_refs.clone(),
        })
        .collect::<Vec<_>>();
    for constraint in &contract.architecture_constraints {
        if constraint.severity == ConstraintSeverity::Forbidden {
            forbidden_rules.push(BoundaryForbiddenRule {
                pattern: constraint
                    .rule
                    .strip_prefix("forbidden-boundary:")
                    .unwrap_or(&constraint.rule)
                    .into(),
                reason: constraint.reason.clone(),
                evidence_refs: evidence_ref_strings(&constraint.evidence_refs),
            });
        }
    }
    ChangeBoundary {
        allowed_files: contract_file_paths(&contract.primary_files),
        caution_files: contract_file_paths(&contract.secondary_files),
        forbidden_files: contract_file_paths(&contract.forbidden_files),
        evidence_refs: evidence_refs.clone(),
        allowed_symbols: contract
            .impacted_symbols
            .iter()
            .map(|symbol| symbol.0.clone())
            .collect(),
        allowed_rules: Vec::new(),
        caution_rules: Vec::new(),
        forbidden_rules,
        expansion_requirements: contract
            .expansion_approval_requirements
            .iter()
            .map(|requirement| BoundaryExpansionRequirement {
                reason: requirement.reason.clone(),
                required_evidence_refs: evidence_ref_strings(&requirement.required_evidence_refs),
            })
            .collect(),
        signal_hooks: Default::default(),
    }
}

fn contract_validation_targets(contract: &ChangeContractV1) -> Vec<TestTarget> {
    contract
        .required_tests
        .iter()
        .enumerate()
        .map(|(index, test)| TestTarget {
            id: test.target.clone(),
            name: test.target.clone(),
            file_id: FileId::new(&test.target),
            range: None,
            command: contract
                .validation_commands
                .get(index)
                .or_else(|| contract.validation_commands.first())
                .map(|command| command.command.clone()),
            confidence: Confidence::High,
            reason: test.reason.clone(),
            evidence_refs: evidence_ref_strings(&test.evidence_refs),
            score_breakdown: Vec::new(),
        })
        .collect()
}

fn policy_snapshot(
    contract: &ChangeContractV1,
    traceability_strict: bool,
) -> VerificationPolicySnapshot {
    VerificationPolicySnapshot {
        contract_version: contract.version.to_string(),
        traceability_strict,
        primary_files: contract_file_paths(&contract.primary_files),
        secondary_files: contract_file_paths(&contract.secondary_files),
        forbidden_files: contract_file_paths(&contract.forbidden_files),
        architecture_constraints: contract
            .architecture_constraints
            .iter()
            .map(|constraint| constraint.rule.clone())
            .collect(),
        expansion_requirements: contract
            .expansion_approval_requirements
            .iter()
            .map(|requirement| requirement.scope.clone())
            .collect(),
    }
}

fn contract_file_paths(files: &[open_kioku_contract::ContractFile]) -> Vec<PathBuf> {
    files
        .iter()
        .map(|file| PathBuf::from(file.as_str()))
        .collect()
}

fn evidence_ref_strings(refs: &[EvidenceRef]) -> Vec<String> {
    refs.iter().map(|reference| reference.0.clone()).collect()
}

fn contract_risk_report(contract: &ChangeContractV1) -> RiskReport {
    RiskReport {
        score: contract.risk.score as f32,
        level: match contract.risk.level {
            open_kioku_contract::RiskLevel::Low => "low",
            open_kioku_contract::RiskLevel::Medium => "medium",
            open_kioku_contract::RiskLevel::High => "high",
            open_kioku_contract::RiskLevel::Critical => "critical",
        }
        .into(),
        reasons: contract.risk.reasons.clone(),
    }
}

fn contract_confidence(contract: &ChangeContractV1) -> Confidence {
    match contract.confidence.level {
        open_kioku_contract::ConfidenceLevel::Low => Confidence::Low,
        open_kioku_contract::ConfidenceLevel::Medium => Confidence::Medium,
        open_kioku_contract::ConfidenceLevel::High => Confidence::High,
        open_kioku_contract::ConfidenceLevel::Exact => Confidence::Exact,
    }
}

pub fn changed_files_from_unified_diff(diff: &str) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    let mut pending_old: Option<String> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let parts = rest.split_whitespace().collect::<Vec<_>>();
            if let Some(path) = parts.get(1).and_then(|part| part.strip_prefix("b/")) {
                paths.insert(PathBuf::from(path));
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("--- ") {
            pending_old = diff_path(path);
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ ") {
            if let Some(path) = diff_path(path).or_else(|| pending_old.take()) {
                paths.insert(PathBuf::from(path));
            }
        }
    }
    paths.into_iter().collect()
}

fn changed_files_from_input(input: &VerifyChangeInput) -> Vec<PathBuf> {
    let mut paths = input
        .changed_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    if let Some(diff) = &input.unified_diff {
        paths.extend(
            changed_files_from_unified_diff(diff)
                .into_iter()
                .map(|p| normalize_path(&p)),
        );
    }
    paths.into_iter().map(PathBuf::from).collect()
}

fn diff_path(raw: &str) -> Option<String> {
    let path = raw.split_whitespace().next().unwrap_or_default();
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(path)
            .to_string(),
    )
}

fn boundary_violations(
    plan: &PlanReport,
    changed_files: &[PathBuf],
    evidence_refs: &[String],
) -> Vec<VerificationFinding> {
    let boundary = &plan.recommended_change_boundary;
    let allowed = boundary
        .allowed_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    let caution = boundary
        .caution_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    let forbidden = boundary
        .forbidden_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    let mut findings = Vec::new();
    for path in changed_files {
        let normalized = normalize_path(path);
        if forbidden.contains(&normalized) {
            findings.push(VerificationFinding {
                path: Some(path.clone()),
                kind: "forbidden_boundary".into(),
                reason: "matches forbidden contract file".into(),
                evidence_refs: boundary.evidence_refs.clone(),
            });
            continue;
        }
        if let Some(rule) = boundary
            .forbidden_rules
            .iter()
            .find(|rule| boundary_pattern_matches(&rule.pattern, &normalized))
        {
            findings.push(VerificationFinding {
                path: Some(path.clone()),
                kind: "forbidden_boundary".into(),
                reason: format!(
                    "matches forbidden pattern `{}`: {}",
                    rule.pattern, rule.reason
                ),
                evidence_refs: rule.evidence_refs.clone(),
            });
            continue;
        }
        if allowed.contains(&normalized) || caution.contains(&normalized) {
            continue;
        }
        if evidence_refs.is_empty() {
            findings.push(VerificationFinding {
                path: Some(path.clone()),
                kind: "out_of_boundary".into(),
                reason:
                    "path is outside the saved plan boundary and no expansion evidence was supplied"
                        .into(),
                evidence_refs: Vec::new(),
            });
        }
    }
    findings
}

fn caution_warnings(plan: &PlanReport, changed_files: &[PathBuf]) -> Vec<VerificationFinding> {
    let boundary = &plan.recommended_change_boundary;
    changed_files
        .iter()
        .filter_map(|path| {
            let normalized = normalize_path(path);
            boundary
                .caution_rules
                .iter()
                .find(|rule| normalize_path(&rule.path) == normalized)
                .map(|rule| VerificationFinding {
                    path: Some(path.clone()),
                    kind: "caution_boundary".into(),
                    reason: rule.reason.clone(),
                    evidence_refs: rule.evidence_refs.clone(),
                })
        })
        .collect()
}

fn expansion_warnings(
    plan: &PlanReport,
    changed_files: &[PathBuf],
    evidence_refs: &[String],
) -> Vec<VerificationFinding> {
    if evidence_refs.is_empty() {
        return Vec::new();
    }
    let boundary = &plan.recommended_change_boundary;
    let allowed = boundary
        .allowed_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    let caution = boundary
        .caution_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    changed_files
        .iter()
        .filter_map(|path| {
            let normalized = normalize_path(path);
            if allowed.contains(&normalized)
                || caution.contains(&normalized)
                || boundary
                    .forbidden_rules
                    .iter()
                    .any(|rule| boundary_pattern_matches(&rule.pattern, &normalized))
            {
                return None;
            }
            Some(VerificationFinding {
                path: Some(path.clone()),
                kind: "boundary_expansion".into(),
                reason: "path is outside the saved boundary but explicit expansion evidence was supplied".into(),
                evidence_refs: evidence_refs.to_vec(),
            })
        })
        .collect()
}

fn unknown_evidence_ref_violations(
    plan: &PlanReport,
    evidence_refs: &[String],
) -> Vec<VerificationFinding> {
    if evidence_refs.is_empty() {
        return Vec::new();
    }
    let known = known_plan_evidence_refs(plan);
    evidence_refs
        .iter()
        .filter(|evidence_ref| !known.contains(evidence_ref.as_str()))
        .map(|evidence_ref| VerificationFinding {
            path: None,
            kind: "unknown_evidence_ref".into(),
            reason: format!("evidence ref `{evidence_ref}` is not present in the saved plan"),
            evidence_refs: vec![evidence_ref.clone()],
        })
        .collect()
}

fn verification_traceability(
    plan: &PlanReport,
    input: &VerifyChangeInput,
) -> Vec<VerificationTrace> {
    let mut traces = vec![
        VerificationTrace {
            field: "changed_files".into(),
            rationale: "Changed files are normalized from explicit changed_files and unified diff verification input".into(),
            evidence_refs: Vec::new(),
        },
        VerificationTrace {
            field: "boundary_violations".into(),
            rationale: "Boundary findings are derived from the saved plan allowed, caution, forbidden, and expansion rules".into(),
            evidence_refs: boundary_plan_evidence_refs(plan),
        },
        VerificationTrace {
            field: "missing_tests".into(),
            rationale: "Missing-test findings compare post-edit recommendations with saved plan validation targets".into(),
            evidence_refs: validation_plan_evidence_refs(plan),
        },
        VerificationTrace {
            field: "changed_impact".into(),
            rationale: "Changed-impact findings compare post-edit impact candidates with saved plan impact and boundary evidence".into(),
            evidence_refs: impact_plan_evidence_refs(plan),
        },
    ];
    if !input.evidence_refs.is_empty() {
        traces.push(VerificationTrace {
            field: "boundary_expansion".into(),
            rationale: "Caller-supplied evidence references are used to justify boundary expansion and are checked in strict mode".into(),
            evidence_refs: input.evidence_refs.clone(),
        });
    }
    traces
}

fn boundary_plan_evidence_refs(plan: &PlanReport) -> Vec<String> {
    let mut refs = BTreeSet::new();
    push_evidence_refs(&mut refs, &plan.recommended_change_boundary.evidence_refs);
    for rule in &plan.recommended_change_boundary.allowed_rules {
        push_evidence_refs(&mut refs, &rule.evidence_refs);
    }
    for rule in &plan.recommended_change_boundary.caution_rules {
        push_evidence_refs(&mut refs, &rule.evidence_refs);
    }
    for rule in &plan.recommended_change_boundary.forbidden_rules {
        push_evidence_refs(&mut refs, &rule.evidence_refs);
    }
    for requirement in &plan.recommended_change_boundary.expansion_requirements {
        push_evidence_refs(&mut refs, &requirement.required_evidence_refs);
    }
    refs.into_iter().collect()
}

fn validation_plan_evidence_refs(plan: &PlanReport) -> Vec<String> {
    let mut refs = BTreeSet::new();
    for test in &plan.validation {
        push_evidence_refs(&mut refs, &test.evidence_refs);
    }
    refs.into_iter().collect()
}

fn impact_plan_evidence_refs(plan: &PlanReport) -> Vec<String> {
    let mut refs = BTreeSet::new();
    for evidence in &plan.impact.evidence {
        push_evidence_ref(&mut refs, &evidence.id.0);
    }
    if let Some(section_refs) = plan.evidence_by_section.get("impact") {
        push_evidence_refs(&mut refs, section_refs);
    }
    refs.into_iter().collect()
}

fn known_plan_evidence_refs(plan: &PlanReport) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    for evidence in &plan.evidence {
        push_evidence_ref(&mut refs, &evidence.id.0);
    }
    for refs_for_section in plan.evidence_by_section.values() {
        push_evidence_refs(&mut refs, refs_for_section);
    }
    for ctx in &plan.primary_context {
        for evidence_ref in ctx.derived_evidence_ids() {
            push_evidence_ref(&mut refs, &evidence_ref);
        }
    }
    for evidence in &plan.impact.evidence {
        push_evidence_ref(&mut refs, &evidence.id.0);
    }
    push_evidence_refs(&mut refs, &plan.recommended_change_boundary.evidence_refs);
    for rule in &plan.recommended_change_boundary.allowed_rules {
        push_evidence_refs(&mut refs, &rule.evidence_refs);
    }
    for rule in &plan.recommended_change_boundary.caution_rules {
        push_evidence_refs(&mut refs, &rule.evidence_refs);
    }
    for rule in &plan.recommended_change_boundary.forbidden_rules {
        push_evidence_refs(&mut refs, &rule.evidence_refs);
    }
    for requirement in &plan.recommended_change_boundary.expansion_requirements {
        push_evidence_refs(&mut refs, &requirement.required_evidence_refs);
    }
    for test in &plan.validation {
        push_evidence_refs(&mut refs, &test.evidence_refs);
    }
    refs
}

fn push_evidence_refs(refs: &mut BTreeSet<String>, values: &[String]) {
    for value in values {
        push_evidence_ref(refs, value);
    }
}

fn push_evidence_ref(refs: &mut BTreeSet<String>, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        refs.insert(value.to_string());
    }
}

fn changed_symbols(store: &dyn MetadataStore, changed_files: &[PathBuf]) -> Result<Vec<String>> {
    let mut symbols = BTreeSet::new();
    for path in changed_files {
        if let Some(file) = store.get_file_by_path(path)? {
            for symbol in store.symbols_for_file(&file.id)? {
                symbols.insert(symbol.qualified_name);
            }
        }
    }
    Ok(symbols.into_iter().collect())
}

fn recommended_tests(store: &dyn OkStore, changed_files: &[PathBuf]) -> Result<Vec<TestTarget>> {
    let selector = TestSelector::new(store);
    let mut tests = Vec::new();
    let mut seen = BTreeSet::new();
    for path in changed_files {
        for test in selector.for_changed_path_with_evidence(path, 8)? {
            if seen.insert(test.id.clone()) {
                tests.push(test);
            }
        }
    }
    Ok(tests)
}

fn missing_tests(plan: &PlanReport, recommended_tests: &[TestTarget]) -> Vec<VerificationFinding> {
    let planned = plan
        .validation
        .iter()
        .flat_map(|test| [test.id.clone(), test.name.clone()])
        .collect::<BTreeSet<_>>();
    recommended_tests
        .iter()
        .filter(|test| !planned.contains(&test.id) && !planned.contains(&test.name))
        .map(|test| VerificationFinding {
            path: Some(PathBuf::from(test.file_id.0.clone())),
            kind: "missing_test".into(),
            reason: format!("recommended test `{}` is not in the saved plan", test.name),
            evidence_refs: test.evidence_refs.clone(),
        })
        .collect()
}

fn changed_impact(
    store: &dyn OkStore,
    search_index: Option<&dyn SearchIndex>,
    plan: &PlanReport,
    changed_files: &[PathBuf],
) -> Result<Vec<VerificationFinding>> {
    let planned_impacts = plan
        .impact
        .direct_impacts
        .iter()
        .chain(plan.impact.indirect_impacts.iter())
        .map(|result| normalize_path(&result.path))
        .chain(
            plan.recommended_change_boundary
                .allowed_files
                .iter()
                .map(|path| normalize_path(path)),
        )
        .chain(
            plan.recommended_change_boundary
                .caution_files
                .iter()
                .map(|path| normalize_path(path)),
        )
        .collect::<BTreeSet<_>>();
    let impact_engine = ImpactEngine::new(store).with_search_index(search_index);
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    for path in changed_files {
        let impact = impact_engine.for_file(path)?;
        for result in impact
            .direct_impacts
            .iter()
            .chain(impact.indirect_impacts.iter())
            .take(12)
        {
            let normalized = normalize_path(&result.path);
            if !planned_impacts.contains(&normalized) && seen.insert(normalized.clone()) {
                findings.push(impact_finding(result));
            }
        }
    }
    Ok(findings)
}

fn runtime_warnings(
    store: &dyn MetadataStore,
    changed_files: &[PathBuf],
) -> Result<Vec<VerificationFinding>> {
    let runtime_facts = store.analysis_facts(Some(EvidenceSourceType::Runtime), 500)?;
    if runtime_facts.is_empty() {
        return Ok(Vec::new());
    }
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    for path in changed_files {
        let Some(file) = store.get_file_by_path(path)? else {
            continue;
        };
        for fact in runtime_facts
            .iter()
            .filter(|fact| fact.file_id == file.id)
            .take(5)
        {
            if seen.insert((normalize_path(path), fact.id.clone())) {
                findings.push(runtime_finding(path, fact));
            }
        }
    }
    Ok(findings)
}

fn runtime_finding(path: &Path, fact: &AnalysisFact) -> VerificationFinding {
    let requires_validation = runtime_fact_requires_validation(fact);
    VerificationFinding {
        path: Some(path.to_path_buf()),
        kind: if requires_validation {
            "runtime_validation_required"
        } else {
            "nearby_runtime_signal"
        }
        .into(),
        reason: if requires_validation {
            format!(
                "changed file has high-risk local runtime aggregate evidence `{}`; run targeted validation before accepting the change: {}",
                fact.target, fact.message
            )
        } else {
            format!(
                "changed file has local runtime trace/log/incident evidence `{}`: {}",
                fact.target, fact.message
            )
        },
        evidence_refs: vec![fact.id.clone()],
    }
}

fn runtime_fact_requires_validation(fact: &AnalysisFact) -> bool {
    if fact.source != "open-kioku-runtime:aggregate" {
        return false;
    }
    let Some(error_rate) = runtime_message_metric(&fact.message, "error_rate") else {
        return false;
    };
    let error_count = runtime_message_metric(&fact.message, "error_count").unwrap_or(0.0);
    error_count >= 1.0 && error_rate >= HIGH_RUNTIME_ERROR_RATE
}

fn runtime_message_metric(message: &str, name: &str) -> Option<f32> {
    let mut parts = message.split(|ch: char| ch.is_whitespace() || ch == ',');
    while let Some(part) = parts.next() {
        if part == name {
            return parts.next()?.parse::<f32>().ok();
        }
    }
    None
}

fn impact_finding(result: &SearchResult) -> VerificationFinding {
    VerificationFinding {
        path: Some(result.path.clone()),
        kind: "changed_impact".into(),
        reason: format!(
            "post-edit impact candidate was not present in the saved plan: {}",
            result.match_reason
        ),
        evidence_refs: result.derived_evidence_ids(),
    }
}

fn run_validation_commands(repo: &Path, plan: &PlanReport) -> Vec<ValidationCommandResult> {
    let mut seen = BTreeSet::new();
    let commands = plan
        .validation
        .iter()
        .filter_map(|test| test.command.clone())
        .filter(|command| seen.insert(command.clone()))
        .collect::<Vec<_>>();
    commands
        .into_iter()
        .map(|command| run_validation_command(repo, &command))
        .collect()
}

fn run_validation_command(repo: &Path, command: &str) -> ValidationCommandResult {
    let output = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .current_dir(repo)
        .output();
    match output {
        Ok(output) => ValidationCommandResult {
            command: command.into(),
            status: if output.status.success() {
                "pass".into()
            } else {
                "fail".into()
            },
            exit_code: output.status.code(),
            stdout: truncate_output(&String::from_utf8_lossy(&output.stdout)),
            stderr: truncate_output(&String::from_utf8_lossy(&output.stderr)),
        },
        Err(err) => ValidationCommandResult {
            command: command.into(),
            status: "fail".into(),
            exit_code: None,
            stdout: String::new(),
            stderr: truncate_output(&err.to_string()),
        },
    }
}

fn truncate_output(value: &str) -> String {
    const MAX: usize = 4000;
    if value.len() <= MAX {
        value.into()
    } else {
        format!("{}... <truncated>", &value[..MAX])
    }
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("./")
        .to_string()
}

fn boundary_pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_start_matches("./").replace('\\', "/");
    if pattern == path {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        if let Some(middle) = prefix.strip_prefix("**/") {
            return path == middle
                || path.starts_with(&format!("{middle}/"))
                || path.contains(&format!("/{middle}/"));
        }
        return path == prefix || path.starts_with(&format!("{prefix}/"));
    }
    if pattern.contains('*') {
        let mut remainder = path;
        for part in pattern.split('*').filter(|part| !part.is_empty()) {
            if let Some(index) = remainder.find(part) {
                remainder = &remainder[index + part.len()..];
            } else {
                return false;
            }
        }
        return true;
    }
    false
}

fn stable_id(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_contract::{ContractFile, ContractStore, FsContractStore};
    use open_kioku_core::{
        ChangeBoundary, CodeChunk, Confidence, ConfidenceBreakdown, File, FileId, GraphEdge,
        GraphEdgeType, GraphNode, GraphNodeType, Import, IndexManifest, Language, LineRange,
        RepositoryId, RiskReport, Symbol, SymbolId, SymbolOccurrence,
    };
    use open_kioku_errors::Result;
    use open_kioku_plan::ContractBuilder;
    use open_kioku_storage::{GraphStore, IndexData};
    use std::fs;

    struct RuntimeStore {
        file: File,
        fact: AnalysisFact,
    }

    impl RuntimeStore {
        fn new() -> Self {
            let file = File {
                id: FileId::new("handler"),
                repository_id: RepositoryId::new("repo"),
                path: PathBuf::from("src/handler.rs"),
                language: Language::Rust,
                size_bytes: 100,
                content_hash: "handler".into(),
                is_generated: false,
                is_vendor: false,
            };
            let fact = AnalysisFact {
                id: "runtime-incident".into(),
                file_id: file.id.clone(),
                symbol_id: None,
                target: "panic in checkout flow".into(),
                target_kind: GraphNodeType::RuntimeError,
                edge_type: GraphEdgeType::FailedIn,
                range: Some(LineRange::single(9)),
                confidence: Confidence::High,
                source: "open-kioku-runtime:.ok/runtime/incidents.jsonl".into(),
                source_type: EvidenceSourceType::Runtime,
                message: "runtime incident observed in local log or failure artifact".into(),
            };
            Self { file, fact }
        }

        fn with_fact(mut self, fact: AnalysisFact) -> Self {
            self.fact = fact;
            self
        }
    }

    impl MetadataStore for RuntimeStore {
        fn initialize(&self) -> Result<()> {
            Ok(())
        }

        fn put_manifest(&self, _manifest: &IndexManifest) -> Result<()> {
            Ok(())
        }

        fn manifest(&self) -> Result<Option<IndexManifest>> {
            Ok(None)
        }

        fn replace_index(&self, _data: IndexData<'_>) -> Result<()> {
            Ok(())
        }

        fn list_files(&self, _limit: usize, _offset: usize) -> Result<Vec<File>> {
            Ok(vec![self.file.clone()])
        }

        fn get_file_by_path(&self, path: &Path) -> Result<Option<File>> {
            Ok((path == self.file.path).then(|| self.file.clone()))
        }

        fn list_symbols(
            &self,
            _query: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<Symbol>> {
            Ok(Vec::new())
        }

        fn symbol_by_id(&self, _id: &SymbolId) -> Result<Option<Symbol>> {
            Ok(None)
        }

        fn chunks_for_file(&self, _file_id: &FileId) -> Result<Vec<CodeChunk>> {
            Ok(Vec::new())
        }

        fn all_chunks(&self) -> Result<Vec<CodeChunk>> {
            Ok(Vec::new())
        }

        fn tests(&self) -> Result<Vec<TestTarget>> {
            Ok(Vec::new())
        }

        fn imports(&self) -> Result<Vec<Import>> {
            Ok(Vec::new())
        }

        fn analysis_facts(
            &self,
            source_type: Option<EvidenceSourceType>,
            _limit: usize,
        ) -> Result<Vec<AnalysisFact>> {
            if source_type == Some(EvidenceSourceType::Runtime) {
                Ok(vec![self.fact.clone()])
            } else {
                Ok(Vec::new())
            }
        }

        fn references_for_symbol(
            &self,
            _id: &SymbolId,
            _limit: usize,
        ) -> Result<Vec<SymbolOccurrence>> {
            Ok(Vec::new())
        }

        fn occurrences_for_file(&self, _file_id: &FileId) -> Result<Vec<SymbolOccurrence>> {
            Ok(Vec::new())
        }
    }

    impl GraphStore for RuntimeStore {
        fn replace_graph(&self, _nodes: &[GraphNode], _edges: &[GraphEdge]) -> Result<()> {
            Ok(())
        }

        fn neighbors(
            &self,
            _node: &str,
            _limit: usize,
        ) -> Result<(Vec<GraphNode>, Vec<GraphEdge>)> {
            Ok((Vec::new(), Vec::new()))
        }

        fn shortest_path(
            &self,
            _from: &str,
            _to: &str,
            _max_depth: usize,
        ) -> Result<Vec<GraphEdge>> {
            Ok(Vec::new())
        }

        fn node_type_stats(
            &self,
        ) -> Result<std::collections::HashMap<String, open_kioku_storage::TypeStats>> {
            Ok(std::collections::HashMap::new())
        }

        fn edge_type_stats(
            &self,
        ) -> Result<std::collections::HashMap<String, open_kioku_storage::TypeStats>> {
            Ok(std::collections::HashMap::new())
        }
    }

    #[test]
    fn runtime_warnings_surface_nearby_incidents() {
        let store = RuntimeStore::new();
        let warnings = runtime_warnings(&store, &[PathBuf::from("src/handler.rs")]).unwrap();

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, "nearby_runtime_signal");
        assert!(warnings[0].reason.contains("panic in checkout flow"));
        assert_eq!(warnings[0].evidence_refs, vec!["runtime-incident"]);
    }

    #[test]
    fn runtime_aggregates_require_validation_when_error_rate_is_high() {
        let base = RuntimeStore::new();
        let aggregate = AnalysisFact {
            id: "runtime-aggregate".into(),
            file_id: base.file.id.clone(),
            symbol_id: None,
            target: "POST /checkout".into(),
            target_kind: GraphNodeType::Endpoint,
            edge_type: GraphEdgeType::ExposesEndpoint,
            range: None,
            confidence: Confidence::High,
            source: "open-kioku-runtime:aggregate".into(),
            source_type: EvidenceSourceType::Runtime,
            message: "runtime aggregate observed: count 10, error_count 3, error_rate 0.30, p95_ms 900.0, freshness recent".into(),
        };
        let store = RuntimeStore::new().with_fact(aggregate);
        let warnings = runtime_warnings(&store, &[PathBuf::from("src/handler.rs")]).unwrap();

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].kind, "runtime_validation_required");
        assert!(warnings[0].reason.contains("run targeted validation"));
        assert_eq!(warnings[0].evidence_refs, vec!["runtime-aggregate"]);
    }

    #[test]
    fn strict_traceability_rejects_unknown_expansion_evidence_refs() {
        let store = RuntimeStore::new();
        let plan = plan_with_boundary_evidence();
        let report = ChangeVerifier::new(&store)
            .verify(
                Path::new("."),
                &plan,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/outside.rs")],
                    evidence_refs: vec!["tampered:evidence".into()],
                    traceability_strict: true,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.verdict, VerificationVerdict::Fail);
        assert!(report
            .boundary_violations
            .iter()
            .any(|finding| finding.kind == "unknown_evidence_ref"));
    }

    #[test]
    fn strict_traceability_accepts_known_expansion_evidence_refs() {
        let store = RuntimeStore::new();
        let plan = plan_with_boundary_evidence();
        let report = ChangeVerifier::new(&store)
            .verify(
                Path::new("."),
                &plan,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/outside.rs")],
                    evidence_refs: vec!["boundary:allow".into()],
                    traceability_strict: true,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.verdict, VerificationVerdict::Warn);
        assert!(report.boundary_violations.is_empty());
        assert!(report
            .warnings
            .iter()
            .any(|finding| finding.kind == "boundary_expansion"));
        assert!(report.traceability.iter().any(|trace| {
            trace.field == "boundary_expansion"
                && trace.evidence_refs == vec!["boundary:allow".to_string()]
        }));
    }

    #[test]
    fn plan_verification_uses_contract_adapter_without_changing_verdict() {
        let store = RuntimeStore::new();
        let plan = plan_with_boundary_evidence();
        let input = VerifyChangeInput {
            changed_files: vec![PathBuf::from("src/outside.rs")],
            evidence_refs: vec!["boundary:allow".into()],
            traceability_strict: true,
            ..Default::default()
        };
        let verifier = ChangeVerifier::new(&store);

        let direct = verifier
            .verify_plan_direct(Path::new("."), &plan, input.clone())
            .unwrap();
        let adapted = verifier.verify(Path::new("."), &plan, input).unwrap();

        assert_eq!(adapted.verdict, direct.verdict);
        assert_eq!(
            adapted.boundary_violations.len(),
            direct.boundary_violations.len()
        );
        assert_eq!(adapted.warnings.len(), direct.warnings.len());
    }

    #[test]
    fn contract_verifier_reports_contract_id_and_policy_snapshot() {
        let store = RuntimeStore::new();
        let plan = plan_with_boundary_evidence();
        let contract = ContractBuilder::from_plan(&plan).unwrap();

        let report = ContractVerifier::new(&store)
            .verify(
                Path::new("."),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    traceability_strict: true,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.contract_id, contract.id.0);
        assert_eq!(report.decision, VerificationDecision::Warn);
        assert_eq!(
            report.policy_snapshot.primary_files,
            vec![PathBuf::from("src/handler.rs")]
        );
        assert!(report
            .change_report
            .warnings
            .iter()
            .any(|finding| finding.kind == "nearby_runtime_signal"));
    }

    #[test]
    fn contract_verifier_fails_exact_forbidden_contract_files() {
        let store = RuntimeStore::new();
        let plan = plan_with_boundary_evidence();
        let mut contract = ContractBuilder::from_plan(&plan).unwrap();
        contract
            .forbidden_files
            .push(ContractFile::new("src/forbidden.rs"));

        let report = ContractVerifier::new(&store)
            .verify(
                Path::new("."),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/forbidden.rs")],
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.decision, VerificationDecision::Fail);
        assert!(report
            .change_report
            .boundary_violations
            .iter()
            .any(|finding| finding.kind == "forbidden_boundary"));
    }

    #[test]
    fn contract_verifier_persists_verification_records_when_store_is_present() {
        let store = RuntimeStore::new();
        let plan = plan_with_boundary_evidence();
        let contract = ContractBuilder::from_plan(&plan).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let contract_store = FsContractStore::new(dir.path());
        contract_store.save(&contract).unwrap();

        let report = ContractVerifier::new(&store)
            .with_contract_store(Some(&contract_store))
            .verify(
                Path::new("."),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    ..Default::default()
                },
            )
            .unwrap();

        let verification_path = dir.path().join(format!("{}.verify.jsonl", contract.id.0));
        let jsonl = fs::read_to_string(verification_path).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        assert_eq!(record["success"], true);
        assert_eq!(record["report"]["contract_id"], report.contract_id);
    }

    fn plan_with_boundary_evidence() -> PlanReport {
        PlanReport {
            task: "change handler".into(),
            summary: "summary".into(),
            primary_context: vec![],
            relevant_symbols: vec![],
            impact: open_kioku_core::ImpactReport {
                target: "target".into(),
                direct_impacts: vec![],
                indirect_impacts: vec![],
                risk_report: RiskReport {
                    score: 0.1,
                    level: "low".into(),
                    reasons: vec!["low impact".into()],
                },
                evidence: vec![],
                score_breakdown: vec![],
            },
            validation: vec![],
            risk: RiskReport {
                score: 0.1,
                level: "low".into(),
                reasons: vec!["low risk".into()],
            },
            recommended_change_boundary: ChangeBoundary {
                allowed_files: vec![PathBuf::from("src/handler.rs")],
                evidence_refs: vec!["boundary:allow".into()],
                ..Default::default()
            },
            recommended_next_steps: vec![],
            tool_calls: vec![],
            memory_facts: vec![],
            runtime_signals: vec![],
            evidence: vec![],
            evidence_by_section: Default::default(),
            negative_evidence: vec![],
            confidence_summary: "medium confidence".into(),
            confidence_breakdown: ConfidenceBreakdown {
                overall_enum: Confidence::Medium,
                overall_score: 0.6,
                components: vec![],
                blockers: vec![],
                caveats: vec!["runtime corroboration is absent".into()],
            },
            score_breakdown: vec![],
        }
    }
}
