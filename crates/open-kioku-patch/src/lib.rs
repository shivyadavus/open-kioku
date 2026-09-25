use open_kioku_actions::PolicyGate;
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
    GraphNode, ImpactReport, LineRange, PatchId, PatchPlan, PlanReport, RiskReport, SearchResult,
    Symbol, SymbolKind, TestTarget,
};
use open_kioku_errors::{OkError, Result};
use open_kioku_git::unified_diff::{DiffLine, HunkScanner, MalformedDiff};
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
                "Apply reviewed source edits with the normal editor".into(),
                "Run the recommended validation plan after editing".into(),
            ],
            risks: context.risk_report.reasons,
            assumptions: vec![
                "Generated and vendor files remain out of scope".into(),
                "Source edits are applied outside Open Kioku MCP and verified against this plan"
                    .into(),
            ],
            tests: context.test_candidates,
            rollback_notes: vec!["Revert the unified diff if validation fails".into()],
            unified_diff: None,
            requires_approval: self.config.security.approval_required,
            evidence: context.evidence,
        })
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
    /// Post-edit line ranges per changed path. The `@@` hunk headers of `unified_diff` are
    /// added to these; a caller without a diff may supply them directly. A path with no
    /// ranges is verified at file granularity and the report says so.
    #[serde(default)]
    pub changed_ranges: BTreeMap<PathBuf, Vec<LineRange>>,
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
    /// The rename and copy pairs among `changed_files`. A rename's previous path is also in
    /// `changed_files` and held to the boundary like any edit; a copy's is not, but is still
    /// held to the forbidden rules.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_paths: Vec<PreviousPath>,
    pub changed_symbols: Vec<String>,
    /// Hunks (`<path>:<start>-<end>`, post-edit lines) that no indexed symbol range covers:
    /// file-level code, comments, or files the index does not hold. Reported rather than
    /// dropped so a change outside every symbol stays visible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_regions_without_symbol: Vec<String>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviousPathKind {
    Rename,
    Copy,
}

/// A changed path and the path its content came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousPath {
    pub path: PathBuf,
    pub previous_path: PathBuf,
    pub kind: PreviousPathKind,
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
        let contract_error = match ContractBuilder::from_plan(plan) {
            Ok(contract) => {
                return ContractVerifier::new(self.store)
                    .with_search_index(self.search_index)
                    .with_contract_store(self.contract_store)
                    .verify_plan_adapter(repo, &contract, plan, input)
                    .map(|report| report.change_report);
            }
            Err(err) => err,
        };
        // Both delta checks classify against a contract, and this plan could not become one.
        // A check that was enabled but did not run is said so, and keeps the verdict off `pass`.
        let skipped = [
            (
                input.check_api_surface,
                "api_surface_check_not_run",
                "API-surface",
            ),
            (
                input.check_dependency_delta,
                "dependency_delta_check_not_run",
                "dependency-delta",
            ),
        ]
        .into_iter()
        .filter(|(enabled, _, _)| *enabled)
        .map(|(_, kind, check)| VerificationFinding {
            path: None,
            kind: kind.into(),
            reason: format!(
                "the {check} check was enabled but not run: it compares against a change \
                 contract, and this plan could not become one ({contract_error})"
            ),
            evidence_refs: Vec::new(),
        })
        .collect::<Vec<_>>();
        let mut report = self.verify_plan_direct(repo, plan, input)?;
        if !skipped.is_empty() && report.verdict == VerificationVerdict::Pass {
            report.verdict = VerificationVerdict::Warn;
        }
        report.warnings.extend(skipped);
        Ok(report)
    }

    fn verify_plan_direct(
        &self,
        repo: &Path,
        plan: &PlanReport,
        input: VerifyChangeInput,
    ) -> Result<ChangeVerificationReport> {
        let changed_files = changed_files_from_input(&input);
        if changed_files.is_empty() {
            return Err(OkError::InvalidInput(
                "verify requires at least one changed file or a non-empty unified diff".into(),
            ));
        }
        let diff_pairs = diff_pairs_from_input(&input);
        let previous_paths = diff_pairs
            .iter()
            .map(|pair| pair.previous.clone())
            .collect::<Vec<_>>();
        let changed_regions = changed_regions_from_input(&input, &previous_paths);
        let scoped_paths = diff_scoped_paths(&diff_pairs);

        let mut boundary_violations =
            boundary_violations(plan, &changed_files, &previous_paths, &input.evidence_refs);
        boundary_violations.extend(malformed_diff_violation(input.unified_diff.as_deref()));
        if input.traceability_strict {
            boundary_violations.extend(unknown_evidence_ref_violations(plan, &input.evidence_refs));
        }
        let evidence_quality = plan.evidence_quality.clone();
        boundary_violations.extend(evidence_quality_failures(
            &evidence_quality,
            input.traceability_strict,
        ));
        let ChangedSymbols {
            symbols: changed_symbols,
            regions_without_symbol: changed_regions_without_symbol,
            granularity_warnings,
        } = changed_symbols(
            self.store,
            &changed_files,
            &changed_regions,
            &scoped_paths,
            input.unified_diff.as_deref(),
        )?;
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

        // A granularity warning describes how precisely the report could attribute the
        // change, not the change itself, so it is reported without moving the verdict.
        let verdict_warnings = warnings
            .iter()
            .any(|warning| warning.kind != SYMBOL_GRANULARITY_WARNING);
        warnings.extend(granularity_warnings);

        let verdict = if !boundary_violations.is_empty() || !command_failures.is_empty() {
            VerificationVerdict::Fail
        } else if verdict_warnings || !missing_tests.is_empty() || !changed_impact.is_empty() {
            VerificationVerdict::Warn
        } else {
            VerificationVerdict::Pass
        };

        let mut all_boundary_violations = boundary_violations;
        all_boundary_violations.extend(command_failures);

        Ok(ChangeVerificationReport {
            verdict,
            changed_files,
            previous_paths,
            changed_symbols,
            changed_regions_without_symbol,
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
                &change_report.previous_paths,
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
            direct_impacts_omitted: 0,
            indirect_impacts_omitted: 0,
            proven_impact: Vec::new(),
            possible_impact: Vec::new(),
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
            // A contract names its required tests itself; nothing was extracted from a file, so
            // the target has no test-file or registration provenance to carry.
            origin: open_kioku_core::TestTargetOrigin::Symbol,
            selection_tier: open_kioku_core::TestSelectionTier::default(),
            tier_justification: Vec::new(),
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
    previous_paths: &[PreviousPath],
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

    // A public item a rename carries to the new path is paired with its old-path fingerprint,
    // so an unchanged item reads as a move and only a real removal or signature change fails.
    let renamed_to = previous_paths
        .iter()
        .filter(|previous| previous.kind == PreviousPathKind::Rename)
        .map(|previous| {
            (
                normalize_path(&previous.previous_path),
                normalize_path(&previous.path),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut paired_after = BTreeSet::new();
    let mut findings = Vec::new();
    for (key, before_fingerprint) in &before_by_key {
        match after_by_key.get(key) {
            None => {
                let moved = renamed_to
                    .get(&normalize_path(Path::new(&before_fingerprint.path.0)))
                    .map(|new_path| (new_path.clone(), key.1.clone(), key.2.clone()))
                    .filter(|moved_key| !before_by_key.contains_key(moved_key))
                    .and_then(|moved_key| after_by_key.get_key_value(&moved_key));
                match moved {
                    Some((moved_key, after_fingerprint))
                        if before_fingerprint.signature == after_fingerprint.signature =>
                    {
                        paired_after.insert(moved_key.clone());
                        findings.push(api_surface_moved_finding(
                            contract,
                            before_fingerprint,
                            after_fingerprint,
                        ));
                    }
                    Some((moved_key, after_fingerprint)) => {
                        paired_after.insert(moved_key.clone());
                        findings.push(api_surface_delta_finding(
                            contract,
                            ApiSurfaceChangeKind::SignatureChanged,
                            Some(before_fingerprint),
                            Some(after_fingerprint),
                        ));
                    }
                    None => findings.push(api_surface_delta_finding(
                        contract,
                        ApiSurfaceChangeKind::Removed,
                        Some(before_fingerprint),
                        None,
                    )),
                }
            }
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
        if !before_by_key.contains_key(key) && !paired_after.contains(key) {
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
                "api_surface_review_required" | "api_surface_moved" => {
                    report.warnings.push(finding.clone())
                }
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
    } else {
        let rest = line.strip_prefix("pub(")?;
        let close = rest.find(')')?;
        rest.get(close + 1..)?.trim_start()
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

/// A public item a rename carried to its new path with kind, name and signature unchanged. The
/// item still exists, so the move warns. Its import path changes, though, so a contract whose
/// `api_surface_constraints` forbid removals in the previous path's scope, or additions in the
/// new path's scope, fails it: the explicit contract outranks the pairing on both sides.
fn api_surface_moved_finding(
    contract: &ChangeContractV1,
    before: &PublicApiFingerprint,
    after: &PublicApiFingerprint,
) -> VerificationFinding {
    let mut evidence_refs = evidence_ref_strings(&before.evidence_refs);
    evidence_refs.extend(evidence_ref_strings(&after.evidence_refs));
    let violation =
        forbidding_api_constraint(contract, &before.path.0, ApiSurfaceChangeKind::Removed)
            .map(|(index, constraint)| {
                (
                    index,
                    constraint,
                    &before.path.0,
                    "removes it from",
                    "removals",
                )
            })
            .or_else(|| {
                forbidding_api_constraint(contract, &after.path.0, ApiSurfaceChangeKind::Added).map(
                    |(index, constraint)| {
                        (index, constraint, &after.path.0, "adds it to", "additions")
                    },
                )
            });
    match violation {
        Some((index, constraint, scope_path, effect, forbidden)) => {
            evidence_refs.extend(evidence_ref_strings(&constraint.evidence_refs));
            VerificationFinding {
                path: Some(PathBuf::from(scope_path)),
                kind: "api_surface_violation".into(),
                reason: format!(
                    "public {} `{}` moved with the rename from `{}` to `{}`, which {effect} a scope where api_surface_constraints[{index}] forbids {forbidden}: {}",
                    after.kind, after.symbol, before.path.0, after.path.0, constraint.reason
                ),
                evidence_refs,
            }
        }
        None => VerificationFinding {
            path: Some(PathBuf::from(&after.path.0)),
            kind: "api_surface_moved".into(),
            reason: format!(
                "public {} `{}` moved with the rename from `{}` to `{}`, signature unchanged (`{}`); review callers that refer to it by path",
                after.kind, after.symbol, before.path.0, after.path.0, after.signature
            ),
            evidence_refs,
        },
    }
}

/// The constraint that forbids `change` in `path`'s scope, unless a constraint covering that scope
/// allows it; the same precedence `api_surface_delta_finding` applies to an unpaired change.
fn forbidding_api_constraint<'a>(
    contract: &'a ChangeContractV1,
    path: &str,
    change: ApiSurfaceChangeKind,
) -> Option<(usize, &'a open_kioku_contract::ApiSurfaceConstraint)> {
    let matching = contract
        .api_surface_constraints
        .iter()
        .enumerate()
        .filter(|(_, constraint)| constraint_matches_scope(&constraint.scope, path))
        .collect::<Vec<_>>();
    if matching
        .iter()
        .any(|(_, constraint)| constraint.allowed_changes.contains(&change))
    {
        return None;
    }
    matching
        .into_iter()
        .find(|(_, constraint)| constraint.severity == ConstraintSeverity::Forbidden)
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

/// Every path a unified diff adds, modifies or removes. A rename contributes both of its
/// paths, as the deletion and addition git reports without rename detection would; a copy
/// contributes only its destination.
pub fn changed_files_from_unified_diff(diff: &str) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    let mut pending_old: Option<String> = None;
    let mut scanner = HunkScanner::new();
    for line in diff.lines() {
        // A hunk's content lines are never headers: a removed `-- x` or an added `++ y` reads
        // as `--- x` or `+++ y` and is not a path.
        if scanner.scan(line) != DiffLine::Header {
            continue;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            pending_old = None;
            if let (_, Some(path)) = git_header_paths(rest) {
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
    for previous in previous_paths_from_unified_diff(diff) {
        if previous.kind == PreviousPathKind::Rename {
            paths.insert(previous.previous_path);
        }
        paths.insert(previous.path);
    }
    paths.into_iter().collect()
}

/// The extended header of one `diff --git` entry: everything before its first hunk.
#[derive(Default)]
struct GitEntryHeader {
    header_old: Option<String>,
    header_new: Option<String>,
    /// `Some(None)` is a `/dev/null` side; `None` is a side with no `---`/`+++` line.
    marker_old: Option<Option<String>>,
    marker_new: Option<Option<String>>,
    from: Option<String>,
    to: Option<String>,
    kind: Option<PreviousPathKind>,
    added_or_deleted: bool,
    binary: bool,
    in_hunks: bool,
}

/// A rename or copy pair from a diff. Git reports a binary pair without hunks even when its
/// content changed, so only a text pair's hunks state every changed line.
struct DiffPair {
    previous: PreviousPath,
    binary: bool,
}

impl GitEntryHeader {
    fn diff_pair(self) -> Option<DiffPair> {
        if self.added_or_deleted {
            return None;
        }
        let old = match (self.from, self.marker_old) {
            (Some(path), _) | (None, Some(Some(path))) => path,
            (None, Some(None)) => return None,
            (None, None) => self.header_old?,
        };
        let new = match (self.to, self.marker_new) {
            (Some(path), _) | (None, Some(Some(path))) => path,
            (None, Some(None)) => return None,
            (None, None) => self.header_new?,
        };
        if old == new {
            return None;
        }
        // Git writes `rename`/`copy` lines for every entry whose paths differ. An entry
        // without them is still checked as a rename rather than trusted to leave `old` alone.
        Some(DiffPair {
            previous: PreviousPath {
                path: PathBuf::from(new),
                previous_path: PathBuf::from(old),
                kind: self.kind.unwrap_or(PreviousPathKind::Rename),
            },
            binary: self.binary,
        })
    }
}

/// Renames and copies declared by the `diff --git` entries of a diff, in diff order.
fn previous_paths_from_unified_diff(diff: &str) -> Vec<PreviousPath> {
    diff_pairs_from_unified_diff(diff)
        .into_iter()
        .map(|pair| pair.previous)
        .collect()
}

/// The rename and copy pairs of a diff's `diff --git` entries, in diff order. Only each entry's
/// header is read, never its hunks, so an added or removed line that begins with `--- ` or
/// `+++ ` cannot be taken for a path.
fn diff_pairs_from_unified_diff(diff: &str) -> Vec<DiffPair> {
    let mut pairs = Vec::new();
    let mut entry: Option<GitEntryHeader> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            pairs.extend(entry.take().and_then(GitEntryHeader::diff_pair));
            let (header_old, header_new) = git_header_paths(rest);
            entry = Some(GitEntryHeader {
                header_old,
                header_new,
                ..Default::default()
            });
            continue;
        }
        let Some(header) = entry.as_mut().filter(|header| !header.in_hunks) else {
            continue;
        };
        if line.starts_with("@@ ") {
            header.in_hunks = true;
        } else if let Some(value) = line.strip_prefix("rename from ") {
            header.from = Some(extended_header_path(value));
            header.kind = Some(PreviousPathKind::Rename);
        } else if let Some(value) = line.strip_prefix("rename to ") {
            header.to = Some(extended_header_path(value));
            header.kind = Some(PreviousPathKind::Rename);
        } else if let Some(value) = line.strip_prefix("copy from ") {
            header.from = Some(extended_header_path(value));
            header.kind = Some(PreviousPathKind::Copy);
        } else if let Some(value) = line.strip_prefix("copy to ") {
            header.to = Some(extended_header_path(value));
            header.kind = Some(PreviousPathKind::Copy);
        } else if line.starts_with("new file mode ") || line.starts_with("deleted file mode ") {
            header.added_or_deleted = true;
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            header.binary = true;
        } else if let Some(value) = line.strip_prefix("--- ") {
            header.marker_old = Some(diff_path(value));
        } else if let Some(value) = line.strip_prefix("+++ ") {
            header.marker_new = Some(diff_path(value));
        }
    }
    pairs.extend(entry.and_then(GitEntryHeader::diff_pair));
    pairs
}

fn diff_pairs_from_input(input: &VerifyChangeInput) -> Vec<DiffPair> {
    let Some(diff) = &input.unified_diff else {
        return Vec::new();
    };
    let mut pairs: Vec<DiffPair> = Vec::new();
    for pair in diff_pairs_from_unified_diff(diff) {
        let previous = PreviousPath {
            path: PathBuf::from(normalize_path(&pair.previous.path)),
            previous_path: PathBuf::from(normalize_path(&pair.previous.previous_path)),
            kind: pair.previous.kind,
        };
        match pairs.iter_mut().find(|known| known.previous == previous) {
            // The same pair from two joined diffs is binary if either one says so.
            Some(known) => known.binary |= pair.binary,
            None => pairs.push(DiffPair {
                previous,
                binary: pair.binary,
            }),
        }
    }
    pairs
}

/// Paths whose changed lines a diff states hunk by hunk: both sides of a text rename and the
/// destination of a text copy. Such a path with no hunk changed no lines.
fn diff_scoped_paths(pairs: &[DiffPair]) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    for pair in pairs.iter().filter(|pair| !pair.binary) {
        paths.insert(pair.previous.path.clone());
        if pair.previous.kind == PreviousPathKind::Rename {
            paths.insert(pair.previous.previous_path.clone());
        }
    }
    paths
}

/// The `a/` and `b/` paths of a `diff --git a/<old> b/<new>` line. An unquoted path may hold
/// spaces, so a line naming one path twice is split at its midpoint, as git does; any other
/// line is split into two path tokens.
fn git_header_paths(rest: &str) -> (Option<String>, Option<String>) {
    let (old, new) = match same_path_header(rest) {
        Some(path) => (format!("a/{path}"), format!("b/{path}")),
        None => {
            let Some((old, remainder)) = take_path_token(rest) else {
                return (None, None);
            };
            let new = take_path_token(remainder)
                .map(|(new, _)| new)
                .unwrap_or_default();
            (old, new)
        }
    };
    (
        old.strip_prefix("a/").map(str::to_string),
        new.strip_prefix("b/").map(str::to_string),
    )
}

fn same_path_header(rest: &str) -> Option<&str> {
    if rest.starts_with('"') || rest.len() % 2 == 0 {
        return None;
    }
    let half = rest.len() / 2;
    let old = rest.get(..half)?.strip_prefix("a/")?;
    let new = rest.get(half..)?.strip_prefix(" b/")?;
    (old == new).then_some(old)
}

/// One path token: a git-quoted path, or the text up to the next whitespace.
fn take_path_token(raw: &str) -> Option<(String, &str)> {
    let raw = raw.trim_start();
    if raw.is_empty() {
        return None;
    }
    if let Some(quoted) = unquote_diff_path(raw) {
        return Some(quoted);
    }
    let end = raw.find(char::is_whitespace).unwrap_or(raw.len());
    Some((raw[..end].to_string(), &raw[end..]))
}

/// The path of a `rename from`/`rename to`/`copy from`/`copy to` line, which is the whole rest
/// of the line and carries no `a/` or `b/` prefix.
fn extended_header_path(value: &str) -> String {
    // `str::lines` keeps the `\r` of a CRLF diff's last line when it has no final newline.
    let value = value.trim_end_matches('\r');
    unquote_diff_path(value)
        .map(|(path, _)| path)
        .unwrap_or_else(|| value.to_string())
}

/// Git quotes a path holding a double quote, backslash, control or (under `core.quotePath`)
/// non-ASCII byte, with C escapes and octal bytes. Returns the decoded path and the text after
/// the closing quote, or `None` when `raw` does not start with a well-formed quoted path.
fn unquote_diff_path(raw: &str) -> Option<(String, &str)> {
    let inner = raw.strip_prefix('"')?;
    let bytes = inner.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'"' => {
                let path = String::from_utf8_lossy(&decoded).into_owned();
                return Some((path, &inner[index + 1..]));
            }
            b'\\' => {
                let escaped = *bytes.get(index + 1)?;
                if escaped.is_ascii_digit() {
                    let digits = std::str::from_utf8(bytes.get(index + 1..index + 4)?).ok()?;
                    decoded.push(u8::from_str_radix(digits, 8).ok()?);
                    index += 4;
                    continue;
                }
                decoded.push(match escaped {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 0x0b,
                    b'f' => 0x0c,
                    b'r' => b'\r',
                    other => other,
                });
                index += 2;
            }
            _ => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    None
}

/// One `@@` hunk of a unified diff. `new` is the post-edit range; `old` is the pre-edit
/// range when the hunk came from a diff, which is what an index built before the edit
/// still describes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChangedRegion {
    new: Option<LineRange>,
    old: Option<LineRange>,
}

/// The pre-edit and post-edit line ranges of one diff hunk, in that order; a side that holds
/// no lines is `None`.
pub type HunkRanges = (Option<LineRange>, Option<LineRange>);

/// Hunk ranges per path from the `@@ -a,b +c,d @@` headers of a unified diff, in file order.
/// A pure deletion (`+c,0`) is reported at line `c`, the line after which text was removed.
pub fn changed_hunks_from_unified_diff(diff: &str) -> BTreeMap<PathBuf, Vec<HunkRanges>> {
    scan_unified_diff(diff).hunks
}

/// Where a unified diff stops matching its own hunk headers, with the path of the entry it
/// happened in when one had been named. The file lists read from such a diff can miss an
/// entry whose headers an over-counted hunk consumed, or hold a content line read as a path.
fn unified_diff_malformation(diff: &str) -> Option<(Option<PathBuf>, MalformedDiff)> {
    scan_unified_diff(diff).malformed
}

struct UnifiedDiffScan {
    hunks: BTreeMap<PathBuf, Vec<HunkRanges>>,
    malformed: Option<(Option<PathBuf>, MalformedDiff)>,
}

fn scan_unified_diff(diff: &str) -> UnifiedDiffScan {
    let mut hunks = BTreeMap::<PathBuf, Vec<HunkRanges>>::new();
    let mut current: Option<PathBuf> = None;
    let mut pending_old: Option<String> = None;
    let mut scanner = HunkScanner::new();
    let mut malformed = None;
    for line in diff.lines() {
        let kind = scanner.scan(line);
        if malformed.is_none() {
            // Taken before this line is read as a header, so it names the entry the broken
            // hunk belongs to rather than the one the line starts.
            malformed = scanner.malformation().map(|malformation| {
                let path = current
                    .clone()
                    .or_else(|| pending_old.as_ref().map(PathBuf::from));
                (path, malformation.clone())
            });
        }
        match kind {
            DiffLine::Content => {}
            DiffLine::HunkHeader(header) => {
                let (Some(path), Some((old, new))) = (current.as_ref(), parse_hunk_header(header))
                else {
                    continue;
                };
                if old.is_some() || new.is_some() {
                    hunks.entry(path.clone()).or_default().push((old, new));
                }
            }
            DiffLine::Header => {
                if let Some(rest) = line.strip_prefix("diff --git ") {
                    pending_old = None;
                    current = git_header_paths(rest).1.map(PathBuf::from);
                } else if let Some(path) = line.strip_prefix("--- ") {
                    pending_old = diff_path(path);
                } else if let Some(path) = line.strip_prefix("+++ ") {
                    if let Some(path) = diff_path(path).or_else(|| pending_old.take()) {
                        current = Some(PathBuf::from(path));
                    }
                }
            }
        }
    }
    if malformed.is_none() {
        if let Err(malformation) = scanner.finish() {
            malformed = Some((current, malformation));
        }
    }
    UnifiedDiffScan { hunks, malformed }
}

/// `-a,b +c,d` (a `,count` of 1 may be omitted) into `(old, new)` line ranges.
fn parse_hunk_header(header: &str) -> Option<HunkRanges> {
    let mut parts = header.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    Some((hunk_side_range(old)?, hunk_side_range(new)?))
}

/// One side of a hunk header. `None` when the side does not parse; `Some(None)` when it holds
/// no lines (`+42,0` for a pure deletion, `-41,0` for a pure insertion), whose start is only an
/// anchor and must not be read as a changed line of the neighbouring symbol.
fn hunk_side_range(side: &str) -> Option<Option<LineRange>> {
    let (start, count) = match side.split_once(',') {
        Some((start, count)) => (start.parse::<u32>().ok()?, count.parse::<u32>().ok()?),
        None => (side.parse::<u32>().ok()?, 1),
    };
    if count == 0 {
        return Some(None);
    }
    let start = start.max(1);
    Some(Some(LineRange {
        start,
        end: start.saturating_add(count - 1),
    }))
}

/// A supplied diff whose hunks disagree with their headers fails verification: the changed
/// files read from it may lack an entry an over-counted hunk consumed, so no boundary or
/// forbidden-path check can vouch for the change.
fn malformed_diff_violation(diff: Option<&str>) -> Option<VerificationFinding> {
    let (path, malformation) = unified_diff_malformation(diff?)?;
    let entry = path
        .as_ref()
        .map(|path| format!(" in the entry for `{}`", normalize_path(path)))
        .unwrap_or_default();
    Some(VerificationFinding {
        path: path.map(|path| PathBuf::from(normalize_path(&path))),
        kind: "malformed_diff".into(),
        reason: format!(
            "the supplied diff is malformed{entry} at {malformation}; the changed files read \
             from it may be incomplete, so boundary checks cannot vouch for this change. \
             Supply the complete, unedited diff"
        ),
        evidence_refs: Vec::new(),
    })
}

fn changed_files_from_input(input: &VerifyChangeInput) -> Vec<PathBuf> {
    let mut paths = input
        .changed_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    paths.extend(input.changed_ranges.keys().map(|path| normalize_path(path)));
    if let Some(diff) = &input.unified_diff {
        paths.extend(
            changed_files_from_unified_diff(diff)
                .into_iter()
                .map(|p| normalize_path(&p)),
        );
    }
    paths.into_iter().map(PathBuf::from).collect()
}

fn changed_regions_from_input(
    input: &VerifyChangeInput,
    previous_paths: &[PreviousPath],
) -> BTreeMap<PathBuf, Vec<ChangedRegion>> {
    let mut regions = BTreeMap::<PathBuf, Vec<ChangedRegion>>::new();
    for (path, ranges) in &input.changed_ranges {
        regions
            .entry(PathBuf::from(normalize_path(path)))
            .or_default()
            .extend(ranges.iter().map(|range| ChangedRegion {
                new: Some(range.clone()),
                old: None,
            }));
    }
    if let Some(diff) = &input.unified_diff {
        for (path, hunks) in changed_hunks_from_unified_diff(diff) {
            let path = PathBuf::from(normalize_path(&path));
            let pair = previous_paths.iter().find(|previous| previous.path == path);
            for (old, new) in hunks {
                let Some(pair) = pair else {
                    regions
                        .entry(path.clone())
                        .or_default()
                        .push(ChangedRegion { new, old });
                    continue;
                };
                // A hunk's pre-edit lines are lines of the path the content came from, which the
                // index describes; its post-edit lines are lines of the new path. A copy's source
                // did not change, so its pre-edit side is dropped.
                if let Some(new) = new {
                    regions
                        .entry(path.clone())
                        .or_default()
                        .push(ChangedRegion {
                            new: Some(new),
                            old: None,
                        });
                }
                if let (Some(old), PreviousPathKind::Rename) = (old, pair.kind) {
                    regions
                        .entry(pair.previous_path.clone())
                        .or_default()
                        .push(ChangedRegion {
                            new: None,
                            old: Some(old),
                        });
                }
            }
        }
    }
    regions
}

fn diff_path(raw: &str) -> Option<String> {
    let path = match unquote_diff_path(raw) {
        Some((path, _)) => path,
        // A tab ends the name before a timestamp, and git appends one to a name that holds a
        // space; without a tab the name ends at the first whitespace.
        None if raw.contains('\t') => raw.split('\t').next().unwrap_or_default().to_string(),
        None => raw
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string(),
    };
    if path == "/dev/null" {
        return None;
    }
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(&path)
            .to_string(),
    )
}

fn boundary_violations(
    plan: &PlanReport,
    changed_files: &[PathBuf],
    previous_paths: &[PreviousPath],
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
    let forbidden_match = |normalized: &str| -> Option<(String, Vec<String>)> {
        if forbidden.contains(normalized) {
            return Some((
                "matches forbidden contract file".into(),
                boundary.evidence_refs.clone(),
            ));
        }
        boundary
            .forbidden_rules
            .iter()
            .find(|rule| boundary_pattern_matches(&rule.pattern, normalized))
            .map(|rule| {
                (
                    format!(
                        "matches forbidden pattern `{}`: {}",
                        rule.pattern, rule.reason
                    ),
                    rule.evidence_refs.clone(),
                )
            })
    };
    let mut findings = Vec::new();
    for path in changed_files {
        let normalized = normalize_path(path);
        let relation = previous_path_relation(previous_paths, &normalized);
        if let Some((reason, refs)) = forbidden_match(&normalized) {
            findings.push(VerificationFinding {
                path: Some(path.clone()),
                kind: "forbidden_boundary".into(),
                reason: format!("{reason}{relation}"),
                evidence_refs: refs,
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
                reason: format!(
                    "path is outside the saved plan boundary and no expansion evidence was supplied{relation}"
                ),
                evidence_refs: Vec::new(),
            });
        }
    }
    // A copy leaves its source in place, so the source is not a changed file; copying a
    // forbidden file elsewhere is still held to the forbidden rules.
    let changed = changed_files
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    for copy in previous_paths
        .iter()
        .filter(|previous| previous.kind == PreviousPathKind::Copy)
    {
        let source = normalize_path(&copy.previous_path);
        if changed.contains(&source) {
            continue;
        }
        if let Some((reason, refs)) = forbidden_match(&source) {
            findings.push(VerificationFinding {
                reason: format!(
                    "copy source {reason} (copied to `{}`)",
                    normalize_path(&copy.path)
                ),
                path: Some(PathBuf::from(source)),
                kind: "forbidden_boundary".into(),
                evidence_refs: refs,
            });
        }
    }
    findings
}

/// The other side of a rename or copy that `path` belongs to, as a suffix for a boundary
/// finding so the finding names both paths. Empty when `path` is neither.
fn previous_path_relation(previous_paths: &[PreviousPath], path: &str) -> String {
    for previous in previous_paths {
        let old = normalize_path(&previous.previous_path);
        let new = normalize_path(&previous.path);
        match previous.kind {
            PreviousPathKind::Rename if old == path => {
                return format!(" (renamed to `{new}`)");
            }
            PreviousPathKind::Rename if new == path => {
                return format!(" (renamed from `{old}`)");
            }
            PreviousPathKind::Copy if new == path => {
                return format!(" (copied from `{old}`)");
            }
            _ => {}
        }
    }
    String::new()
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

/// Warning kind for a path whose changed symbols could only be attributed at file level.
pub const SYMBOL_GRANULARITY_WARNING: &str = "symbol_granularity";

struct ChangedSymbols {
    symbols: Vec<String>,
    regions_without_symbol: Vec<String>,
    granularity_warnings: Vec<VerificationFinding>,
}

/// Symbols whose indexed ranges overlap a changed region, innermost per region so a module
/// or class symbol cannot stand in for the whole file, plus any symbol the region covers
/// entirely, since a hunk that replaces a whole `impl` changed the `impl` itself. A path with
/// no regions falls back to every symbol in the file and says why; a region no symbol covers
/// is reported, not dropped.
fn changed_symbols(
    store: &dyn MetadataStore,
    changed_files: &[PathBuf],
    changed_regions: &BTreeMap<PathBuf, Vec<ChangedRegion>>,
    scoped_paths: &BTreeSet<PathBuf>,
    unified_diff: Option<&str>,
) -> Result<ChangedSymbols> {
    // A path the supplied diff names but states no hunk for, once the scoped sides of a text
    // rename are skipped below: a binary or mode-only entry. That is a different caveat from a
    // path no diff described at all.
    let diff_paths = unified_diff
        .map(changed_files_from_unified_diff)
        .unwrap_or_default()
        .iter()
        .map(|path| normalize_path(path))
        .collect::<BTreeSet<_>>();
    let mut symbols = BTreeSet::new();
    let mut regions_without_symbol = Vec::new();
    let mut granularity_warnings = Vec::new();
    for path in changed_files {
        let regions = changed_regions
            .get(path)
            .map(Vec::as_slice)
            .unwrap_or_default();
        // The diff states every changed line of this path and names none: a side of a rename
        // that the edit did not touch. Listing the whole file would claim a change it lacks.
        if regions.is_empty() && scoped_paths.contains(path) {
            continue;
        }
        let file_symbols = match store.get_file_by_path(path)? {
            Some(file) => store.symbols_for_file(&file.id)?,
            None => Vec::new(),
        };
        let ranged = file_symbols.iter().any(|symbol| symbol.range.is_some());
        if regions.is_empty() || !ranged {
            if !file_symbols.is_empty() {
                let reason = if !regions.is_empty() {
                    "indexed symbols for this path carry no line ranges, so changed_symbols lists every symbol in the file"
                } else if diff_paths.contains(&normalize_path(path)) {
                    "the supplied diff has no hunk ranges for this path (a binary or mode-only entry), so changed_symbols lists every symbol in the file"
                } else {
                    "no diff was supplied for this path, so changed_symbols lists every symbol in the file"
                };
                granularity_warnings.push(VerificationFinding {
                    path: Some(path.clone()),
                    kind: SYMBOL_GRANULARITY_WARNING.into(),
                    reason: reason.into(),
                    evidence_refs: Vec::new(),
                });
            }
            if file_symbols.is_empty() {
                regions_without_symbol
                    .extend(regions.iter().map(|region| region_label(path, region)));
            }
            symbols.extend(file_symbols.into_iter().map(|symbol| symbol.qualified_name));
            continue;
        }
        for region in regions {
            let overlapping = file_symbols
                .iter()
                .filter(|symbol| symbol_overlaps_region(symbol, region))
                .collect::<Vec<_>>();
            let attributed = overlapping
                .iter()
                .filter(|symbol| {
                    region_covers_symbol(symbol, region)
                        || !overlapping
                            .iter()
                            .any(|other| other.id != symbol.id && symbol_contains(symbol, other))
                })
                .map(|symbol| symbol.qualified_name.clone())
                .collect::<Vec<_>>();
            if attributed.is_empty() {
                regions_without_symbol.push(region_label(path, region));
            }
            symbols.extend(attributed);
        }
    }
    Ok(ChangedSymbols {
        symbols: symbols.into_iter().collect(),
        regions_without_symbol,
        granularity_warnings,
    })
}

/// Whether one side of `region` spans every line of `symbol`'s indexed range.
fn region_covers_symbol(symbol: &Symbol, region: &ChangedRegion) -> bool {
    let Some(range) = &symbol.range else {
        return false;
    };
    [&region.new, &region.old]
        .into_iter()
        .flatten()
        .any(|side| side.start <= range.start && range.end <= side.end)
}

fn region_label(path: &Path, region: &ChangedRegion) -> String {
    match (&region.new, &region.old) {
        (Some(new), _) => format!("{}:{}-{}", path.display(), new.start, new.end),
        (None, Some(old)) => format!(
            "{}:{}-{} (removed; pre-edit lines)",
            path.display(),
            old.start,
            old.end
        ),
        (None, None) => path.display().to_string(),
    }
}

fn ranges_overlap(left: &LineRange, right: &LineRange) -> bool {
    left.start <= right.end && right.start <= left.end
}

fn symbol_overlaps_region(symbol: &Symbol, region: &ChangedRegion) -> bool {
    let Some(range) = &symbol.range else {
        return false;
    };
    region
        .new
        .as_ref()
        .is_some_and(|new| ranges_overlap(range, new))
        || region
            .old
            .as_ref()
            .is_some_and(|old| ranges_overlap(range, old))
}

/// Whether `outer` strictly encloses `inner`, so that `inner` is the more precise attribution.
fn symbol_contains(outer: &Symbol, inner: &Symbol) -> bool {
    match (&outer.range, &inner.range) {
        (Some(outer), Some(inner)) => {
            outer.start <= inner.start && inner.end <= outer.end && outer != inner
        }
        _ => false,
    }
}

fn recommended_tests(store: &dyn OkStore, changed_files: &[PathBuf]) -> Result<Vec<TestTarget>> {
    let selector = TestSelector::new(store);
    let mut tests = Vec::new();
    for path in changed_files {
        tests.extend(selector.for_changed_path_with_evidence(path, 8)?);
    }
    // The plan's predicate, not its bounds. Verify keeps every plausible recommendation: capping
    // here would let a verdict pass because the unplanned targets fell past a limit, and the
    // plan's per-file suite preference would drop registration targets that sit beside a helper.
    Ok(open_kioku_plan::plausible_validation_targets(tests))
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
    let impact_engine = ImpactEngine::new(store)
        .with_search_index(search_index)
        .with_graph_store(Some(store));
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
        BoundaryForbiddenRule, ChangeBoundary, CodeChunk, Confidence, ConfidenceBreakdown, File,
        FileId, GraphEdge, GraphEdgeType, GraphNode, GraphNodeType, Import, IndexManifest,
        Language, LineRange, RepositoryId, RiskReport, Symbol, SymbolId, SymbolOccurrence,
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
        test_targets: Vec<TestTarget>,
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
                test_targets: Vec::new(),
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

        fn with_test_target(mut self, target: TestTarget) -> Self {
            self.test_targets.push(target);
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

    fn registration_target(name: &str, file_id: &str, disabled: bool) -> TestTarget {
        TestTarget {
            selection_tier: open_kioku_core::TestSelectionTier::default(),
            tier_justification: Vec::new(),
            id: format!("registration:{name}"),
            name: name.into(),
            file_id: FileId::new(file_id),
            range: Some(LineRange { start: 1, end: 2 }),
            command: Some("npm test".into()),
            confidence: if disabled {
                Confidence::Low
            } else {
                Confidence::High
            },
            reason: "test registration call in a test-path file".into(),
            evidence_refs: Vec::new(),
            score_breakdown: Vec::new(),
            origin: if disabled {
                open_kioku_core::TestTargetOrigin::DisabledRegistrationCall
            } else {
                open_kioku_core::TestTargetOrigin::RegistrationCall
            },
        }
    }

    fn file_symbol_target(name: &str, file_id: &str, confidence: Confidence) -> TestTarget {
        TestTarget {
            selection_tier: open_kioku_core::TestSelectionTier::default(),
            tier_justification: Vec::new(),
            id: format!("file-symbol:{name}"),
            name: name.into(),
            file_id: FileId::new(file_id),
            range: Some(LineRange { start: 1, end: 2 }),
            command: Some("cargo test".into()),
            confidence,
            reason: "test-like path, annotation, or naming convention".into(),
            evidence_refs: Vec::new(),
            score_breakdown: Vec::new(),
            origin: open_kioku_core::TestTargetOrigin::TestFileSymbol,
        }
    }

    /// Verify must recommend every plausible test, not the plan's bounded selection. A change
    /// touching two files whose second file contributes only unplanned tests used to pass
    /// silently once the first file filled the plan's cap: exit 0 would have meant "the first
    /// eight recommendations were planned".
    #[test]
    fn verify_recommends_past_the_plans_cap_so_the_verdict_cannot_hide_unplanned_tests() {
        let mut store = RuntimeStore::new()
            .with_file_text("tests/alpha_test.rs", "fn alpha_one() {}")
            .with_file_text("tests/beta_test.rs", "fn beta_one() {}");
        let planned = (0..8)
            .map(|index| {
                file_symbol_target(
                    &format!("alpha_case_{index}"),
                    "tests_alpha_test_rs",
                    Confidence::High,
                )
            })
            .collect::<Vec<_>>();
        let unplanned = (0..6)
            .map(|index| {
                file_symbol_target(
                    &format!("beta_case_{index}"),
                    "tests_beta_test_rs",
                    Confidence::Medium,
                )
            })
            .collect::<Vec<_>>();
        for target in planned.iter().chain(unplanned.iter()) {
            store = store.with_test_target(target.clone());
        }

        let recommended = recommended_tests(
            &store,
            &[PathBuf::from("src/alpha.rs"), PathBuf::from("src/beta.rs")],
        )
        .unwrap();
        let names = recommended
            .iter()
            .map(|test| test.name.as_str())
            .collect::<BTreeSet<_>>();
        for target in planned.iter().chain(unplanned.iter()) {
            assert!(names.contains(target.name.as_str()), "{}", target.name);
        }
        // Exactly the fourteen, so the assertion cannot pass because the stem match pulled every
        // target in for both paths and the count merely looked large enough.
        assert_eq!(names.len(), 14, "{names:?}");
        assert_eq!(recommended.len(), 14, "{recommended:?}");

        let mut plan = plan_with_validation_command("cargo test");
        plan.validation = planned;
        let missing = missing_tests(&plan, &recommended);
        assert_eq!(missing.len(), 6, "{missing:?}");
    }

    /// `ok verify` recommends what the plan would have planned. A test registered by a runner
    /// call is plannable, so it must not be reported as missing; a disabled one is planned by
    /// neither side, so it must not be recommended either.
    #[test]
    fn recommended_tests_keep_a_registered_test_and_drop_a_disabled_one() {
        let store = RuntimeStore::new()
            .with_file_text("tests/rates_test.ts", "test(\"rounds half up\", () => {});")
            .with_test_target(registration_target(
                "rounds half up",
                "tests_rates_test_ts",
                false,
            ))
            .with_test_target(registration_target(
                "skips stale rows",
                "tests_rates_test_ts",
                true,
            ));

        let recommended = recommended_tests(&store, &[PathBuf::from("src/rates.ts")]).unwrap();
        let names = recommended
            .iter()
            .map(|test| test.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"rounds half up"), "{names:?}");
        assert!(!names.contains(&"skips stale rows"), "{names:?}");

        let mut plan = plan_with_validation_command("npm test");
        plan.validation = recommended.clone();
        assert!(missing_tests(&plan, &recommended).is_empty());
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
            Ok(self.test_targets.clone())
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

    fn handler_symbol(name: &str, kind: SymbolKind, start: u32, end: u32) -> Symbol {
        Symbol {
            id: SymbolId::new(format!("symbol-{name}")),
            name: name.into(),
            qualified_name: format!("handler::{name}"),
            kind,
            file_id: FileId::new("handler"),
            range: Some(LineRange { start, end }),
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

    fn store_with_handler_symbols() -> RuntimeStore {
        let mut store = RuntimeStore::new().without_runtime();
        store.symbols = vec![
            handler_symbol("handler", SymbolKind::Module, 1, 40),
            handler_symbol("checkout", SymbolKind::Function, 5, 12),
            handler_symbol("refund", SymbolKind::Function, 14, 20),
        ];
        store
    }

    fn verify_handler(store: &RuntimeStore, input: VerifyChangeInput) -> ChangeVerificationReport {
        ChangeVerifier::new(store)
            .verify(Path::new("."), &plan_with_boundary_evidence(), input)
            .unwrap()
    }

    #[test]
    fn parses_hunk_headers_with_and_without_counts() {
        let diff = "diff --git a/src/handler.rs b/src/handler.rs\n--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -6 +6 @@ fn checkout() {\n-old\n+new\n@@ -41,0 +42,3 @@\n+// a\n";
        let hunks = changed_hunks_from_unified_diff(diff);
        assert_eq!(
            hunks.get(Path::new("src/handler.rs")).unwrap(),
            &vec![
                (
                    Some(LineRange { start: 6, end: 6 }),
                    Some(LineRange { start: 6, end: 6 })
                ),
                (None, Some(LineRange { start: 42, end: 44 })),
            ]
        );
    }

    fn plan_forbidding_secrets(allowed_files: &[&str]) -> PlanReport {
        let mut plan = plan_with_boundary_evidence();
        plan.recommended_change_boundary.allowed_files =
            allowed_files.iter().map(PathBuf::from).collect();
        plan.recommended_change_boundary.forbidden_rules = vec![BoundaryForbiddenRule {
            pattern: "src/secrets/**".into(),
            reason: "secrets stay in place".into(),
            evidence_refs: vec!["boundary:forbid-secrets".into()],
        }];
        plan
    }

    fn verify_diff(plan: &PlanReport, diff: &str) -> ChangeVerificationReport {
        let store = RuntimeStore::new().without_runtime();
        ChangeVerifier::new(&store)
            .verify(
                Path::new("."),
                plan,
                VerifyChangeInput {
                    unified_diff: Some(diff.into()),
                    ..Default::default()
                },
            )
            .unwrap()
    }

    #[test]
    fn diff_entries_record_both_sides_of_renames_and_copies_and_the_old_side_of_deletions() {
        let diff = r#"diff --git a/src/secrets/keys.rs b/src/keys.rs
similarity index 91%
--- a/src/secrets/keys.rs
+++ b/src/keys.rs
@@ -2 +2 @@
-    old();
+    new();
diff --git a/src/template.rs b/src/generated.rs
similarity index 100%
copy from src/template.rs
copy to src/generated.rs
diff --git a/src/gone.rs b/src/gone.rs
deleted file mode 100644
--- a/src/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-pub fn gone() {}
diff --git "a/src/caf\303\251 menu.rs" b/src/menu.rs
similarity index 100%
rename from "src/caf\303\251 menu.rs"
rename to src/menu.rs
"#;

        assert_eq!(
            changed_files_from_unified_diff(diff),
            [
                "src/caf\u{e9} menu.rs",
                "src/generated.rs",
                "src/gone.rs",
                "src/keys.rs",
                "src/menu.rs",
                "src/secrets/keys.rs",
            ]
            .map(PathBuf::from)
        );
        let previous = |path: &str, previous_path: &str, kind| PreviousPath {
            path: path.into(),
            previous_path: previous_path.into(),
            kind,
        };
        assert_eq!(
            previous_paths_from_unified_diff(diff),
            vec![
                previous(
                    "src/keys.rs",
                    "src/secrets/keys.rs",
                    PreviousPathKind::Rename
                ),
                previous(
                    "src/generated.rs",
                    "src/template.rs",
                    PreviousPathKind::Copy
                ),
                previous(
                    "src/menu.rs",
                    "src/caf\u{e9} menu.rs",
                    PreviousPathKind::Rename
                ),
            ]
        );

        // A plain unified diff names a backup as its old side; that is not a rename.
        let plain = "--- src/lib.rs.orig\t2026-09-01 10:00:00\n+++ src/lib.rs\t2026-09-01 10:05:00\n@@ -1 +1 @@\n-a\n+b\n";
        assert_eq!(
            changed_files_from_unified_diff(plain),
            vec![PathBuf::from("src/lib.rs")]
        );
        assert!(previous_paths_from_unified_diff(plain).is_empty());
    }

    #[test]
    fn a_rename_out_of_a_forbidden_directory_fails_naming_both_paths() {
        let plan = plan_forbidding_secrets(&["src/handler.rs", "src/keys.rs"]);
        let report = verify_diff(
            &plan,
            "diff --git a/src/secrets/keys.rs b/src/keys.rs\nsimilarity index 100%\nrename from src/secrets/keys.rs\nrename to src/keys.rs\n",
        );

        assert_eq!(report.verdict, VerificationVerdict::Fail);
        assert_eq!(
            report.changed_files,
            vec![
                PathBuf::from("src/keys.rs"),
                PathBuf::from("src/secrets/keys.rs")
            ]
        );
        assert_eq!(
            report.previous_paths,
            vec![PreviousPath {
                path: PathBuf::from("src/keys.rs"),
                previous_path: PathBuf::from("src/secrets/keys.rs"),
                kind: PreviousPathKind::Rename,
            }]
        );
        let violation = report
            .boundary_violations
            .iter()
            .find(|finding| finding.kind == "forbidden_boundary")
            .expect("the previous path of the rename is forbidden");
        assert_eq!(
            violation.path.as_deref(),
            Some(Path::new("src/secrets/keys.rs"))
        );
        assert!(
            violation.reason.contains("`src/secrets/**`")
                && violation.reason.contains("renamed to `src/keys.rs`"),
            "{}",
            violation.reason
        );
    }

    #[test]
    fn a_rename_into_the_boundary_from_outside_fails_out_of_boundary_for_the_old_path() {
        let plan = plan_forbidding_secrets(&["src/handler.rs"]);
        let report = verify_diff(
            &plan,
            "diff --git a/src/outside.rs b/src/handler.rs\nsimilarity index 100%\nrename from src/outside.rs\nrename to src/handler.rs\n",
        );

        assert_eq!(report.verdict, VerificationVerdict::Fail);
        let out_of_boundary = report
            .boundary_violations
            .iter()
            .filter(|finding| finding.kind == "out_of_boundary")
            .collect::<Vec<_>>();
        assert_eq!(out_of_boundary.len(), 1, "{out_of_boundary:?}");
        assert_eq!(
            out_of_boundary[0].path.as_deref(),
            Some(Path::new("src/outside.rs"))
        );
        assert!(
            out_of_boundary[0]
                .reason
                .contains("renamed to `src/handler.rs`"),
            "{}",
            out_of_boundary[0].reason
        );
    }

    #[test]
    fn a_rename_within_the_boundary_passes_the_boundary_check() {
        let plan = plan_forbidding_secrets(&["src/handler.rs", "src/checkout.rs"]);
        let report = verify_diff(
            &plan,
            "diff --git a/src/handler.rs b/src/checkout.rs\nsimilarity index 100%\nrename from src/handler.rs\nrename to src/checkout.rs\n",
        );

        assert!(
            report.boundary_violations.is_empty(),
            "{:?}",
            report.boundary_violations
        );
        assert_ne!(report.verdict, VerificationVerdict::Fail);
        assert_eq!(
            report.changed_files,
            vec![
                PathBuf::from("src/checkout.rs"),
                PathBuf::from("src/handler.rs")
            ]
        );
    }

    #[test]
    fn a_copy_from_a_forbidden_path_fails_on_the_copy_source() {
        let plan = plan_forbidding_secrets(&["src/handler.rs"]);
        let report = verify_diff(
            &plan,
            "diff --git a/src/secrets/keys.rs b/src/handler.rs\nsimilarity index 100%\ncopy from src/secrets/keys.rs\ncopy to src/handler.rs\n",
        );

        assert_eq!(report.verdict, VerificationVerdict::Fail);
        assert_eq!(report.changed_files, vec![PathBuf::from("src/handler.rs")]);
        let violation = report
            .boundary_violations
            .iter()
            .find(|finding| finding.kind == "forbidden_boundary")
            .expect("the copy source is forbidden");
        assert_eq!(
            violation.path.as_deref(),
            Some(Path::new("src/secrets/keys.rs"))
        );
        assert!(
            violation
                .reason
                .starts_with("copy source matches forbidden pattern")
                && violation.reason.contains("copied to `src/handler.rs`"),
            "{}",
            violation.reason
        );
    }

    #[test]
    fn hunk_lines_that_look_like_file_headers_name_no_path_in_a_git_entry() {
        let modified = "diff --git a/db/schema.sql b/db/schema.sql\n--- a/db/schema.sql\n+++ b/db/schema.sql\n@@ -3 +3 @@\n--- legacy index\n+++ replacement index\n";
        assert_eq!(
            changed_files_from_unified_diff(modified),
            vec![PathBuf::from("db/schema.sql")]
        );
        assert!(previous_paths_from_unified_diff(modified).is_empty());
        assert_eq!(
            changed_hunks_from_unified_diff(modified)
                .keys()
                .collect::<Vec<_>>(),
            vec![Path::new("db/schema.sql")]
        );

        let renamed = "diff --git a/db/old.sql b/db/new.sql\nsimilarity index 91%\nrename from db/old.sql\nrename to db/new.sql\n--- a/db/old.sql\n+++ b/db/new.sql\n@@ -1 +1 @@\n--- dropped view\n+++ kept view\n";
        assert_eq!(
            changed_files_from_unified_diff(renamed),
            vec![PathBuf::from("db/new.sql"), PathBuf::from("db/old.sql")]
        );
        assert_eq!(
            previous_paths_from_unified_diff(renamed),
            vec![PreviousPath {
                path: PathBuf::from("db/new.sql"),
                previous_path: PathBuf::from("db/old.sql"),
                kind: PreviousPathKind::Rename,
            }]
        );
        assert_eq!(
            changed_hunks_from_unified_diff(renamed)
                .keys()
                .collect::<Vec<_>>(),
            vec![Path::new("db/new.sql")]
        );
    }

    #[test]
    fn a_plain_entry_after_a_git_entry_is_still_a_changed_file() {
        // `git diff > p.diff; diff -u a b >> p.diff` joins a git entry and a plain entry.
        let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n--- src/b.rs.orig\n+++ src/b.rs\n@@ -1,2 +1,2 @@\n context\n-x\n+y\n";

        assert_eq!(
            changed_files_from_unified_diff(diff),
            vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")]
        );
        assert_eq!(
            changed_hunks_from_unified_diff(diff)
                .keys()
                .collect::<Vec<_>>(),
            vec![Path::new("src/a.rs"), Path::new("src/b.rs")]
        );
        let report = verify_diff(&plan_forbidding_secrets(&["src/a.rs"]), diff);
        assert!(
            report.boundary_violations.iter().any(|finding| {
                finding.kind == "out_of_boundary"
                    && finding.path.as_deref() == Some(Path::new("src/b.rs"))
            }),
            "{:?}",
            report.boundary_violations
        );
    }

    #[test]
    fn plain_diff_hunk_counts_decide_where_content_ends() {
        // Removed `-- x` and added `++ y` lines inside counted hunks, and hunk headers whose
        // single-line counts are omitted.
        let diff = "--- a/db/schema.sql\n+++ b/db/schema.sql\n@@ -3,2 +3,2 @@\n--- legacy index\n+++ replacement index\n--- old view\n+++ new view\n--- a/src/x.rs\n+++ b/src/x.rs\n@@ -3 +3 @@\n--- one\n+++ two\n--- a/src/y.rs\n+++ b/src/y.rs\n@@ -1 +0,0 @@\n--- gone\n";

        assert_eq!(
            changed_files_from_unified_diff(diff),
            vec![
                PathBuf::from("db/schema.sql"),
                PathBuf::from("src/x.rs"),
                PathBuf::from("src/y.rs")
            ]
        );
        assert_eq!(
            changed_hunks_from_unified_diff(diff)
                .iter()
                .map(|(path, hunks)| (path.as_path(), hunks.len()))
                .collect::<Vec<_>>(),
            vec![
                (Path::new("db/schema.sql"), 1),
                (Path::new("src/x.rs"), 1),
                (Path::new("src/y.rs"), 1)
            ]
        );
    }

    fn malformed_diff_finding(report: &ChangeVerificationReport) -> Option<&VerificationFinding> {
        report
            .boundary_violations
            .iter()
            .find(|finding| finding.kind == "malformed_diff")
    }

    #[test]
    fn a_well_formed_diff_is_not_reported_as_malformed() {
        let diff = "--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -1,2 +1,2 @@\n context\n--- old\n+++ new\n\\ No newline at end of file\n";
        let report = verify_diff(&plan_forbidding_secrets(&["src/handler.rs"]), diff);
        assert!(malformed_diff_finding(&report).is_none(), "{report:?}");
        assert_eq!(report.changed_files, vec![PathBuf::from("src/handler.rs")]);
    }

    #[test]
    fn an_over_counted_hunk_that_swallows_the_next_entry_fails_naming_its_file() {
        // The first hunk declares three added lines; the next entry's headers would be two of
        // them, leaving `src/secrets/keys.rs` out of the changed files.
        let diff = "--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -1,2 +1,3 @@\n-a\n+b\n--- a/src/secrets/keys.rs\n+++ b/src/secrets/keys.rs\n@@ -1 +1 @@\n-c\n+d\n";
        let report = verify_diff(&plan_forbidding_secrets(&["src/handler.rs"]), diff);

        assert_eq!(report.verdict, VerificationVerdict::Fail);
        let finding = malformed_diff_finding(&report).expect("malformed diff is reported");
        assert_eq!(finding.path.as_deref(), Some(Path::new("src/handler.rs")));
        assert!(finding.reason.contains("line 8"), "{}", finding.reason);
    }

    #[test]
    fn a_truncated_diff_fails_naming_the_file_it_stops_in() {
        // `git diff | head` cuts the last hunk short.
        let diff = "diff --git a/src/handler.rs b/src/handler.rs\n--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -1,4 +1,4 @@\n-a\n-b\n+c\n";
        let report = verify_diff(&plan_forbidding_secrets(&["src/handler.rs"]), diff);

        assert_eq!(report.verdict, VerificationVerdict::Fail);
        let finding = malformed_diff_finding(&report).expect("malformed diff is reported");
        assert_eq!(finding.path.as_deref(), Some(Path::new("src/handler.rs")));
    }

    #[test]
    fn an_under_counted_hunk_fails_rather_than_reading_content_as_a_path() {
        let diff = "--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -1 +1 @@\n-a\n+b\n+++ b/src/elsewhere.rs\n+more\n";
        let report = verify_diff(&plan_forbidding_secrets(&["src/handler.rs"]), diff);

        assert_eq!(report.verdict, VerificationVerdict::Fail);
        let finding = malformed_diff_finding(&report).expect("malformed diff is reported");
        assert_eq!(finding.path.as_deref(), Some(Path::new("src/handler.rs")));
        assert!(finding.reason.contains("line 6"), "{}", finding.reason);
    }

    #[test]
    fn a_hunk_header_whose_count_does_not_parse_fails() {
        // An unparsed count used to become zero, leaving the hunk's lines to be read as
        // headers.
        let diff = "--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -1,x +1,2 @@\n-a\n+b\n+c\n";
        let report = verify_diff(&plan_forbidding_secrets(&["src/handler.rs"]), diff);

        assert_eq!(report.verdict, VerificationVerdict::Fail);
        let finding = malformed_diff_finding(&report).expect("malformed diff is reported");
        assert_eq!(finding.path.as_deref(), Some(Path::new("src/handler.rs")));
        assert!(
            finding.reason.contains("does not parse"),
            "{}",
            finding.reason
        );
    }

    #[test]
    fn a_requested_delta_check_that_cannot_run_on_the_plan_path_is_reported() {
        let mut plan = plan_with_boundary_evidence();
        plan.recommended_change_boundary.evidence_refs.clear();
        assert!(ContractBuilder::from_plan(&plan).is_err());
        let store = RuntimeStore::new().without_runtime();
        let verify = |check_api_surface, check_dependency_delta| {
            ChangeVerifier::new(&store)
                .verify(
                    Path::new("."),
                    &plan,
                    VerifyChangeInput {
                        changed_files: vec![PathBuf::from("src/handler.rs")],
                        check_api_surface,
                        check_dependency_delta,
                        ..Default::default()
                    },
                )
                .unwrap()
        };
        let kinds = |report: &ChangeVerificationReport| {
            report
                .warnings
                .iter()
                .map(|warning| warning.kind.as_str())
                .filter(|kind| kind.ends_with("_check_not_run"))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };

        let unrequested = verify(false, false);
        assert!(kinds(&unrequested).is_empty(), "{unrequested:?}");

        let requested = verify(true, true);
        assert_eq!(
            kinds(&requested),
            vec![
                "api_surface_check_not_run",
                "dependency_delta_check_not_run"
            ]
        );
        assert!(requested.api_surface_deltas.is_empty());
        assert_ne!(requested.verdict, VerificationVerdict::Pass);
        let reason = &requested
            .warnings
            .iter()
            .find(|w| w.kind == "api_surface_check_not_run")
            .unwrap()
            .reason;
        assert!(reason.contains("evidence reference"), "{reason}");
    }

    #[test]
    fn a_crlf_rename_matches_exact_forbidden_files() {
        let mut plan = plan_forbidding_secrets(&["src/handler.rs", "src/moved.rs"]);
        plan.recommended_change_boundary.forbidden_files = vec![PathBuf::from("src/forbidden.rs")];
        // The last line has no final newline, so `str::lines` leaves its `\r` in place.
        let report = verify_diff(
            &plan,
            "diff --git a/src/forbidden.rs b/src/moved.rs\r\nsimilarity index 100%\r\nrename to src/moved.rs\r\nrename from src/forbidden.rs\r",
        );

        assert_eq!(
            report.previous_paths,
            vec![PreviousPath {
                path: PathBuf::from("src/moved.rs"),
                previous_path: PathBuf::from("src/forbidden.rs"),
                kind: PreviousPathKind::Rename,
            }]
        );
        assert_eq!(
            report.changed_files,
            vec![
                PathBuf::from("src/forbidden.rs"),
                PathBuf::from("src/moved.rs")
            ]
        );
        assert!(
            report.boundary_violations.iter().any(|finding| {
                finding.kind == "forbidden_boundary"
                    && finding.path.as_deref() == Some(Path::new("src/forbidden.rs"))
                    && finding
                        .reason
                        .starts_with("matches forbidden contract file")
            }),
            "{:?}",
            report.boundary_violations
        );
    }

    #[test]
    fn a_rename_with_edits_scopes_changed_symbols_to_each_side_of_its_hunks() {
        let store = store_with_handler_symbols();
        let plan = plan_forbidding_secrets(&["src/handler.rs", "src/checkout.rs"]);
        let report = ChangeVerifier::new(&store)
            .verify(
                Path::new("."),
                &plan,
                VerifyChangeInput {
                    unified_diff: Some(
                        "diff --git a/src/handler.rs b/src/checkout.rs\nsimilarity index 95%\nrename from src/handler.rs\nrename to src/checkout.rs\n--- a/src/handler.rs\n+++ b/src/checkout.rs\n@@ -6 +6 @@\n-    old();\n+    new();\n".into(),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();

        // Pre-edit line 6 is read against the previous path, which the index holds; post-edit
        // line 6 belongs to the new path, which it does not.
        assert_eq!(
            report.changed_symbols,
            vec!["handler::checkout".to_string()]
        );
        assert_eq!(
            report.changed_regions_without_symbol,
            vec!["src/checkout.rs:6-6".to_string()]
        );
        assert!(
            !report
                .warnings
                .iter()
                .any(|warning| warning.kind == SYMBOL_GRANULARITY_WARNING),
            "{:?}",
            report.warnings
        );
    }

    #[test]
    fn a_pure_rename_lists_no_changed_symbols_and_no_granularity_warning() {
        let store = store_with_handler_symbols();
        let plan = plan_forbidding_secrets(&["src/handler.rs", "src/checkout.rs"]);
        let report = ChangeVerifier::new(&store)
            .verify(
                Path::new("."),
                &plan,
                VerifyChangeInput {
                    unified_diff: Some(
                        "diff --git a/src/handler.rs b/src/checkout.rs\nsimilarity index 100%\nrename from src/handler.rs\nrename to src/checkout.rs\n".into(),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();

        assert!(
            report.changed_symbols.is_empty(),
            "{:?}",
            report.changed_symbols
        );
        assert!(report.changed_regions_without_symbol.is_empty());
        assert!(
            !report
                .warnings
                .iter()
                .any(|warning| warning.kind == SYMBOL_GRANULARITY_WARNING),
            "{:?}",
            report.warnings
        );
    }

    fn verify_api_surface_of_rename(
        indexed: &str,
        renamed: &str,
        diff: &str,
        constraints: Vec<ApiSurfaceConstraint>,
    ) -> ContractVerificationReport {
        let repo = tempfile::tempdir().unwrap();
        fs::create_dir_all(repo.path().join("src")).unwrap();
        fs::write(repo.path().join("src/checkout.rs"), renamed).unwrap();
        let store = RuntimeStore::new()
            .without_runtime()
            .with_file_text("src/handler.rs", indexed);
        let mut contract = ContractBuilder::from_plan(&plan_forbidding_secrets(&[
            "src/handler.rs",
            "src/checkout.rs",
        ]))
        .unwrap();
        contract.api_surface_constraints = constraints
            .into_iter()
            .map(|mut constraint| {
                constraint.evidence_refs = contract.evidence_refs.clone();
                constraint
            })
            .collect();
        ContractVerifier::new(&store)
            .verify(
                repo.path(),
                &contract,
                VerifyChangeInput {
                    unified_diff: Some(diff.into()),
                    check_api_surface: true,
                    ..Default::default()
                },
            )
            .unwrap()
    }

    #[test]
    fn a_rename_that_keeps_its_public_api_warns_as_moved() {
        let report = verify_api_surface_of_rename(
            "pub fn handle() {}\n",
            "pub fn handle() {}\n",
            "diff --git a/src/handler.rs b/src/checkout.rs\nsimilarity index 100%\nrename from src/handler.rs\nrename to src/checkout.rs\n",
            Vec::new(),
        );

        assert_ne!(report.decision, VerificationDecision::Fail);
        assert!(
            !report
                .change_report
                .boundary_violations
                .iter()
                .any(|finding| finding.kind.starts_with("api_surface")),
            "{:?}",
            report.change_report.boundary_violations
        );
        let moved = report
            .change_report
            .warnings
            .iter()
            .find(|finding| finding.kind == "api_surface_moved")
            .expect("the unchanged public fn is reported as moved");
        assert!(
            moved.reason.contains("`handle`")
                && moved.reason.contains("`src/handler.rs`")
                && moved.reason.contains("`src/checkout.rs`"),
            "{}",
            moved.reason
        );
        assert!(!report
            .change_report
            .api_surface_deltas
            .iter()
            .any(|finding| finding.kind == "api_surface_review_required"));
    }

    #[test]
    fn a_rename_out_of_a_scope_that_forbids_api_removals_fails() {
        let report = verify_api_surface_of_rename(
            "pub fn handle() {}\n",
            "pub fn handle() {}\n",
            "diff --git a/src/handler.rs b/src/checkout.rs\nsimilarity index 100%\nrename from src/handler.rs\nrename to src/checkout.rs\n",
            vec![ApiSurfaceConstraint {
                scope: "src/handler.rs".into(),
                allowed_changes: Vec::new(),
                severity: ConstraintSeverity::Forbidden,
                reason: "the handler API is frozen".into(),
                evidence_refs: Vec::new(),
            }],
        );

        assert_eq!(report.decision, VerificationDecision::Fail);
        let violation = report
            .change_report
            .boundary_violations
            .iter()
            .find(|finding| finding.kind == "api_surface_violation")
            .expect("a move out of a scope that forbids removals fails");
        assert_eq!(violation.path.as_deref(), Some(Path::new("src/handler.rs")));
        assert!(
            violation.reason.contains("`handle`")
                && violation.reason.contains("`src/handler.rs`")
                && violation.reason.contains("`src/checkout.rs`")
                && violation.reason.contains("api_surface_constraints[0]")
                && violation.reason.contains("the handler API is frozen"),
            "{}",
            violation.reason
        );
        assert!(!report
            .change_report
            .warnings
            .iter()
            .any(|finding| finding.kind == "api_surface_moved"));
    }

    #[test]
    fn a_rename_into_a_scope_that_forbids_api_additions_fails() {
        let report = verify_api_surface_of_rename(
            "pub fn handle() {}\n",
            "pub fn handle() {}\n",
            "diff --git a/src/handler.rs b/src/checkout.rs\nsimilarity index 100%\nrename from src/handler.rs\nrename to src/checkout.rs\n",
            vec![ApiSurfaceConstraint {
                scope: "src/checkout.rs".into(),
                allowed_changes: Vec::new(),
                severity: ConstraintSeverity::Forbidden,
                reason: "public entry points are added only by review".into(),
                evidence_refs: Vec::new(),
            }],
        );

        assert_eq!(report.decision, VerificationDecision::Fail);
        let violation = report
            .change_report
            .boundary_violations
            .iter()
            .find(|finding| finding.kind == "api_surface_violation")
            .expect("a move into a scope that forbids additions fails");
        assert_eq!(
            violation.path.as_deref(),
            Some(Path::new("src/checkout.rs"))
        );
        assert!(
            violation.reason.contains("`handle`")
                && violation.reason.contains("`src/handler.rs`")
                && violation.reason.contains(
                    "adds it to a scope where api_surface_constraints[0] forbids additions"
                )
                && violation
                    .reason
                    .contains("public entry points are added only by review"),
            "{}",
            violation.reason
        );
        assert!(!report
            .change_report
            .warnings
            .iter()
            .any(|finding| finding.kind == "api_surface_moved"));
    }

    #[test]
    fn a_rename_that_drops_a_public_fn_fails_for_that_fn_only() {
        let report = verify_api_surface_of_rename(
            "pub fn handle() {}\npub fn legacy() {}\n",
            "pub fn handle() {}\n",
            "diff --git a/src/handler.rs b/src/checkout.rs\nsimilarity index 60%\nrename from src/handler.rs\nrename to src/checkout.rs\n--- a/src/handler.rs\n+++ b/src/checkout.rs\n@@ -2 +1,0 @@\n-pub fn legacy() {}\n",
            Vec::new(),
        );

        assert_eq!(report.decision, VerificationDecision::Fail);
        let violations = report
            .change_report
            .boundary_violations
            .iter()
            .filter(|finding| finding.kind == "api_surface_violation")
            .collect::<Vec<_>>();
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(
            violations[0].reason.contains("Removed") && violations[0].reason.contains("`legacy`"),
            "{}",
            violations[0].reason
        );
        assert!(report
            .change_report
            .warnings
            .iter()
            .any(|finding| finding.kind == "api_surface_moved"
                && finding.reason.contains("`handle`")));
    }

    #[test]
    fn one_hunk_diff_reports_only_the_innermost_overlapping_symbol() {
        let store = store_with_handler_symbols();
        let report = verify_handler(
            &store,
            VerifyChangeInput {
                unified_diff: Some(
                    "diff --git a/src/handler.rs b/src/handler.rs\n--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -6 +6 @@\n-    old();\n+    new();\n".into(),
                ),
                ..Default::default()
            },
        );

        assert_eq!(
            report.changed_symbols,
            vec!["handler::checkout".to_string()]
        );
        assert!(report.changed_regions_without_symbol.is_empty());
        assert!(!report
            .warnings
            .iter()
            .any(|warning| warning.kind == SYMBOL_GRANULARITY_WARNING));
    }

    #[test]
    fn hunk_outside_every_symbol_is_reported_as_a_region_not_dropped() {
        let store = store_with_handler_symbols();
        let report = verify_handler(
            &store,
            VerifyChangeInput {
                unified_diff: Some(
                    "diff --git a/src/handler.rs b/src/handler.rs\n--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -41,0 +42 @@\n+// trailing comment\n".into(),
                ),
                ..Default::default()
            },
        );

        assert!(
            report.changed_symbols.is_empty(),
            "{:?}",
            report.changed_symbols
        );
        assert_eq!(
            report.changed_regions_without_symbol,
            vec!["src/handler.rs:42-42".to_string()]
        );
    }

    #[test]
    fn changed_files_without_a_diff_fall_back_to_file_granularity_with_a_warning() {
        let store = store_with_handler_symbols();
        let with_diff_verdict = verify_handler(
            &store,
            VerifyChangeInput {
                unified_diff: Some(
                    "diff --git a/src/handler.rs b/src/handler.rs\n--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -6 +6 @@\n-a\n+b\n".into(),
                ),
                ..Default::default()
            },
        )
        .verdict;
        let report = verify_handler(
            &store,
            VerifyChangeInput {
                changed_files: vec![PathBuf::from("src/handler.rs")],
                ..Default::default()
            },
        );

        assert_eq!(report.changed_symbols.len(), 3);
        let granularity = report
            .warnings
            .iter()
            .filter(|warning| warning.kind == SYMBOL_GRANULARITY_WARNING)
            .collect::<Vec<_>>();
        assert_eq!(granularity.len(), 1);
        assert_eq!(
            granularity[0].path.as_deref(),
            Some(Path::new("src/handler.rs"))
        );
        assert!(
            granularity[0]
                .reason
                .starts_with("no diff was supplied for this path"),
            "{}",
            granularity[0].reason
        );
        // The warning states how precisely the change was attributed; it does not move the verdict.
        assert_eq!(report.verdict, with_diff_verdict);
    }

    #[test]
    fn a_hunk_count_past_the_last_line_number_saturates() {
        assert_eq!(
            hunk_side_range("10,4294967295"),
            Some(Some(LineRange {
                start: 10,
                end: u32::MAX
            }))
        );
        assert_eq!(hunk_side_range("10,0"), Some(None));
        assert_eq!(hunk_side_range("ten,1"), None);
    }

    #[test]
    fn pure_insertion_after_a_symbol_is_not_attributed_to_it() {
        let store = store_with_handler_symbols();
        // `-40,0`: the module ends exactly at the anchor line; nothing of it changed.
        let report = verify_handler(
            &store,
            VerifyChangeInput {
                unified_diff: Some(
                    "diff --git a/src/handler.rs b/src/handler.rs\n--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -40,0 +41,5 @@\n+fn appended() {}\n".into(),
                ),
                ..Default::default()
            },
        );

        assert!(
            report.changed_symbols.is_empty(),
            "{:?}",
            report.changed_symbols
        );
        assert_eq!(
            report.changed_regions_without_symbol,
            vec!["src/handler.rs:41-45".to_string()]
        );
    }

    #[test]
    fn pure_deletion_is_attributed_by_its_removed_lines_only() {
        let store = store_with_handler_symbols();
        // `-13,1 +12,0`: the removed line sat between `checkout` (5-12) and `refund` (14-20),
        // inside the module; `checkout` ends at the new-side anchor and did not change.
        let inside = verify_handler(
            &store,
            VerifyChangeInput {
                unified_diff: Some(
                    "diff --git a/src/handler.rs b/src/handler.rs\n--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -13,1 +12,0 @@\n-// gap\n".into(),
                ),
                ..Default::default()
            },
        );
        assert_eq!(inside.changed_symbols, vec!["handler::handler".to_string()]);

        let past_every_symbol = verify_handler(
            &store,
            VerifyChangeInput {
                unified_diff: Some(
                    "diff --git a/src/handler.rs b/src/handler.rs\n--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -41,2 +40,0 @@\n-// a\n-// b\n".into(),
                ),
                ..Default::default()
            },
        );
        assert!(past_every_symbol.changed_symbols.is_empty());
        assert_eq!(
            past_every_symbol.changed_regions_without_symbol,
            vec!["src/handler.rs:41-42 (removed; pre-edit lines)".to_string()]
        );
    }

    #[test]
    fn a_hunk_replacing_a_whole_impl_lists_the_impl_beside_its_methods() {
        let mut store = store_with_handler_symbols();
        store.symbols.extend([
            handler_symbol("Cart", SymbolKind::Class, 22, 38),
            handler_symbol("add", SymbolKind::Method, 24, 28),
            handler_symbol("remove", SymbolKind::Method, 30, 36),
        ]);
        // `-22,17 +22,19`: the whole `impl` (22-38) was replaced; the module around it was not.
        let whole = verify_handler(
            &store,
            VerifyChangeInput {
                unified_diff: Some(
                    "diff --git a/src/handler.rs b/src/handler.rs\n--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -22,17 +22,19 @@\n-impl Cart {}\n+impl Cart {}\n".into(),
                ),
                ..Default::default()
            },
        );
        assert_eq!(
            whole.changed_symbols,
            vec![
                "handler::Cart".to_string(),
                "handler::add".to_string(),
                "handler::remove".to_string()
            ]
        );

        // An edit inside one method still names only that method.
        let partial = verify_handler(
            &store,
            VerifyChangeInput {
                unified_diff: Some(
                    "diff --git a/src/handler.rs b/src/handler.rs\n--- a/src/handler.rs\n+++ b/src/handler.rs\n@@ -25 +25 @@\n-a\n+b\n".into(),
                ),
                ..Default::default()
            },
        );
        assert_eq!(partial.changed_symbols, vec!["handler::add".to_string()]);
    }

    #[test]
    fn a_binary_or_mode_only_entry_is_not_called_a_missing_diff() {
        // Neither entry states a hunk, and neither is a side of a text rename, so each falls
        // back to file granularity: the diff named the path but described no lines of it.
        for diff in [
            "diff --git a/src/handler.rs b/src/handler.rs\nold mode 100644\nnew mode 100755\n",
            "diff --git a/src/handler.rs b/src/handler.rs\nindex 0f1d0e1..9a2b3c4 100644\nBinary files a/src/handler.rs and b/src/handler.rs differ\n",
        ] {
            let store = store_with_handler_symbols();
            let report = verify_handler(
                &store,
                VerifyChangeInput {
                    unified_diff: Some(diff.into()),
                    ..Default::default()
                },
            );

            let reasons = report
                .warnings
                .iter()
                .filter(|warning| {
                    warning.kind == SYMBOL_GRANULARITY_WARNING
                        && warning.path.as_deref() == Some(Path::new("src/handler.rs"))
                })
                .map(|warning| warning.reason.as_str())
                .collect::<Vec<_>>();
            assert_eq!(reasons.len(), 1, "{diff}: {:?}", report.warnings);
            assert!(
                reasons[0].starts_with("the supplied diff has no hunk ranges for this path"),
                "{diff}: {}",
                reasons[0]
            );
            assert!(!reasons[0].contains("no diff was supplied"), "{diff}");
        }
    }

    #[test]
    fn a_binary_rename_reports_file_granularity_with_the_diff_supplied_reason() {
        // Git reports a binary pair without hunks even when its content changed, so a binary
        // rename is not a side whose changed lines the diff states: it falls back to file
        // granularity, and the reason is the supplied diff, not a missing one.
        let store = store_with_handler_symbols();
        let report = verify_handler(
            &store,
            VerifyChangeInput {
                unified_diff: Some(
                    "diff --git a/src/old_handler.rs b/src/handler.rs\nsimilarity index 78%\nrename from src/old_handler.rs\nrename to src/handler.rs\nindex 0f1d0e1..9a2b3c4 100644\nBinary files a/src/old_handler.rs and b/src/handler.rs differ\n".into(),
                ),
                ..Default::default()
            },
        );

        assert_eq!(
            report.previous_paths,
            vec![PreviousPath {
                path: PathBuf::from("src/handler.rs"),
                previous_path: PathBuf::from("src/old_handler.rs"),
                kind: PreviousPathKind::Rename,
            }]
        );
        assert_eq!(
            report.changed_symbols,
            vec![
                "handler::checkout".to_string(),
                "handler::handler".to_string(),
                "handler::refund".to_string()
            ]
        );

        let reasons = report
            .warnings
            .iter()
            .filter(|warning| {
                warning.kind == SYMBOL_GRANULARITY_WARNING
                    && warning.path.as_deref() == Some(Path::new("src/handler.rs"))
            })
            .map(|warning| warning.reason.as_str())
            .collect::<Vec<_>>();
        assert_eq!(reasons.len(), 1, "{:?}", report.warnings);
        assert!(
            reasons[0].starts_with("the supplied diff has no hunk ranges for this path"),
            "{}",
            reasons[0]
        );
    }
    #[test]
    fn an_edit_to_a_low_ranked_lexical_impact_is_outside_the_boundary() {
        let store = RuntimeStore::new().without_runtime();
        let hit = |path: &str, score: f32, match_reason: &str| SearchResult {
            path: PathBuf::from(path),
            line_range: Some(LineRange { start: 1, end: 3 }),
            snippet: "fn handler() {}".into(),
            symbol: None,
            score,
            match_reason: match_reason.into(),
            evidence: vec![format!("{match_reason} in {path}")],
            evidence_refs: Vec::new(),
            confidence: 0.5,
            score_breakdown: Vec::new(),
            exact_reference_provenance: None,
        };
        let mut supporting_files = (0..10)
            .map(|rank| {
                hit(
                    &format!("src/lexical_{rank}.rs"),
                    1.0 - rank as f32 * 0.05,
                    "tantivy hybrid lexical match",
                )
            })
            .collect::<Vec<_>>();
        let mut exact = hit("src/caller.rs", 0.1, "exact symbol reference via SCIP");
        exact.exact_reference_provenance = Some(open_kioku_core::EvidenceSourceType::Scip);
        supporting_files.push(exact);
        // Bounded-search evidence makes the plan reuse these supporting files as its impact.
        let context = open_kioku_core::ContextPack {
            task: "change handler".into(),
            primary_files: vec![hit("src/handler.rs", 2.0, "tantivy hybrid lexical match")],
            supporting_files,
            evidence: vec![open_kioku_core::Evidence {
                id: open_kioku_core::EvidenceId::new("context:bounded-search"),
                message: "bounded context".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let plan = open_kioku_plan::PlanEngine::new(&store)
            .plan_from_context("change handler", 5, context)
            .unwrap();
        let verify = |path: &str| {
            ChangeVerifier::new(&store)
                .verify(
                    Path::new("."),
                    &plan,
                    VerifyChangeInput {
                        changed_files: vec![PathBuf::from(path)],
                        ..Default::default()
                    },
                )
                .unwrap()
        };

        let low_ranked = verify("src/lexical_9.rs");
        assert_eq!(low_ranked.verdict, VerificationVerdict::Fail);
        assert!(low_ranked
            .boundary_violations
            .iter()
            .any(|finding| finding.kind == "out_of_boundary"));
        for admitted in ["src/lexical_0.rs", "src/caller.rs"] {
            assert!(
                !verify(admitted)
                    .boundary_violations
                    .iter()
                    .any(|finding| finding.kind == "out_of_boundary"),
                "{admitted} should be a caution file"
            );
        }
    }

    #[test]
    fn verify_without_any_changed_file_is_invalid_input() {
        let store = RuntimeStore::new().without_runtime();
        let err = ChangeVerifier::new(&store)
            .verify(
                Path::new("."),
                &plan_with_boundary_evidence(),
                VerifyChangeInput::default(),
            )
            .unwrap_err();
        assert!(matches!(err, OkError::InvalidInput(_)), "{err:?}");
        assert!(err
            .to_string()
            .starts_with("invalid input: verify requires at least one changed file"));
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
                direct_impacts_omitted: 0,
                indirect_impacts_omitted: 0,
                proven_impact: Vec::new(),
                possible_impact: Vec::new(),
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
            selection_tier: open_kioku_core::TestSelectionTier::default(),
            tier_justification: Vec::new(),
            id: "validation:handler".into(),
            name: "handler validation".into(),
            file_id: FileId::new("tests/handler_test.rs"),
            range: None,
            command: Some(command.into()),
            confidence: Confidence::High,
            reason: "validate handler change".into(),
            evidence_refs: vec!["boundary:allow".into()],
            score_breakdown: Vec::new(),
            origin: Default::default(),
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
