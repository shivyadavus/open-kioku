use open_kioku_actions::{ActionKind, PolicyGate};
use open_kioku_config::OcfConfig;
use open_kioku_context::ContextPackBuilder;
use open_kioku_core::{PatchId, PatchPlan};
use open_kioku_errors::{OcfError, Result};
use open_kioku_storage::OcfStore;
use sha2::{Digest, Sha256};

pub struct PatchPlanner<'a> {
    config: &'a OcfConfig,
    store: &'a dyn OcfStore,
}

impl<'a> PatchPlanner<'a> {
    pub fn new(config: &'a OcfConfig, store: &'a dyn OcfStore) -> Self {
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
            return Err(OcfError::PolicyDenied(
                "patch application requires explicit approval".into(),
            ));
        }
        Err(OcfError::Unsupported(
            "patch application is intentionally not implemented without a diff applicator".into(),
        ))
    }
}

fn stable_id(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    format!("{:x}", hasher.finalize())
}
