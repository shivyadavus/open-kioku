#!/usr/bin/env python3
"""Check per-dimension retrieval quality against version-controlled floors (CC7).

Reads the retrieval baseline produced by `ok retrieval-bench --write-baseline` and the
per-dimension threshold contract. In `advisory` mode, violations are printed as GitHub
warning annotations and the exit code stays zero, so a per-language or per-task-family
regression is visible without blocking; in `blocking` mode violations fail the run.
Flipping the mode is a reviewed change to the thresholds file, never an implicit one.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 3:
        print(
            "usage: check-retrieval-dimension-thresholds.py <baseline.json> <thresholds.json>",
            file=sys.stderr,
        )
        return 2
    baseline = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
    thresholds = json.loads(Path(sys.argv[2]).read_text(encoding="utf-8"))

    if thresholds["schema_version"] != baseline["schema_version"]:
        print("dimension threshold schema does not match benchmark schema", file=sys.stderr)
        return 1
    if thresholds["corpus_id"] != baseline["corpus_id"]:
        print("dimension thresholds target a different corpus", file=sys.stderr)
        return 1
    mode = thresholds.get("mode", "advisory")
    if mode not in ("advisory", "blocking"):
        print(f"unknown dimension threshold mode: {mode}", file=sys.stderr)
        return 1

    strategy = next(
        item
        for item in baseline["strategies"]
        if item["strategy"] == thresholds["strategy"]
    )

    violations: list[str] = []
    for dimension, groups in thresholds["dimensions"].items():
        observed_groups = strategy.get(dimension)
        if observed_groups is None:
            violations.append(f"{dimension}: dimension missing from benchmark output")
            continue
        for group, contract in groups.items():
            observed = observed_groups.get(group)
            if observed is None:
                violations.append(f"{dimension}.{group}: group missing from benchmark output")
                continue
            for metric, minimum in contract.get("minimums", {}).items():
                value = observed[metric]
                if value < minimum:
                    violations.append(
                        f"{dimension}.{group}: {metric} {value:.6f} is below floor {minimum:.6f}"
                    )
            for metric, maximum in contract.get("maximums", {}).items():
                value = observed[metric]
                if value > maximum:
                    violations.append(
                        f"{dimension}.{group}: {metric} {value:.6f} exceeds ceiling {maximum:.6f}"
                    )

    if not violations:
        checked = sum(len(groups) for groups in thresholds["dimensions"].values())
        print(f"retrieval per-dimension thresholds passed ({checked} groups, mode={mode})")
        return 0

    for violation in violations:
        if mode == "advisory":
            print(f"::warning title=retrieval dimension regression::{violation}")
        else:
            print(f"retrieval dimension gate: {violation}", file=sys.stderr)
    if mode == "advisory":
        print(
            f"retrieval per-dimension thresholds: {len(violations)} advisory violation(s); "
            "not blocking (mode=advisory)"
        )
        return 0
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
