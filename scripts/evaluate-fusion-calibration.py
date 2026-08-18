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
from collections import defaultdict
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


def development_cases(strategy: dict[str, Any], label: str) -> list[dict[str, Any]]:
    cases = strategy.get("cases")
    if not isinstance(cases, list):
        raise SystemExit(f"{label}.cases must be a list")
    development = [case for case in cases if case.get("split") == "development"]
    if not development:
        raise SystemExit(f"{label} has no development cases")
    for index, case in enumerate(development):
        if not isinstance(case, dict):
            raise SystemExit(f"{label}.development case {index} must be an object")
        if not isinstance(case.get("id"), str) or not case["id"]:
            raise SystemExit(f"{label}.development case {index} is missing a case id")
    return development


def case_recall_at_10(case: dict[str, Any], label: str) -> float:
    recall = case.get("recall_at")
    if not isinstance(recall, dict):
        raise SystemExit(f"{label}.recall_at must be an object")
    value = recall.get("10", recall.get(10))
    return finite_number(value, f"{label}.recall_at_10")


def summarize_cases(cases: list[dict[str, Any]], label: str) -> dict[str, float]:
    positives = [case for case in cases if not case.get("no_gold_expected", False)]
    no_gold = [case for case in cases if case.get("no_gold_expected", False)]

    def mean_positive(metric: str) -> float:
        if not positives:
            return 0.0
        if metric == "recall_at_10":
            values = [
                case_recall_at_10(case, f"{label}.{case['id']}") for case in positives
            ]
        else:
            source = {
                "mean_reciprocal_rank": "reciprocal_rank",
                "file_f1_at_10": "file_f1_at_10",
            }[metric]
            values = [
                finite_number(case.get(source), f"{label}.{case['id']}.{source}")
                for case in positives
            ]
        return sum(values) / len(values)

    false_positive_rate = 0.0
    if no_gold:
        false_positive_rate = sum(bool(case.get("returned_any", False)) for case in no_gold) / len(
            no_gold
        )

    return {
        "recall_at_10": mean_positive("recall_at_10"),
        "mean_reciprocal_rank": mean_positive("mean_reciprocal_rank"),
        "file_f1_at_10": mean_positive("file_f1_at_10"),
        "no_gold_false_positive_rate": false_positive_rate,
    }


def development_subgroups(
    strategy: dict[str, Any], label: str
) -> dict[str, dict[str, float]]:
    cases = development_cases(strategy, label)
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for case in cases:
        task_family = case.get("task_family")
        if not isinstance(task_family, str) or not task_family:
            raise SystemExit(f"{label}.{case['id']} is missing task_family")
        grouped[f"task_family:{task_family}"].append(case)

        shape = case.get("expected_query_shape")
        if shape is not None:
            if not isinstance(shape, str) or not shape:
                raise SystemExit(f"{label}.{case['id']} has invalid expected_query_shape")
            grouped[f"query_shape:{shape}"].append(case)
            grouped[f"task_family_query_shape:{task_family}:{shape}"].append(case)

    return {
        group: summarize_cases(group_cases, f"{label}.{group}")
        for group, group_cases in sorted(grouped.items())
    }


def subgroup_regression_report(
    baseline_strategy: dict[str, Any],
    candidate_strategy: dict[str, Any],
    baseline_label: str,
    candidate_label: str,
    epsilon: float,
) -> tuple[dict[str, dict[str, float]], list[str]]:
    baseline_cases = development_cases(baseline_strategy, baseline_label)
    candidate_cases = development_cases(candidate_strategy, candidate_label)
    baseline_ids = {case["id"] for case in baseline_cases}
    candidate_ids = {case["id"] for case in candidate_cases}
    if baseline_ids != candidate_ids:
        missing = sorted(baseline_ids - candidate_ids)
        extra = sorted(candidate_ids - baseline_ids)
        raise SystemExit(
            "baseline/candidate development case identities differ "
            f"(missing={missing}, extra={extra})"
        )

    baseline_groups = development_subgroups(baseline_strategy, baseline_label)
    candidate_groups = development_subgroups(candidate_strategy, candidate_label)
    if baseline_groups.keys() != candidate_groups.keys():
        raise SystemExit("baseline/candidate development subgroup identities differ")

    deltas: dict[str, dict[str, float]] = {}
    regressions: list[str] = []
    for group in baseline_groups:
        group_deltas: dict[str, float] = {}
        for metric in PRIMARY_HIGHER_IS_BETTER:
            delta = candidate_groups[group][metric] - baseline_groups[group][metric]
            group_deltas[metric] = delta
            if delta < -epsilon:
                regressions.append(f"{group}:{metric}")
        for metric in PRIMARY_LOWER_IS_BETTER:
            delta = baseline_groups[group][metric] - candidate_groups[group][metric]
            group_deltas[metric] = delta
            if delta < -epsilon:
                regressions.append(f"{group}:{metric}")
        deltas[group] = group_deltas
    return deltas, regressions


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

    subgroup_deltas, subgroup_regressions = subgroup_regression_report(
        baseline_strategy,
        candidate_strategy,
        args.baseline,
        args.candidate,
        epsilon,
    )

    baseline_p95 = baseline["p95_ms"]
    candidate_p95 = candidate["p95_ms"]
    if baseline_p95 <= 0:
        raise SystemExit("baseline development p95_ms must be positive")
    p95_ratio = candidate_p95 / baseline_p95
    latency_acceptable = p95_ratio <= args.max_p95_regression_ratio + epsilon

    promote = (
        not regressions
        and not subgroup_regressions
        and bool(meaningful_gains)
        and latency_acceptable
    )
    reasons: list[str] = []
    if regressions:
        reasons.append("candidate regresses calibration quality: " + ", ".join(regressions))
    if subgroup_regressions:
        reasons.append(
            "candidate regresses development subgroup quality: "
            + ", ".join(subgroup_regressions)
        )
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
        "schema_version": "1.1.0",
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
        "development_subgroup_improvements": subgroup_deltas,
        "development_subgroup_regressions": subgroup_regressions,
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
