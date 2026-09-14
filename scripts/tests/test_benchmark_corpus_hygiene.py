#!/usr/bin/env python3
"""Unit tests for what the commit-derived benchmark workflows print and upload:
scripts/mask-corpus-identity.sh, `commit-derived-cases.py --quiet`, and
scripts/reduce-benchmark-report.py. Stdlib and git only; run with
`python3 -m unittest discover -s scripts/tests`.
"""

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


reduce_report = load("reduce-benchmark-report")


def run_mask(url, base):
    env = {**os.environ, "REPO_URL": url, "BASE_SHA": base}
    out = subprocess.run(["bash", str(SCRIPTS / "mask-corpus-identity.sh")], env=env,
                         capture_output=True, text=True, check=True)
    return out.stdout.splitlines()


class MaskCorpusIdentityTests(unittest.TestCase):
    # Placeholders assembled at runtime; no real repository or commit is named here.
    URL = "/".join(["https:", "", "example.invalid", "team", "project.git"])
    BASE = "".join(format(i % 16, "x") for i in range(40))

    def test_masks_every_form_a_log_can_show(self):
        lines = run_mask(self.URL + "/", self.BASE)
        self.assertTrue(all(line.startswith("::add-mask::") for line in lines), lines)
        masked = {line[len("::add-mask::"):] for line in lines}
        stem = self.URL[: -len(".git")]
        for value in (stem, "team/project", "team", "project", self.BASE,
                      self.BASE[:7], self.BASE[:8], self.BASE[:10], self.BASE[:12]):
            self.assertIn(value, masked)

    def test_scp_style_url_yields_owner_and_name(self):
        masked = {line[len("::add-mask::"):] for line in run_mask(":".join(["git@example.invalid", "team/project.git"]), "")}
        self.assertTrue({"team/project", "team", "project"} <= masked, masked)

    def test_empty_values_mask_nothing(self):
        self.assertEqual(run_mask("", ""), [])


def git(repo, *args):
    subprocess.run(["git", "-C", repo, "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                    "-c", "commit.gpgsign=false", *args], check=True, capture_output=True)


class QuietCaseDerivationTests(unittest.TestCase):
    SUBJECT = "make the widget parser tolerate blank lines"

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = os.path.join(self.tmp.name, "repo")
        os.makedirs(os.path.join(self.repo, "pkg"))
        git(self.repo, "init", "-q")
        Path(self.repo, "pkg", "widget_parser.go").write_text("package pkg\n\nfunc Parse() {}\n")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-q", "-m", "initial layout of the package")
        self.base = subprocess.run(["git", "-C", self.repo, "rev-parse", "HEAD"],
                                   capture_output=True, text=True, check=True).stdout.strip()
        Path(self.repo, "pkg", "widget_parser.go").write_text("package pkg\n\nfunc Parse() { _ = 1 }\n")
        git(self.repo, "commit", "-q", "-am", self.SUBJECT)
        self.out = os.path.join(self.tmp.name, "cases.tsv")

    def tearDown(self):
        self.tmp.cleanup()

    def derive(self, *extra):
        return subprocess.run(
            [sys.executable, str(SCRIPTS / "commit-derived-cases.py"), self.repo, "--quiet",
             "--ext", ".go", "--after", "10", "--out", self.out, *extra],
            capture_output=True, text=True,
        )

    def assert_names_nothing(self, result):
        printed = result.stdout + result.stderr
        for secret in (self.base[:7], self.SUBJECT, "widget_parser", self.repo):
            self.assertNotIn(secret, printed)
        self.assertNotIn("Traceback", printed)

    def test_success_prints_counts_only(self):
        result = self.derive("--base", self.base)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assert_names_nothing(result)
        self.assertIn("1 cases written", result.stderr)
        self.assertEqual(len(Path(self.out).read_text().splitlines()), 1)

    def test_git_failure_withholds_the_command(self):
        result = self.derive("--base", "no-such-base-commit")
        self.assertEqual(result.returncode, 1)
        self.assert_names_nothing(result)
        self.assertNotIn("no-such-base-commit", result.stdout + result.stderr)
        self.assertIn("withheld", result.stderr)


class ReduceBenchmarkReportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def test_drops_rows_and_unlisted_fields_and_counts_cases(self):
        report = {
            "label": "holdout", "metrics": {"R@5": 0.5}, "ci": {"R@5": [0.4, 0.6]}, "median_secs": 1.0,
            "yield_budgets": [4000], "coverage_line": "1 of 1 files indexed", "coverage_error": None,
            "coverage": {"discovered": 3, "indexed": 2, "skipped": {"hidden": 1},
                         "policy_excluded_dirs": {"some-dir": 1},
                         "by_language": {"go": {"discovered": 2, "indexed": 2, "sample": ["x.go"]}}},
            "rows": [{"sha": "s", "query": "q", "top": ["p"], "rank": 1}, {"sha": "t", "err": "e"}],
            "future_per_case_field": [{"path": "p"}],
        }
        path = self.dir / "holdout.json"
        path.write_text(json.dumps(report))
        self.assertEqual(reduce_report.main([str(path)]), 0)
        reduced = json.loads(path.read_text())
        self.assertNotIn("rows", reduced)
        self.assertNotIn("future_per_case_field", reduced)
        self.assertEqual((reduced["cases_scored"], reduced["cases_errored"]), (1, 1))
        self.assertEqual(reduced["metrics"], report["metrics"])
        self.assertEqual(reduced["coverage_line"], report["coverage_line"])
        self.assertNotIn("policy_excluded_dirs", reduced["coverage"])
        self.assertEqual(reduced["coverage"]["by_language"], {"go": {"discovered": 2, "indexed": 2}})
        # Idempotent: a reduced report keeps its counts.
        self.assertEqual(reduce_report.main([str(path)]), 0)
        self.assertEqual(json.loads(path.read_text()), reduced)

    def test_coverage_file_and_absent_paths(self):
        coverage = self.dir / "coverage.json"
        coverage.write_text(json.dumps({"discovered": 1, "indexed": 1, "policy_excluded_dirs": {"d": 1}}))
        self.assertEqual(reduce_report.main([str(self.dir / "missing.json"), "--coverage", str(coverage)]), 0)
        self.assertEqual(json.loads(coverage.read_text()), {"discovered": 1, "indexed": 1})

    def test_unreadable_report_fails(self):
        bad = self.dir / "dev.json"
        bad.write_text("{not json")
        self.assertEqual(reduce_report.main([str(bad)]), 1)


if __name__ == "__main__":
    unittest.main()
