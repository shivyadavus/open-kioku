use open_kioku_actions::{ActionKind, PolicyGate};
use open_kioku_architecture::PolicyResolver;
use open_kioku_config::{ArchitecturePolicy, DependencyAction, OkConfig};
use open_kioku_context::ContextPackBuilder;
use open_kioku_contract::{
    validate_traceability, ApiSurfaceChangeKind, AttestedCommandResult, ChangeContractV1,
    CommandAllowlistStatus, ConstraintSeverity, ContractFile, ContractStore,
    ContractVerificationRecord, DependencyDeltaAction, DependencyDeltaClassification,
    DependencyDeltaFinding, EvidenceRef, PublicApiFingerprint, StoreError, ValidationAttestation,
    ValidationAttestationSummary, ValidationLedger, ValidationOutcome, ValidationRequirement,
};
use open_kioku_core::{
    AnalysisFact, BoundaryExpansionRequirement, BoundaryForbiddenRule, ChangeBoundary, Confidence,
    ConfidenceBreakdown, EvidenceQuality, EvidenceSourceType, FileId, GraphEdge, GraphEdgeType,
    GraphNode, ImpactReport, PatchId, PatchPlan, PlanReport, RiskReport, SearchResult, SymbolKind,
    TestTarget,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_impact::ImpactEngine;
use open_kioku_plan::ContractBuilder;
use open_kioku_storage::{MetadataStore, OkStore, SearchIndex};
use open_kioku_tests::TestSelector;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
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
    contract_store: Option<&'a dyn ContractStore>,
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
    pub write_attestation: bool,
    #[serde(default)]
    pub validation_attestations: Vec<ValidationAttestation>,
    #[serde(default)]
    pub traceability_strict: bool,
    #[serde(default)]
    pub check_api_surface: bool,
    #[serde(default)]
    pub check_dependency_delta: bool,
    #[serde(skip)]
    pub architecture_policy: Option<ArchitecturePolicy>,
    #[serde(skip)]
    pub suppress_plan_validation_pending: bool,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub api_surface_deltas: Vec<VerificationFinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependency_deltas: Vec<DependencyDeltaFinding>,
    pub recommended_tests: Vec<TestTarget>,
    pub command_results: Vec<ValidationCommandResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_attestations: Vec<ValidationAttestation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_ledger_path: Option<PathBuf>,
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub evidence_quality: EvidenceQuality,
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
    #[serde(default)]
    pub evidence_quality: EvidenceQuality,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractVerificationReport {
    pub contract_id: String,
    pub decision: VerificationDecision,
    pub policy_snapshot: VerificationPolicySnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_surface: Option<ApiSurfaceDeltaReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_delta: Option<DependencyDeltaReport>,
    pub change_report: ChangeVerificationReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiSurfaceDeltaReport {
    pub before: Vec<PublicApiFingerprint>,
    pub after: Vec<PublicApiFingerprint>,
    pub findings: Vec<VerificationFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyDeltaReport {
    pub findings: Vec<DependencyDeltaFinding>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_run_id: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

impl<'a> ChangeVerifier<'a> {
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
        plan: &PlanReport,
        input: VerifyChangeInput,
    ) -> Result<ChangeVerificationReport> {
        if let Ok(contract) = ContractBuilder::from_plan(plan) {
            return ContractVerifier::new(self.store)
                .with_search_index(self.search_index)
                .with_contract_store(self.contract_store)
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
        let evidence_quality = plan.evidence_quality.clone();
        boundary_violations.extend(evidence_quality_failures(
            &evidence_quality,
            input.traceability_strict,
        ));
        let changed_symbols = changed_symbols(self.store, &changed_files)?;
        let recommended_tests = recommended_tests(self.store, &changed_files)?;
        let missing_tests = missing_tests(plan, &recommended_tests);
        let changed_impact = changed_impact(self.store, self.search_index, plan, &changed_files)?;
        let command_results = if input.run_commands {
            run_validation_commands(repo, plan)?
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
        warnings.extend(evidence_quality_warnings(
            &evidence_quality,
            input.traceability_strict,
        ));
        warnings.extend(plan_caveat_warnings(plan));
        warnings.extend(pending_plan_validation_warnings(plan, &input));
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
            api_surface_deltas: Vec::new(),
            dependency_deltas: Vec::new(),
            recommended_tests,
            command_results,
            validation_attestations: Vec::new(),
            validation_ledger_path: None,
            evidence_refs: input.evidence_refs,
            evidence_quality,
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
        let check_api_surface = input.check_api_surface;
        let check_dependency_delta = input.check_dependency_delta;
        let delta_input = input.clone();
        let validation_input = input.clone();
        let mut base_input = input;
        base_input.run_commands = false;
        base_input.write_attestation = false;
        base_input.validation_attestations.clear();
        base_input.suppress_plan_validation_pending = true;
        let mut change_report = ChangeVerifier {
            store: self.store,
            search_index: self.search_index,
            contract_store: None,
        }
        .verify_plan_direct(repo, plan, base_input)?;
        let validation_report =
            verify_contract_validation(repo, contract, &validation_input, self.contract_store)?;
        apply_validation_report(&mut change_report, validation_report);
        let api_surface = if check_api_surface {
            Some(diff_public_api_surface(
                self.store,
                repo,
                contract,
                &change_report.changed_files,
            )?)
        } else {
            None
        };
        let dependency_delta = if check_dependency_delta {
            Some(diff_dependencies(self.store, repo, contract, &delta_input)?)
        } else {
            None
        };
        apply_delta_reports(
            &mut change_report,
            api_surface.as_ref(),
            dependency_delta.as_ref(),
        );
        let decision = VerificationDecision::from(change_report.verdict);
        let report = ContractVerificationReport {
            contract_id: contract.id.0.clone(),
            decision,
            policy_snapshot: policy_snapshot(contract, traceability_strict),
            api_surface,
            dependency_delta,
            change_report,
        };
        if let Some(store) = self.contract_store {
            let report_value = serde_json::to_value(&report).map_err(OkError::Json)?;
            let record = ContractVerificationRecord {
                verified_at: chrono::Utc::now(),
                success: decision != VerificationDecision::Fail,
                stdout: serde_json::to_string(&report).map_err(OkError::Json)?,
                stderr: String::new(),
                validation_attestations: report
                    .change_report
                    .validation_attestations
                    .iter()
                    .map(|attestation| ValidationAttestationSummary {
                        id: attestation.id.clone(),
                        contract_id: attestation.contract_id.clone(),
                        verification_run_id: attestation.verification_run_id.clone(),
                        command: attestation.result.command.clone(),
                        outcome: attestation.result.outcome,
                        ledger_path: report
                            .change_report
                            .validation_ledger_path
                            .as_ref()
                            .map(|path| path.to_string_lossy().replace('\\', "/")),
                        created_at: attestation.created_at,
                    })
                    .collect(),
                report: Some(report_value),
            };
            match store.append_verification(&contract.id, &record) {
                Ok(()) => {}
                Err(StoreError::NotFound(_)) => {
                    store
                        .save(contract)
                        .map_err(|err| OkError::Storage(err.to_string()))?;
                    store
                        .append_verification(&contract.id, &record)
                        .map_err(|err| OkError::Storage(err.to_string()))?;
                }
                Err(err) => return Err(OkError::Storage(err.to_string())),
            }
        }
        Ok(report)
    }
}

struct ValidationRunReport {
    command_results: Vec<ValidationCommandResult>,
    attestations: Vec<ValidationAttestation>,
    ledger_path: Option<PathBuf>,
    findings: Vec<VerificationFinding>,
    warnings: Vec<VerificationFinding>,
}

fn verify_contract_validation(
    repo: &Path,
    contract: &ChangeContractV1,
    input: &VerifyChangeInput,
    contract_store: Option<&dyn ContractStore>,
) -> Result<ValidationRunReport> {
    let requirements = validation_requirements_for_contract(contract);
    if requirements.is_empty() {
        return Ok(ValidationRunReport {
            command_results: Vec::new(),
            attestations: Vec::new(),
            ledger_path: None,
            findings: Vec::new(),
            warnings: Vec::new(),
        });
    }

    let contract_digest = digest_json(contract)?;
    let run_id = validation_run_id(contract, &requirements);
    let (attestations, command_results) = if input.run_commands {
        let config = OkConfig::load_from_repo(repo)?;
        run_attested_validation_commands(
            repo,
            contract,
            &requirements,
            &contract_digest,
            &run_id,
            &config,
        )
    } else {
        (
            input.validation_attestations.clone(),
            input
                .validation_attestations
                .iter()
                .map(validation_command_result_from_attestation)
                .collect(),
        )
    };

    let mut warnings = Vec::new();
    if !input.run_commands && attestations.is_empty() {
        for requirement in &requirements {
            warnings.push(VerificationFinding {
                path: None,
                kind: "validation_attestation_pending".into(),
                reason: format!(
                    "required validation command `{}` has not been run and no attestation was supplied",
                    requirement.command
                ),
                evidence_refs: evidence_ref_strings(&requirement.evidence_refs),
            });
        }
    }

    let mut findings =
        validate_attestations(contract, &requirements, &contract_digest, &attestations);
    let mut ledger_path = None;
    if input.write_attestation {
        if attestations.is_empty() {
            findings.push(VerificationFinding {
                path: None,
                kind: "validation_attestation_missing".into(),
                reason: "write_attestation was requested but no validation attestation records were available".into(),
                evidence_refs: Vec::new(),
            });
        } else if let Some(store) = contract_store {
            let ledger = ValidationLedger {
                run_id: run_id.clone(),
                contract_id: contract.id.clone(),
                contract_digest: contract_digest.clone(),
                generated_at: chrono::Utc::now(),
                attestations: attestations.clone(),
            };
            ledger_path = Some(
                store
                    .save_validation_ledger(&ledger)
                    .map_err(|err| OkError::Storage(err.to_string()))?,
            );
        } else {
            findings.push(VerificationFinding {
                path: None,
                kind: "validation_ledger_store_missing".into(),
                reason: "write_attestation requires a contract store so the validation ledger can be persisted".into(),
                evidence_refs: Vec::new(),
            });
        }
    }

    Ok(ValidationRunReport {
        command_results,
        attestations,
        ledger_path,
        findings,
        warnings,
    })
}

fn apply_validation_report(report: &mut ChangeVerificationReport, validation: ValidationRunReport) {
    report.command_results = validation.command_results;
    report.validation_attestations = validation.attestations;
    report.validation_ledger_path = validation.ledger_path;
    report.boundary_violations.extend(validation.findings);
    report.warnings.extend(validation.warnings);
    refresh_verdict(report);
}

fn validation_requirements_for_contract(contract: &ChangeContractV1) -> Vec<ValidationRequirement> {
    if !contract.validation_requirements.is_empty() {
        return contract.validation_requirements.clone();
    }
    contract
        .validation_commands
        .iter()
        .map(|command| ValidationRequirement {
            command: command.command.clone(),
            cwd: None,
            reason: command.reason.clone(),
            evidence_refs: Vec::new(),
        })
        .collect()
}

fn run_attested_validation_commands(
    repo: &Path,
    contract: &ChangeContractV1,
    requirements: &[ValidationRequirement],
    contract_digest: &str,
    run_id: &str,
    config: &OkConfig,
) -> (Vec<ValidationAttestation>, Vec<ValidationCommandResult>) {
    let attestations = requirements
        .iter()
        .map(|requirement| {
            run_attested_validation_command(
                repo,
                contract,
                requirement,
                contract_digest,
                run_id,
                config,
            )
        })
        .collect::<Vec<_>>();
    let command_results = attestations
        .iter()
        .map(validation_command_result_from_attestation)
        .collect();
    (attestations, command_results)
}

fn run_attested_validation_command(
    repo: &Path,
    contract: &ChangeContractV1,
    requirement: &ValidationRequirement,
    contract_digest: &str,
    run_id: &str,
    config: &OkConfig,
) -> ValidationAttestation {
    let started_at = chrono::Utc::now();
    let requirement_digest =
        digest_json(requirement).unwrap_or_else(|err| stable_id(&format!("{requirement:?}:{err}")));
    let cwd = requirement
        .cwd
        .as_ref()
        .map(|cwd| cwd.as_str().to_string())
        .unwrap_or_else(|| ".".into());
    let mut result = AttestedCommandResult {
        command: requirement.command.clone(),
        cwd: cwd.clone(),
        started_at,
        finished_at: started_at,
        exit_code: None,
        allowlist_status: CommandAllowlistStatus::Allowed,
        outcome: ValidationOutcome::Error,
        stdout_summary: String::new(),
        stderr_summary: String::new(),
    };

    if let Err(err) = PolicyGate::new(config).ensure_command_allowed(&requirement.command) {
        result.allowlist_status = CommandAllowlistStatus::Denied;
        result.outcome = ValidationOutcome::Denied;
        result.stderr_summary = truncate_output(&err.to_string());
        result.finished_at = chrono::Utc::now();
        return attestation_from_result(
            contract,
            contract_digest,
            run_id,
            &requirement_digest,
            result,
        );
    }

    let current_dir = requirement
        .cwd
        .as_ref()
        .map(|cwd| repo.join(cwd.as_path()))
        .unwrap_or_else(|| repo.to_path_buf());
    let output = Command::new("sh")
        .arg("-lc")
        .arg(&requirement.command)
        .current_dir(current_dir)
        .output();
    match output {
        Ok(output) => {
            result.exit_code = output.status.code();
            result.outcome = if output.status.success() {
                ValidationOutcome::Passed
            } else {
                ValidationOutcome::Failed
            };
            result.stdout_summary = truncate_output(&String::from_utf8_lossy(&output.stdout));
            result.stderr_summary = truncate_output(&String::from_utf8_lossy(&output.stderr));
        }
        Err(err) => {
            result.outcome = ValidationOutcome::Error;
            result.stderr_summary = truncate_output(&err.to_string());
        }
    }
    result.finished_at = chrono::Utc::now();
    attestation_from_result(
        contract,
        contract_digest,
        run_id,
        &requirement_digest,
        result,
    )
}

fn attestation_from_result(
    contract: &ChangeContractV1,
    contract_digest: &str,
    run_id: &str,
    requirement_digest: &str,
    result: AttestedCommandResult,
) -> ValidationAttestation {
    let id = stable_id(&format!(
        "{}:{}:{}:{}",
        contract.id, run_id, requirement_digest, result.started_at
    ));
    ValidationAttestation {
        id,
        contract_id: contract.id.clone(),
        verification_run_id: run_id.into(),
        contract_digest: contract_digest.into(),
        requirement_digest: requirement_digest.into(),
        created_at: chrono::Utc::now(),
        result,
    }
}

fn validate_attestations(
    contract: &ChangeContractV1,
    requirements: &[ValidationRequirement],
    contract_digest: &str,
    attestations: &[ValidationAttestation],
) -> Vec<VerificationFinding> {
    let mut findings = Vec::new();
    if attestations.is_empty() {
        return findings;
    }

    let mut attestations_by_requirement = BTreeMap::new();
    for attestation in attestations {
        attestations_by_requirement.insert(attestation.requirement_digest.as_str(), attestation);
        if attestation.contract_id != contract.id {
            findings.push(validation_finding(
                "validation_attestation_contract_mismatch",
                format!(
                    "attestation `{}` references contract `{}` but verification is for `{}`",
                    attestation.id, attestation.contract_id, contract.id
                ),
            ));
        }
        if attestation.contract_digest != contract_digest {
            findings.push(validation_finding(
                "validation_attestation_contract_mismatch",
                format!(
                    "attestation `{}` contract digest does not match the current contract",
                    attestation.id
                ),
            ));
        }
        if attestation.created_at < contract.timestamps.updated_at {
            findings.push(validation_finding(
                "validation_attestation_stale",
                format!(
                    "attestation `{}` was created before the contract was last updated",
                    attestation.id
                ),
            ));
        }
        match attestation.result.outcome {
            ValidationOutcome::Passed => {}
            ValidationOutcome::Failed => findings.push(validation_finding(
                "validation_command_failed",
                format!(
                    "validation command `{}` exited with {:?}",
                    attestation.result.command, attestation.result.exit_code
                ),
            )),
            ValidationOutcome::Denied => findings.push(validation_finding(
                "validation_command_denied",
                format!(
                    "validation command `{}` was denied by the command allowlist",
                    attestation.result.command
                ),
            )),
            ValidationOutcome::Error => findings.push(validation_finding(
                "validation_command_error",
                format!(
                    "validation command `{}` could not be executed: {}",
                    attestation.result.command, attestation.result.stderr_summary
                ),
            )),
        }
    }

    for requirement in requirements {
        let requirement_digest = match digest_json(requirement) {
            Ok(digest) => digest,
            Err(err) => {
                findings.push(validation_finding(
                    "validation_requirement_digest_error",
                    format!(
                        "could not digest validation requirement `{}`: {err}",
                        requirement.command
                    ),
                ));
                continue;
            }
        };
        let Some(attestation) = attestations_by_requirement.get(requirement_digest.as_str()) else {
            findings.push(validation_finding(
                "validation_attestation_missing",
                format!(
                    "required validation command `{}` does not have a matching attestation",
                    requirement.command
                ),
            ));
            continue;
        };
        let cwd = requirement
            .cwd
            .as_ref()
            .map(|cwd| cwd.as_str().to_string())
            .unwrap_or_else(|| ".".into());
        if attestation.result.command != requirement.command || attestation.result.cwd != cwd {
            findings.push(validation_finding(
                "validation_command_replay_mismatch",
                format!(
                    "attestation `{}` does not match required command `{}` in cwd `{}`",
                    attestation.id, requirement.command, cwd
                ),
            ));
        }
    }

    findings
}

fn validation_command_result_from_attestation(
    attestation: &ValidationAttestation,
) -> ValidationCommandResult {
    ValidationCommandResult {
        command: attestation.result.command.clone(),
        status: validation_status(attestation.result.outcome).into(),
        exit_code: attestation.result.exit_code,
        attestation_id: Some(attestation.id.clone()),
        verification_run_id: Some(attestation.verification_run_id.clone()),
        stdout: attestation.result.stdout_summary.clone(),
        stderr: attestation.result.stderr_summary.clone(),
    }
}

fn validation_status(outcome: ValidationOutcome) -> &'static str {
    match outcome {
        ValidationOutcome::Passed => "pass",
        ValidationOutcome::Failed | ValidationOutcome::Denied | ValidationOutcome::Error => "fail",
    }
}

fn validation_finding(kind: impl Into<String>, reason: impl Into<String>) -> VerificationFinding {
    VerificationFinding {
        path: None,
        kind: kind.into(),
        reason: reason.into(),
        evidence_refs: Vec::new(),
    }
}

fn validation_run_id(
    contract: &ChangeContractV1,
    requirements: &[ValidationRequirement],
) -> String {
    stable_id(&format!(
        "{}:{}:{:?}",
        contract.id,
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        requirements
    ))
}

fn digest_json<T: Serialize>(value: &T) -> Result<String> {
    let json = serde_json::to_vec(value).map_err(OkError::Json)?;
    let mut hasher = Sha256::new();
    hasher.update(&json);
    Ok(format!("{:x}", hasher.finalize()))
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
            architecture_policy: None,
            score_breakdown: Vec::new(),
        },
        validation: contract_validation_targets(contract),
        risk: contract_risk_report(contract),
        recommended_change_boundary: contract_change_boundary(contract),
        recommended_next_steps: Vec::new(),
        tool_calls: Vec::new(),
        memory_facts: Vec::new(),
        runtime_signals: Vec::new(),
        architecture_policy: None,
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
        evidence_quality: contract_evidence_quality(contract),
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
        evidence_quality: contract_evidence_quality(contract),
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

fn contract_evidence_quality(contract: &ChangeContractV1) -> EvidenceQuality {
    contract
        .extensions
        .get("evidence_quality")
        .cloned()
        .and_then(|value| serde_json::from_value::<EvidenceQuality>(value).ok())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicApiFingerprintSource {
    Indexed,
    WorkingTree,
}

pub fn fingerprint_public_api(
    store: &dyn MetadataStore,
    repo: &Path,
    changed_files: &[PathBuf],
    source: PublicApiFingerprintSource,
) -> Result<Vec<PublicApiFingerprint>> {
    let mut fingerprints = Vec::new();
    let mut seen = BTreeSet::new();
    for path in changed_files {
        let normalized = PathBuf::from(normalize_path(path));
        let extracted = match source {
            PublicApiFingerprintSource::Indexed => {
                if let Some(text) = indexed_file_text(store, &normalized)? {
                    public_api_fingerprints_from_text(&normalized, &text)
                } else {
                    indexed_symbol_fingerprints(store, &normalized)?
                }
            }
            PublicApiFingerprintSource::WorkingTree => {
                let path_on_disk = repo.join(&normalized);
                if path_on_disk.exists() {
                    public_api_fingerprints_from_text(
                        &normalized,
                        &fs::read_to_string(path_on_disk)?,
                    )
                } else {
                    Vec::new()
                }
            }
        };
        for fingerprint in extracted {
            let key = (
                fingerprint.path.0.clone(),
                fingerprint.kind.clone(),
                fingerprint.symbol.clone(),
            );
            if seen.insert(key) {
                fingerprints.push(fingerprint);
            }
        }
    }
    fingerprints.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.symbol.cmp(&right.symbol))
    });
    Ok(fingerprints)
}

fn diff_public_api_surface(
    store: &dyn MetadataStore,
    repo: &Path,
    contract: &ChangeContractV1,
    changed_files: &[PathBuf],
) -> Result<ApiSurfaceDeltaReport> {
    let before = fingerprint_public_api(
        store,
        repo,
        changed_files,
        PublicApiFingerprintSource::Indexed,
    )?;
    let after = fingerprint_public_api(
        store,
        repo,
        changed_files,
        PublicApiFingerprintSource::WorkingTree,
    )?;
    let before_by_key = before
        .iter()
        .map(|fingerprint| (api_fingerprint_key(fingerprint), fingerprint))
        .collect::<BTreeMap<_, _>>();
    let after_by_key = after
        .iter()
        .map(|fingerprint| (api_fingerprint_key(fingerprint), fingerprint))
        .collect::<BTreeMap<_, _>>();

    let mut findings = Vec::new();
    for (key, before_fingerprint) in &before_by_key {
        match after_by_key.get(key) {
            None => findings.push(api_surface_delta_finding(
                contract,
                ApiSurfaceChangeKind::Removed,
                Some(before_fingerprint),
                None,
            )),
            Some(after_fingerprint) if before_fingerprint.digest != after_fingerprint.digest => {
                findings.push(api_surface_delta_finding(
                    contract,
                    ApiSurfaceChangeKind::SignatureChanged,
                    Some(before_fingerprint),
                    Some(after_fingerprint),
                ));
            }
            Some(_) => {}
        }
    }
    for (key, after_fingerprint) in &after_by_key {
        if !before_by_key.contains_key(key) {
            findings.push(api_surface_delta_finding(
                contract,
                ApiSurfaceChangeKind::Added,
                None,
                Some(after_fingerprint),
            ));
        }
    }
    if findings.is_empty() {
        findings.push(VerificationFinding {
            path: None,
            kind: "api_surface_no_relevant_delta".into(),
            reason: "no public API additions, removals, or signature changes were detected".into(),
            evidence_refs: Vec::new(),
        });
    }
    Ok(ApiSurfaceDeltaReport {
        before,
        after,
        findings,
    })
}

pub fn diff_dependencies(
    store: &dyn OkStore,
    repo: &Path,
    contract: &ChangeContractV1,
    input: &VerifyChangeInput,
) -> Result<DependencyDeltaReport> {
    let changed_files = changed_files_from_input(input);
    let before = dependency_edges_from_index(store, &changed_files)?;
    let after = dependency_edges_from_worktree(repo, &changed_files)?;
    let before_by_key = before
        .iter()
        .map(|edge| (edge.key.clone(), edge.clone()))
        .collect::<BTreeMap<_, _>>();
    let after_by_key = after
        .iter()
        .map(|edge| (edge.key.clone(), edge.clone()))
        .collect::<BTreeMap<_, _>>();

    let mut findings = Vec::new();
    for (key, edge) in &after_by_key {
        if !before_by_key.contains_key(key) {
            findings.push(classify_dependency_delta(
                contract,
                input.architecture_policy.as_ref(),
                edge,
                DependencyEdgeChange::Added,
            )?);
        }
    }
    for (key, edge) in &before_by_key {
        if !after_by_key.contains_key(key) {
            findings.push(classify_dependency_delta(
                contract,
                input.architecture_policy.as_ref(),
                edge,
                DependencyEdgeChange::Removed,
            )?);
        }
    }
    if findings.is_empty() {
        findings.push(DependencyDeltaFinding {
            classification: DependencyDeltaClassification::NoRelevantDelta,
            edge_type: "imports/references/calls".into(),
            source: "changed files".into(),
            target: "indexed dependency graph".into(),
            source_path: None,
            target_path: None,
            reason: "no dependency graph delta was detected for the changed files".into(),
            evidence_refs: Vec::new(),
            rule_refs: Vec::new(),
        });
    }
    findings.sort_by(|left, right| {
        left.classification
            .to_string_key()
            .cmp(right.classification.to_string_key())
            .then_with(|| left.source.cmp(&right.source))
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| left.edge_type.cmp(&right.edge_type))
    });
    Ok(DependencyDeltaReport { findings })
}

fn apply_delta_reports(
    report: &mut ChangeVerificationReport,
    api_surface: Option<&ApiSurfaceDeltaReport>,
    dependency_delta: Option<&DependencyDeltaReport>,
) {
    if let Some(api_surface) = api_surface {
        for finding in &api_surface.findings {
            report.api_surface_deltas.push(finding.clone());
            match finding.kind.as_str() {
                "api_surface_violation" => report.boundary_violations.push(finding.clone()),
                "api_surface_review_required" => report.warnings.push(finding.clone()),
                _ => {}
            }
        }
    }
    if let Some(dependency_delta) = dependency_delta {
        for finding in &dependency_delta.findings {
            report.dependency_deltas.push(finding.clone());
            if finding.classification == DependencyDeltaClassification::ViolatingDelta {
                report
                    .boundary_violations
                    .push(dependency_delta_verification_finding(finding));
            }
        }
    }
    refresh_verdict(report);
}

fn refresh_verdict(report: &mut ChangeVerificationReport) {
    report.verdict = if !report.boundary_violations.is_empty() {
        VerificationVerdict::Fail
    } else if !report.warnings.is_empty()
        || !report.missing_tests.is_empty()
        || !report.changed_impact.is_empty()
    {
        VerificationVerdict::Warn
    } else {
        VerificationVerdict::Pass
    };
}

fn indexed_file_text(store: &dyn MetadataStore, path: &Path) -> Result<Option<String>> {
    let Some(file) = store.get_file_by_path(path)? else {
        return Ok(None);
    };
    let mut chunks = store.chunks_for_file(&file.id)?;
    if chunks.is_empty() {
        return Ok(None);
    }
    chunks.sort_by(|left, right| {
        left.range
            .start
            .cmp(&right.range.start)
            .then_with(|| left.range.end.cmp(&right.range.end))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(Some(
        chunks
            .into_iter()
            .map(|chunk| chunk.text)
            .collect::<Vec<_>>()
            .join("\n"),
    ))
}

fn indexed_symbol_fingerprints(
    store: &dyn MetadataStore,
    path: &Path,
) -> Result<Vec<PublicApiFingerprint>> {
    let Some(file) = store.get_file_by_path(path)? else {
        return Ok(Vec::new());
    };
    let mut fingerprints = Vec::new();
    for symbol in store.symbols_for_file(&file.id)? {
        if !public_symbol_kind(&symbol.kind) || symbol.name.starts_with('_') {
            continue;
        }
        let kind = format!("{:?}", symbol.kind).to_ascii_lowercase();
        let signature = symbol.qualified_name.clone();
        fingerprints.push(PublicApiFingerprint {
            path: ContractFile::new(path),
            symbol: symbol.name,
            kind,
            digest: stable_id(&signature),
            signature,
            evidence_refs: vec![EvidenceRef::new(format!("symbol:{}", symbol.id.0))],
        });
    }
    Ok(fingerprints)
}

fn public_symbol_kind(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Class
            | SymbolKind::Trait
            | SymbolKind::Interface
            | SymbolKind::Function
            | SymbolKind::Method
            | SymbolKind::Constant
            | SymbolKind::Endpoint
            | SymbolKind::DatabaseTable
    )
}

fn public_api_fingerprints_from_text(path: &Path, text: &str) -> Vec<PublicApiFingerprint> {
    let mut fingerprints = Vec::new();
    let mut seen = BTreeSet::new();
    let extension = path.extension().and_then(|value| value.to_str());
    for line in text.lines() {
        let Some((symbol, kind, signature)) = public_api_signature(line, extension) else {
            continue;
        };
        let key = (kind.clone(), symbol.clone(), signature.clone());
        if !seen.insert(key) {
            continue;
        }
        fingerprints.push(PublicApiFingerprint {
            path: ContractFile::new(path),
            symbol: symbol.clone(),
            kind,
            digest: stable_id(&format!("{}:{}:{signature}", normalize_path(path), symbol)),
            signature,
            evidence_refs: vec![EvidenceRef::new(format!(
                "api:{}:{symbol}",
                normalize_path(path)
            ))],
        });
    }
    fingerprints
}

fn public_api_signature(line: &str, extension: Option<&str>) -> Option<(String, String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with('*')
    {
        return None;
    }
    match extension.unwrap_or_default() {
        "rs" => rust_public_signature(trimmed),
        "ts" | "tsx" | "js" | "jsx" => ts_public_signature(trimmed),
        "py" => python_public_signature(line),
        "go" => go_public_signature(trimmed),
        "java" | "kt" => java_public_signature(trimmed),
        _ => rust_public_signature(trimmed)
            .or_else(|| ts_public_signature(trimmed))
            .or_else(|| python_public_signature(line))
            .or_else(|| go_public_signature(trimmed))
            .or_else(|| java_public_signature(trimmed)),
    }
}

fn rust_public_signature(line: &str) -> Option<(String, String, String)> {
    let rest = if let Some(rest) = line.strip_prefix("pub ") {
        rest
    } else if let Some(rest) = line.strip_prefix("pub(") {
        let close = rest.find(')')?;
        rest.get(close + 1..)?.trim_start()
    } else {
        return None;
    };
    let rest = strip_prefix_words(rest, &["async", "unsafe", "extern", "const"]);
    keyword_signature(
        rest,
        &[
            ("fn", "function"),
            ("struct", "struct"),
            ("enum", "enum"),
            ("trait", "trait"),
            ("type", "type"),
            ("const", "constant"),
            ("static", "constant"),
            ("mod", "module"),
        ],
    )
}

fn ts_public_signature(line: &str) -> Option<(String, String, String)> {
    let mut rest = line.strip_prefix("export ")?;
    rest = rest.strip_prefix("default ").unwrap_or(rest);
    rest = strip_prefix_words(rest, &["async", "declare"]);
    keyword_signature(
        rest,
        &[
            ("function", "function"),
            ("class", "class"),
            ("interface", "interface"),
            ("type", "type"),
            ("const", "constant"),
            ("let", "variable"),
            ("var", "variable"),
            ("enum", "enum"),
        ],
    )
}

fn python_public_signature(line: &str) -> Option<(String, String, String)> {
    if line.starts_with(' ') || line.starts_with('\t') {
        return None;
    }
    let trimmed = line.trim();
    let (symbol, kind, signature) =
        keyword_signature(trimmed, &[("def", "function"), ("class", "class")])?;
    (!symbol.starts_with('_')).then_some((symbol, kind, signature))
}

fn go_public_signature(line: &str) -> Option<(String, String, String)> {
    let (symbol, kind, signature) = keyword_signature(
        line,
        &[
            ("func", "function"),
            ("type", "type"),
            ("const", "constant"),
            ("var", "variable"),
        ],
    )?;
    symbol
        .chars()
        .next()
        .is_some_and(char::is_uppercase)
        .then_some((symbol, kind, signature))
}

fn java_public_signature(line: &str) -> Option<(String, String, String)> {
    if !line.split_whitespace().any(|part| part == "public") {
        return None;
    }
    let compact = normalize_signature(line);
    for (keyword, kind) in [
        ("class", "class"),
        ("interface", "interface"),
        ("enum", "enum"),
        ("record", "class"),
    ] {
        if let Some(index) = compact.find(&format!("{keyword} ")) {
            let symbol = take_ident(compact[index + keyword.len()..].trim_start())?;
            return Some((symbol, kind.into(), compact));
        }
    }
    let before_paren = compact.split('(').next()?;
    let symbol = before_paren.split_whitespace().last()?.to_string();
    (!symbol.is_empty()).then_some((symbol, "method".into(), compact))
}

fn keyword_signature(line: &str, keywords: &[(&str, &str)]) -> Option<(String, String, String)> {
    for (keyword, kind) in keywords {
        if let Some(rest) = line.strip_prefix(&format!("{keyword} ")) {
            let symbol = take_ident(rest.trim_start())?;
            return Some((symbol, (*kind).into(), normalize_signature(line)));
        }
    }
    None
}

fn strip_prefix_words<'a>(mut value: &'a str, words: &[&str]) -> &'a str {
    loop {
        let mut changed = false;
        for word in words {
            if let Some(rest) = value.strip_prefix(&format!("{word} ")) {
                value = rest.trim_start();
                changed = true;
            }
        }
        if !changed {
            return value;
        }
    }
}

fn take_ident(value: &str) -> Option<String> {
    let mut ident = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            ident.push(ch);
        } else {
            break;
        }
    }
    (!ident.is_empty()).then_some(ident)
}

fn normalize_signature(value: &str) -> String {
    let value = value
        .split("//")
        .next()
        .unwrap_or(value)
        .split('{')
        .next()
        .unwrap_or(value)
        .split(';')
        .next()
        .unwrap_or(value);
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn api_fingerprint_key(fingerprint: &PublicApiFingerprint) -> (String, String, String) {
    (
        fingerprint.path.0.clone(),
        fingerprint.kind.clone(),
        fingerprint.symbol.clone(),
    )
}

fn api_surface_delta_finding(
    contract: &ChangeContractV1,
    change: ApiSurfaceChangeKind,
    before: Option<&PublicApiFingerprint>,
    after: Option<&PublicApiFingerprint>,
) -> VerificationFinding {
    let fingerprint = after.or(before).expect("delta has at least one side");
    let matching_constraints = contract
        .api_surface_constraints
        .iter()
        .enumerate()
        .filter(|(_, constraint)| constraint_matches_scope(&constraint.scope, &fingerprint.path.0))
        .collect::<Vec<_>>();
    let allowed_constraint = matching_constraints
        .iter()
        .find(|(_, constraint)| constraint.allowed_changes.contains(&change));
    let forbidden_constraint = matching_constraints.iter().find(|(_, constraint)| {
        constraint.severity == ConstraintSeverity::Forbidden
            && !constraint.allowed_changes.contains(&change)
    });

    let (kind, reason, evidence_refs) = if let Some((index, constraint)) = allowed_constraint {
        (
            "api_surface_allowed_delta",
            format!(
                "public API {:?} for `{}` is allowed by api_surface_constraints[{index}]: {}",
                change, fingerprint.symbol, constraint.reason
            ),
            evidence_ref_strings(&constraint.evidence_refs),
        )
    } else if let Some((index, constraint)) = forbidden_constraint {
        (
            "api_surface_violation",
            format!(
                "public API {:?} for `{}` violates api_surface_constraints[{index}]: {}",
                change, fingerprint.symbol, constraint.reason
            ),
            evidence_ref_strings(&constraint.evidence_refs),
        )
    } else if change == ApiSurfaceChangeKind::Added {
        (
            "api_surface_review_required",
            format!(
                "public API addition detected for `{}`; review compatibility before accepting",
                fingerprint.symbol
            ),
            evidence_ref_strings(&fingerprint.evidence_refs),
        )
    } else {
        (
            "api_surface_violation",
            format!(
                "public API {:?} detected for `{}`; removals and signature changes require explicit approval",
                change, fingerprint.symbol
            ),
            evidence_ref_strings(&fingerprint.evidence_refs),
        )
    };

    let before_signature = before
        .map(|fingerprint| fingerprint.signature.as_str())
        .unwrap_or("<none>");
    let after_signature = after
        .map(|fingerprint| fingerprint.signature.as_str())
        .unwrap_or("<none>");
    VerificationFinding {
        path: Some(PathBuf::from(&fingerprint.path.0)),
        kind: kind.into(),
        reason: format!("{reason}; before `{before_signature}`, after `{after_signature}`"),
        evidence_refs,
    }
}

fn constraint_matches_scope(scope: &str, path: &str) -> bool {
    let scope = scope.trim();
    scope == "*"
        || scope == "public_api"
        || scope == path
        || boundary_pattern_matches(scope, path)
        || path.starts_with(scope.trim_end_matches('/'))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct DependencyEdgeKey {
    source_path: PathBuf,
    target: String,
    edge_type: String,
}

#[derive(Debug, Clone)]
struct DependencyEdgeSnapshot {
    key: DependencyEdgeKey,
    target_path: Option<PathBuf>,
    evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DependencyEdgeChange {
    Added,
    Removed,
}

fn dependency_edges_from_index(
    store: &dyn OkStore,
    changed_files: &[PathBuf],
) -> Result<Vec<DependencyEdgeSnapshot>> {
    let changed = changed_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    let files = store.list_files(usize::MAX, 0)?;
    let files_by_id = files
        .iter()
        .map(|file| (file.id.0.clone(), file.path.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();

    for import in store.imports()? {
        let Some(source_path) = files_by_id.get(&import.file_id.0) else {
            continue;
        };
        if !changed.contains(&normalize_path(source_path)) {
            continue;
        }
        let snapshot = DependencyEdgeSnapshot {
            key: DependencyEdgeKey {
                source_path: source_path.clone(),
                target: import.imported.clone(),
                edge_type: "imports".into(),
            },
            target_path: None,
            evidence_refs: vec![format!(
                "import:{}:{}",
                normalize_path(source_path),
                import.imported
            )],
        };
        if seen.insert(snapshot.key.clone()) {
            edges.push(snapshot);
        }
    }

    for edge_type in [
        GraphEdgeType::Imports,
        GraphEdgeType::References,
        GraphEdgeType::Calls,
    ] {
        let mut offset = 0;
        loop {
            let batch = store.edges_by_type(edge_type.clone(), 1_000, offset)?;
            if batch.is_empty() {
                break;
            }
            for edge in &batch {
                let Some(snapshot) = graph_dependency_edge(store, &files_by_id, edge)? else {
                    continue;
                };
                if changed.contains(&normalize_path(&snapshot.key.source_path))
                    && seen.insert(snapshot.key.clone())
                {
                    edges.push(snapshot);
                }
            }
            offset += batch.len();
            if batch.len() < 1_000 {
                break;
            }
        }
    }
    Ok(edges)
}

fn dependency_edges_from_worktree(
    repo: &Path,
    changed_files: &[PathBuf],
) -> Result<Vec<DependencyEdgeSnapshot>> {
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();
    for path in changed_files {
        let normalized = PathBuf::from(normalize_path(path));
        let path_on_disk = repo.join(&normalized);
        if !path_on_disk.exists() {
            continue;
        }
        let text = fs::read_to_string(&path_on_disk)?;
        for target in dependency_targets_from_text(&text, &normalized) {
            let target_path = resolve_dependency_target(repo, &normalized, &target);
            let snapshot = DependencyEdgeSnapshot {
                key: DependencyEdgeKey {
                    source_path: normalized.clone(),
                    target: target.clone(),
                    edge_type: "imports".into(),
                },
                target_path,
                evidence_refs: vec![format!(
                    "dependency:{}:{target}",
                    normalize_path(&normalized)
                )],
            };
            if seen.insert(snapshot.key.clone()) {
                edges.push(snapshot);
            }
        }
    }
    Ok(edges)
}

fn graph_dependency_edge(
    store: &dyn OkStore,
    files_by_id: &BTreeMap<String, PathBuf>,
    edge: &GraphEdge,
) -> Result<Option<DependencyEdgeSnapshot>> {
    let Some(source_node) = store.node_by_id(&edge.from.0)? else {
        return Ok(None);
    };
    let Some(target_node) = store.node_by_id(&edge.to.0)? else {
        return Ok(None);
    };
    let Some(source_path) = graph_node_path(&source_node, files_by_id) else {
        return Ok(None);
    };
    let target_path = graph_node_path(&target_node, files_by_id);
    let target = target_path
        .as_ref()
        .map(|path| normalize_path(path))
        .unwrap_or_else(|| target_node.label.clone());
    Ok(Some(DependencyEdgeSnapshot {
        key: DependencyEdgeKey {
            source_path,
            target,
            edge_type: graph_edge_type_name(&edge.edge_type),
        },
        target_path,
        evidence_refs: vec![edge.evidence.id.0.clone()],
    }))
}

fn graph_node_path(node: &GraphNode, files_by_id: &BTreeMap<String, PathBuf>) -> Option<PathBuf> {
    if let Some(file_id) = &node.file_id {
        if let Some(path) = files_by_id.get(&file_id.0) {
            return Some(path.clone());
        }
    }
    if node.node_type == open_kioku_core::GraphNodeType::File && !node.label.is_empty() {
        return Some(PathBuf::from(&node.label));
    }
    node.properties
        .get("path")
        .and_then(|value| value.as_str())
        .map(PathBuf::from)
}

fn graph_edge_type_name(edge_type: &GraphEdgeType) -> String {
    match edge_type {
        GraphEdgeType::Imports => "imports",
        GraphEdgeType::References => "references",
        GraphEdgeType::Calls => "calls",
        _ => "dependency",
    }
    .into()
}

fn dependency_targets_from_text(text: &str, source_path: &Path) -> Vec<String> {
    let extension = source_path.extension().and_then(|value| value.to_str());
    let mut targets = Vec::new();
    let mut in_go_import_block = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        }
        if extension == Some("go") && trimmed == "import (" {
            in_go_import_block = true;
            continue;
        }
        if in_go_import_block && trimmed == ")" {
            in_go_import_block = false;
            continue;
        }
        if in_go_import_block {
            if let Some(target) = quoted_value(trimmed) {
                targets.push(target);
            }
            continue;
        }
        if let Some(target) = dependency_target_from_line(trimmed, extension) {
            targets.push(target);
        }
    }
    targets.sort();
    targets.dedup();
    targets
}

fn dependency_target_from_line(line: &str, extension: Option<&str>) -> Option<String> {
    match extension.unwrap_or_default() {
        "rs" => rust_dependency_target(line),
        "ts" | "tsx" | "js" | "jsx" => ts_dependency_target(line),
        "py" => python_dependency_target(line),
        "go" => go_dependency_target(line),
        "java" | "kt" => java_dependency_target(line),
        _ => rust_dependency_target(line)
            .or_else(|| ts_dependency_target(line))
            .or_else(|| python_dependency_target(line))
            .or_else(|| go_dependency_target(line))
            .or_else(|| java_dependency_target(line)),
    }
}

fn rust_dependency_target(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("use ")
        .or_else(|| line.strip_prefix("pub use "))?;
    Some(
        rest.trim_end_matches(';')
            .split(" as ")
            .next()
            .unwrap_or(rest)
            .trim()
            .to_string(),
    )
}

fn ts_dependency_target(line: &str) -> Option<String> {
    if line.starts_with("import ") || line.starts_with("export ") {
        if let Some(index) = line.find(" from ") {
            return quoted_value(&line[index + " from ".len()..]);
        }
        return quoted_value(line);
    }
    if let Some(index) = line.find("require(") {
        return quoted_value(&line[index + "require(".len()..]);
    }
    None
}

fn python_dependency_target(line: &str) -> Option<String> {
    if let Some(rest) = line.strip_prefix("import ") {
        return rest
            .split(',')
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    if let Some(rest) = line.strip_prefix("from ") {
        return rest
            .split_whitespace()
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_string);
    }
    None
}

fn go_dependency_target(line: &str) -> Option<String> {
    line.strip_prefix("import ")
        .and_then(quoted_value)
        .or_else(|| quoted_value(line))
}

fn java_dependency_target(line: &str) -> Option<String> {
    line.strip_prefix("import ")
        .map(|rest| rest.trim_end_matches(';').trim().to_string())
}

fn quoted_value(value: &str) -> Option<String> {
    let value = value.trim();
    for quote in ['"', '\''] {
        let Some(start) = value.find(quote) else {
            continue;
        };
        let rest = &value[start + 1..];
        let Some(end) = rest.find(quote) else {
            continue;
        };
        let target = rest[..end].trim();
        if !target.is_empty() {
            return Some(target.to_string());
        }
    }
    None
}

fn resolve_dependency_target(repo: &Path, source_path: &Path, target: &str) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if target.starts_with('.') {
        if let Some(parent) = source_path.parent() {
            candidates.push(parent.join(target));
        }
    } else if let Some(rest) = target.strip_prefix("crate::") {
        candidates.push(PathBuf::from("src").join(rest.replace("::", "/")));
    } else if target.contains("::") {
        candidates.push(PathBuf::from("src").join(target.replace("::", "/")));
    } else if target.contains('.') {
        candidates.push(PathBuf::from(target.replace('.', "/")));
    } else if target.contains('/') {
        candidates.push(PathBuf::from(target));
    }

    for candidate in candidates {
        if let Some(path) = existing_source_path(repo, &candidate) {
            return Some(path);
        }
    }
    None
}

fn existing_source_path(repo: &Path, candidate: &Path) -> Option<PathBuf> {
    let normalized = normalize_relative_path(candidate);
    let mut candidates = vec![normalized.clone()];
    if normalized.extension().is_none() {
        for extension in ["rs", "ts", "tsx", "js", "jsx", "py", "go", "java"] {
            candidates.push(normalized.with_extension(extension));
        }
        candidates.push(normalized.join("mod.rs"));
        candidates.push(normalized.join("index.ts"));
        candidates.push(normalized.join("index.js"));
        candidates.push(normalized.join("__init__.py"));
    }
    candidates.into_iter().find(|path| repo.join(path).exists())
}

fn normalize_relative_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Normal(value) => normalized.push(value),
            _ => {}
        }
    }
    normalized
}

fn classify_dependency_delta(
    contract: &ChangeContractV1,
    policy: Option<&ArchitecturePolicy>,
    edge: &DependencyEdgeSnapshot,
    change: DependencyEdgeChange,
) -> Result<DependencyDeltaFinding> {
    let mut rule_refs = Vec::new();
    let mut evidence_refs = edge
        .evidence_refs
        .iter()
        .map(|value| EvidenceRef::new(value.clone()))
        .collect::<Vec<_>>();
    let mut reason = match change {
        DependencyEdgeChange::Added => "dependency edge was added".to_string(),
        DependencyEdgeChange::Removed => "dependency edge was removed".to_string(),
    };

    if change == DependencyEdgeChange::Added {
        for (index, constraint) in contract.dependency_delta_constraints.iter().enumerate() {
            if dependency_constraint_matches(constraint, edge) {
                rule_refs.push(format!("dependency_delta_constraints[{index}]"));
                evidence_refs.extend(constraint.evidence_refs.clone());
                reason = constraint.reason.clone();
                if constraint.action == DependencyDeltaAction::Forbid {
                    return Ok(dependency_delta_finding(
                        DependencyDeltaClassification::ViolatingDelta,
                        edge,
                        reason,
                        evidence_refs,
                        rule_refs,
                    ));
                }
            }
        }
        if let Some(policy) = policy {
            let policy_rules = forbidden_policy_rule_refs(policy, edge)?;
            if !policy_rules.is_empty() {
                reason = "added dependency edge violates architecture policy".into();
                rule_refs.extend(policy_rules);
                return Ok(dependency_delta_finding(
                    DependencyDeltaClassification::ViolatingDelta,
                    edge,
                    reason,
                    evidence_refs,
                    rule_refs,
                ));
            }
        }
    }

    Ok(dependency_delta_finding(
        DependencyDeltaClassification::AllowedDelta,
        edge,
        reason,
        evidence_refs,
        rule_refs,
    ))
}

fn dependency_delta_finding(
    classification: DependencyDeltaClassification,
    edge: &DependencyEdgeSnapshot,
    reason: String,
    evidence_refs: Vec<EvidenceRef>,
    rule_refs: Vec<String>,
) -> DependencyDeltaFinding {
    DependencyDeltaFinding {
        classification,
        edge_type: edge.key.edge_type.clone(),
        source: normalize_path(&edge.key.source_path),
        target: edge.key.target.clone(),
        source_path: Some(ContractFile::new(&edge.key.source_path)),
        target_path: edge.target_path.as_ref().map(ContractFile::new),
        reason,
        evidence_refs,
        rule_refs,
    }
}

fn dependency_delta_verification_finding(finding: &DependencyDeltaFinding) -> VerificationFinding {
    VerificationFinding {
        path: finding
            .source_path
            .as_ref()
            .map(|path| PathBuf::from(path.as_str())),
        kind: "dependency_delta_violation".into(),
        reason: format!(
            "{}: {} -> {} ({})",
            finding.reason, finding.source, finding.target, finding.edge_type
        ),
        evidence_refs: evidence_ref_strings(&finding.evidence_refs),
    }
}

fn dependency_constraint_matches(
    constraint: &open_kioku_contract::DependencyDeltaConstraint,
    edge: &DependencyEdgeSnapshot,
) -> bool {
    let source = normalize_path(&edge.key.source_path);
    let target_path = edge
        .target_path
        .as_ref()
        .map(|path| normalize_path(path))
        .unwrap_or_default();
    let edge_type = edge.key.edge_type.to_ascii_lowercase();
    let edge_type_matches = constraint.edge_types.is_empty()
        || constraint
            .edge_types
            .iter()
            .any(|candidate| candidate.to_ascii_lowercase() == edge_type);
    edge_type_matches
        && pattern_or_exact_matches(&constraint.source, &source)
        && (pattern_or_exact_matches(&constraint.target, &edge.key.target)
            || (!target_path.is_empty()
                && pattern_or_exact_matches(&constraint.target, &target_path)))
}

fn pattern_or_exact_matches(pattern: &str, value: &str) -> bool {
    pattern == "*"
        || pattern == value
        || boundary_pattern_matches(pattern, value)
        || value.contains(pattern.trim_matches('*'))
}

fn forbidden_policy_rule_refs(
    policy: &ArchitecturePolicy,
    edge: &DependencyEdgeSnapshot,
) -> Result<Vec<String>> {
    let Some(target_path) = &edge.target_path else {
        return Ok(Vec::new());
    };
    let resolver = PolicyResolver::new(policy)?;
    let source_components = resolver.resolve_file(&edge.key.source_path);
    let target_components = resolver.resolve_file(target_path);
    let mut rule_refs = Vec::new();
    for source_component in &source_components {
        for target_component in &target_components {
            for rule in policy.dependency_rules.iter().filter(|rule| {
                rule.action == DependencyAction::Forbid
                    && (rule.from == "*" || rule.from == source_component.component_id)
                    && (rule.to == "*" || rule.to == target_component.component_id)
            }) {
                rule_refs.push(rule.id.clone());
            }
        }
    }
    rule_refs.sort();
    rule_refs.dedup();
    Ok(rule_refs)
}

trait DependencyDeltaClassificationKey {
    fn to_string_key(self) -> &'static str;
}

impl DependencyDeltaClassificationKey for DependencyDeltaClassification {
    fn to_string_key(self) -> &'static str {
        match self {
            DependencyDeltaClassification::NoRelevantDelta => "0-no-relevant-delta",
            DependencyDeltaClassification::AllowedDelta => "1-allowed-delta",
            DependencyDeltaClassification::ViolatingDelta => "2-violating-delta",
        }
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

fn evidence_quality_failures(
    quality: &EvidenceQuality,
    traceability_strict: bool,
) -> Vec<VerificationFinding> {
    if traceability_strict && quality.is_stale() {
        return vec![VerificationFinding {
            path: None,
            kind: "stale_evidence_quality".into(),
            reason: "source plan evidence quality is stale under strict verification policy".into(),
            evidence_refs: Vec::new(),
        }];
    }
    Vec::new()
}

fn evidence_quality_warnings(
    quality: &EvidenceQuality,
    traceability_strict: bool,
) -> Vec<VerificationFinding> {
    let mut warnings = Vec::new();
    if !(traceability_strict && quality.is_stale()) {
        for caveat in &quality.caveats {
            warnings.push(VerificationFinding {
                path: None,
                kind: if caveat.contains("stale") {
                    "stale_evidence_quality"
                } else {
                    "evidence_quality_caveat"
                }
                .into(),
                reason: caveat.clone(),
                evidence_refs: Vec::new(),
            });
        }
    }
    warnings
}

fn plan_caveat_warnings(plan: &PlanReport) -> Vec<VerificationFinding> {
    plan.confidence_breakdown
        .caveats
        .iter()
        .filter(|caveat| !plan.evidence_quality.caveats.contains(*caveat))
        .map(|caveat| VerificationFinding {
            path: None,
            kind: "confidence_caveat".into(),
            reason: caveat.clone(),
            evidence_refs: evidence_refs_for_caveat(plan, caveat),
        })
        .collect()
}

fn pending_plan_validation_warnings(
    plan: &PlanReport,
    input: &VerifyChangeInput,
) -> Vec<VerificationFinding> {
    if input.suppress_plan_validation_pending
        || input.run_commands
        || !input.validation_attestations.is_empty()
    {
        return Vec::new();
    }
    plan.validation
        .iter()
        .filter_map(|test| {
            test.command.as_ref().map(|command| VerificationFinding {
                path: Some(PathBuf::from(test.file_id.0.clone())),
                kind: "validation_command_pending".into(),
                reason: format!(
                    "planned validation command `{command}` has not been run during verification"
                ),
                evidence_refs: test.evidence_refs.clone(),
            })
        })
        .collect()
}

fn evidence_refs_for_caveat(plan: &PlanReport, caveat: &str) -> Vec<String> {
    if caveat.contains("validation") {
        return validation_plan_evidence_refs(plan);
    }
    if caveat.contains("boundary") {
        return boundary_plan_evidence_refs(plan);
    }
    if caveat.contains("runtime") {
        return plan
            .runtime_signals
            .iter()
            .map(|signal| signal.id.clone())
            .collect();
    }
    if caveat.contains("exact") || caveat.contains("reference") || caveat.contains("symbol") {
        return impact_plan_evidence_refs(plan);
    }
    Vec::new()
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

fn run_validation_commands(repo: &Path, plan: &PlanReport) -> Result<Vec<ValidationCommandResult>> {
    let config = OkConfig::load_from_repo(repo)?;
    let mut seen = BTreeSet::new();
    let commands = plan
        .validation
        .iter()
        .filter_map(|test| test.command.clone())
        .filter(|command| seen.insert(command.clone()))
        .collect::<Vec<_>>();
    Ok(commands
        .into_iter()
        .map(|command| run_validation_command(repo, &command, &config))
        .collect())
}

fn run_validation_command(
    repo: &Path,
    command: &str,
    config: &OkConfig,
) -> ValidationCommandResult {
    if let Err(err) = PolicyGate::new(config).ensure_command_allowed(command) {
        return ValidationCommandResult {
            command: command.into(),
            status: "fail".into(),
            exit_code: None,
            attestation_id: None,
            verification_run_id: None,
            stdout: String::new(),
            stderr: truncate_output(&err.to_string()),
        };
    }
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
            attestation_id: None,
            verification_run_id: None,
            stdout: truncate_output(&String::from_utf8_lossy(&output.stdout)),
            stderr: truncate_output(&String::from_utf8_lossy(&output.stderr)),
        },
        Err(err) => ValidationCommandResult {
            command: command.into(),
            status: "fail".into(),
            exit_code: None,
            attestation_id: None,
            verification_run_id: None,
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
    use open_kioku_config::{ArchitecturePolicy, PolicyLayer, PolicyVersion, Severity};
    use open_kioku_contract::{
        ApiSurfaceChangeKind, ApiSurfaceConstraint, ContractFile, ContractStore,
        DependencyDeltaAction, DependencyDeltaConstraint, FsContractStore,
    };
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
        files: Vec<File>,
        chunks: Vec<CodeChunk>,
        symbols: Vec<Symbol>,
        imports: Vec<Import>,
        nodes: Vec<GraphNode>,
        edges: Vec<GraphEdge>,
        fact: AnalysisFact,
        facts: Vec<AnalysisFact>,
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
            Self {
                file: file.clone(),
                files: vec![file],
                chunks: Vec::new(),
                symbols: Vec::new(),
                imports: Vec::new(),
                nodes: Vec::new(),
                edges: Vec::new(),
                fact: fact.clone(),
                facts: vec![fact],
            }
        }

        fn with_fact(mut self, fact: AnalysisFact) -> Self {
            self.fact = fact.clone();
            self.facts = vec![fact];
            self
        }

        fn without_runtime(mut self) -> Self {
            self.facts.clear();
            self
        }

        fn with_file_text(mut self, path: &str, text: &str) -> Self {
            let file = self.ensure_file(path);
            self.chunks.push(CodeChunk {
                id: format!("chunk-{path}"),
                file_id: file.id,
                range: LineRange {
                    start: 1,
                    end: text.lines().count().max(1) as u32,
                },
                language: Language::Rust,
                text: text.into(),
                symbol_id: None,
            });
            self
        }

        fn ensure_file(&mut self, path: &str) -> File {
            if let Some(file) = self.files.iter().find(|file| file.path == Path::new(path)) {
                return file.clone();
            }
            let file = File {
                id: FileId::new(path.replace(['/', '.'], "_")),
                repository_id: RepositoryId::new("repo"),
                path: PathBuf::from(path),
                language: Language::Rust,
                size_bytes: 100,
                content_hash: format!("hash-{path}"),
                is_generated: false,
                is_vendor: false,
            };
            self.files.push(file.clone());
            file
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
            Ok(self.files.clone())
        }

        fn get_file_by_path(&self, path: &Path) -> Result<Option<File>> {
            Ok(self.files.iter().find(|file| file.path == path).cloned())
        }

        fn list_symbols(
            &self,
            _query: Option<&str>,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<Symbol>> {
            Ok(self.symbols.clone())
        }

        fn symbol_by_id(&self, _id: &SymbolId) -> Result<Option<Symbol>> {
            Ok(None)
        }

        fn chunks_for_file(&self, file_id: &FileId) -> Result<Vec<CodeChunk>> {
            Ok(self
                .chunks
                .iter()
                .filter(|chunk| chunk.file_id == *file_id)
                .cloned()
                .collect())
        }

        fn all_chunks(&self) -> Result<Vec<CodeChunk>> {
            Ok(self.chunks.clone())
        }

        fn tests(&self) -> Result<Vec<TestTarget>> {
            Ok(Vec::new())
        }

        fn imports(&self) -> Result<Vec<Import>> {
            Ok(self.imports.clone())
        }

        fn analysis_facts(
            &self,
            source_type: Option<EvidenceSourceType>,
            _limit: usize,
        ) -> Result<Vec<AnalysisFact>> {
            if source_type == Some(EvidenceSourceType::Runtime) {
                Ok(self.facts.clone())
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

        fn symbols_for_file(&self, file_id: &FileId) -> Result<Vec<Symbol>> {
            Ok(self
                .symbols
                .iter()
                .filter(|symbol| symbol.file_id == *file_id)
                .cloned()
                .collect())
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

        fn node_by_id(&self, id: &str) -> Result<Option<GraphNode>> {
            Ok(self.nodes.iter().find(|node| node.id.0 == id).cloned())
        }

        fn edges_by_type(
            &self,
            edge_type: GraphEdgeType,
            limit: usize,
            offset: usize,
        ) -> Result<Vec<GraphEdge>> {
            Ok(self
                .edges
                .iter()
                .filter(|edge| edge.edge_type == edge_type)
                .skip(offset)
                .take(limit)
                .cloned()
                .collect())
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
        assert!(adapted.warnings.len() >= direct.warnings.len());
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
    fn contract_verifier_fails_stale_quality_under_strict_policy() {
        let store = RuntimeStore::new().without_runtime();
        let mut plan = plan_with_boundary_evidence();
        plan.evidence_quality = stale_evidence_quality();
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

        assert_eq!(report.decision, VerificationDecision::Fail);
        assert!(report
            .change_report
            .boundary_violations
            .iter()
            .any(|finding| finding.kind == "stale_evidence_quality"));
    }

    #[test]
    fn contract_verifier_warns_when_validation_evidence_is_missing() {
        let store = RuntimeStore::new().without_runtime();
        let mut plan = plan_with_validation_command("cargo test");
        plan.evidence_quality = complete_evidence_quality();
        plan.confidence_breakdown.caveats.clear();
        let contract = ContractBuilder::from_plan(&plan).unwrap();

        let report = ContractVerifier::new(&store)
            .verify(
                Path::new("."),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.decision, VerificationDecision::Warn);
        assert!(report
            .change_report
            .warnings
            .iter()
            .any(|finding| finding.kind == "validation_attestation_pending"));
    }

    #[test]
    fn contract_verifier_passes_when_quality_and_validation_are_complete() {
        let store = RuntimeStore::new().without_runtime();
        let mut plan = plan_with_validation_command("cargo test");
        plan.evidence_quality = complete_evidence_quality();
        plan.confidence_summary = "exact confidence".into();
        plan.confidence_breakdown = ConfidenceBreakdown {
            overall_enum: Confidence::Exact,
            overall_score: 0.96,
            components: vec![],
            blockers: vec![],
            caveats: vec![],
        };
        let contract = ContractBuilder::from_plan(&plan).unwrap();
        let attestation = passed_attestation_for_contract(&contract);

        let report = ContractVerifier::new(&store)
            .verify(
                Path::new("."),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    validation_attestations: vec![attestation],
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.decision, VerificationDecision::Pass);
        assert!(report.change_report.warnings.is_empty());
        assert!(report.change_report.boundary_violations.is_empty());
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

    #[test]
    fn contract_verifier_writes_validation_ledger_and_attestation_summary() {
        let repo = tempfile::tempdir().unwrap();
        write_minimal_cargo_project(repo.path());
        let store = RuntimeStore::new();
        let plan = plan_with_validation_command("cargo test");
        let contract = ContractBuilder::from_plan(&plan).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let contract_store = FsContractStore::new(dir.path());

        let report = ContractVerifier::new(&store)
            .with_contract_store(Some(&contract_store))
            .verify(
                repo.path(),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    run_commands: true,
                    write_attestation: true,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.change_report.command_results[0].status, "pass");
        assert_eq!(report.change_report.validation_attestations.len(), 1);
        let ledger_path = report
            .change_report
            .validation_ledger_path
            .as_ref()
            .expect("ledger path recorded");
        assert!(ledger_path.exists());
        let ledger: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(ledger_path).unwrap()).unwrap();
        assert_eq!(ledger["attestations"][0]["result"]["command"], "cargo test");

        let verification_path = dir.path().join(format!("{}.verify.jsonl", contract.id.0));
        let jsonl = fs::read_to_string(verification_path).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        assert_eq!(
            record["validation_attestations"][0]["id"],
            report.change_report.validation_attestations[0].id
        );
    }

    #[test]
    fn contract_verifier_denies_unallowlisted_validation_command() {
        let repo = tempfile::tempdir().unwrap();
        let store = RuntimeStore::new();
        let plan = plan_with_validation_command("rm -rf /");
        let contract = ContractBuilder::from_plan(&plan).unwrap();

        let report = ContractVerifier::new(&store)
            .verify(
                repo.path(),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    run_commands: true,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.decision, VerificationDecision::Fail);
        assert_eq!(
            report.change_report.validation_attestations[0]
                .result
                .allowlist_status,
            CommandAllowlistStatus::Denied
        );
        assert!(report
            .change_report
            .boundary_violations
            .iter()
            .any(|finding| finding.kind == "validation_command_denied"));
    }

    #[test]
    fn contract_verifier_rejects_stale_validation_attestation() {
        let store = RuntimeStore::new();
        let plan = plan_with_validation_command("cargo test");
        let contract = ContractBuilder::from_plan(&plan).unwrap();
        let mut attestation = passed_attestation_for_contract(&contract);
        attestation.created_at = contract.timestamps.updated_at - chrono::Duration::seconds(1);

        let report = ContractVerifier::new(&store)
            .verify(
                Path::new("."),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    validation_attestations: vec![attestation],
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.decision, VerificationDecision::Fail);
        assert!(report
            .change_report
            .boundary_violations
            .iter()
            .any(|finding| finding.kind == "validation_attestation_stale"));
    }

    #[test]
    fn contract_verifier_detects_command_replay_mismatch() {
        let store = RuntimeStore::new();
        let plan = plan_with_validation_command("cargo test");
        let contract = ContractBuilder::from_plan(&plan).unwrap();
        let mut attestation = passed_attestation_for_contract(&contract);
        attestation.result.command = "cargo check".into();

        let report = ContractVerifier::new(&store)
            .verify(
                Path::new("."),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    validation_attestations: vec![attestation],
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.decision, VerificationDecision::Fail);
        assert!(report
            .change_report
            .boundary_violations
            .iter()
            .any(|finding| finding.kind == "validation_command_replay_mismatch"));
    }

    #[test]
    fn contract_verifier_warns_on_public_api_addition() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("src")).unwrap();
        fs::write(
            repo.path().join("src/handler.rs"),
            "pub fn handle() {}\npub fn new_endpoint() {}\n",
        )
        .unwrap();
        let store = RuntimeStore::new().with_file_text("src/handler.rs", "pub fn handle() {}\n");
        let contract = ContractBuilder::from_plan(&plan_with_boundary_evidence()).unwrap();

        let report = ContractVerifier::new(&store)
            .verify(
                repo.path(),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    check_api_surface: true,
                    ..Default::default()
                },
            )
            .unwrap();

        assert!(report.api_surface.is_some());
        assert!(report
            .change_report
            .api_surface_deltas
            .iter()
            .any(|finding| finding.kind == "api_surface_review_required"
                && finding.reason.contains("new_endpoint")));
        assert!(report
            .change_report
            .warnings
            .iter()
            .any(|finding| finding.kind == "api_surface_review_required"));
    }

    #[test]
    fn contract_verifier_fails_on_public_api_signature_change() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("src")).unwrap();
        fs::write(
            repo.path().join("src/handler.rs"),
            "pub fn handle(user_id: &str) {}\n",
        )
        .unwrap();
        let store = RuntimeStore::new().with_file_text("src/handler.rs", "pub fn handle() {}\n");
        let contract = ContractBuilder::from_plan(&plan_with_boundary_evidence()).unwrap();

        let report = ContractVerifier::new(&store)
            .verify(
                repo.path(),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    check_api_surface: true,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.decision, VerificationDecision::Fail);
        assert!(report
            .change_report
            .boundary_violations
            .iter()
            .any(|finding| finding.kind == "api_surface_violation"
                && finding.reason.contains("SignatureChanged")));
    }

    #[test]
    fn contract_verifier_allows_explicit_public_api_signature_change() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("src")).unwrap();
        fs::write(
            repo.path().join("src/handler.rs"),
            "pub fn handle(user_id: &str) {}\n",
        )
        .unwrap();
        let store = RuntimeStore::new().with_file_text("src/handler.rs", "pub fn handle() {}\n");
        let mut contract = ContractBuilder::from_plan(&plan_with_boundary_evidence()).unwrap();
        contract.api_surface_constraints = vec![ApiSurfaceConstraint {
            scope: "src/handler.rs".into(),
            allowed_changes: vec![ApiSurfaceChangeKind::SignatureChanged],
            severity: ConstraintSeverity::Required,
            reason: "approved API migration".into(),
            evidence_refs: contract.evidence_refs.clone(),
        }];

        let report = ContractVerifier::new(&store)
            .verify(
                repo.path(),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    check_api_surface: true,
                    ..Default::default()
                },
            )
            .unwrap();

        assert!(!report
            .change_report
            .boundary_violations
            .iter()
            .any(|finding| finding.kind == "api_surface_violation"));
        assert!(report
            .change_report
            .api_surface_deltas
            .iter()
            .any(|finding| finding.kind == "api_surface_allowed_delta"));
    }

    #[test]
    fn contract_verifier_fails_for_forbidden_dependency_delta_constraint() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("src/forbidden")).unwrap();
        fs::write(
            repo.path().join("src/handler.rs"),
            "use crate::forbidden::secret;\npub fn handle() {}\n",
        )
        .unwrap();
        fs::write(
            repo.path().join("src/forbidden/secret.rs"),
            "pub fn secret() {}\n",
        )
        .unwrap();
        let store = RuntimeStore::new().with_file_text("src/handler.rs", "pub fn handle() {}\n");
        let mut contract = ContractBuilder::from_plan(&plan_with_boundary_evidence()).unwrap();
        contract.dependency_delta_constraints = vec![DependencyDeltaConstraint {
            source: "src/handler.rs".into(),
            target: "crate::forbidden::*".into(),
            edge_types: vec!["imports".into()],
            action: DependencyDeltaAction::Forbid,
            severity: ConstraintSeverity::Forbidden,
            reason: "handler must not import forbidden internals".into(),
            evidence_refs: contract.evidence_refs.clone(),
        }];

        let report = ContractVerifier::new(&store)
            .verify(
                repo.path(),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/handler.rs")],
                    check_dependency_delta: true,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.decision, VerificationDecision::Fail);
        assert!(report
            .change_report
            .dependency_deltas
            .iter()
            .any(|finding| finding.classification
                == DependencyDeltaClassification::ViolatingDelta
                && finding.reason.contains("forbidden internals")));
    }

    #[test]
    fn contract_verifier_uses_architecture_policy_for_dependency_delta() {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("src/domain")).unwrap();
        fs::create_dir_all(repo.path().join("src/api")).unwrap();
        fs::write(
            repo.path().join("src/domain/order.rs"),
            "use crate::api::secret;\npub fn order() {}\n",
        )
        .unwrap();
        fs::write(
            repo.path().join("src/api/secret.rs"),
            "pub fn secret() {}\n",
        )
        .unwrap();
        let store = RuntimeStore::new()
            .with_file_text("src/domain/order.rs", "pub fn order() {}\n")
            .with_file_text("src/api/secret.rs", "pub fn secret() {}\n");
        let contract = ContractBuilder::from_plan(&plan_with_boundary_evidence()).unwrap();
        let policy = ArchitecturePolicy {
            version: PolicyVersion::V1,
            layers: vec![
                PolicyLayer {
                    id: "domain".into(),
                    description: None,
                    paths: vec!["src/domain/**".into()],
                },
                PolicyLayer {
                    id: "api".into(),
                    description: None,
                    paths: vec!["src/api/**".into()],
                },
            ],
            contexts: Vec::new(),
            dependency_rules: vec![open_kioku_config::DependencyRule {
                id: "domain-must-not-import-api".into(),
                from: "domain".into(),
                to: "api".into(),
                action: DependencyAction::Forbid,
                severity: Severity::Error,
                reason: "domain cannot import api".into(),
            }],
            public_api_rules: Vec::new(),
            internal_only_rules: Vec::new(),
            exemptions: Vec::new(),
            source: Default::default(),
        };

        let report = ContractVerifier::new(&store)
            .verify(
                repo.path(),
                &contract,
                VerifyChangeInput {
                    changed_files: vec![PathBuf::from("src/domain/order.rs")],
                    check_dependency_delta: true,
                    architecture_policy: Some(policy),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.decision, VerificationDecision::Fail);
        assert!(report
            .change_report
            .dependency_deltas
            .iter()
            .any(|finding| finding
                .rule_refs
                .contains(&"domain-must-not-import-api".to_string())));
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
                architecture_policy: None,
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
            architecture_policy: None,
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
            evidence_quality: Default::default(),
        }
    }

    fn plan_with_validation_command(command: &str) -> PlanReport {
        let mut plan = plan_with_boundary_evidence();
        plan.validation = vec![TestTarget {
            id: "validation:handler".into(),
            name: "handler validation".into(),
            file_id: FileId::new("tests/handler_test.rs"),
            range: None,
            command: Some(command.into()),
            confidence: Confidence::High,
            reason: "validate handler change".into(),
            evidence_refs: vec!["boundary:allow".into()],
            score_breakdown: Vec::new(),
        }];
        plan.evidence_by_section
            .insert("validation".into(), vec!["boundary:allow".into()]);
        plan
    }

    fn complete_evidence_quality() -> EvidenceQuality {
        EvidenceQuality {
            index_mode: "full".into(),
            freshness: "fresh".into(),
            exact_reference_available: true,
            runtime_available: true,
            history_available: true,
            test_coverage_available: true,
            skipped_path_count: 0,
            unresolved_import_count: 0,
            ambiguous_edge_count: 0,
            failed_optional_passes: Vec::new(),
            caveats: Vec::new(),
        }
    }

    fn stale_evidence_quality() -> EvidenceQuality {
        let mut quality = complete_evidence_quality();
        quality.freshness = "stale".into();
        quality.refresh_caveats();
        quality
    }

    fn write_minimal_cargo_project(repo: &Path) {
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"attestation-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn fixture() -> bool { true }\n",
        )
        .unwrap();
    }

    fn passed_attestation_for_contract(contract: &ChangeContractV1) -> ValidationAttestation {
        let requirement = validation_requirements_for_contract(contract)
            .into_iter()
            .next()
            .expect("contract has validation requirement");
        let contract_digest = digest_json(contract).unwrap();
        let requirement_digest = digest_json(&requirement).unwrap();
        let now = chrono::Utc::now();
        ValidationAttestation {
            id: "attestation-1".into(),
            contract_id: contract.id.clone(),
            verification_run_id: "run-1".into(),
            contract_digest,
            requirement_digest,
            created_at: now,
            result: AttestedCommandResult {
                command: requirement.command,
                cwd: ".".into(),
                started_at: now,
                finished_at: now,
                exit_code: Some(0),
                allowlist_status: CommandAllowlistStatus::Allowed,
                outcome: ValidationOutcome::Passed,
                stdout_summary: "ok".into(),
                stderr_summary: String::new(),
            },
        }
    }
}
