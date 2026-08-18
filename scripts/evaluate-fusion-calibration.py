#!/usr/bin/env python3
"""Evaluate a CC2 fusion candidate without tuning on the holdout split.

The retrieval benchmark already emits advisory CC2 stream/profile reports. This
script turns those reports into an explicit, machine-readable calibration
choice. It deliberately uses only the development split for the promotion
choice; holdout remains reserved for final evaluation after a candidate has
earned promotion eligibility.
"""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any

PRIMARY_HIGHER_IS_BETTER = (
    "recall_at_10",
    "mean_reciprocal_rank",
    "file_f1_at_10",
)
PRIMARY_LOWER_IS_BETTER = ("no_gold_false_positive_rate",)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("report", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--baseline", default="cc2:rrf_unweighted")
    parser.add_argument("--candidate", default="cc2:rrf_evidence_prior")
    parser.add_argument(
        "--min-quality-gain",
        type=float,
        default=0.01,
        help="Minimum absolute improvement in at least one higher-is-better metric.",
    )
    parser.add_argument(
        "--min-fp-gain",
        type=float,
        default=0.05,
        help="Minimum absolute reduction in no-gold false-positive rate.",
    )
    parser.add_argument(
        "--max-p95-regression-ratio",
        type=float,
        default=1.15,
        help="Maximum allowed development p95 latency ratio candidate/baseline.",
    )
    return parser.parse_args()


def finite_number(value: Any, label: str) -> float:
    if not isinstance(value, (int, float)):
        raise SystemExit(f"{label} must be numeric")
    value = float(value)
    if not math.isfinite(value):
        raise SystemExit(f"{label} must be finite")
    return value


def find_strategy(report: dict[str, Any], label: str) -> dict[str, Any]:
    matches = [
        strategy
        for strategy in report.get("stream_ablations", [])
        if strategy.get("strategy") == label
    ]
    if len(matches) != 1:
        raise SystemExit(
            f"expected exactly one stream ablation named {label!r}, got {len(matches)}"
        )
    return matches[0]


def development_metrics(strategy: dict[str, Any], label: str) -> dict[str, float]:
    split = strategy.get("by_split", {}).get("development")
    if split is None:
        raise SystemExit(f"{label} is missing the development split")
    quality = split.get("quality", {})
    latency = split.get("latency", {})
    values = {
        metric: finite_number(quality.get(metric), f"{label}.development.{metric}")
        for metric in (*PRIMARY_HIGHER_IS_BETTER, *PRIMARY_LOWER_IS_BETTER)
    }
    values["p95_ms"] = finite_number(
        latency.get("p95_ms"), f"{label}.development.p95_ms"
    )
    return values


def main() -> None:
    args = parse_args()
    if args.min_quality_gain < 0 or args.min_fp_gain < 0:
        raise SystemExit("minimum gains must be non-negative")
    if args.max_p95_regression_ratio < 1.0:
        raise SystemExit("max p95 regression ratio must be at least 1.0")

    report = json.loads(args.report.read_text())
    baseline_strategy = find_strategy(report, args.baseline)
    candidate_strategy = find_strategy(report, args.candidate)
    baseline = development_metrics(baseline_strategy, args.baseline)
    candidate = development_metrics(candidate_strategy, args.candidate)

    deltas: dict[str, float] = {}
    regressions: list[str] = []
    meaningful_gains: list[str] = []
    epsilon = 1e-12

    for metric in PRIMARY_HIGHER_IS_BETTER:
        delta = candidate[metric] - baseline[metric]
        deltas[metric] = delta
        if delta < -epsilon:
            regressions.append(metric)
        if delta >= args.min_quality_gain:
            meaningful_gains.append(metric)

    for metric in PRIMARY_LOWER_IS_BETTER:
        # Positive delta means the candidate improved because lower is better.
        delta = baseline[metric] - candidate[metric]
        deltas[metric] = delta
        if delta < -epsilon:
            regressions.append(metric)
        if delta >= args.min_fp_gain:
            meaningful_gains.append(metric)

    baseline_p95 = baseline["p95_ms"]
    candidate_p95 = candidate["p95_ms"]
    if baseline_p95 <= 0:
        raise SystemExit("baseline development p95_ms must be positive")
    p95_ratio = candidate_p95 / baseline_p95
    latency_acceptable = p95_ratio <= args.max_p95_regression_ratio + epsilon

    promote = not regressions and bool(meaningful_gains) and latency_acceptable
    reasons: list[str] = []
    if regressions:
        reasons.append("candidate regresses calibration quality: " + ", ".join(regressions))
    if not meaningful_gains:
        reasons.append("candidate has no meaningful development-split quality gain")
    if not latency_acceptable:
        reasons.append(
            "candidate development p95 latency regression exceeds allowed ratio "
            f"({p95_ratio:.4f} > {args.max_p95_regression_ratio:.4f})"
        )
    if promote:
        reasons.append(
            "candidate is eligible for separate holdout evaluation; this script does not inspect "
            "holdout metrics for calibration"
        )

    decision = {
        "schema_version": "1.0.0",
        "corpus_id": report.get("corpus_id"),
        "cases_file": report.get("cases_file"),
        "calibration_split": "development",
        "holdout_used_for_promotion_decision": False,
        "baseline_profile": args.baseline,
        "candidate_profile": args.candidate,
        "thresholds": {
            "min_quality_gain": args.min_quality_gain,
            "min_false_positive_rate_gain": args.min_fp_gain,
            "max_p95_regression_ratio": args.max_p95_regression_ratio,
        },
        "baseline": baseline,
        "candidate": candidate,
        "candidate_improvements": deltas,
        "meaningful_gains": meaningful_gains,
        "regressions": regressions,
        "p95_latency_ratio": p95_ratio,
        "promote_candidate_to_holdout_evaluation": promote,
        "production_default_changed": False,
        "reasons": reasons,
    }

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(decision, indent=2, sort_keys=True) + "\n")
    print(json.dumps(decision, sort_keys=True))


if __name__ == "__main__":
    main()
