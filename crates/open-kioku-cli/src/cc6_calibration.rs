use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeSet;

/// Dataset partition used by CC6 abstention calibration.
///
/// Policies are selected exclusively from `Development`; `Holdout` is evaluation-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationSplit {
    Development,
    Holdout,
}

/// Observable retrieval features that may be used to calibrate an abstention policy.
///
/// These are evidence-strength signals, not probabilities. In particular, `top_score_margin`
/// must compare candidates in the same authority tier; authority is represented separately by
/// `exact_evidence_present` and is never inferred from a score.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbstentionCalibrationCase {
    pub id: String,
    pub split: CalibrationSplit,
    pub no_gold_expected: bool,
    pub exact_evidence_present: bool,
    pub top_score_margin: Option<f64>,
    pub independent_stream_count: usize,
    pub ambiguity_unresolved_count: usize,
}

/// Product safety constraints supplied by the caller. The calibration algorithm intentionally
/// contains no hidden benchmark-specific acceptance threshold.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AbstentionCalibrationConstraints {
    /// Maximum fraction of development positive cases that the selected policy may abstain on.
    pub max_positive_abstention_rate: f64,
    /// Minimum development no-gold abstention recall required before a policy is eligible.
    pub min_no_gold_abstention_recall: f64,
}

impl AbstentionCalibrationConstraints {
    fn validate(self) -> Result<Self, CalibrationError> {
        for (name, value) in [
            (
                "max_positive_abstention_rate",
                self.max_positive_abstention_rate,
            ),
            (
                "min_no_gold_abstention_recall",
                self.min_no_gold_abstention_recall,
            ),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(CalibrationError::InvalidConstraint(format!(
                    "{name} must be a finite value in [0, 1]"
                )));
            }
        }
        Ok(self)
    }
}

/// A calibrated abstention policy. Every gate is optional so calibration can establish which
/// evidence dimensions add value instead of forcing an arbitrary conjunction into production.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbstentionPolicy {
    /// Abstain when the same-authority top-score margin is below this value. Missing margin is
    /// insufficient evidence whenever this gate is enabled.
    pub min_top_score_margin: Option<f64>,
    /// Abstain when fewer independent retrieval streams support the result.
    pub min_independent_streams: Option<usize>,
    /// Abstain when unresolved/ambiguous signals exceed this bound.
    pub max_ambiguity_unresolved: Option<usize>,
}

impl AbstentionPolicy {
    pub fn never_abstain() -> Self {
        Self {
            min_top_score_margin: None,
            min_independent_streams: None,
            max_ambiguity_unresolved: None,
        }
    }

    /// Evaluate a case without upgrading or suppressing evidence authority.
    ///
    /// Exact evidence prevents weak score-margin or stream-agreement heuristics from erasing an
    /// authoritative match. It does not override an explicit ambiguity/unresolved gate: exact
    /// evidence can still be ambiguous, and that conflict must remain visible and actionable.
    pub fn should_abstain(&self, case: &AbstentionCalibrationCase) -> bool {
        let unresolved = self
            .max_ambiguity_unresolved
            .is_some_and(|maximum| case.ambiguity_unresolved_count > maximum);
        if unresolved {
            return true;
        }
        if case.exact_evidence_present {
            return false;
        }

        let weak_margin = self
            .min_top_score_margin
            .is_some_and(|minimum| case.top_score_margin.is_none_or(|margin| margin < minimum));
        let weak_agreement = self
            .min_independent_streams
            .is_some_and(|minimum| case.independent_stream_count < minimum);
        weak_margin || weak_agreement
    }

    fn complexity(&self) -> usize {
        usize::from(self.min_top_score_margin.is_some())
            + usize::from(self.min_independent_streams.is_some())
            + usize::from(self.max_ambiguity_unresolved.is_some())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AbstentionMetrics {
    pub positive_cases: usize,
    pub no_gold_cases: usize,
    pub abstained_cases: usize,
    pub correct_no_gold_abstentions: usize,
    pub incorrect_positive_abstentions: usize,
    pub precision: f64,
    pub no_gold_recall: f64,
    pub positive_abstention_rate: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbstentionCalibrationResult {
    pub policy: AbstentionPolicy,
    /// Safety envelope used to select the policy, retained for report auditability.
    pub constraints: AbstentionCalibrationConstraints,
    pub development: AbstentionMetrics,
    pub holdout: AbstentionMetrics,
    /// Stable case identities make it auditable which partition selected policy parameters.
    pub development_case_ids: Vec<String>,
    pub holdout_case_ids: Vec<String>,
    pub selection_basis: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalibrationError {
    EmptyCaseId,
    DuplicateCaseId(String),
    InvalidFeature(String),
    InvalidConstraint(String),
    MissingDevelopmentPositiveCases,
    MissingDevelopmentNoGoldCases,
    MissingHoldoutCases,
    NoEligiblePolicy,
}

impl std::fmt::Display for CalibrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyCaseId => write!(f, "calibration case id must be non-empty"),
            Self::DuplicateCaseId(id) => write!(f, "duplicate calibration case id `{id}`"),
            Self::InvalidFeature(message) => write!(f, "invalid calibration feature: {message}"),
            Self::InvalidConstraint(message) => {
                write!(f, "invalid calibration constraint: {message}")
            }
            Self::MissingDevelopmentPositiveCases => write!(
                f,
                "calibration requires at least one development positive case"
            ),
            Self::MissingDevelopmentNoGoldCases => write!(
                f,
                "calibration requires at least one development no-gold case"
            ),
            Self::MissingHoldoutCases => write!(
                f,
                "calibration requires an evaluation-only holdout partition"
            ),
            Self::NoEligiblePolicy => write!(
                f,
                "no development-only abstention policy satisfies the supplied safety constraints"
            ),
        }
    }
}

impl std::error::Error for CalibrationError {}

/// Select an abstention policy using development cases only, then evaluate the frozen policy on
/// holdout. Holdout labels and features never participate in candidate generation, filtering,
/// scoring, or tie-breaking.
pub fn calibrate_abstention_policy(
    cases: &[AbstentionCalibrationCase],
    constraints: AbstentionCalibrationConstraints,
) -> Result<AbstentionCalibrationResult, CalibrationError> {
    let constraints = constraints.validate()?;
    validate_cases(cases)?;

    let mut development = cases
        .iter()
        .filter(|case| case.split == CalibrationSplit::Development)
        .collect::<Vec<_>>();
    let mut holdout = cases
        .iter()
        .filter(|case| case.split == CalibrationSplit::Holdout)
        .collect::<Vec<_>>();
    development.sort_by(|left, right| left.id.cmp(&right.id));
    holdout.sort_by(|left, right| left.id.cmp(&right.id));

    if !development.iter().any(|case| !case.no_gold_expected) {
        return Err(CalibrationError::MissingDevelopmentPositiveCases);
    }
    if !development.iter().any(|case| case.no_gold_expected) {
        return Err(CalibrationError::MissingDevelopmentNoGoldCases);
    }
    if holdout.is_empty() {
        return Err(CalibrationError::MissingHoldoutCases);
    }

    let mut best: Option<(AbstentionPolicy, AbstentionMetrics)> = None;
    for policy in candidate_policies(&development) {
        let metrics = evaluate_policy_refs(&policy, &development);
        if metrics.positive_abstention_rate > constraints.max_positive_abstention_rate
            || metrics.no_gold_recall < constraints.min_no_gold_abstention_recall
        {
            continue;
        }

        match &best {
            Some((best_policy, best_metrics))
                if compare_policy_quality(&policy, &metrics, best_policy, best_metrics)
                    != Ordering::Greater => {}
            _ => best = Some((policy, metrics)),
        }
    }

    let Some((policy, development_metrics)) = best else {
        return Err(CalibrationError::NoEligiblePolicy);
    };
    if policy == AbstentionPolicy::never_abstain()
        && constraints.min_no_gold_abstention_recall > 0.0
    {
        return Err(CalibrationError::NoEligiblePolicy);
    }

    let holdout_metrics = evaluate_policy_refs(&policy, &holdout);
    Ok(AbstentionCalibrationResult {
        policy,
        constraints,
        development: development_metrics,
        holdout: holdout_metrics,
        development_case_ids: development.iter().map(|case| case.id.clone()).collect(),
        holdout_case_ids: holdout.iter().map(|case| case.id.clone()).collect(),
        selection_basis: "policy parameters selected exclusively from development cases; holdout is evaluation-only; exact evidence protects against weak score/agreement gates but never suppresses explicit ambiguity".into(),
    })
}

/// Evaluate a frozen abstention policy without calibrating or changing it.
pub fn evaluate_abstention_policy(
    policy: &AbstentionPolicy,
    cases: &[AbstentionCalibrationCase],
) -> Result<AbstentionMetrics, CalibrationError> {
    validate_cases(cases)?;
    Ok(evaluate_policy_refs(
        policy,
        &cases.iter().collect::<Vec<_>>(),
    ))
}

fn validate_cases(cases: &[AbstentionCalibrationCase]) -> Result<(), CalibrationError> {
    let mut ids = BTreeSet::new();
    for case in cases {
        if case.id.trim().is_empty() {
            return Err(CalibrationError::EmptyCaseId);
        }
        if !ids.insert(case.id.clone()) {
            return Err(CalibrationError::DuplicateCaseId(case.id.clone()));
        }
        if let Some(margin) = case.top_score_margin {
            if !margin.is_finite() || margin < 0.0 {
                return Err(CalibrationError::InvalidFeature(format!(
                    "case `{}` top_score_margin must be finite and non-negative",
                    case.id
                )));
            }
        }
    }
    Ok(())
}

fn candidate_policies(development: &[&AbstentionCalibrationCase]) -> Vec<AbstentionPolicy> {
    let mut margins = development
        .iter()
        .filter_map(|case| case.top_score_margin)
        .collect::<Vec<_>>();
    margins.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    margins.dedup_by(|left, right| (*left - *right).abs() <= f64::EPSILON);

    let max_streams = development
        .iter()
        .map(|case| case.independent_stream_count)
        .max()
        .unwrap_or(0);
    let max_ambiguity = development
        .iter()
        .map(|case| case.ambiguity_unresolved_count)
        .max()
        .unwrap_or(0);

    let margin_options = std::iter::once(None)
        .chain(margins.into_iter().map(Some))
        .collect::<Vec<_>>();
    let stream_options = std::iter::once(None)
        .chain((1..=max_streams.saturating_add(1)).map(Some))
        .collect::<Vec<_>>();
    let ambiguity_options = std::iter::once(None)
        .chain((0..max_ambiguity).map(Some))
        .collect::<Vec<_>>();

    let mut policies = Vec::new();
    for margin in &margin_options {
        for streams in &stream_options {
            for ambiguity in &ambiguity_options {
                policies.push(AbstentionPolicy {
                    min_top_score_margin: *margin,
                    min_independent_streams: *streams,
                    max_ambiguity_unresolved: *ambiguity,
                });
            }
        }
    }
    policies.sort_by(compare_policy_identity);
    policies.dedup();
    policies
}

fn compare_policy_quality(
    policy: &AbstentionPolicy,
    metrics: &AbstentionMetrics,
    best_policy: &AbstentionPolicy,
    best_metrics: &AbstentionMetrics,
) -> Ordering {
    metrics
        .no_gold_recall
        .partial_cmp(&best_metrics.no_gold_recall)
        .unwrap_or(Ordering::Equal)
        .then_with(|| {
            metrics
                .precision
                .partial_cmp(&best_metrics.precision)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| {
            best_metrics
                .positive_abstention_rate
                .partial_cmp(&metrics.positive_abstention_rate)
                .unwrap_or(Ordering::Equal)
        })
        .then_with(|| best_policy.complexity().cmp(&policy.complexity()))
        .then_with(|| compare_policy_identity(best_policy, policy))
}

fn compare_policy_identity(left: &AbstentionPolicy, right: &AbstentionPolicy) -> Ordering {
    option_f64_key(left.min_top_score_margin)
        .cmp(&option_f64_key(right.min_top_score_margin))
        .then_with(|| {
            left.min_independent_streams
                .cmp(&right.min_independent_streams)
        })
        .then_with(|| {
            left.max_ambiguity_unresolved
                .cmp(&right.max_ambiguity_unresolved)
        })
}

fn option_f64_key(value: Option<f64>) -> (u8, u64) {
    match value {
        None => (0, 0),
        Some(value) => (1, value.to_bits()),
    }
}

fn evaluate_policy_refs(
    policy: &AbstentionPolicy,
    cases: &[&AbstentionCalibrationCase],
) -> AbstentionMetrics {
    let positive_cases = cases.iter().filter(|case| !case.no_gold_expected).count();
    let no_gold_cases = cases.iter().filter(|case| case.no_gold_expected).count();
    let mut abstained_cases = 0usize;
    let mut correct_no_gold_abstentions = 0usize;
    let mut incorrect_positive_abstentions = 0usize;

    for case in cases {
        if !policy.should_abstain(case) {
            continue;
        }
        abstained_cases += 1;
        if case.no_gold_expected {
            correct_no_gold_abstentions += 1;
        } else {
            incorrect_positive_abstentions += 1;
        }
    }

    AbstentionMetrics {
        positive_cases,
        no_gold_cases,
        abstained_cases,
        correct_no_gold_abstentions,
        incorrect_positive_abstentions,
        precision: ratio(correct_no_gold_abstentions, abstained_cases),
        no_gold_recall: ratio(correct_no_gold_abstentions, no_gold_cases),
        positive_abstention_rate: ratio(incorrect_positive_abstentions, positive_cases),
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(
        id: &str,
        split: CalibrationSplit,
        no_gold_expected: bool,
        exact: bool,
        margin: Option<f64>,
        streams: usize,
        ambiguity: usize,
    ) -> AbstentionCalibrationCase {
        AbstentionCalibrationCase {
            id: id.into(),
            split,
            no_gold_expected,
            exact_evidence_present: exact,
            top_score_margin: margin,
            independent_stream_count: streams,
            ambiguity_unresolved_count: ambiguity,
        }
    }

    fn constraints() -> AbstentionCalibrationConstraints {
        AbstentionCalibrationConstraints {
            max_positive_abstention_rate: 0.0,
            min_no_gold_abstention_recall: 0.5,
        }
    }

    #[test]
    fn holdout_cannot_change_selected_policy() {
        let development = vec![
            case(
                "dev-positive-a",
                CalibrationSplit::Development,
                false,
                false,
                Some(0.8),
                3,
                0,
            ),
            case(
                "dev-positive-b",
                CalibrationSplit::Development,
                false,
                false,
                Some(0.6),
                2,
                0,
            ),
            case(
                "dev-negative",
                CalibrationSplit::Development,
                true,
                false,
                Some(0.1),
                1,
                0,
            ),
        ];
        let mut first = development.clone();
        first.extend([
            case(
                "holdout-a",
                CalibrationSplit::Holdout,
                false,
                false,
                Some(0.01),
                0,
                9,
            ),
            case(
                "holdout-b",
                CalibrationSplit::Holdout,
                true,
                false,
                Some(100.0),
                20,
                0,
            ),
        ]);
        let mut second = development;
        second.extend([
            case(
                "holdout-a",
                CalibrationSplit::Holdout,
                true,
                true,
                None,
                0,
                0,
            ),
            case(
                "holdout-b",
                CalibrationSplit::Holdout,
                false,
                false,
                None,
                0,
                99,
            ),
        ]);

        let first_result = calibrate_abstention_policy(&first, constraints()).unwrap();
        let second_result = calibrate_abstention_policy(&second, constraints()).unwrap();
        assert_eq!(first_result.policy, second_result.policy);
        assert_eq!(first_result.development, second_result.development);
        assert_eq!(
            first_result.development_case_ids,
            second_result.development_case_ids
        );
    }

    #[test]
    fn exact_evidence_is_not_erased_by_weak_margin_or_agreement() {
        let policy = AbstentionPolicy {
            min_top_score_margin: Some(100.0),
            min_independent_streams: Some(10),
            max_ambiguity_unresolved: Some(0),
        };
        let exact = case(
            "exact",
            CalibrationSplit::Development,
            true,
            true,
            None,
            0,
            0,
        );
        assert!(!policy.should_abstain(&exact));
    }

    #[test]
    fn exact_evidence_does_not_suppress_explicit_ambiguity() {
        let policy = AbstentionPolicy {
            min_top_score_margin: Some(100.0),
            min_independent_streams: Some(10),
            max_ambiguity_unresolved: Some(0),
        };
        let ambiguous_exact = case(
            "ambiguous-exact",
            CalibrationSplit::Development,
            false,
            true,
            Some(1.0),
            10,
            1,
        );
        assert!(policy.should_abstain(&ambiguous_exact));
    }

    #[test]
    fn enabled_margin_gate_fails_closed_when_margin_is_unavailable() {
        let policy = AbstentionPolicy {
            min_top_score_margin: Some(0.2),
            min_independent_streams: None,
            max_ambiguity_unresolved: None,
        };
        let missing = case(
            "missing-margin",
            CalibrationSplit::Development,
            true,
            false,
            None,
            3,
            0,
        );
        assert!(policy.should_abstain(&missing));
    }

    #[test]
    fn calibration_is_deterministic_under_case_reordering() {
        let mut cases = vec![
            case(
                "dev-positive",
                CalibrationSplit::Development,
                false,
                false,
                Some(0.7),
                3,
                0,
            ),
            case(
                "dev-negative-a",
                CalibrationSplit::Development,
                true,
                false,
                Some(0.1),
                1,
                1,
            ),
            case(
                "dev-negative-b",
                CalibrationSplit::Development,
                true,
                false,
                Some(0.2),
                1,
                0,
            ),
            case(
                "holdout-positive",
                CalibrationSplit::Holdout,
                false,
                false,
                Some(0.8),
                3,
                0,
            ),
            case(
                "holdout-negative",
                CalibrationSplit::Holdout,
                true,
                false,
                Some(0.1),
                1,
                1,
            ),
        ];
        let first = calibrate_abstention_policy(&cases, constraints()).unwrap();
        cases.reverse();
        let second = calibrate_abstention_policy(&cases, constraints()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn ambiguity_can_be_selected_without_manufacturing_score_confidence() {
        let cases = vec![
            case(
                "dev-positive",
                CalibrationSplit::Development,
                false,
                false,
                Some(0.5),
                2,
                0,
            ),
            case(
                "dev-negative",
                CalibrationSplit::Development,
                true,
                false,
                Some(0.5),
                2,
                2,
            ),
            case(
                "holdout-positive",
                CalibrationSplit::Holdout,
                false,
                false,
                Some(0.5),
                2,
                0,
            ),
            case(
                "holdout-negative",
                CalibrationSplit::Holdout,
                true,
                false,
                Some(0.5),
                2,
                3,
            ),
        ];
        let result = calibrate_abstention_policy(&cases, constraints()).unwrap();
        assert_eq!(result.policy.max_ambiguity_unresolved, Some(0));
        assert_eq!(result.development.no_gold_recall, 1.0);
        assert_eq!(result.development.positive_abstention_rate, 0.0);
        assert_eq!(result.holdout.no_gold_recall, 1.0);
    }

    #[test]
    fn invalid_features_and_duplicate_ids_are_rejected() {
        let invalid = vec![
            case(
                "bad-margin",
                CalibrationSplit::Development,
                false,
                false,
                Some(f64::NAN),
                1,
                0,
            ),
            case(
                "holdout",
                CalibrationSplit::Holdout,
                true,
                false,
                Some(0.1),
                1,
                0,
            ),
        ];
        assert!(matches!(
            calibrate_abstention_policy(&invalid, constraints()),
            Err(CalibrationError::InvalidFeature(_))
        ));

        let duplicate = vec![
            case(
                "same",
                CalibrationSplit::Development,
                false,
                false,
                Some(0.8),
                2,
                0,
            ),
            case(
                "same",
                CalibrationSplit::Development,
                true,
                false,
                Some(0.1),
                1,
                0,
            ),
            case(
                "holdout",
                CalibrationSplit::Holdout,
                true,
                false,
                Some(0.1),
                1,
                0,
            ),
        ];
        assert!(matches!(
            calibrate_abstention_policy(&duplicate, constraints()),
            Err(CalibrationError::DuplicateCaseId(id)) if id == "same"
        ));
    }

    #[test]
    fn impossible_safety_constraint_does_not_silently_pick_a_policy() {
        let cases = vec![
            case(
                "dev-positive",
                CalibrationSplit::Development,
                false,
                false,
                Some(0.1),
                1,
                0,
            ),
            case(
                "dev-negative",
                CalibrationSplit::Development,
                true,
                true,
                None,
                0,
                0,
            ),
            case(
                "holdout",
                CalibrationSplit::Holdout,
                true,
                false,
                Some(0.1),
                1,
                0,
            ),
        ];
        let result = calibrate_abstention_policy(
            &cases,
            AbstentionCalibrationConstraints {
                max_positive_abstention_rate: 0.0,
                min_no_gold_abstention_recall: 1.0,
            },
        );
        assert_eq!(result, Err(CalibrationError::NoEligiblePolicy));
    }

    #[test]
    fn invalid_constraints_fail_closed() {
        let error = calibrate_abstention_policy(
            &[],
            AbstentionCalibrationConstraints {
                max_positive_abstention_rate: 1.1,
                min_no_gold_abstention_recall: 0.0,
            },
        )
        .unwrap_err();
        assert!(matches!(error, CalibrationError::InvalidConstraint(_)));
    }
}
