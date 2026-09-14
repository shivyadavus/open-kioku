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
scored cases. Every other family, and every family when the baseline has no `by_task_family`
section, is printed and not gated. Case counts are printed for both sides because the router
assigns a family, not the case file, so a routing change moves cases between families.

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


def watched_values(entry):
    return "  ".join(f"{k} {entry['metrics'][k]:.4f}" for k in WATCHED)


def compare_families(report, baseline, slack=SLACK):
    """Print the per-family comparison and return the gated regressions as 'family:metric'."""
    section = family_section(report)
    if section is None:
        print("  per routed task family: the report has no by_task_family section; nothing compared")
        return []
    min_cases = section["min_cases"]
    families = section["families"]
    print(f"  per routed task family ({section['assignment']}; gated only with {min_cases} or more "
          f"cases on both sides; {section['unassigned_cases']} cases unassigned):")
    base_section = family_section(baseline)
    if base_section is None:
        for family, entry in families.items():
            reason = f"insufficient, fewer than {min_cases} cases" if entry["cases"] < min_cases else "no family baseline"
            print(f"  {family:20} {entry['cases']:>4} cases  {watched_values(entry)}  ({reason}; not gated)")
        if baseline is not None:
            print("  the baseline has no by_task_family section; per-family metrics are informational until one is frozen")
        return []
    base_families = base_section["families"]
    failed = []
    for family in list(families) + [f for f in base_families if f not in families]:
        now, base = families.get(family), base_families.get(family)
        if now is None:
            print(f"  {family:20} baseline {base['cases']} cases -> absent from the report (not gated)")
            continue
        if base is None:
            print(f"  {family:20} {now['cases']:>4} cases  {watched_values(now)}  (absent from the baseline; not gated)")
            continue
        short = [side for side, entry in (("report", now), ("baseline", base)) if entry["cases"] < min_cases]
        status = "gated" if not short else f"insufficient on the {' and '.join(short)}, fewer than {min_cases} cases; not gated"
        print(f"  {family:20} cases {base['cases']} -> {now['cases']} ({status})")
        for k in WATCHED:
            b, n = base["metrics"][k], now["metrics"][k]
            marker = ""
            if n < b - slack:
                if short:
                    marker = "  <-- below slack (not gated)"
                else:
                    marker = "  <-- REGRESSION"
                    failed.append(f"{family}:{k}")
            print(f"    {k:16} baseline {b:.4f} -> {n:.4f} ({n - b:+.4f}){marker}")
    return failed


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
