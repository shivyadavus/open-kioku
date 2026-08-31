//! Runtime abstention: the deployed form of CC6 calibrated abstention.
//!
//! The benchmark calibrates an [`RuntimeAbstentionPolicy`] on the frozen corpus and the
//! fail-closed activation-readiness gate decides whether it may be activated. This module
//! holds the policy semantics and the [`ContextPack`]-based signal derivation so the
//! measured behavior and the deployed behavior are the same code path — the benchmark
//! delegates here, and the context compiler applies the identical policy at runtime.

use crate::{ContextPack, RetrievalAuthority, RetrievalUnitKey};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The evidence signals a calibrated abstention decision is allowed to consume.
///
/// Candidate ordering, fuzzy similarity, and raw confidence are intentionally absent;
/// abstention reasons must stay inspectable and evidence-shaped.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AbstentionSignals {
    /// Whether any selected unit is backed by exact evidence.
    pub exact_evidence_present: bool,
    /// Score margin between the top result and the next result of the same retrieval
    /// authority. `None` when no same-authority runner-up exists.
    pub top_score_margin: Option<f64>,
    /// Independent retrieval streams contributing to the top result.
    pub independent_stream_count: usize,
    /// Unresolved/ambiguous signals recorded by selection.
    pub ambiguity_unresolved_count: usize,
}

/// A calibrated abstention policy. Every gate is optional so calibration can establish
/// which evidence dimensions add value instead of forcing an arbitrary conjunction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeAbstentionPolicy {
    /// Abstain when the same-authority top-score margin is below this value. A missing
    /// margin is insufficient evidence whenever this gate is enabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_top_score_margin: Option<f64>,
    /// Abstain when fewer independent retrieval streams support the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_independent_streams: Option<usize>,
    /// Abstain when unresolved/ambiguous signals exceed this bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_ambiguity_unresolved: Option<usize>,
}

impl RuntimeAbstentionPolicy {
    pub fn never_abstain() -> Self {
        Self {
            min_top_score_margin: None,
            min_independent_streams: None,
            max_ambiguity_unresolved: None,
        }
    }

    /// Evaluate signals without upgrading or suppressing evidence authority.
    ///
    /// Exact evidence prevents weak score-margin or stream-agreement heuristics from
    /// erasing an authoritative match. It does not override an explicit
    /// ambiguity/unresolved gate: exact evidence can still be ambiguous, and that
    /// conflict must remain visible and actionable.
    pub fn should_abstain(&self, signals: &AbstentionSignals) -> bool {
        let unresolved = self
            .max_ambiguity_unresolved
            .is_some_and(|maximum| signals.ambiguity_unresolved_count > maximum);
        if unresolved {
            return true;
        }
        if signals.exact_evidence_present {
            return false;
        }

        let weak_margin = self.min_top_score_margin.is_some_and(|minimum| {
            signals
                .top_score_margin
                .is_none_or(|margin| margin < minimum)
        });
        let weak_agreement = self
            .min_independent_streams
            .is_some_and(|minimum| signals.independent_stream_count < minimum);
        weak_margin || weak_agreement
    }

    /// Human-readable justification for an abstention decision on these signals.
    pub fn explain(&self, signals: &AbstentionSignals) -> String {
        let mut reasons = Vec::new();
        if self
            .max_ambiguity_unresolved
            .is_some_and(|maximum| signals.ambiguity_unresolved_count > maximum)
        {
            reasons.push(format!(
                "{} unresolved/ambiguous signal(s) exceed the calibrated bound",
                signals.ambiguity_unresolved_count
            ));
        } else if !signals.exact_evidence_present {
            if self.min_top_score_margin.is_some_and(|minimum| {
                signals
                    .top_score_margin
                    .is_none_or(|margin| margin < minimum)
            }) {
                reasons.push(match signals.top_score_margin {
                    Some(margin) => {
                        format!("top-score margin {margin:.4} is below the calibrated minimum")
                    }
                    None => "no same-authority runner-up to establish a score margin".into(),
                });
            }
            if self
                .min_independent_streams
                .is_some_and(|minimum| signals.independent_stream_count < minimum)
            {
                reasons.push(format!(
                    "only {} independent retrieval stream(s) support the top result",
                    signals.independent_stream_count
                ));
            }
        }
        if reasons.is_empty() {
            "calibrated gates were satisfied".into()
        } else {
            reasons.join("; ")
        }
    }
}

/// Derive abstention signals from a completed [`ContextPack`].
///
/// Returns `None` when any selected primary unit lacks exact trace provenance: a
/// faithful decision is impossible then, and the fail-closed behavior is to not
/// abstain rather than to guess.
pub fn derive_abstention_signals(pack: &ContextPack) -> Option<AbstentionSignals> {
    let mut traced = Vec::with_capacity(pack.primary_files.len());
    for result in &pack.primary_files {
        let unit = RetrievalUnitKey::from_result(result);
        let trace = pack
            .retrieval_diagnostics
            .traces
            .iter()
            .find(|trace| trace.unit_key.as_ref() == Some(&unit))?;
        let stream_count = trace
            .contributions
            .iter()
            .map(|contribution| contribution.source)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        traced.push((trace.authority, result.score as f64, stream_count));
    }
    Some(AbstentionSignals {
        exact_evidence_present: pack.retrieval_diagnostics.selection.exact_evidence_count > 0,
        top_score_margin: same_authority_top_score_margin(&traced),
        independent_stream_count: traced.first().map(|(_, _, streams)| *streams).unwrap_or(0),
        ambiguity_unresolved_count: pack
            .retrieval_diagnostics
            .selection
            .ambiguity_unresolved_count,
    })
}

/// Score margin between the top result and the next result sharing its retrieval
/// authority. Cross-authority comparisons are intentionally excluded: authority and
/// score remain distinct concepts.
pub fn same_authority_top_score_margin(traced: &[(RetrievalAuthority, f64, usize)]) -> Option<f64> {
    let (top_authority, top_score, _) = *traced.first()?;
    if !top_score.is_finite() {
        return None;
    }
    let second_score = traced.iter().skip(1).find_map(|(authority, score, _)| {
        (*authority == top_authority && score.is_finite()).then_some(*score)
    })?;
    let margin = top_score - second_score;
    (margin >= 0.0).then_some(margin)
}

/// The reason prefix used when calibrated abstention fires at runtime, shared by every
/// surface that renders or filters on it.
pub const CALIBRATED_ABSTENTION_REASON_PREFIX: &str = "calibrated_cc6_abstention";

/// Fail-closed activation artifact for runtime abstention.
///
/// Written only by an activation flow that passed the CC6 activation-readiness gate;
/// loaded by CLI/MCP at pack-build time. Anything invalid, unreadable, or not marked
/// ready deactivates the feature rather than degrading it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AbstentionActivation {
    pub schema_version: u32,
    /// The corpus the policy was calibrated on.
    pub corpus_id: String,
    /// RFC3339 timestamp of activation.
    pub activated_at: String,
    /// Whether the fail-closed readiness evaluation passed at activation time.
    pub readiness_passed: bool,
    /// The calibrated policy to apply.
    pub policy: RuntimeAbstentionPolicy,
    /// Measured holdout evidence recorded at activation for later audit.
    #[serde(default)]
    pub holdout_evidence: std::collections::BTreeMap<String, f64>,
}

pub const ABSTENTION_ACTIVATION_SCHEMA_VERSION: u32 = 1;

impl AbstentionActivation {
    /// Load an activation artifact from a repository's `.ok` directory, fail-closed:
    /// missing file, unreadable JSON, wrong schema version, or `readiness_passed = false`
    /// all yield `None` (feature off) rather than an error or a partial policy.
    pub fn load_for_repo(repo: &std::path::Path) -> Option<Self> {
        let path = repo.join(".ok").join("abstention-policy.json");
        let text = std::fs::read_to_string(path).ok()?;
        let activation = serde_json::from_str::<Self>(&text).ok()?;
        (activation.schema_version == ABSTENTION_ACTIVATION_SCHEMA_VERSION
            && activation.readiness_passed)
            .then_some(activation)
    }
}
