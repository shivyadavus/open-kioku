use crate::cc6_calibration::{AbstentionCalibrationResult, AbstentionMetrics};
use serde::{Deserialize, Serialize};

/// Product-level safety thresholds used to decide whether an already-calibrated CC6 policy is
/// eligible for production activation.
///
/// These criteria are intentionally separate from calibration. They may evaluate untouched
/// holdout and operational measurements, but they can never change the calibrated policy.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AbstentionActivationCriteria {
    /// Maximum holdout positive-case abstention rate. A value of `0.0` forbids suppressing any
    /// positive holdout case.
    pub max_holdout_positive_abstention_rate: f64,
    /// Minimum holdout no-gold abstention recall required for activation.
    pub min_holdout_no_gold_abstention_recall: f64,
    /// Maximum permitted additional p95 latency in milliseconds. `None` means latency is
    /// reported but is not yet a blocking activation criterion.
    pub max_additional_p95_latency_ms: Option<f64>,
    /// Maximum permitted additional steady-state resident memory in MiB. `None` leaves memory
    /// advisory while still preserving the observation in the decision record.
    pub max_additional_steady_rss_mib: Option<f64>,
}

impl AbstentionActivationCriteria {
    fn validate(self) -> Result<Self, AbstentionActivationError> {
        for (name, value) in [
            (
                "max_holdout_positive_abstention_rate",
                self.max_holdout_positive_abstention_rate,
            ),
            (
                "min_holdout_no_gold_abstention_recall",
                self.min_holdout_no_gold_abstention_recall,
            ),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(AbstentionActivationError::InvalidCriteria(format!(
                    "{name} must be a finite value in [0, 1]"
                )));
            }
        }
        for (name, value) in [
            (
                "max_additional_p95_latency_ms",
                self.max_additional_p95_latency_ms,
            ),
            (
                "max_additional_steady_rss_mib",
                self.max_additional_steady_rss_mib,
            ),
        ] {
            if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
                return Err(AbstentionActivationError::InvalidCriteria(format!(
                    "{name} must be finite and non-negative when configured"
                )));
            }
        }
        Ok(self)
    }
}

/// Operational overhead measurements for applying the already-selected policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct AbstentionActivationCost {
    pub additional_p95_latency_ms: Option<f64>,
    pub additional_steady_rss_mib: Option<f64>,
}

impl AbstentionActivationCost {
    fn validate(self) -> Result<Self, AbstentionActivationError> {
        for (name, value) in [
            ("additional_p95_latency_ms", self.additional_p95_latency_ms),
            ("additional_steady_rss_mib", self.additional_steady_rss_mib),
        ] {
            if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
                return Err(AbstentionActivationError::InvalidMeasurement(format!(
                    "{name} must be finite and non-negative when present"
                )));
            }
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbstentionActivationBlockerKind {
    MissingHoldoutPositiveCases,
    MissingHoldoutNoGoldCases,
    PositiveAbstentionRegression,
    InsufficientNoGoldRecall,
    MissingLatencyMeasurement,
    LatencyRegression,
    MissingMemoryMeasurement,
    MemoryRegression,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbstentionActivationBlocker {
    pub kind: AbstentionActivationBlockerKind,
    pub message: String,
}

/// Auditable decision about whether a frozen CC6 policy is eligible for production activation.
///
/// `ready` is true only when every configured quality/resource criterion is satisfied. The
/// calibrated policy itself is deliberately not copied or modified here; callers must retain the
/// calibration result as the source of policy identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AbstentionActivationReadiness {
    pub ready: bool,
    pub criteria: AbstentionActivationCriteria,
    pub cost: AbstentionActivationCost,
    pub blockers: Vec<AbstentionActivationBlocker>,
    pub evaluation_basis: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbstentionActivationError {
    InvalidCriteria(String),
    InvalidMeasurement(String),
    InvalidEvaluationEvidence(String),
}

impl std::fmt::Display for AbstentionActivationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCriteria(message) => write!(f, "invalid activation criteria: {message}"),
            Self::InvalidMeasurement(message) => {
                write!(f, "invalid activation measurement: {message}")
            }
            Self::InvalidEvaluationEvidence(message) => {
                write!(f, "invalid activation evaluation evidence: {message}")
            }
        }
    }
}

impl std::error::Error for AbstentionActivationError {}

/// Evaluate whether an already-selected abstention policy is safe to activate.
///
/// Holdout results are evaluation-only. This function has no path to mutate policy parameters,
/// candidate thresholds, or calibration inputs, which prevents benchmark holdout leakage back
/// into policy selection.
pub fn evaluate_abstention_activation_readiness(
    calibration: &AbstentionCalibrationResult,
    criteria: AbstentionActivationCriteria,
    cost: AbstentionActivationCost,
) -> Result<AbstentionActivationReadiness, AbstentionActivationError> {
    let criteria = criteria.validate()?;
    let cost = cost.validate()?;
    validate_holdout_metrics(&calibration.holdout)?;
    let holdout = &calibration.holdout;
    let mut blockers = Vec::new();

    if holdout.positive_cases == 0 {
        blockers.push(blocker(
            AbstentionActivationBlockerKind::MissingHoldoutPositiveCases,
            "activation requires at least one untouched positive holdout case",
        ));
    } else if holdout.positive_abstention_rate > criteria.max_holdout_positive_abstention_rate {
        blockers.push(blocker(
            AbstentionActivationBlockerKind::PositiveAbstentionRegression,
            format!(
                "holdout positive abstention rate {:.6} exceeds allowed {:.6}",
                holdout.positive_abstention_rate, criteria.max_holdout_positive_abstention_rate
            ),
        ));
    }

    if holdout.no_gold_cases == 0 {
        blockers.push(blocker(
            AbstentionActivationBlockerKind::MissingHoldoutNoGoldCases,
            "activation requires at least one untouched no-gold holdout case",
        ));
    } else if holdout.no_gold_recall < criteria.min_holdout_no_gold_abstention_recall {
        blockers.push(blocker(
            AbstentionActivationBlockerKind::InsufficientNoGoldRecall,
            format!(
                "holdout no-gold abstention recall {:.6} is below required {:.6}",
                holdout.no_gold_recall, criteria.min_holdout_no_gold_abstention_recall
            ),
        ));
    }

    evaluate_cost_gate(
        criteria.max_additional_p95_latency_ms,
        cost.additional_p95_latency_ms,
        AbstentionActivationBlockerKind::MissingLatencyMeasurement,
        AbstentionActivationBlockerKind::LatencyRegression,
        "additional p95 latency",
        "ms",
        &mut blockers,
    );
    evaluate_cost_gate(
        criteria.max_additional_steady_rss_mib,
        cost.additional_steady_rss_mib,
        AbstentionActivationBlockerKind::MissingMemoryMeasurement,
        AbstentionActivationBlockerKind::MemoryRegression,
        "additional steady RSS",
        "MiB",
        &mut blockers,
    );

    Ok(AbstentionActivationReadiness {
        ready: blockers.is_empty(),
        criteria,
        cost,
        blockers,
        evaluation_basis: "policy selected exclusively from development cases; activation evaluated against validated untouched holdout plus independently configured operational gates; holdout never changes policy parameters".into(),
    })
}

fn validate_holdout_metrics(metrics: &AbstentionMetrics) -> Result<(), AbstentionActivationError> {
    if metrics.correct_no_gold_abstentions > metrics.no_gold_cases {
        return Err(AbstentionActivationError::InvalidEvaluationEvidence(
            "correct no-gold abstentions exceed no-gold holdout cases".into(),
        ));
    }
    if metrics.incorrect_positive_abstentions > metrics.positive_cases {
        return Err(AbstentionActivationError::InvalidEvaluationEvidence(
            "incorrect positive abstentions exceed positive holdout cases".into(),
        ));
    }
    if metrics.abstained_cases
        != metrics.correct_no_gold_abstentions + metrics.incorrect_positive_abstentions
    {
        return Err(AbstentionActivationError::InvalidEvaluationEvidence(
            "abstained holdout count is inconsistent with positive/no-gold abstention counts".into(),
        ));
    }

    for (name, value) in [
        ("precision", metrics.precision),
        ("no_gold_recall", metrics.no_gold_recall),
        ("positive_abstention_rate", metrics.positive_abstention_rate),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(AbstentionActivationError::InvalidEvaluationEvidence(format!(
                "holdout {name} must be a finite value in [0, 1]"
            )));
        }
    }

    let expected_precision = ratio(
        metrics.correct_no_gold_abstentions,
        metrics.abstained_cases,
    );
    let expected_recall = ratio(metrics.correct_no_gold_abstentions, metrics.no_gold_cases);
    let expected_positive_rate = ratio(
        metrics.incorrect_positive_abstentions,
        metrics.positive_cases,
    );
    for (name, actual, expected) in [
        ("precision", metrics.precision, expected_precision),
        ("no_gold_recall", metrics.no_gold_recall, expected_recall),
        (
            "positive_abstention_rate",
            metrics.positive_abstention_rate,
            expected_positive_rate,
        ),
    ] {
        if (actual - expected).abs() > f64::EPSILON {
            return Err(AbstentionActivationError::InvalidEvaluationEvidence(format!(
                "holdout {name} is inconsistent with its source counts"
            )));
        }
    }
    Ok(())
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_cost_gate(
    maximum: Option<f64>,
    observed: Option<f64>,
    missing_kind: AbstentionActivationBlockerKind,
    regression_kind: AbstentionActivationBlockerKind,
    label: &str,
    unit: &str,
    blockers: &mut Vec<AbstentionActivationBlocker>,
) {
    let Some(maximum) = maximum else {
        return;
    };
    let Some(observed) = observed else {
        blockers.push(blocker(
            missing_kind,
            format!("activation criterion for {label} is configured but no measurement is available"),
        ));
        return;
    };
    if observed > maximum {
        blockers.push(blocker(
            regression_kind,
            format!("{label} {observed:.6} {unit} exceeds allowed {maximum:.6} {unit}"),
        ));
    }
}

fn blocker(
    kind: AbstentionActivationBlockerKind,
    message: impl Into<String>,
) -> AbstentionActivationBlocker {
    AbstentionActivationBlocker {
        kind,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cc6_calibration::{
        AbstentionCalibrationConstraints, AbstentionMetrics, AbstentionPolicy,
    };

    fn metrics(
        positive_cases: usize,
        no_gold_cases: usize,
        incorrect_positive_abstentions: usize,
        correct_no_gold_abstentions: usize,
    ) -> AbstentionMetrics {
        let abstained_cases = incorrect_positive_abstentions + correct_no_gold_abstentions;
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

    fn calibration(holdout: AbstentionMetrics) -> AbstentionCalibrationResult {
        AbstentionCalibrationResult {
            policy: AbstentionPolicy::never_abstain(),
            constraints: AbstentionCalibrationConstraints {
                max_positive_abstention_rate: 0.0,
                min_no_gold_abstention_recall: 0.0,
            },
            development: metrics(2, 2, 0, 0),
            holdout,
            development_case_ids: vec!["dev-positive".into(), "dev-no-gold".into()],
            holdout_case_ids: vec!["holdout-positive".into(), "holdout-no-gold".into()],
            selection_basis: "development only".into(),
        }
    }

    fn strict_criteria() -> AbstentionActivationCriteria {
        AbstentionActivationCriteria {
            max_holdout_positive_abstention_rate: 0.0,
            min_holdout_no_gold_abstention_recall: 0.5,
            max_additional_p95_latency_ms: Some(2.0),
            max_additional_steady_rss_mib: Some(1.0),
        }
    }

    #[test]
    fn readiness_requires_holdout_quality_and_configured_cost_evidence() {
        let result = evaluate_abstention_activation_readiness(
            &calibration(metrics(2, 2, 0, 1)),
            strict_criteria(),
            AbstentionActivationCost {
                additional_p95_latency_ms: Some(1.25),
                additional_steady_rss_mib: Some(0.5),
            },
        )
        .unwrap();

        assert!(result.ready);
        assert!(result.blockers.is_empty());
    }

    #[test]
    fn readiness_fails_closed_when_required_measurements_are_missing() {
        let result = evaluate_abstention_activation_readiness(
            &calibration(metrics(1, 1, 0, 1)),
            strict_criteria(),
            AbstentionActivationCost::default(),
        )
        .unwrap();

        assert!(!result.ready);
        assert_eq!(
            result
                .blockers
                .iter()
                .map(|blocker| &blocker.kind)
                .collect::<Vec<_>>(),
            vec![
                &AbstentionActivationBlockerKind::MissingLatencyMeasurement,
                &AbstentionActivationBlockerKind::MissingMemoryMeasurement,
            ]
        );
    }

    #[test]
    fn readiness_blocks_positive_suppression_even_with_perfect_no_gold_recall() {
        let result = evaluate_abstention_activation_readiness(
            &calibration(metrics(4, 2, 1, 2)),
            AbstentionActivationCriteria {
                max_additional_p95_latency_ms: None,
                max_additional_steady_rss_mib: None,
                ..strict_criteria()
            },
            AbstentionActivationCost::default(),
        )
        .unwrap();

        assert!(!result.ready);
        assert_eq!(
            result.blockers[0].kind,
            AbstentionActivationBlockerKind::PositiveAbstentionRegression
        );
    }

    #[test]
    fn readiness_requires_both_holdout_classes_instead_of_treating_zero_denominators_as_success() {
        let result = evaluate_abstention_activation_readiness(
            &calibration(metrics(0, 0, 0, 0)),
            AbstentionActivationCriteria {
                max_additional_p95_latency_ms: None,
                max_additional_steady_rss_mib: None,
                ..strict_criteria()
            },
            AbstentionActivationCost::default(),
        )
        .unwrap();

        assert!(!result.ready);
        assert_eq!(result.blockers.len(), 2);
        assert_eq!(
            result.blockers[0].kind,
            AbstentionActivationBlockerKind::MissingHoldoutPositiveCases
        );
        assert_eq!(
            result.blockers[1].kind,
            AbstentionActivationBlockerKind::MissingHoldoutNoGoldCases
        );
    }

    #[test]
    fn advisory_cost_metrics_do_not_block_when_no_operational_limit_is_configured() {
        let result = evaluate_abstention_activation_readiness(
            &calibration(metrics(1, 1, 0, 1)),
            AbstentionActivationCriteria {
                max_additional_p95_latency_ms: None,
                max_additional_steady_rss_mib: None,
                ..strict_criteria()
            },
            AbstentionActivationCost {
                additional_p95_latency_ms: Some(500.0),
                additional_steady_rss_mib: Some(500.0),
            },
        )
        .unwrap();

        assert!(result.ready);
    }

    #[test]
    fn invalid_nan_and_negative_inputs_fail_closed() {
        let calibration = calibration(metrics(1, 1, 0, 0));
        let invalid_criteria = AbstentionActivationCriteria {
            max_holdout_positive_abstention_rate: f64::NAN,
            min_holdout_no_gold_abstention_recall: 0.0,
            max_additional_p95_latency_ms: None,
            max_additional_steady_rss_mib: None,
        };
        assert!(matches!(
            evaluate_abstention_activation_readiness(
                &calibration,
                invalid_criteria,
                AbstentionActivationCost::default()
            ),
            Err(AbstentionActivationError::InvalidCriteria(_))
        ));

        assert!(matches!(
            evaluate_abstention_activation_readiness(
                &calibration,
                AbstentionActivationCriteria {
                    max_holdout_positive_abstention_rate: 1.0,
                    min_holdout_no_gold_abstention_recall: 0.0,
                    max_additional_p95_latency_ms: Some(1.0),
                    max_additional_steady_rss_mib: None,
                },
                AbstentionActivationCost {
                    additional_p95_latency_ms: Some(-0.1),
                    additional_steady_rss_mib: None,
                }
            ),
            Err(AbstentionActivationError::InvalidMeasurement(_))
        ));
    }

    #[test]
    fn malformed_holdout_metrics_cannot_fail_open_on_nan_or_inconsistent_counts() {
        let mut nan_metrics = metrics(1, 1, 0, 1);
        nan_metrics.positive_abstention_rate = f64::NAN;
        assert!(matches!(
            evaluate_abstention_activation_readiness(
                &calibration(nan_metrics),
                AbstentionActivationCriteria {
                    max_additional_p95_latency_ms: None,
                    max_additional_steady_rss_mib: None,
                    ..strict_criteria()
                },
                AbstentionActivationCost::default()
            ),
            Err(AbstentionActivationError::InvalidEvaluationEvidence(_))
        ));

        let mut inconsistent = metrics(1, 1, 0, 1);
        inconsistent.abstained_cases = 0;
        assert!(matches!(
            evaluate_abstention_activation_readiness(
                &calibration(inconsistent),
                AbstentionActivationCriteria {
                    max_additional_p95_latency_ms: None,
                    max_additional_steady_rss_mib: None,
                    ..strict_criteria()
                },
                AbstentionActivationCost::default()
            ),
            Err(AbstentionActivationError::InvalidEvaluationEvidence(_))
        ));
    }

    #[test]
    fn holdout_evaluation_cannot_mutate_or_replace_the_calibrated_policy() {
        let calibration = calibration(metrics(1, 1, 0, 1));
        let policy_before = calibration.policy.clone();

        let _ = evaluate_abstention_activation_readiness(
            &calibration,
            AbstentionActivationCriteria {
                max_holdout_positive_abstention_rate: 0.0,
                min_holdout_no_gold_abstention_recall: 1.0,
                max_additional_p95_latency_ms: None,
                max_additional_steady_rss_mib: None,
            },
            AbstentionActivationCost::default(),
        )
        .unwrap();

        assert_eq!(calibration.policy, policy_before);
    }
}
