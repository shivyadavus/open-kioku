#!/usr/bin/env python3
"""Create or validate the stable projection of a relationship conformance report.

The raw report intentionally contains run provenance that changes by commit. The release baseline
captures only deterministic conformance output: corpus/schema identity, observation digest, quality
metrics, capability results, proof/strategy distributions, and metamorphic equivalence.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def stable_projection(report: dict[str, Any]) -> dict[str, Any]:
    required = [
        "schema_version",
        "corpus_version",
        "corpus_status",
        "observation_digest",
        "overall",
        "by_language",
        "by_relationship",
        "by_language_relationship",
        "observed_proof_kind_counts",
        "by_resolver_strategy",
        "by_proof_kind",
        "metamorphic_groups",
        "metamorphic_equivalent_groups",
        "metamorphic_equivalence",
        "capabilities",
    ]
    missing = [key for key in required if key not in report]
    if missing:
        raise SystemExit(f"relationship report is missing stable field(s): {missing}")
    gate = report.get("gate")
    if gate is not None and not gate.get("passed", False):
        raise SystemExit(f"cannot baseline a failing relationship report: {gate.get('failures', [])}")
    if report.get("diagnostics"):
        raise SystemExit(
            f"cannot baseline a relationship report with {len(report['diagnostics'])} diagnostic(s)"
        )
    projection = {key: report[key] for key in required}
    projection["baseline_schema_version"] = "1.0.0"
    projection["baseline_role"] = "first_approved_v3_relationship_conformance_baseline"
    return projection


def canonical_json(value: dict[str, Any]) -> str:
    return json.dumps(value, indent=2, sort_keys=True) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("report", type=Path)
    parser.add_argument("baseline", type=Path)
    parser.add_argument(
        "--write",
        action="store_true",
        help="write the stable projection to baseline instead of comparing",
    )
    args = parser.parse_args()

    report = json.loads(args.report.read_text())
    observed = stable_projection(report)
    rendered = canonical_json(observed)
    if args.write:
        args.baseline.parent.mkdir(parents=True, exist_ok=True)
        args.baseline.write_text(rendered)
        print(f"wrote relationship baseline {args.baseline}")
        return

    if not args.baseline.is_file():
        raise SystemExit(f"relationship baseline is missing: {args.baseline}")
    expected = json.loads(args.baseline.read_text())
    if observed != expected:
        observed_path = args.baseline.with_suffix(args.baseline.suffix + ".observed")
        observed_path.write_text(rendered)
        raise SystemExit(
            "relationship conformance baseline changed; inspect the full report and the observed "
            f"projection at {observed_path}. Update the checked-in baseline only after explicit review."
        )
    print("relationship conformance matches the checked-in approved baseline")


if __name__ == "__main__":
    main()
