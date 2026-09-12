#!/usr/bin/env python3
"""Score the production `ok context` path against commit-derived cases.

    scripts/score-context-cases.py --ok target/release/ok --repo /path/to/repo-at-base \
        --cases cases.tsv --label dev [--workers 3] [--max-cases 0] [--out report.json]

Cases come from `scripts/commit-derived-cases.py` (TSV: sha, date, query, gold|gold, and
optionally the modified line ranges per gold file). Every query goes through
`ok context --json`, the same builder that `ok plan` and the MCP `build_context_pack` tool
use, and files are ranked in the order the pack presents them: primary files first, then
supporting files, duplicates collapsed.

Reports Recall@k and MRR (first gold hit) plus gold recall in the returned set, each with a
95% bootstrap confidence interval so a small delta can be told from noise.

Gold yield at a token budget: the pack's selected units are walked in order, each costing the
builder's own `estimated_tokens`, until the next unit would overflow the budget. Everything
before that point is what an agent reading the pack top-down sees within B tokens.
`gold_file_yield@B` is the fraction of gold files with a selected unit inside the budget, averaged
over every scored case -- a pack that selects nothing scores 0 rather than being excluded, so the
mean matches what a caller receives and stays comparable with `gold_recall@20`;
`gold_line_yield@B` (only when the cases carry line ranges) is the fraction of modified
lines that those units' line ranges cover; `tokens_to_first_gold` is what was spent before
the first gold unit. See docs/retrieval-benchmark.md, "Gold yield at a token budget".

The report also records the index's coverage (source files discovered versus indexed,
per language, with every omission attributed to a skip reason) read from
`ok --json status`, so a ranking number is never read without knowing how much of the
corpus the index actually held.
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

BUDGETS = (4000, 8000, 16000)


def rank_files(pack, repo):
    ranked = []
    for entry in pack.get("primary_files", []) + pack.get("supporting_files", []):
        path = relative_path(entry.get("path", ""), repo)
        if path not in ranked:
            ranked.append(path)
    return ranked


def relative_path(path, repo):
    if path.startswith(repo):
        path = path[len(repo):].lstrip("/")
    return path


def parse_ranges(field, gold):
    """'26-33,36-44|1-1' aligned with the gold list -> {path: [(26, 33), (36, 44)], ...}.

    Returns None when the column is absent (a four-column cases file) or does not line up
    with the gold files, so an older corpus scores file yield but no line yield.
    """
    if field is None:
        return None
    parts = field.split("|")
    if len(parts) != len(gold):
        return None
    ranges = {}
    for path, spec in zip(gold, parts):
        spans = []
        for item in filter(None, spec.split(",")):
            start, _, end = item.partition("-")
            spans.append((int(start), int(end or start)))
        if spans:
            ranges[path] = spans
    return ranges or None


# Packs built before `kind` existed are classified by the rationale the builder wrote; the
# structured field is authoritative whenever it is present. `scripts/tests/test_context_yield.py`
# pins both, and `open-kioku-context` pins the field it emits.
SUPPORTING_UNIT_MARKER = "supporting file listed from impact expansion"


def is_primary_unit(unit):
    kind = unit.get("kind")
    if kind is not None:
        return kind != "supporting"
    return SUPPORTING_UNIT_MARKER not in (unit.get("rationale") or "")


def selected_units(pack, repo):
    """The pack's selected units in order as (path, start, end, tokens, primary).

    A pack may list supporting files in the same ledger, costed at their listing size. They
    are marked so yield can be scored over the primary units alone: retrieval changes that
    only affect which regions of the primary files are shown must not be credited with the
    files that impact expansion contributed. Packs that list no supporting units are
    unaffected, so the primary-only view is comparable across versions.
    """
    units = []
    selection = pack.get("retrieval_diagnostics", {}).get("selection", {})
    for unit in selection.get("selected_units") or []:
        span = unit.get("line_range") or {}
        units.append((
            relative_path(unit.get("path", ""), repo),
            int(span.get("start", 0) or 0),
            int(span.get("end", 0) or 0),
            int(unit.get("estimated_tokens", 0) or 0),
            is_primary_unit(unit),
        ))
    return units


def primary_units(units):
    return [unit for unit in units if unit[4]]


def yield_at(units, gold, ranges, budget):
    """(file yield, line yield or None) for the units that fit in `budget` tokens, in order."""
    consumed = 0
    files_hit = set()
    lines_hit = {path: set() for path in (ranges or {})}
    for path, start, end, tokens, _primary in units:
        if consumed + tokens > budget:
            break
        consumed += tokens
        if path not in gold:
            continue
        files_hit.add(path)
        for lo, hi in (ranges or {}).get(path, []):
            overlap = range(max(lo, start), min(hi, end) + 1)
            lines_hit[path].update(overlap)
    file_yield = len(files_hit) / len(gold)
    if not ranges:
        return file_yield, None
    total = sum(hi - lo + 1 for spans in ranges.values() for lo, hi in spans)
    covered = sum(len(hit) for hit in lines_hit.values())
    return file_yield, covered / total if total else None


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(int(len(ordered) * fraction), len(ordered) - 1)]


def tokens_to_first_gold(units, gold):
    consumed = 0
    for path, _, _, tokens, _primary in units:
        if path in gold:
            return consumed
        consumed += tokens
    return None


def yield_row(units, gold, ranges, suffix=""):
    """Per-case yield fields; None when the binary exposes no selected units."""
    if not units:
        return {
            f"gold_file_yield{suffix}": None,
            f"gold_line_yield{suffix}": None,
            f"line_yield_measurable{suffix}": bool(ranges),
            f"tokens_to_first_gold{suffix}": None,
            f"pack_tokens{suffix}": 0,
        }
    file_yield = {}
    line_yield = {}
    for budget in BUDGETS:
        f, l = yield_at(units, gold, ranges, budget)
        file_yield[str(budget)] = f
        line_yield[str(budget)] = l
    return {
        f"gold_file_yield{suffix}": file_yield,
        f"gold_line_yield{suffix}": line_yield if ranges else None,
        f"line_yield_measurable{suffix}": bool(ranges),
        f"tokens_to_first_gold{suffix}": tokens_to_first_gold(units, gold),
        f"pack_tokens{suffix}": sum(tokens for _, _, _, tokens, _p in units),
    }


def index_coverage(ok, repo):
    """(coverage, error): the `coverage` object from `ok --json status` (None when the index
    predates it) and, separately, why the status could not be read at all."""
    try:
        out = subprocess.run([ok, "--repo", repo, "--json", "status"], capture_output=True, text=True, timeout=120).stdout
        return json.loads(out).get("coverage"), None
    except Exception as err:  # noqa: BLE001 - coverage is informational; the failure is reported, not hidden
        return None, str(err)[:120]


PROGRAMMING_LANGUAGES = ("rust", "java", "type_script", "java_script", "python", "go", "sql")

# Skips a policy chose (`SkipReason::is_policy` in open-kioku-core). They are reported
# beside the ratio and left out of its denominator, as `ok index` and `ok doctor` do.
POLICY_SKIP_REASONS = ("ignored", "denied", "hidden", "generated", "vendor", "fast_mode",
                       "secret_policy", "symlink_policy")


def considered(entry):
    """Discovered files minus policy exclusions: the coverage denominator."""
    skipped = entry.get("skipped", {})
    excluded = sum(n for reason, n in skipped.items() if reason in POLICY_SKIP_REASONS)
    return entry.get("discovered", 0) - excluded, excluded


def coverage_line(coverage, error=None):
    """`921 of 922 programming-language files indexed (99.9%); 1,417 of 1,461 recognised files indexed (97.0%) overall; 40 excluded by policy (40 hidden); skipped: ...`

    The programming-language ratio comes first because that is the one `ok doctor`
    judges; config and prose files are reported in the overall figure beside it. Both
    denominators are the files the index would consider under the current policy, the
    same definition `ok index` prints (docs/indexing-pipeline.md, "Coverage").
    """
    if error:
        return f"status unavailable ({error})"
    if not coverage or not coverage.get("discovered"):
        return "not recorded"
    indexed = coverage["indexed"]
    all_considered, excluded = considered(coverage)
    if not all_considered:
        return f"no source files considered under the current policy; {excluded:,} excluded by policy"
    overall = f"{indexed:,} of {all_considered:,} recognised files indexed ({100.0 * indexed / all_considered:.1f}%)"
    by_language = coverage.get("by_language", {})
    source = [v for k, v in by_language.items() if k in PROGRAMMING_LANGUAGES]
    src_considered = sum(considered(v)[0] for v in source)
    src_indexed = sum(v.get("indexed", 0) for v in source)
    if src_considered:
        line = (f"{src_indexed:,} of {src_considered:,} programming-language files indexed "
                f"({100.0 * src_indexed / src_considered:.1f}%); {overall} overall")
    else:
        line = f"no programming-language files considered; {overall}"
    skipped = sorted(coverage.get("skipped", {}).items(), key=lambda kv: (-kv[1], kv[0]))
    policy = [(reason, n) for reason, n in skipped if reason in POLICY_SKIP_REASONS]
    judged = [(reason, n) for reason, n in skipped if reason not in POLICY_SKIP_REASONS]
    if policy:
        line += f"; {excluded:,} excluded by policy (" + ", ".join(
            f"{n:,} {reason.replace('_', '-')}" for reason, n in policy) + ")"
    if judged:
        line += "; skipped: " + ", ".join(f"{n:,} {reason.replace('_', '-')}" for reason, n in judged)
    if coverage.get("pruned_dirs"):
        line += f"; {coverage['pruned_dirs']:,} directories pruned by name"
    if coverage.get("walk_errors"):
        line += f"; {coverage['walk_errors']:,} walk errors"
    return line


def metrics(sample):
    def recall_at(k):
        return sum(1 for r in sample if r["rank"] and r["rank"] <= k) / len(sample)

    out = {
        "R@1": recall_at(1),
        "R@5": recall_at(5),
        "R@10": recall_at(10),
        "R@20": recall_at(20),
        "MRR": sum(1 / r["rank"] for r in sample if r["rank"]) / len(sample),
        "gold_recall@20": statistics.mean(r["gold_recall"] for r in sample),
    }
    # Yield is averaged over every scored case, not only the cases whose pack selected units.
    # A pack that selects nothing delivered no gold lines, so its yield is 0, not undefined;
    # dropping those cases from the denominator inflates the mean and makes yield read better
    # than what the caller actually receives. gold_recall@20 above already averages over every
    # case, so a conditional yield mean would not be comparable with it.
    # Line yield is averaged over the cases where line ranges exist to measure against
    # (`line_yield_measurable`), which is a property of the case file, not of the run.
    with_units = [r for r in sample if r.get("gold_file_yield")]
    for suffix in ("", "_primary"):
        any_units = [r for r in sample if r.get(f"gold_file_yield{suffix}")]
        line_measurable = [r for r in sample if r.get(f"line_yield_measurable{suffix}")]
        for budget in BUDGETS:
            key = str(budget)
            if any_units:
                out[f"gold_file_yield{suffix}@{budget}"] = statistics.mean(
                    (r[f"gold_file_yield{suffix}"][key] if r.get(f"gold_file_yield{suffix}") else 0.0)
                    for r in sample
                )
            if line_measurable and any(r.get(f"gold_line_yield{suffix}") for r in line_measurable):
                out[f"gold_line_yield{suffix}@{budget}"] = statistics.mean(
                    (r[f"gold_line_yield{suffix}"][key] if r.get(f"gold_line_yield{suffix}") else 0.0)
                    for r in line_measurable
                )
    first = [r["tokens_to_first_gold"] for r in with_units if r["tokens_to_first_gold"] is not None]
    if first:
        out["tokens_to_first_gold_p50"] = statistics.median(first)
    return out


YIELD_KEYS = (
    tuple(f"gold_file_yield@{b}" for b in BUDGETS)
    + tuple(f"gold_line_yield@{b}" for b in BUDGETS)
    + tuple(f"gold_file_yield_primary@{b}" for b in BUDGETS)
    + tuple(f"gold_line_yield_primary@{b}" for b in BUDGETS)
)


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
        fields = line.rstrip("\n").split("\t")
        if len(fields) < 4:
            continue
        sha, date, query, gold = fields[:4]
        gold = gold.split("|")
        cases.append((sha, date, query, gold, parse_ranges(fields[4] if len(fields) > 4 else None, gold)))
    if args.max_cases:
        cases = cases[: args.max_cases]

    def run(case):
        sha, date, query, gold, ranges = case
        gold_set = set(gold)
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
        rank = next((i for i, p in enumerate(ranked, 1) if p in gold_set), None)
        row = {
            "sha": sha, "date": date, "query": query, "rank": rank,
            "gold_recall": len(gold_set & set(ranked)) / len(gold_set),
            "returned": len(ranked), "top": ranked[:5],
            "confidence": pack.get("confidence_breakdown", {}).get("overall_enum"),
            "secs": time.time() - started,
        }
        units = selected_units(pack, repo)
        row.update(yield_row(units, gold_set, ranges))
        # Scored twice: over every ledger unit, and over the primary units alone. Only the
        # second isolates what retrieval put in front of the caller from what impact
        # expansion appended, so a change to one is never credited to the other.
        row.update(yield_row(primary_units(units), gold_set, ranges, "_primary"))
        row["pack_bytes"] = len(out.encode("utf-8"))
        return row

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
    ci = {}
    for k in summary:
        values = sorted(b[k] for b in boots if k in b)
        if len(values) >= 40:
            ci[k] = (values[int(len(values) * 0.025)], values[min(int(len(values) * 0.975), len(values) - 1)])
    median_secs = statistics.median(r["secs"] for r in rows)
    coverage, coverage_error = index_coverage(args.ok, repo)
    print(f"\n== {args.label}: {len(scored)} cases scored ({len(rows) - len(scored)} errors), median {median_secs:.1f}s/query ==")
    for k in ("R@1", "R@5", "R@10", "R@20", "MRR", "gold_recall@20"):
        print(f"  {k:16} {summary[k]:.4f}   95% CI [{ci[k][0]:.4f}, {ci[k][1]:.4f}]")

    with_units = [r for r in scored if r.get("gold_file_yield")]
    with_lines = [r for r in with_units if r.get("gold_line_yield")]
    if not with_units:
        print("  (no selected units in the pack output; gold yield not scored)")
    else:
        pack_tokens = [r["pack_tokens"] for r in with_units]
        pack_bytes = [r["pack_bytes"] for r in scored if r.get("pack_bytes")]
        # A pack's payload can be large for reasons unrelated to the code it selects
        # (evidence and diagnostics dominate), so report the ledger and the payload.
        print(f"  -- gold yield at a token budget: averaged over all {len(scored)} scored cases "
              f"({len(with_units)} selected units, the rest score 0), "
              f"{len(with_lines)} with modified line ranges, pack estimated tokens p50 "
              f"{statistics.median(pack_tokens):.0f} p95 {percentile(pack_tokens, 0.95):.0f} "
              f"max {max(pack_tokens)}, JSON bytes p50 {statistics.median(pack_bytes):.0f} "
              f"p95 {percentile(pack_bytes, 0.95):.0f} --")
        for k in YIELD_KEYS:
            if k in summary:
                print(f"  {k:22} {summary[k]:.4f}   95% CI [{ci[k][0]:.4f}, {ci[k][1]:.4f}]")
        first = [r for r in with_units if r["tokens_to_first_gold"] is not None]
        if first:
            print(f"  {'tokens_to_first_gold':22} median {summary['tokens_to_first_gold_p50']:.0f}   "
                  f"95% CI [{ci['tokens_to_first_gold_p50'][0]:.0f}, {ci['tokens_to_first_gold_p50'][1]:.0f}]   "
                  f"({len(first)}/{len(with_units)} cases reach a gold unit)")
    print(f"  {'coverage':16} {coverage_line(coverage, coverage_error)}")
    if args.out:
        json.dump({"label": args.label, "metrics": summary, "ci": ci, "median_secs": median_secs,
                   "yield_budgets": list(BUDGETS), "coverage": coverage,
                   "coverage_line": coverage_line(coverage, coverage_error),
                   "coverage_error": coverage_error, "rows": rows},
                  open(args.out, "w"), indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
