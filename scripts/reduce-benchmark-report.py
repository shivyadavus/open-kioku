#!/usr/bin/env python3
"""Reduce commit-derived benchmark reports to aggregates before they are uploaded.

    scripts/reduce-benchmark-report.py REPORT.json [REPORT.json ...] [--coverage COVERAGE.json]

A report written by `scripts/score-context-cases.py --out` carries per-case `rows`: commit
hashes, commit subjects, and corpus file paths. Each REPORT is rewritten in place with its
aggregate fields only, plus `cases_scored` and `cases_errored` counted from the rows it drops.
Every coverage object, inside a report or in a `--coverage` file (the `coverage` object of
`ok --json status`), is reduced to its counts; per-directory breakdowns are dropped.

Both the fields and the shape of each value are allowlisted. A kept value must be one of:
- a number;
- a map of metric or reason names to numbers;
- a map of metric names to confidence-interval pairs;
- the per-family section, built from those maps.
Map keys must be short names, not paths or commit hashes. A field that is not listed, or whose
value has another shape (a list of cases under a listed key, say), is dropped and named. Only
`label`, `coverage_line`, and `coverage_error` are text, as `score-context-cases.py` builds them.

Run it after the baseline comparison and the job summary, which read the full report, and upload
only when it succeeded, so corpus identity stays out of public logs and artifacts. A path that
does not exist is skipped, because a step that failed wrote none. A file that exists but cannot be
reduced exits 1, and only its exception type is printed.
"""
import argparse
import json
import re
import sys
from pathlib import Path

NAME = re.compile(r"[A-Za-z][A-Za-z0-9_@.\-]{0,63}")
# A run of seven or more hex characters containing a digit reads as an abbreviated commit.
HASH_LIKE = re.compile(r"(?=[0-9a-fA-F]*[0-9])[0-9a-fA-F]{7,}")
TEXT_LIMIT = 600
INVALID = object()

COVERAGE_COUNTS = ("discovered", "indexed", "generated", "pruned_dirs", "walk_errors")
COVERAGE_MAPS = ("skipped", "policy_excluded_by_source")
LANGUAGE_COUNTS = ("discovered", "indexed", "generated")
FAMILY_COUNTS = ("min_cases", "scored_cases", "unassigned_cases")


def is_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def is_name(value):
    return isinstance(value, str) and bool(NAME.fullmatch(value)) and not HASH_LIKE.search(value)


def is_text(value):
    return isinstance(value, str) and len(value) <= TEXT_LIMIT


def is_number_map(value):
    return isinstance(value, dict) and all(is_name(k) and is_number(v) for k, v in value.items())


def is_interval_map(value):
    return isinstance(value, dict) and all(
        is_name(k) and isinstance(v, (list, tuple)) and len(v) == 2 and all(is_number(x) for x in v)
        for k, v in value.items()
    )


def reduce_coverage(coverage):
    if not isinstance(coverage, dict):
        return None
    reduced = {k: coverage[k] for k in COVERAGE_COUNTS if is_number(coverage.get(k))}
    reduced.update({k: coverage[k] for k in COVERAGE_MAPS if is_number_map(coverage.get(k))})
    languages = coverage.get("by_language")
    if isinstance(languages, dict):
        reduced["by_language"] = {}
        for language, entry in languages.items():
            if not is_name(language) or not isinstance(entry, dict):
                continue
            kept = {k: entry[k] for k in LANGUAGE_COUNTS if is_number(entry.get(k))}
            if is_number_map(entry.get("skipped")):
                kept["skipped"] = entry["skipped"]
            reduced["by_language"][language] = kept
    return reduced


def reduce_family_section(section, notes):
    if not isinstance(section, dict) or not isinstance(section.get("families"), dict):
        return INVALID
    reduced = {k: section[k] for k in FAMILY_COUNTS if is_number(section.get(k))}
    if is_name(section.get("assignment")):
        reduced["assignment"] = section["assignment"]
    families = {}
    for family, entry in section["families"].items():
        well_formed = (
            is_name(family) and isinstance(entry, dict) and is_number(entry.get("cases"))
            and isinstance(entry.get("insufficient"), bool) and is_number_map(entry.get("metrics"))
            and is_interval_map(entry.get("ci")) and is_number_map(entry.get("case_coverage", {}))
        )
        if not well_formed:
            notes.append("dropped a by_task_family entry of unexpected shape")
            continue
        families[family] = {k: entry[k] for k in ("cases", "insufficient", "metrics", "ci")}
        families[family]["case_coverage"] = entry.get("case_coverage", {})
    reduced["families"] = families
    return reduced


def keep_if(predicate):
    return lambda value, notes: value if predicate(value) else INVALID


FIELDS = {
    "label": keep_if(is_name),
    "metrics": keep_if(is_number_map),
    "ci": keep_if(is_interval_map),
    "median_secs": keep_if(is_number),
    "yield_budgets": keep_if(lambda v: isinstance(v, list) and all(is_number(x) for x in v)),
    "coverage": lambda v, notes: None if v is None else (reduce_coverage(v) if isinstance(v, dict) else INVALID),
    "coverage_line": keep_if(is_text),
    "coverage_error": keep_if(lambda v: v is None or is_text(v)),
    "by_task_family": reduce_family_section,
    "cases_scored": keep_if(is_number),
    "cases_errored": keep_if(is_number),
}


def reduce_report(report, notes):
    if not isinstance(report, dict):
        raise TypeError("a report is a JSON object")
    source = dict(report)
    rows = source.get("rows")
    if isinstance(rows, list):
        source["cases_scored"] = sum(1 for r in rows if isinstance(r, dict) and "err" not in r)
        source["cases_errored"] = len(rows) - source["cases_scored"]
    reduced = {}
    for key, value in source.items():
        label = key if is_name(key) else "a field without a plain name"
        check = FIELDS.get(key)
        if check is None:
            notes.append(f"dropped {label}")
            continue
        kept = check(value, notes)
        if kept is INVALID:
            notes.append(f"dropped {label} (unexpected shape)")
            continue
        reduced[key] = kept
    return reduced


def reduce_coverage_file(coverage, notes):
    if coverage is not None and not isinstance(coverage, dict):
        raise TypeError("a coverage file holds a JSON object or null")
    return reduce_coverage(coverage)


def rewrite(path, reducer):
    """Reduce one file in place; returns False when it exists and could not be reduced."""
    if not path.exists():
        print(f"{path}: absent, skipped")
        return True
    notes = []
    try:
        reduced = reducer(json.loads(path.read_text()), notes)
        path.write_text(json.dumps(reduced, indent=1) + "\n")
    except Exception as err:  # noqa: BLE001 - the message can quote the file's content
        print(f"{path}: could not be reduced ({type(err).__name__})", file=sys.stderr)
        return False
    print(f"{path}: {len(reduced or {})} aggregate fields kept" + "".join(f"; {n}" for n in notes))
    return True


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("reports", nargs="*", type=Path)
    ap.add_argument("--coverage", action="append", default=[], type=Path)
    args = ap.parse_args(argv)
    results = [rewrite(p, reduce_report) for p in args.reports]
    results += [rewrite(p, reduce_coverage_file) for p in args.coverage]
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
