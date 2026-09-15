#!/usr/bin/env python3
"""Unit tests for the per-task-family section of score-context-cases.py, its gate in
compare-commit-derived-report.py, and the baseline validator and freeze in
commit-derived-baselines.py. Stdlib only; run with
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
reduce_report = load("reduce-benchmark-report")
baselines = load("commit-derived-baselines")
baseline_problems = baselines.baseline_problems

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


SAME_MEMBERS = "sha256:" + "a" * 64


def entry(cases, fingerprint=SAME_MEMBERS, **values):
    metrics = {"R@5": 0.6, "R@20": 0.8, "MRR": 0.5, "gold_recall@20": 0.7}
    metrics.update(values)
    out = {"cases": cases, "insufficient": cases < score.MIN_FAMILY_CASES, "metrics": metrics, "ci": {}}
    if fingerprint is not None:
        out["membership_fingerprint"] = fingerprint
    return out


def report_with(families, min_cases=score.MIN_FAMILY_CASES):
    return {"by_task_family": {"assignment": score.FAMILY_ASSIGNMENT, "min_cases": min_cases,
                               "scored_cases": sum(f["cases"] for f in families.values()),
                               "unassigned_cases": 0, "families": families}}


def quiet(fn, *args):
    buffer = io.StringIO()
    with redirect_stdout(buffer):
        result = fn(*args)
    return result, buffer.getvalue()


CASE_DATA_KEYS = baselines.CASE_DATA_KEYS
FULL_HASH = baselines.FULL_HASH


def leaves(value):
    return baselines.leaves(value)


def baseline_files():
    """Every frozen split baseline under benchmarks/commit-derived/, recognised by its content."""
    return baselines.baseline_files(REPO / "benchmarks/commit-derived")


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

    def test_sufficient_family_regression_beyond_tolerance_fails(self):
        # 40 cases: tolerance max(0.03, 2/40) = 0.05, and 0.60 -> 0.54 falls 0.06.
        failed, out = quiet(compare.compare_families,
                            report_with({"issue_to_code": entry(40, **{"R@5": 0.54})}),
                            report_with({"issue_to_code": entry(40)}))
        self.assertEqual(failed, ["issue_to_code:R@5"])
        self.assertIn("REGRESSION", out)

    def test_drop_within_tolerance_and_improvement_pass(self):
        # 0.60 -> 0.555 falls 0.045: beyond the aggregate's 0.03 but within 2/40.
        failed, _ = quiet(compare.compare_families,
                          report_with({"issue_to_code": entry(40, **{"R@5": 0.555, "MRR": 0.9})}),
                          report_with({"issue_to_code": entry(40)}))
        self.assertEqual(failed, [])

    def test_insufficient_family_on_either_side_is_reported_but_never_gated(self):
        for now_cases, base_cases in ((33, 40), (40, 33), (5, 5)):
            failed, out = quiet(compare.compare_families,
                                report_with({"documentation": entry(now_cases, **{"R@20": 0.1})}),
                                report_with({"documentation": entry(base_cases)}))
            self.assertEqual(failed, [], (now_cases, base_cases))
            self.assertIn("not gated", out)
            self.assertIn("below tolerance", out)

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

    def test_documented_freeze_runs_the_freeze_script_on_a_checked_run(self):
        doc = (REPO / "docs/retrieval-benchmark.md").read_text()
        block = doc.split("**Freezing per-family baselines.**", 1)[1].split("```sh\n", 1)[1].split("\n```", 1)[0]
        self.assertIn("gh run view \"$RUN\" --json databaseId,workflowName,status,conclusion,headSha", block)
        self.assertIn("scripts/commit-derived-baselines.py freeze --run-json", block)
        self.assertNotIn("baseline.update(report)", block)
        with tempfile.TemporaryDirectory() as tmp:
            target, download = freeze_fixture(Path(tmp), self.report)
            run_json = Path(tmp) / "run.json"
            run_json.write_text(json.dumps(successful_run()))
            frozen_run = subprocess.run([sys.executable, str(SCRIPTS / "commit-derived-baselines.py"), "freeze",
                                         "--run-json", str(run_json), "--download", str(download), "--dir", str(target)],
                                        capture_output=True, text=True)
            self.assertEqual(frozen_run.returncode, 0, frozen_run.stderr)
            frozen_files = sorted(target.glob("*.json"))
            self.assertEqual(len(frozen_files), len(list(baseline_files())))
            for path in frozen_files:
                frozen = json.loads(path.read_text())
                self.assertEqual(baseline_problems(frozen), [], path.name)
                self.assertEqual(frozen["provenance"]["source_commit"], "ab" * 20)
                self.assertEqual(frozen["by_task_family"]["scored_cases"], 47)
                self.assertEqual(quiet(compare.compare_families, self.report, frozen)[0], [], path.name)
            checked = subprocess.run([sys.executable, str(SCRIPTS / "commit-derived-baselines.py"), "check",
                                      "--dir", str(target)], capture_output=True, text=True)
            self.assertEqual(checked.returncode, 0, checked.stderr)

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
        # 40 cases: the family tolerance is 2/40 = 0.05, so the drop must exceed it.
        baseline["by_task_family"]["families"]["issue_to_code"]["metrics"]["R@20"] += 0.06
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


def successful_run(**overrides):
    run = {"databaseId": 12345, "workflowName": "commit-derived-bench", "status": "completed",
           "conclusion": "success", "headSha": "ab" * 20}
    run.update(overrides)
    return run


def freeze_fixture(tmp, report):
    """Copies of the checked-in baselines and reduced artifacts built from `report`."""
    target = tmp / "repo/benchmarks/commit-derived"
    target.mkdir(parents=True)
    download = tmp / "download"
    for path, baseline in baseline_files():
        shutil.copy(path, target / path.name)
        # The artifact as the workflow uploads it: reduced to aggregates, at the artifact root.
        out = download / path.name.rsplit("-", 1)[0] / f"{baseline['split']}.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        uploaded = reduce_report.reduce_report(json.loads(json.dumps(dict(report, label=baseline["split"]))), [])
        out.write_text(json.dumps(uploaded))
    return target, download


def synthetic_report(families=(("issue_to_code", 40), ("documentation", 5)), unassigned=2):
    """A scored report (rows included) built by the scorer's own functions."""
    rows = []
    for family, cases in families:
        rows += [row(family, 1 + i % 7 if i % 4 else None) for i in range(cases)]
    rows += [row(None, 1)] * unassigned
    return {"label": "holdout", "metrics": score.metrics(rows), "ci": score.bootstrap_ci(rows, random.Random(7)),
            "median_secs": 1.0, "yield_budgets": list(score.BUDGETS), "coverage": None,
            "coverage_line": "not recorded", "coverage_error": None, "rows": rows,
            "by_task_family": score.family_breakdown(rows)}


class MembershipFingerprint(unittest.TestCase):
    """A family's fingerprint names the split-file positions of its cases and nothing else."""

    def test_stable_and_well_formed(self):
        first = score.membership_fingerprint([3, 1, 2], 10)
        self.assertEqual(first, score.membership_fingerprint([3, 1, 2], 10))
        self.assertRegex(first, r"^sha256:[0-9a-f]{64}$")
        # Pinned: a change to what is hashed must be a deliberate, versioned change.
        import hashlib
        expected = hashlib.sha256(b"open-kioku task-family membership v1\nsplit_cases 10\n1\n2\n3\n").hexdigest()
        self.assertEqual(first, "sha256:" + expected)

    def test_order_independent(self):
        positions = list(range(0, 90, 3))
        shuffled = positions[:]
        random.Random(1).shuffle(shuffled)
        self.assertNotEqual(positions, shuffled)
        self.assertEqual(score.membership_fingerprint(positions, 90), score.membership_fingerprint(shuffled, 90))

    def test_a_swap_at_equal_count_or_another_split_size_changes_it(self):
        base = score.membership_fingerprint([0, 1, 2, 3], 10)
        self.assertNotEqual(base, score.membership_fingerprint([0, 1, 2, 4], 10))
        self.assertNotEqual(base, score.membership_fingerprint([0, 1, 2, 3], 11))

    def test_breakdown_fingerprints_each_family_by_split_file_position(self):
        rows = [row("general", 1), row("issue_to_code", 1), row("general", None), row(None, 1)]
        # The row at split-file position 2 errored, so the scored rows sit at 0, 1, 3, 4.
        section = score.family_breakdown(rows, positions=[0, 1, 3, 4], split_cases=5)
        self.assertEqual(section["families"]["general"]["membership_fingerprint"],
                         score.membership_fingerprint([0, 3], 5))
        self.assertEqual(section["families"]["issue_to_code"]["membership_fingerprint"],
                         score.membership_fingerprint([1], 5))
        with self.assertRaises(ValueError):
            score.family_breakdown(rows, positions=[0, 1])

    def test_fingerprint_hashes_no_case_identity(self):
        rows = [dict(row("general", 1), sha=f"{i:040x}", query=f"subject {i}") for i in range(3)]
        renamed = [dict(r, sha="f" * 40, query="another subject") for r in rows]
        self.assertEqual(score.family_breakdown(rows)["families"]["general"]["membership_fingerprint"],
                         score.family_breakdown(renamed)["families"]["general"]["membership_fingerprint"])

    def test_scorer_run_fingerprints_positions_in_the_split_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            ok = tmp / "ok"
            ok.write_text(FAKE_OK.format(python=sys.executable))
            ok.chmod(ok.stat().st_mode | stat.S_IEXEC)
            packs = {f"q{i}": fake_pack("general" if i % 2 else "documentation", [f"src/g{i}.py"]) for i in range(6)}
            (tmp / "packs.json").write_text(json.dumps(packs))
            (tmp / "cases.tsv").write_text("".join(f"{i:040x}\t2026-01-01\tq{i}\tsrc/g{i}.py\n" for i in range(6)))
            out = tmp / "r.json"
            result = subprocess.run(
                [sys.executable, str(SCRIPTS / "score-context-cases.py"), "--ok", str(ok), "--repo", str(tmp),
                 "--cases", str(tmp / "cases.tsv"), "--workers", "3", "--out", str(out)],
                capture_output=True, text=True, env={**os.environ, "FAKE_OK_PACKS": str(tmp / "packs.json")})
            self.assertEqual(result.returncode, 0, result.stderr)
            families = json.loads(out.read_text())["by_task_family"]["families"]
        self.assertEqual(families["general"]["membership_fingerprint"], score.membership_fingerprint([1, 3, 5], 6))
        self.assertEqual(families["documentation"]["membership_fingerprint"], score.membership_fingerprint([0, 2, 4], 6))


class MembershipGate(unittest.TestCase):
    def test_membership_changed_is_printed_and_not_gated(self):
        other = "sha256:" + "b" * 64
        failed, out = quiet(compare.compare_families,
                            report_with({"issue_to_code": entry(40, fingerprint=other, **{"R@5": 0.1})}),
                            report_with({"issue_to_code": entry(40)}))
        self.assertEqual(failed, [])
        self.assertIn("membership changed", out)
        self.assertIn("below tolerance (not gated)", out)
        rows = compare.summary_rows("dev", report_with({"issue_to_code": entry(40, fingerprint=other)}),
                                    report_with({"issue_to_code": entry(40)}))
        self.assertIn("membership changed", rows[0])
        self.assertEqual(compare.gated_rows("dev", report_with({"issue_to_code": entry(40, fingerprint=other)}),
                                            report_with({"issue_to_code": entry(40)})), [])

    def test_a_side_without_a_fingerprint_is_not_gated(self):
        for now_fp, base_fp, side in ((None, SAME_MEMBERS, "report"), (SAME_MEMBERS, None, "baseline")):
            failed, out = quiet(compare.compare_families,
                                report_with({"general": entry(40, fingerprint=now_fp, **{"R@5": 0.0})}),
                                report_with({"general": entry(40, fingerprint=base_fp)}))
            self.assertEqual(failed, [], side)
            self.assertIn(f"membership unverified: no membership fingerprint on the {side}", out)

    def test_matching_fingerprint_is_gated(self):
        failed, _ = quiet(compare.compare_families,
                          report_with({"general": entry(40, **{"R@5": 0.0})}),
                          report_with({"general": entry(40)}))
        self.assertEqual(failed, ["general:R@5"])


class FamilyTolerance(unittest.TestCase):
    def test_tolerance_is_max_of_slack_and_two_over_n(self):
        self.assertAlmostEqual(compare.family_tolerance(34), 2 / 34)
        self.assertAlmostEqual(compare.family_tolerance(40), 0.05)
        self.assertAlmostEqual(compare.family_tolerance(66), 2 / 66)
        self.assertEqual(compare.family_tolerance(67), 0.03)
        self.assertEqual(compare.family_tolerance(500), 0.03)
        self.assertEqual(compare.family_tolerance(40, slack=0.1), 0.1)

    def gate(self, n, base_hits, now_hits, metric="R@5"):
        """Compare n-case families whose `metric` is hits/n; the baseline is rounded as the freeze rounds it."""
        base = report_with({"general": entry(n, **{metric: round(base_hits / n, 4)})})
        now = report_with({"general": entry(n, **{metric: now_hits / n})})
        return quiet(compare.compare_families, now, base)

    def test_small_family_two_cases_pass_three_fail(self):
        # n=34: one case moves 1/34, the tolerance is 2/34; rounding the baseline must not flip it.
        for base_hits in range(3, 35):
            self.assertEqual(self.gate(34, base_hits, base_hits - 2)[0], [], base_hits)
            self.assertEqual(self.gate(34, base_hits, base_hits - 3)[0], ["general:R@5"], base_hits)

    def test_large_family_uses_the_aggregate_slack(self):
        # n=200: 2/200 = 0.01 < 0.03, so the tolerance is 0.03 = 6 cases.
        self.assertEqual(self.gate(200, 120, 114)[0], [])
        self.assertEqual(self.gate(200, 120, 113)[0], ["general:R@5"])

    def test_output_prints_baseline_interval_delta_tolerance_and_result_for_every_watched_metric(self):
        base = entry(40)
        base["ci"] = {k: [0.4, 0.9] for k in compare.WATCHED}
        now = entry(40, **{"gold_recall@20": 0.6})
        failed, out = quiet(compare.compare_families, report_with({"general": now}), report_with({"general": base}))
        self.assertEqual(failed, ["general:gold_recall@20"])
        for k in compare.WATCHED:
            line = next(l for l in out.splitlines() if l.strip().startswith(k))
            self.assertIn("baseline", line)
            self.assertIn("95% CI [0.4000, 0.9000]", line)
            self.assertIn("delta", line)
            self.assertIn("tolerance 0.0500", line)
        self.assertTrue(next(l for l in out.splitlines() if "gold_recall@20" in l).endswith("REGRESSION"))
        self.assertTrue(next(l for l in out.splitlines() if l.strip().startswith("R@5")).endswith("pass"))

    def test_summary_lists_gated_metrics_with_baseline_interval_delta_and_result(self):
        base = entry(40)
        base["ci"] = {"gold_recall@20": [0.6, 0.8]}
        lines = compare.summary_table({"dev": (report_with({"general": entry(40, **{"gold_recall@20": 0.6})}),
                                               report_with({"general": base}))})
        row_ = next(l for l in lines if l.startswith("| dev | general | gold_recall@20 |"))
        self.assertEqual(row_, "| dev | general | gold_recall@20 | 0.7000 [0.6000, 0.8000] | 0.6000 | -0.1000 | 0.0500 | REGRESSION |")
        self.assertTrue(any(l.startswith("| dev | general | R@5 |") and l.endswith("| pass |") for l in lines))
        self.assertIn("gold_recall@20 [95% CI]", lines[2])
        ungated = compare.summary_table({"dev": (report_with({"general": entry(40)}), None)})
        self.assertIn("No family is gated in this run.", ungated)


class BaselineValidator(unittest.TestCase):
    def valid(self):
        report = synthetic_report()
        base = {k: v for k, v in report.items() if k != "rows"}
        base = json.loads(json.dumps(base))
        base.update({"split": "holdout", "cases": 47,
                     "provenance": {"frozen_from": "run 1", "frozen_on": "2026-01-01", "source_commit": "ab" * 20}})
        return base

    def test_valid_baseline_has_no_problems(self):
        self.assertEqual(baseline_problems(self.valid()), [])

    def assert_rejected(self, mutate, expected):
        broken = self.valid()
        mutate(broken)
        problems = baseline_problems(broken)
        self.assertTrue(any(expected in p for p in problems), (expected, problems))

    def test_rejects_family_counts_that_do_not_add_up_to_scored_cases(self):
        def more_unassigned(b):
            b["by_task_family"]["unassigned_cases"] += 1
        def fewer_family_cases(b):
            fam = b["by_task_family"]["families"]["issue_to_code"]
            fam["cases"] -= 1
        for mutate in (more_unassigned, fewer_family_cases):
            self.assert_rejected(mutate, "plus unassigned_cases")

    def test_requires_source_commit(self):
        self.assert_rejected(lambda b: b["provenance"].pop("source_commit"), "provenance.source_commit missing")
        self.assert_rejected(lambda b: b["provenance"].update(source_commit="abc123"), "not a full Open Kioku commit")
        self.assert_rejected(lambda b: b.pop("provenance"), "provenance.source_commit missing")

    def test_requires_a_well_formed_membership_fingerprint(self):
        self.assert_rejected(lambda b: b["by_task_family"]["families"]["issue_to_code"].pop("membership_fingerprint"),
                             "membership_fingerprint")
        self.assert_rejected(
            lambda b: b["by_task_family"]["families"]["issue_to_code"].update(membership_fingerprint="ab" * 32),
            "membership_fingerprint is not")

    def test_check_command_fails_on_an_invalid_baseline(self):
        with tempfile.TemporaryDirectory() as tmp:
            broken = self.valid()
            broken["by_task_family"]["unassigned_cases"] = 9
            (Path(tmp) / "x-holdout.json").write_text(json.dumps(broken))
            result = subprocess.run([sys.executable, str(SCRIPTS / "commit-derived-baselines.py"), "check",
                                     "--dir", tmp], capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("plus unassigned_cases", result.stderr)


class AtomicFreeze(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.target, self.download = freeze_fixture(Path(self.tmp.name), synthetic_report())
        self.before = self.snapshot()

    def tearDown(self):
        self.tmp.cleanup()

    def snapshot(self):
        return {p.name: p.read_bytes() for p in sorted(self.target.iterdir())}

    def test_refuses_a_run_that_did_not_succeed(self):
        for run in (successful_run(conclusion="failure"), successful_run(status="in_progress", conclusion=""),
                    successful_run(conclusion="cancelled"), successful_run(workflowName="ci"),
                    successful_run(headSha="main"), successful_run(databaseId=None), []):
            with self.assertRaises(baselines.FreezeRefused, msg=run):
                baselines.plan_freeze(run, self.download, self.target, "2026-09-15")
        run_json = Path(self.tmp.name) / "run.json"
        run_json.write_text(json.dumps(successful_run(conclusion="failure")))
        result = subprocess.run([sys.executable, str(SCRIPTS / "commit-derived-baselines.py"), "freeze", "--run-json",
                                 str(run_json), "--download", str(self.download), "--dir", str(self.target)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("did not succeed", result.stderr)
        self.assertEqual(self.snapshot(), self.before)

    def test_refuses_before_writing_when_any_report_is_missing_or_invalid(self):
        (self.download / "py-a" / "dev.json").unlink()
        with self.assertRaises(baselines.FreezeRefused):
            baselines.plan_freeze(successful_run(), self.download, self.target, "2026-09-15")
        self.assertEqual(self.snapshot(), self.before)
        report = json.loads((self.download / "go-a" / "holdout.json").read_text())
        report["by_task_family"]["unassigned_cases"] += 3
        (self.download / "go-a" / "holdout.json").write_text(json.dumps(report))
        (self.download / "py-a" / "dev.json").write_text(json.dumps(report))
        with self.assertRaises(baselines.FreezeRefused) as refused:
            baselines.plan_freeze(successful_run(), self.download, self.target, "2026-09-15")
        self.assertIn("plus unassigned_cases", str(refused.exception))
        self.assertEqual(self.snapshot(), self.before)

    def test_a_failed_rename_restores_every_baseline(self):
        plan = baselines.plan_freeze(successful_run(), self.download, self.target, "2026-09-15")
        self.assertEqual(len(plan), 8)
        real_replace = os.replace
        calls = []

        def failing_replace(src, dst):
            calls.append(dst)
            # Three baselines are replaced, the fourth rename fails; the rollback's own renames succeed.
            if len(calls) == 4:
                raise OSError("simulated rename failure")
            return real_replace(src, dst)

        baselines.os.replace = failing_replace
        try:
            with self.assertRaises(OSError):
                baselines.write_all(plan)
        finally:
            baselines.os.replace = real_replace
        self.assertEqual(self.snapshot(), self.before)
        self.assertGreater(len(calls), 4, "the rollback restored the replaced baselines")

    def test_a_failed_temporary_write_changes_nothing(self):
        plan = baselines.plan_freeze(successful_run(), self.download, self.target, "2026-09-15")
        real_write = baselines._write_temp
        written = []

        def failing_write(path, text):
            if len(written) == 5:
                raise OSError("simulated disk full")
            written.append(path)
            return real_write(path, text)

        baselines._write_temp = failing_write
        try:
            with self.assertRaises(OSError):
                baselines.write_all(plan)
        finally:
            baselines._write_temp = real_write
        self.assertEqual(self.snapshot(), self.before)

    def test_success_replaces_every_baseline_with_a_valid_one(self):
        baselines.write_all(baselines.plan_freeze(successful_run(), self.download, self.target, "2026-09-15"))
        after = self.snapshot()
        self.assertEqual(set(after), set(self.before))
        for name, content in after.items():
            frozen = json.loads(content)
            self.assertEqual(baseline_problems(frozen), [], name)
            self.assertEqual(frozen["provenance"]["frozen_from"], "commit-derived-bench run 12345 (ubuntu-latest)")
            self.assertEqual(frozen["provenance"]["frozen_on"], "2026-09-15")
            self.assertNotIn("yield_note", frozen["provenance"])



class ReducerKeepsMembership(unittest.TestCase):
    def test_fingerprint_is_kept_and_no_case_level_data_is(self):
        report = synthetic_report()
        notes = []
        reduced = reduce_report.reduce_report(json.loads(json.dumps(report)), notes)
        for family, entry_ in report["by_task_family"]["families"].items():
            self.assertEqual(reduced["by_task_family"]["families"][family]["membership_fingerprint"],
                             entry_["membership_fingerprint"])
        self.assertNotIn("rows", reduced)
        for path, value in leaves(reduced):
            self.assertNotIn(value, CASE_DATA_KEYS, path)
            if isinstance(value, str):
                self.assertIsNone(FULL_HASH.search(value), path)
                self.assertNotIn("src/", value, path)
        self.assertEqual(reduced["cases_scored"], 47)

    def test_a_malformed_fingerprint_drops_the_family_entry(self):
        report = synthetic_report()
        report["by_task_family"]["families"]["issue_to_code"]["membership_fingerprint"] = "0123abc" * 6
        notes = []
        reduced = reduce_report.reduce_report(report, notes)
        self.assertNotIn("issue_to_code", reduced["by_task_family"]["families"])
        self.assertIn("documentation", reduced["by_task_family"]["families"])
        self.assertTrue(any("by_task_family entry" in n for n in notes), notes)


if __name__ == "__main__":
    unittest.main()
