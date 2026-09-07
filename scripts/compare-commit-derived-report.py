#!/usr/bin/env python3
"""Compare a commit-derived benchmark report against its frozen baseline.

    scripts/compare-commit-derived-report.py REPORT.json BASELINE.json [--slack 0.03]

Both files are produced by `scripts/score-context-cases.py --out`. A metric may not fall
more than `slack` (absolute) below the baseline; the 95% bootstrap interval is printed so
a reader can tell a real regression from sampling noise. Missing baseline: prints the
report and exits 0, so the first run of a new corpus freezes rather than fails.
"""
import json
import sys
from pathlib import Path

WATCHED = ("R@5", "R@20", "MRR", "gold_recall@20")


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    slack = 0.03
    if "--slack" in sys.argv:
        slack = float(sys.argv[sys.argv.index("--slack") + 1])
    report = json.loads(Path(args[0]).read_text())
    print(f"report {report['label']}: {len([r for r in report['rows'] if 'err' not in r])} cases, median {report['median_secs']:.1f}s/query")
    for k in WATCHED:
        lo, hi = report["ci"][k]
        print(f"  {k:16} {report['metrics'][k]:.4f}   95% CI [{lo:.4f}, {hi:.4f}]")
    if len(args) < 2 or not Path(args[1]).exists():
        print("no baseline to compare against; freeze this report if it is the first run")
        return 0
    baseline = json.loads(Path(args[1]).read_text())
    failed = []
    for k in WATCHED:
        base = baseline["metrics"][k]
        now = report["metrics"][k]
        marker = ""
        if now < base - slack:
            marker = "  <-- REGRESSION"
            failed.append(k)
        print(f"  {k:16} baseline {base:.4f} -> {now:.4f} ({now - base:+.4f}){marker}")
    if failed:
        print(f"regression beyond {slack} on: {', '.join(failed)}", file=sys.stderr)
        return 1
    print("within slack of the frozen baseline")
    return 0


if __name__ == "__main__":
    sys.exit(main())
