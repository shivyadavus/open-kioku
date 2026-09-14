#!/usr/bin/env python3
"""Unit tests for the per-task-family section of score-context-cases.py and its gate in
compare-commit-derived-report.py. Stdlib only; run with
`python3 -m unittest scripts.tests.test_commit_derived_families`.
"""

import importlib.util
import io
import json
import os
import random
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
REPO = SCRIPTS.parent


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


score = load("score-context-cases")
compare = load("compare-commit-derived-report")

# Top-level report keys and per-row keys written before the per-family section existed.
LEGACY_REPORT_KEYS = {"label", "metrics", "ci", "median_secs", "yield_budgets", "coverage",
                      "coverage_line", "coverage_error", "rows"}
LEGACY_ROW_KEYS = {"sha", "date", "query", "rank", "gold_recall", "returned", "top", "confidence",
                   "secs", "gold_file_yield", "gold_line_yield", "line_yield_measurable",
                   "tokens_to_first_gold", "pack_tokens", "gold_file_yield_primary",
                   "gold_line_yield_primary", "line_yield_measurable_primary",
                   "tokens_to_first_gold_primary", "pack_tokens_primary", "pack_bytes"}


def row(family, rank, units=True, measurable=False):
    """A scored row as `score-context-cases.py` builds it."""
    hit = 1.0 if rank else 0.0
    file_yield = {"4000": hit, "8000": hit, "16000": hit} if units else None
    line_yield = {"4000": hit / 2, "8000": hit / 2, "16000": hit / 2} if units and measurable else None
    out = {"sha": "0" * 40, "rank": rank, "gold_recall": hit, "task_family": family}
    for suffix in ("", "_primary"):
        out.update({
            f"gold_file_yield{suffix}": file_yield,
            f"gold_line_yield{suffix}": line_yield,
            f"line_yield_measurable{suffix}": measurable,
            f"tokens_to_first_gold{suffix}": 0 if units and rank else None,
            f"pack_tokens{suffix}": 100 if units else 0,
        })
    return out


def entry(cases, **values):
    metrics = {"R@5": 0.6, "R@20": 0.8, "MRR": 0.5, "gold_recall@20": 0.7}
    metrics.update(values)
    return {"cases": cases, "insufficient": cases < score.MIN_FAMILY_CASES, "metrics": metrics, "ci": {}}


def report_with(families, min_cases=score.MIN_FAMILY_CASES):
    return {"by_task_family": {"assignment": score.FAMILY_ASSIGNMENT, "min_cases": min_cases,
                               "scored_cases": sum(f["cases"] for f in families.values()),
                               "unassigned_cases": 0, "families": families}}


def quiet(fn, *args):
    buffer = io.StringIO()
    with redirect_stdout(buffer):
        result = fn(*args)
    return result, buffer.getvalue()


CASE_DATA_KEYS = {"rows", "query", "top", "sha"}
FULL_HASH = re.compile(r"(?<![0-9a-f])[0-9a-f]{40}(?![0-9a-f])")
FAMILY_ENTRY_KEYS = {"cases", "insufficient", "metrics", "ci", "case_coverage"}


def leaves(value, path=()):
    """(path, key-or-leaf) pairs: every dict key and every scalar, with its path."""
    if isinstance(value, dict):
        for k, v in value.items():
            yield path + (k,), k
            yield from leaves(v, path + (k,))
    elif isinstance(value, list):
        for i, v in enumerate(value):
            yield from leaves(v, path + (i,))
    else:
        yield path, value


def baseline_problems(baseline):
    """What is wrong with a frozen baseline file; empty when it is well formed.

    Holds before and after a per-family freeze: a baseline without `by_task_family` only has to
    carry no case data; one with the section must also carry its fields and provenance.
    """
    problems = []
    for path, value in leaves(baseline):
        if path and path[-1] == value and value in CASE_DATA_KEYS:
            problems.append(f"case data key at {path}")
        if isinstance(value, str) and FULL_HASH.search(value) and path != ("provenance", "source_commit"):
            problems.append(f"full commit hash at {path}")
    if "by_task_family" not in baseline:
        return problems
    section = compare.family_section(baseline)
    if section is None:
        return problems + ["by_task_family has no families object"]
    if section.get("assignment") != score.FAMILY_ASSIGNMENT:
        problems.append(f"assignment {section.get('assignment')!r}")
    for key in ("min_cases", "scored_cases", "unassigned_cases"):
        if not isinstance(section.get(key), int):
            problems.append(f"by_task_family.{key} is not an integer")
    if section.get("scored_cases") != baseline.get("cases"):
        problems.append("by_task_family.scored_cases differs from the baseline's cases")
    for family, entry in section["families"].items():
        missing = FAMILY_ENTRY_KEYS - set(entry)
        if missing:
            problems.append(f"{family} lacks {sorted(missing)}")
            continue
        if set(compare.WATCHED) - set(entry["metrics"]):
            problems.append(f"{family} lacks watched metrics")
        if entry["insufficient"] != (entry["cases"] < section.get("min_cases", 0)):
            problems.append(f"{family} insufficient flag disagrees with its case count")
    provenance = baseline.get("provenance") or {}
    for key in ("frozen_from", "frozen_on", "source_commit"):
        if not provenance.get(key):
            problems.append(f"provenance.{key} missing")
    if not re.fullmatch(r"[0-9a-f]{40}", str(provenance.get("source_commit", ""))):
        problems.append("provenance.source_commit is not a full Open Kioku commit")
    return problems


def baseline_files():
    """Every frozen split baseline under benchmarks/commit-derived/, recognised by its content."""
    for path in sorted((REPO / "benchmarks/commit-derived").glob("*.json")):
        data = json.loads(path.read_text())
        if isinstance(data, dict) and {"split", "metrics"} <= set(data):
            yield path, data


class FamilyAssignment(unittest.TestCase):
    """A case's family is the routed family its pack reports, named as the core enum names it."""

    def test_reads_the_routed_family_from_the_pack(self):
        pack = {"retrieval_diagnostics": {"routing": {"task_family": "documentation", "confidence": 0.92}}}
        self.assertEqual(score.task_family(pack), "documentation")

    def test_a_pack_without_routing_is_unassigned(self):
        for pack in ({}, {"retrieval_diagnostics": None}, {"retrieval_diagnostics": {}},
                     {"retrieval_diagnostics": {"routing": {}}},
                     {"retrieval_diagnostics": {"routing": {"task_family": ""}}},
                     {"retrieval_diagnostics": {"routing": {"task_family": 3}}}):
            self.assertIsNone(score.task_family(pack), pack)

    def test_family_names_are_the_core_task_family_enum(self):
        # No second taxonomy: the scorer's names are exactly what `TaskFamily` serializes.
        source = (REPO / "crates/open-kioku-core/src/lib.rs").read_text()
        body = re.search(r"pub enum TaskFamily \{(.*?)\}", source, re.S).group(1)
        variants = [line.strip().rstrip(",") for line in body.splitlines()
                    if line.strip() and not line.strip().startswith(("#", "//"))]
        snake = tuple(re.sub(r"(?<!^)(?=[A-Z])", "_", v).lower() for v in variants)
        self.assertEqual(score.TASK_FAMILIES, snake)


class PerFamilyAggregation(unittest.TestCase):
    def setUp(self):
        self.rows = ([row("issue_to_code", 1, measurable=True)] * 3 + [row("issue_to_code", None, units=False)]
                     + [row("general", 7)] * 2 + [row("documentation", None)] + [row(None, 1)])

    def test_each_family_carries_every_aggregate_metric_over_its_own_cases(self):
        section = score.family_breakdown(self.rows)
        aggregate_keys = set(score.metrics(self.rows))
        for family in ("issue_to_code", "general", "documentation"):
            subset = [r for r in self.rows if r["task_family"] == family]
            got = section["families"][family]
            self.assertEqual(got["metrics"], score.metrics(subset))
            self.assertEqual(got["cases"], len(subset))
            self.assertLessEqual(set(got["metrics"]) - aggregate_keys, set())
        issue = section["families"]["issue_to_code"]
        self.assertAlmostEqual(issue["metrics"]["R@5"], 0.75)
        self.assertAlmostEqual(section["families"]["general"]["metrics"]["R@5"], 0.0)
        self.assertAlmostEqual(section["families"]["general"]["metrics"]["R@10"], 1.0)

    def test_case_coverage_counts_share_units_and_line_ranges(self):
        coverage = score.family_breakdown(self.rows)["families"]["issue_to_code"]["case_coverage"]
        self.assertEqual(coverage, {"share_of_scored": 4 / 8, "with_selected_units": 3, "with_line_ranges": 3})

    def test_unassigned_cases_stay_in_the_aggregate_and_in_no_family(self):
        section = score.family_breakdown(self.rows)
        self.assertEqual(section["scored_cases"], 8)
        self.assertEqual(section["unassigned_cases"], 1)
        self.assertEqual(sum(f["cases"] for f in section["families"].values()), 7)
        self.assertEqual(section["assignment"], "retrieval_diagnostics.routing.task_family")

    def test_families_follow_enum_order_then_unknown_names_sorted(self):
        rows = [row("zeta_new", 1), row("general", 1), row("alpha_new", 1), row("issue_to_code", 1)]
        self.assertEqual(list(score.family_breakdown(rows)["families"]),
                         ["issue_to_code", "general", "alpha_new", "zeta_new"])

    def test_intervals_are_deterministic_and_independent_of_other_families(self):
        rows = [row("issue_to_code", 1 if i % 3 else None) for i in range(45)]
        alone = score.family_breakdown(rows)
        again = score.family_breakdown(rows)
        mixed = score.family_breakdown(rows + [row("general", 2)] * 40)
        self.assertEqual(alone, again)
        self.assertEqual(alone["families"]["issue_to_code"]["ci"], mixed["families"]["issue_to_code"]["ci"])
        self.assertIn("R@5", alone["families"]["issue_to_code"]["ci"])

    def test_aggregate_interval_matches_the_module_seeded_bootstrap(self):
        rows = [row("issue_to_code", (i % 25) or None) for i in range(60)]
        random.seed(7)
        boots = [score.metrics(random.choices(rows, k=len(rows))) for _ in range(1000)]
        legacy = {}
        for k in score.metrics(rows):
            values = sorted(b[k] for b in boots if k in b)
            if len(values) >= 40:
                legacy[k] = (values[int(len(values) * 0.025)], values[min(int(len(values) * 0.975), len(values) - 1)])
        self.assertEqual(score.bootstrap_ci(rows, random.Random(7)), legacy)


class InsufficientSample(unittest.TestCase):
    def test_minimum_is_the_smallest_family_one_case_cannot_move_past_the_slack(self):
        self.assertEqual(score.MIN_FAMILY_CASES, 34)
        self.assertGreater(1 / (score.MIN_FAMILY_CASES - 1), compare.SLACK)
        self.assertLessEqual(1 / score.MIN_FAMILY_CASES, compare.SLACK)

    def test_family_below_the_minimum_is_reported_and_marked(self):
        section = score.family_breakdown([row("general", 1)] * 33 + [row("issue_to_code", 1)] * 34)
        self.assertTrue(section["families"]["general"]["insufficient"])
        self.assertFalse(section["families"]["issue_to_code"]["insufficient"])
        self.assertIn("R@5", section["families"]["general"]["metrics"])
        self.assertIn("insufficient", "\n".join(score.family_lines(section)))

    def test_sufficient_family_regression_beyond_slack_fails(self):
        failed, out = quiet(compare.compare_families,
                            report_with({"issue_to_code": entry(40, **{"R@5": 0.56})}),
                            report_with({"issue_to_code": entry(40)}))
        self.assertEqual(failed, ["issue_to_code:R@5"])
        self.assertIn("REGRESSION", out)

    def test_drop_within_slack_and_improvement_pass(self):
        failed, _ = quiet(compare.compare_families,
                          report_with({"issue_to_code": entry(40, **{"R@5": 0.575, "MRR": 0.9})}),
                          report_with({"issue_to_code": entry(40)}))
        self.assertEqual(failed, [])

    def test_insufficient_family_on_either_side_is_reported_but_never_gated(self):
        for now_cases, base_cases in ((33, 40), (40, 33), (5, 5)):
            failed, out = quiet(compare.compare_families,
                                report_with({"documentation": entry(now_cases, **{"R@20": 0.1})}),
                                report_with({"documentation": entry(base_cases)}))
            self.assertEqual(failed, [], (now_cases, base_cases))
            self.assertIn("not gated", out)
            self.assertIn("below slack", out)

    def test_family_missing_on_one_side_is_not_gated(self):
        failed, out = quiet(compare.compare_families,
                            report_with({"general": entry(50, **{"R@5": 0.0})}),
                            report_with({"issue_to_code": entry(50)}))
        self.assertEqual(failed, [])
        self.assertIn("absent from the baseline", out)
        self.assertIn("absent from the report", out)

    def test_minimum_applied_is_the_reports(self):
        failed, _ = quiet(compare.compare_families,
                          report_with({"general": entry(40, **{"R@5": 0.0})}, min_cases=50),
                          report_with({"general": entry(40)}, min_cases=10))
        self.assertEqual(failed, [])


class GateStatusIsTruthful(unittest.TestCase):
    """No output may imply a family is gated unless the compare would gate it."""

    def test_no_family_is_gated_without_a_family_baseline(self):
        report = report_with({"issue_to_code": entry(40), "general": entry(5)})
        for baseline in (None, {"metrics": {}}):
            rows = compare.summary_rows("holdout", report, baseline)
            self.assertFalse(any(r.endswith("| gated |") for r in rows), rows)
            self.assertIn("informational", rows[0])
            self.assertIn("insufficient", rows[1])

    def test_gated_only_when_both_sides_meet_the_minimum(self):
        report = report_with({"issue_to_code": entry(40), "general": entry(40)})["by_task_family"]
        baseline = report_with({"issue_to_code": entry(40), "general": entry(33)})["by_task_family"]
        self.assertEqual(compare.family_status("issue_to_code", report, baseline), (True, "gated"))
        gated, label = compare.family_status("general", report, baseline)
        self.assertFalse(gated)
        self.assertIn("insufficient", label)
        self.assertIn("baseline", label)
        rows = compare.summary_rows("dev", {"by_task_family": report}, {"by_task_family": baseline})
        self.assertTrue(rows[0].endswith("| gated |"), rows[0])
        self.assertIn("not gated", rows[1])

    def test_family_only_in_the_baseline_is_listed_as_absent(self):
        rows = compare.summary_rows("dev", report_with({"general": entry(40)}),
                                    report_with({"general": entry(40), "documentation": entry(40)}))
        self.assertTrue(any("documentation" in r and "absent from the report" in r for r in rows), rows)

    def test_caveat_and_intervals_accompany_every_per_family_output(self):
        self.assertEqual(score.FAMILY_CAVEAT, compare.FAMILY_CAVEAT)
        self.assertEqual(score.FAMILY_PRINTED, compare.WATCHED)
        text = "\n".join(score.family_lines(score.family_breakdown([row("issue_to_code", 1)] * 34)))
        self.assertIn(score.FAMILY_CAVEAT, text)
        self.assertIn("95% CI [", text)
        self.assertNotIn("[gated]", text)
        _, out = quiet(compare.compare_families, report_with({"issue_to_code": entry(40)}), None)
        self.assertIn(compare.FAMILY_CAVEAT, out)
        self.assertIn("95% CI", out)
        self.assertIn(compare.FAMILY_CAVEAT, compare.summary_table({})[0])


FAKE_OK = """#!{python}
import json, os, sys
if "status" in sys.argv:
    print(json.dumps({{"coverage": {{"discovered": 3, "indexed": 3, "skipped": {{}},
                                   "by_language": {{"python": {{"discovered": 3, "indexed": 3}}}}}}}}))
    sys.exit(0)
print(json.dumps(json.load(open(os.environ["FAKE_OK_PACKS"]))[sys.argv[-1]]))
"""


def fake_pack(family, ranked):
    units = [{"path": p, "line_range": {"start": 1, "end": 9}, "estimated_tokens": 50, "kind": "primary"}
             for p in ranked]
    diagnostics = {"selection": {"selected_units": units}}
    if family is not None:
        diagnostics["routing"] = {"task_family": family, "confidence": 0.8}
    return {"primary_files": [{"path": p} for p in ranked], "supporting_files": [],
            "confidence_breakdown": {"overall_enum": "medium"}, "retrieval_diagnostics": diagnostics}


class ReportSchemaBackwardCompatibility(unittest.TestCase):
    """The scorer runs end to end against a stand-in `ok`; the section is added, nothing moves."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        tmp = Path(cls.tmp.name)
        ok = tmp / "ok"
        ok.write_text(FAKE_OK.format(python=sys.executable))
        ok.chmod(ok.stat().st_mode | stat.S_IEXEC)
        packs, lines = {}, []
        hits = ([("issue_to_code", i % 4 != 0) for i in range(40)]
                + [("documentation", i % 2 == 0) for i in range(5)] + [(None, True)] * 2)
        # A hit's gold file is preceded by `i % 7` other files, so gold ranks run from 1 to 7. With a
        # single rank every metric is a multiple of 1/n and every bootstrap seed gives the same interval.
        cls.plan = [(family, 1 + i % 7 if hit else None) for i, (family, hit) in enumerate(hits)]
        for i, (family, rank) in enumerate(cls.plan):
            query = f"change number {i} for {family}"
            gold = f"src/gold_{i}.py"
            noise = [f"src/noise_{n}.py" for n in range((rank or 2) - 1)]
            packs[query] = fake_pack(family, noise + [gold] if rank else noise)
            lines.append(f"{i:040x}\t2026-01-01\t{query}\t{gold}\t1-4\n")
        (tmp / "packs.json").write_text(json.dumps(packs))
        (tmp / "cases.tsv").write_text("".join(lines))
        cls.out = tmp / "holdout.json"
        cls.scored = subprocess.run(
            [sys.executable, str(SCRIPTS / "score-context-cases.py"), "--ok", str(ok), "--repo", str(tmp),
             "--cases", str(tmp / "cases.tsv"), "--label", "holdout", "--workers", "2", "--out", str(cls.out)],
            capture_output=True, text=True, env={**os.environ, "FAKE_OK_PACKS": str(tmp / "packs.json")},
        )
        cls.report = json.loads(cls.out.read_text()) if cls.out.exists() else None

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def expected(self, family, k=None):
        """Recall@k, or MRR when k is None, computed from the plan without the scorer.

        `family` "*" means every scored case, unassigned ones included.
        """
        ranks = [rank for fam, rank in self.plan if family == "*" or fam == family]
        if k is None:
            return sum(1 / r for r in ranks if r) / len(ranks)
        return sum(1 for r in ranks if r and r <= k) / len(ranks)

    def test_scorer_run_succeeds(self):
        self.assertEqual(self.scored.returncode, 0, self.scored.stderr)
        self.assertIn("per routed task family", self.scored.stdout)

    def test_existing_keys_are_kept_and_only_the_family_section_is_added(self):
        self.assertEqual(set(self.report) - LEGACY_REPORT_KEYS, {"by_task_family"})
        self.assertLessEqual(LEGACY_REPORT_KEYS, set(self.report))
        for r in self.report["rows"]:
            self.assertEqual(set(r) - LEGACY_ROW_KEYS, {"task_family"})
        scored = [r for r in self.report["rows"] if "err" not in r]
        self.assertEqual(self.report["metrics"], score.metrics(scored))
        self.assertAlmostEqual(self.report["metrics"]["R@5"], self.expected("*", 5))
        self.assertAlmostEqual(self.report["metrics"]["MRR"], self.expected("*"))

    def test_aggregate_intervals_match_the_legacy_bootstrap_over_the_same_rows(self):
        # What `main()` wrote before the helper: `random.seed(7)` then 1000 module-level draws.
        scored = [r for r in self.report["rows"] if "err" not in r]
        random.seed(7)
        boots = [score.metrics(random.choices(scored, k=len(scored))) for _ in range(1000)]
        legacy = {}
        for k in score.metrics(scored):
            values = sorted(b[k] for b in boots if k in b)
            if len(values) >= 40:
                legacy[k] = [values[int(len(values) * 0.025)], values[min(int(len(values) * 0.975), len(values) - 1)]]
        self.assertEqual(self.report["ci"], legacy)
        # The comparison above is meaningful only if the seed matters on this corpus.
        self.assertNotEqual(score.bootstrap_ci(scored, random.Random(8)), score.bootstrap_ci(scored, random.Random(7)))

    def test_family_section_from_a_real_run(self):
        section = self.report["by_task_family"]
        self.assertEqual((section["min_cases"], section["scored_cases"], section["unassigned_cases"]), (34, 47, 2))
        issue, docs = section["families"]["issue_to_code"], section["families"]["documentation"]
        self.assertEqual((issue["cases"], issue["insufficient"]), (40, False))
        self.assertEqual((docs["cases"], docs["insufficient"]), (5, True))
        for family, entry in (("issue_to_code", issue), ("documentation", docs)):
            for k in (1, 5, 20):
                self.assertAlmostEqual(entry["metrics"][f"R@{k}"], self.expected(family, k), msg=(family, k))
            self.assertAlmostEqual(entry["metrics"]["MRR"], self.expected(family), msg=family)
        self.assertIn("gold_line_yield@8000", issue["metrics"])

    def compare_exit(self, baseline):
        path = Path(self.tmp.name) / "baseline.json"
        path.write_text(json.dumps(baseline))
        return subprocess.run([sys.executable, str(SCRIPTS / "compare-commit-derived-report.py"),
                               str(self.out), str(path)], capture_output=True, text=True)

    def test_baseline_frozen_without_the_section_compares_as_before(self):
        legacy = {k: v for k, v in self.report.items() if k not in ("rows", "by_task_family")}
        result = self.compare_exit(legacy)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("no by_task_family section", result.stdout)

    def test_checked_in_baselines_are_well_formed_before_and_after_a_family_freeze(self):
        files = list(baseline_files())
        self.assertTrue(files)
        for path, baseline in files:
            self.assertEqual(baseline_problems(baseline), [], path.name)
            if compare.family_section(baseline) is None:
                # Nothing is gated per family against a baseline frozen without the section.
                self.assertEqual(quiet(compare.compare_families, self.report, baseline)[0], [], path.name)
            else:
                self.assertEqual(quiet(compare.compare_families, baseline, baseline)[0], [], path.name)

    def test_validator_rejects_case_data_hashes_and_missing_provenance(self):
        base = {k: v for k, v in self.report.items() if k not in ("rows",)}
        base = json.loads(json.dumps(base))
        base.update({"split": "holdout", "cases": 47, "provenance": {"frozen_from": "run 1", "frozen_on": "2026-01-01",
                                                                     "source_commit": "ab" * 20}})
        self.assertEqual(baseline_problems(base), [])
        for mutate, expected in (
            (lambda b: b.update(rows=[]), "case data key"),
            (lambda b: b["provenance"].update(note="base " + "c" * 40), "full commit hash"),
            (lambda b: b["provenance"].pop("source_commit"), "source_commit"),
            (lambda b: b.update(cases=46), "scored_cases differs"),
        ):
            broken = json.loads(json.dumps(base))
            mutate(broken)
            self.assertTrue(any(expected in p for p in baseline_problems(broken)), (expected, baseline_problems(broken)))

    def test_documented_freeze_writes_valid_baselines_without_case_data(self):
        doc = (REPO / "docs/retrieval-benchmark.md").read_text()
        snippet = re.search(r'python3 - "\$DL" "\$RUN" "\$SOURCE_COMMIT" <<\'PY\'\n(.*?)\nPY\n', doc, re.S).group(1)
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp) / "repo"
            target = repo / "benchmarks/commit-derived"
            target.mkdir(parents=True)
            download = Path(tmp) / "download"
            for path, baseline in baseline_files():
                shutil.copy(path, target / path.name)
                out = download / path.name.rsplit("-", 1)[0] / "artifacts" / f"{baseline['split']}.json"
                out.parent.mkdir(parents=True, exist_ok=True)
                out.write_text(json.dumps(dict(self.report, label=baseline["split"])))
            bad = subprocess.run([sys.executable, "-", str(download), "12345", "main"], input=snippet,
                                 cwd=repo, capture_output=True, text=True)
            self.assertNotEqual(bad.returncode, 0)
            frozen_run = subprocess.run([sys.executable, "-", str(download), "12345", "ab" * 20], input=snippet,
                                        cwd=repo, capture_output=True, text=True)
            self.assertEqual(frozen_run.returncode, 0, frozen_run.stderr)
            frozen_files = sorted(target.glob("*.json"))
            self.assertEqual(len(frozen_files), len(list(baseline_files())))
            for path in frozen_files:
                frozen = json.loads(path.read_text())
                self.assertEqual(baseline_problems(frozen), [], path.name)
                self.assertEqual(frozen["provenance"]["source_commit"], "ab" * 20)
                self.assertEqual(frozen["by_task_family"]["scored_cases"], 47)
                self.assertEqual(quiet(compare.compare_families, self.report, frozen)[0], [], path.name)

    def test_job_summary_prints_the_caveat_intervals_and_the_gate_status_the_compare_applies(self):
        text = (REPO / ".github/workflows/commit-derived-bench.yml").read_text()
        body = text.split("Record coverage next to accuracy in the job summary", 1)[1]
        body = body.split("<<'PY'\n", 1)[1].split("\n          PY\n", 1)[0]
        code = "\n".join(line[10:] for line in body.splitlines())
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            (tmp / "artifacts").mkdir()
            (tmp / "benchmarks/commit-derived").mkdir(parents=True)
            os.symlink(SCRIPTS, tmp / "scripts")
            for split in ("holdout", "dev"):
                (tmp / f"artifacts/{split}.json").write_text(json.dumps(self.report))
            legacy = {k: v for k, v in self.report.items() if k not in ("rows", "by_task_family")}
            (tmp / "benchmarks/commit-derived/java-a-holdout.json").write_text(json.dumps(legacy))
            with_families = {k: v for k, v in self.report.items() if k != "rows"}
            (tmp / "benchmarks/commit-derived/java-a-dev.json").write_text(json.dumps(with_families))
            result = subprocess.run([sys.executable, "-c", code], cwd=tmp, capture_output=True, text=True,
                                    env={**os.environ, "CORPUS_NAME": "java-a"})
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = result.stdout.splitlines()
        self.assertIn(compare.FAMILY_CAVEAT, result.stdout)
        holdout = next(l for l in lines if l.startswith("| holdout | issue_to_code |"))
        dev = next(l for l in lines if l.startswith("| dev | issue_to_code |"))
        self.assertTrue(holdout.endswith("| informational: no family baseline frozen; not gated |"), holdout)
        self.assertTrue(dev.endswith("| gated |"), dev)
        self.assertRegex(holdout, r"\d\.\d{4} \[\d\.\d{4}, \d\.\d{4}\]")
        self.assertIn("insufficient", next(l for l in lines if l.startswith("| holdout | documentation |")))

    def test_family_regression_fails_the_run_while_the_aggregate_holds(self):
        baseline = {k: v for k, v in self.report.items() if k != "rows"}
        baseline = json.loads(json.dumps(baseline))
        baseline["by_task_family"]["families"]["issue_to_code"]["metrics"]["R@20"] += 0.04
        result = self.compare_exit(baseline)
        self.assertEqual(result.returncode, 1, result.stdout)
        self.assertIn("issue_to_code:R@20", result.stderr)
        self.assertNotIn("regression beyond 0.03 on: R@", result.stderr)

    def test_insufficient_family_regression_does_not_fail_the_run(self):
        baseline = json.loads(json.dumps({k: v for k, v in self.report.items() if k != "rows"}))
        baseline["by_task_family"]["families"]["documentation"]["metrics"]["R@5"] = 1.0
        result = self.compare_exit(baseline)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_report_without_the_section_is_not_gated_per_family(self):
        old_report = {k: v for k, v in self.report.items() if k != "by_task_family"}
        self.assertEqual(quiet(compare.compare_families, old_report, self.report)[0], [])


if __name__ == "__main__":
    unittest.main()
