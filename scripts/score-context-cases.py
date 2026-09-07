#!/usr/bin/env python3
"""Score the production `ok context` path against commit-derived cases.

    scripts/score-context-cases.py --ok target/release/ok --repo /path/to/repo-at-base \
        --cases cases.tsv --label dev [--workers 3] [--max-cases 0] [--out report.json]

Cases come from `scripts/commit-derived-cases.py` (TSV: sha, date, query, gold|gold).
Every query goes through `ok context --json`, the same builder that `ok plan` and the
MCP `build_context_pack` tool use, and files are ranked in the order the pack presents
them: primary files first, then supporting files, duplicates collapsed.

Reports Recall@k and MRR (first gold hit) plus gold recall in the returned set, each
with a 95% bootstrap confidence interval so a small delta can be told from noise.
"""
import argparse
import json
import os
import random
import statistics
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor


def rank_files(pack, repo):
    ranked = []
    for entry in pack.get("primary_files", []) + pack.get("supporting_files", []):
        path = entry.get("path", "")
        if path.startswith(repo):
            path = path[len(repo):].lstrip("/")
        if path not in ranked:
            ranked.append(path)
    return ranked


def metrics(sample):
    def recall_at(k):
        return sum(1 for r in sample if r["rank"] and r["rank"] <= k) / len(sample)

    return {
        "R@1": recall_at(1),
        "R@5": recall_at(5),
        "R@10": recall_at(10),
        "R@20": recall_at(20),
        "MRR": sum(1 / r["rank"] for r in sample if r["rank"]) / len(sample),
        "gold_recall@20": statistics.mean(r["gold_recall"] for r in sample),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ok", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--cases", required=True)
    ap.add_argument("--label", default="run")
    ap.add_argument("--workers", type=int, default=3)
    ap.add_argument("--max-cases", type=int, default=0)
    ap.add_argument("--timeout", type=int, default=600, help="seconds per query")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    repo = os.path.abspath(args.repo)

    cases = []
    for line in open(args.cases):
        sha, date, query, gold = line.rstrip("\n").split("\t")
        cases.append((sha, date, query, set(gold.split("|"))))
    if args.max_cases:
        cases = cases[: args.max_cases]

    def run(case):
        sha, date, query, gold = case
        started = time.time()
        try:
            out = subprocess.run(
                [args.ok, "--repo", repo, "context", query],
                capture_output=True, text=True, timeout=args.timeout,
            ).stdout
            pack = json.loads(out)
        except Exception as err:  # noqa: BLE001 - a failed query is a scored error, not a crash
            return {"sha": sha, "rank": None, "err": str(err)[:120], "secs": time.time() - started}
        ranked = rank_files(pack, repo)
        rank = next((i for i, p in enumerate(ranked, 1) if p in gold), None)
        return {
            "sha": sha, "date": date, "query": query, "rank": rank,
            "gold_recall": len(gold & set(ranked)) / len(gold),
            "returned": len(ranked), "top": ranked[:5],
            "confidence": pack.get("confidence_breakdown", {}).get("overall_enum"),
            "secs": time.time() - started,
        }

    t0 = time.time()
    rows = []
    with ThreadPoolExecutor(args.workers) as pool:
        for i, row in enumerate(pool.map(run, cases), 1):
            rows.append(row)
            if i % 25 == 0:
                print(f"  {i}/{len(cases)}  {time.time() - t0:.0f}s", file=sys.stderr, flush=True)

    scored = [r for r in rows if "err" not in r]
    if not scored:
        print("no case scored; every query failed", file=sys.stderr)
        return 1
    summary = metrics(scored)
    random.seed(7)
    boots = [metrics(random.choices(scored, k=len(scored))) for _ in range(1000)]
    ci = {k: (sorted(b[k] for b in boots)[25], sorted(b[k] for b in boots)[975]) for k in summary}
    median_secs = statistics.median(r["secs"] for r in rows)
    print(f"\n== {args.label}: {len(scored)} cases scored ({len(rows) - len(scored)} errors), median {median_secs:.1f}s/query ==")
    for k in ("R@1", "R@5", "R@10", "R@20", "MRR", "gold_recall@20"):
        print(f"  {k:16} {summary[k]:.4f}   95% CI [{ci[k][0]:.4f}, {ci[k][1]:.4f}]")
    if args.out:
        json.dump({"label": args.label, "metrics": summary, "ci": ci, "median_secs": median_secs, "rows": rows},
                  open(args.out, "w"), indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
