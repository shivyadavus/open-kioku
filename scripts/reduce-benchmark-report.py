#!/usr/bin/env python3
"""Reduce commit-derived benchmark reports to aggregates before they are uploaded.

    scripts/reduce-benchmark-report.py REPORT.json [REPORT.json ...] [--coverage COVERAGE.json]

A report written by `scripts/score-context-cases.py --out` carries per-case `rows`: commit
hashes, commit subjects, and corpus file paths. Each REPORT is rewritten in place with its
aggregate fields only, plus `cases_scored` and `cases_errored` counted from the rows it drops.
Every coverage object, inside a report or in a `--coverage` file (the `coverage` object of
`ok --json status`), is reduced to its counts; per-directory breakdowns are dropped.

The fields kept are allowlists, so a field added to either shape later is dropped until it is
reviewed and listed here. Run it after the baseline comparison and the job summary, which read
the full report, and upload only when it succeeded, so corpus identity stays out of public logs
and artifacts. A path that does not exist is skipped (a step that failed wrote none); a file
that exists but cannot be reduced exits 1, and only its exception type is printed.
"""
import argparse
import json
import sys
from pathlib import Path

# Aggregates only. `by_task_family` holds per-family case counts and metrics, never cases.
REPORT_KEYS = (
    "label", "metrics", "ci", "median_secs", "yield_budgets", "coverage", "coverage_line",
    "coverage_error", "by_task_family", "cases_scored", "cases_errored",
)
COVERAGE_KEYS = (
    "discovered", "indexed", "generated", "skipped", "by_language", "pruned_dirs", "walk_errors",
    "policy_excluded_by_source",
)
LANGUAGE_COVERAGE_KEYS = ("discovered", "indexed", "generated", "skipped")


def reduce_coverage(coverage):
    if not isinstance(coverage, dict):
        return None
    reduced = {k: coverage[k] for k in COVERAGE_KEYS if k in coverage}
    if isinstance(reduced.get("by_language"), dict):
        reduced["by_language"] = {
            language: {k: entry[k] for k in LANGUAGE_COVERAGE_KEYS if k in entry}
            for language, entry in reduced["by_language"].items()
            if isinstance(entry, dict)
        }
    return reduced


def reduce_report(report):
    if not isinstance(report, dict):
        raise TypeError("a report is a JSON object")
    reduced = dict(report)
    rows = reduced.get("rows")
    if isinstance(rows, list):
        reduced["cases_scored"] = sum(1 for r in rows if isinstance(r, dict) and "err" not in r)
        reduced["cases_errored"] = len(rows) - reduced["cases_scored"]
    reduced = {k: reduced[k] for k in REPORT_KEYS if k in reduced}
    if "coverage" in reduced:
        reduced["coverage"] = reduce_coverage(reduced["coverage"])
    return reduced


def rewrite(path, reducer):
    """Reduce one file in place; returns False when it exists and could not be reduced."""
    if not path.exists():
        print(f"{path}: absent, skipped")
        return True
    try:
        before = json.loads(path.read_text())
        after = reducer(before)
        path.write_text(json.dumps(after, indent=1) + "\n")
    except Exception as err:  # noqa: BLE001 - the message can quote the file's content
        print(f"{path}: could not be reduced ({type(err).__name__})", file=sys.stderr)
        return False
    dropped = sorted(set(before) - set(after)) if isinstance(before, dict) and isinstance(after, dict) else []
    print(f"{path}: {len(after or {})} aggregate fields kept" + (f", dropped {', '.join(dropped)}" if dropped else ""))
    return True


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("reports", nargs="*", type=Path)
    ap.add_argument("--coverage", action="append", default=[], type=Path)
    args = ap.parse_args(argv)
    ok = all([rewrite(p, reduce_report) for p in args.reports]
             + [rewrite(p, reduce_coverage) for p in args.coverage])
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
