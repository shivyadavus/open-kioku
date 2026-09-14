#!/usr/bin/env python3
"""Compare a commit-derived benchmark report against its frozen baseline.

    scripts/compare-commit-derived-report.py REPORT.json BASELINE.json [--slack 0.03]

Both files are produced by `scripts/score-context-cases.py --out`. A metric may not fall
more than `slack` (absolute) below the baseline; the 95% bootstrap interval is printed so
a reader can tell a real regression from sampling noise. Missing baseline: prints the
report and exits 0, so the first run of a new corpus freezes rather than fails.

Per routed task family (`by_task_family`), the same watched metrics follow the same rule and
the same exit status: a family's metric may not fall more than `slack` below that family's
baseline, and such a regression exits 1 exactly as an aggregate one does. A family is gated
only when the report and the baseline both carry it with at least the report's `min_cases`
scored cases. Every family is printed with its gate status and 95% interval. Families are the
router's labels: a per-family number measures the retrieval policy on the cases routed to it,
not whether routing chose the right family. Case counts are printed for both sides, but equal
counts do not mean equal membership: a routing change can swap cases between families at the
same count, so per-family numbers are not comparable across builds whose routing changed.
`summary_table` renders the same statuses for the nightly job summary.

Gold yield at a token budget (`gold_file_yield@B`, `gold_line_yield@B`, median
`tokens_to_first_gold`) is printed when the report carries it, informationally: it is not
gated yet, and a baseline frozen before the metric existed compares without it.

Index coverage (source files discovered versus indexed) is printed for both sides so a
metric shift can be read against how much of the corpus each index held. It is
informational: a coverage change is not a gate yet.
"""
import json
import sys
from pathlib import Path

WATCHED = ("R@5", "R@20", "MRR", "gold_recall@20")
SLACK = 0.03
INFORMATIONAL = (
    "gold_file_yield@4000", "gold_file_yield@8000", "gold_file_yield@16000",
    "gold_line_yield@4000", "gold_line_yield@8000", "gold_line_yield@16000",
)

# Printed wherever per-family numbers are. `scripts/score-context-cases.py` carries the same
# sentence; `scripts/tests/test_commit_derived_families.py` pins the two together.
FAMILY_CAVEAT = (
    "task families are the router's labels (retrieval_diagnostics.routing.task_family); a "
    "per-family number measures the retrieval policy on the cases routed to it, not whether "
    "routing chose the right family"
)

SUMMARY_METRICS = ("R@5", "R@20", "MRR")


def print_yield(report, baseline=None):
    present = [k for k in INFORMATIONAL if k in report["metrics"]]
    if not present:
        return
    print("  gold yield at a token budget (informational, not gated):")
    for k in present:
        lo, hi = report["ci"].get(k, (float("nan"), float("nan")))
        line = f"  {k:22} {report['metrics'][k]:.4f}   95% CI [{lo:.4f}, {hi:.4f}]"
        if baseline and k in baseline.get("metrics", {}):
            line += f"   baseline {baseline['metrics'][k]:.4f} ({report['metrics'][k] - baseline['metrics'][k]:+.4f})"
        print(line)
    first = report["metrics"].get("tokens_to_first_gold_p50")
    if first is not None:
        line = f"  {'tokens_to_first_gold':22} median {first:.0f}"
        if baseline and baseline.get("metrics", {}).get("tokens_to_first_gold_p50") is not None:
            line += f"   baseline {baseline['metrics']['tokens_to_first_gold_p50']:.0f}"
        print(line)


def family_section(report):
    """The report's `by_task_family` section, or None when it was scored without one."""
    section = (report or {}).get("by_task_family")
    if isinstance(section, dict) and isinstance(section.get("families"), dict):
        return section
    return None


def family_status(family, section, base_section):
    """(gated, label) for one family of a report section against a baseline section or None.

    Gated only when the report and the baseline both carry the family with at least the
    report's `min_cases` cases. Every other status says why the family is not gated.
    """
    min_cases = section["min_cases"]
    now = section["families"].get(family)
    base = (base_section or {}).get("families", {}).get(family)
    if now is None:
        return False, "absent from the report; not gated"
    short = [side for side, entry in (("report", now), ("baseline", base))
             if entry is not None and entry["cases"] < min_cases]
    if short:
        return False, f"insufficient: fewer than {min_cases} cases on the {' and '.join(short)}; not gated"
    if base_section is None:
        return False, "informational: no family baseline frozen; not gated"
    if base is None:
        return False, "informational: absent from the baseline; not gated"
    return True, "gated"


def bounds(entry, k):
    lo, hi = entry.get("ci", {}).get(k) or (None, None)
    return None if lo is None else (lo, hi)


def interval(entry, k):
    span = bounds(entry, k)
    return "95% CI not computed" if span is None else f"95% CI [{span[0]:.4f}, {span[1]:.4f}]"


def compare_families(report, baseline, slack=SLACK):
    """Print the per-family comparison and return the gated regressions as 'family:metric'."""
    section = family_section(report)
    if section is None:
        print("  per routed task family: the report has no by_task_family section; nothing compared")
        return []
    base_section = family_section(baseline)
    print(f"  per routed task family ({section['assignment']}; {section['unassigned_cases']} cases unassigned):")
    print(f"  note: {FAMILY_CAVEAT}")
    if baseline is not None and base_section is None:
        print("  the baseline has no by_task_family section; no family is gated until one is frozen")
    families = section["families"]
    base_families = (base_section or {}).get("families", {})
    failed = []
    for family in list(families) + [f for f in base_families if f not in families]:
        gated, label = family_status(family, section, base_section)
        now, base = families.get(family), base_families.get(family)
        if now is None:
            print(f"  {family:20} baseline {base['cases']} cases [{label}]")
            continue
        counts = f"cases {base['cases']} -> {now['cases']}" if base else f"{now['cases']} cases"
        print(f"  {family:20} {counts} [{label}]")
        for k in WATCHED:
            n = now["metrics"][k]
            if base is None:
                print(f"    {k:16} {n:.4f}   {interval(now, k)}")
                continue
            b = base["metrics"][k]
            marker = ""
            if n < b - slack:
                if gated:
                    marker = "  <-- REGRESSION"
                    failed.append(f"{family}:{k}")
                else:
                    marker = "  <-- below slack (not gated)"
            print(f"    {k:16} baseline {b:.4f} -> {n:.4f} ({n - b:+.4f})   {interval(now, k)}{marker}")
    return failed


def summary_rows(split, report, baseline):
    """Markdown rows for one split: each family's cases, metrics with intervals, and gate status."""
    section = family_section(report)
    if section is None:
        return [f"| {split} | (no by_task_family section) | | | | | |"]
    base_section = family_section(baseline)
    families = section["families"]
    base_families = (base_section or {}).get("families", {})
    rows = []
    for family in list(families) + [f for f in base_families if f not in families]:
        _, label = family_status(family, section, base_section)
        now, base = families.get(family), base_families.get(family)
        if now is None:
            rows.append(f"| {split} | {family} | {base['cases']} -> absent | | | | {label} |")
            continue
        cases = f"{base['cases']} -> {now['cases']}" if base else f"{now['cases']}"
        cells = []
        for k in SUMMARY_METRICS:
            span = bounds(now, k)
            ci = "[not computed]" if span is None else f"[{span[0]:.4f}, {span[1]:.4f}]"
            cells.append(f"{now['metrics'][k]:.4f} {ci}")
        rows.append(f"| {split} | {family} | {cases} | {' | '.join(cells)} | {label} |")
    return rows


def summary_table(splits):
    """The job summary's per-family table; `splits` maps split name to (report, baseline or None)."""
    lines = [
        f"Per routed task family: {FAMILY_CAVEAT}. See docs/retrieval-benchmark.md, \"Per-task-family breakdown\".",
        "",
        "| split | routed task family | cases (baseline -> report) | R@5 [95% CI] | R@20 [95% CI] | MRR [95% CI] | gate |",
        "| --- | --- | ---: | ---: | ---: | ---: | --- |",
    ]
    for split, (report, baseline) in splits.items():
        lines += summary_rows(split, report, baseline)
    return lines


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    slack = SLACK
    if "--slack" in sys.argv:
        slack = float(sys.argv[sys.argv.index("--slack") + 1])
    report = json.loads(Path(args[0]).read_text())
    print(f"report {report['label']}: {len([r for r in report['rows'] if 'err' not in r])} cases, median {report['median_secs']:.1f}s/query")
    for k in WATCHED:
        lo, hi = report["ci"][k]
        print(f"  {k:16} {report['metrics'][k]:.4f}   95% CI [{lo:.4f}, {hi:.4f}]")
    print(f"  {'coverage':16} {report.get('coverage_line', 'not recorded')}")
    if len(args) < 2 or not Path(args[1]).exists():
        print_yield(report)
        compare_families(report, None, slack)
        print("no baseline to compare against; freeze this report if it is the first run")
        return 0
    baseline = json.loads(Path(args[1]).read_text())
    print_yield(report, baseline)
    print(f"  {'coverage':16} baseline {baseline.get('coverage_line', 'not recorded')} -> {report.get('coverage_line', 'not recorded')} (informational)")
    failed = []
    for k in WATCHED:
        base = baseline["metrics"][k]
        now = report["metrics"][k]
        marker = ""
        if now < base - slack:
            marker = "  <-- REGRESSION"
            failed.append(k)
        print(f"  {k:16} baseline {base:.4f} -> {now:.4f} ({now - base:+.4f}){marker}")
    failed += compare_families(report, baseline, slack)
    if failed:
        print(f"regression beyond {slack} on: {', '.join(failed)}", file=sys.stderr)
        return 1
    print("within slack of the frozen baseline")
    return 0


if __name__ == "__main__":
    sys.exit(main())
