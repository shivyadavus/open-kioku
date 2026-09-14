#!/usr/bin/env python3
"""Unit tests for what the commit-derived benchmark workflows print and upload:
scripts/mask-corpus-identity.sh, `commit-derived-cases.py --quiet`,
scripts/reduce-benchmark-report.py, and the `run:` scripts of both workflows. Stdlib, bash and
git only; run with `python3 -m unittest discover -s scripts/tests`.
"""

import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parents[1]
REPO = SCRIPTS.parent
WORKFLOWS = [REPO / ".github" / "workflows" / name for name in ("commit-derived-bench.yml", "semantic-experiment.yml")]


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


reduce_report = load("reduce-benchmark-report")

# Placeholders assembled at runtime; no real repository, commit, or directory is named here.
URL = "/".join(["https:", "", "example.invalid", "Team", "Project.git"])
BASE = "".join(format(i % 16, "x") for i in range(40))


def source_mask(url="", base="", paths=""):
    """Source the mask script; returns (exit status, masks, normalised values)."""
    script = '. "$1" && printf "URL=%s\\nBASE=%s\\nPATHS=%s\\n" "$REPO_URL" "$BASE_SHA" "$CORPUS_PATHS"'
    env = {**os.environ, "REPO_URL": url, "BASE_SHA": base, "CORPUS_PATHS": paths}
    out = subprocess.run(["bash", "-c", script, "_", str(SCRIPTS / "mask-corpus-identity.sh")], env=env,
                         capture_output=True, text=True)
    lines = out.stdout.splitlines()
    masks = {line[len("::add-mask::"):] for line in lines if line.startswith("::add-mask::")}
    values = dict(line.split("=", 1) for line in lines if re.match(r"^(URL|BASE|PATHS)=", line))
    return out.returncode, masks, values, out.stdout + out.stderr


class MaskCorpusIdentityTests(unittest.TestCase):
    def test_masks_every_url_form_in_given_and_lowercase_spelling(self):
        status, masks, values, _ = source_mask(url=" " + URL + "/\n")
        self.assertEqual(status, 0)
        self.assertEqual(values["URL"], URL + "/")
        stem = URL[: -len(".git")]
        for value in (stem, stem.lower(), "Team/Project", "team/project", "Team", "team", "Project", "project"):
            self.assertIn(value, masks)

    def test_base_commit_is_trimmed_lowercased_and_every_prefix_masked(self):
        status, masks, values, _ = source_mask(base=BASE.upper() + "\n")
        self.assertEqual(status, 0)
        self.assertEqual(values["BASE"], BASE)
        self.assertIn(BASE.upper(), masks)
        for width in range(7, 41):
            self.assertIn(BASE[:width], masks)

    def test_scp_style_url_yields_owner_and_name(self):
        _, masks, _, _ = source_mask(url=":".join(["git@example.invalid", "team/project.git"]))
        self.assertTrue({"team/project", "team", "project"} <= masks, masks)

    def test_subtrees_are_masked_with_and_without_the_slash_and_normalised(self):
        status, masks, values, _ = source_mask(paths="  alpha/\tbeta/gamma  ")
        self.assertEqual(status, 0)
        self.assertEqual(values["PATHS"], "alpha/ beta/gamma")
        self.assertTrue({"alpha", "alpha/", "beta/gamma", "beta/gamma/"} <= masks, masks)

    def test_a_subtree_that_is_not_a_plain_relative_path_fails_without_repeating_it(self):
        for bad in ("../outside", "-oexfil", "a b/..", "semi;colon/", "/absolute"):
            status, _, _, printed = source_mask(paths=bad)
            self.assertNotEqual(status, 0, bad)
            self.assertNotIn(bad.split()[-1], printed.replace("::add-mask::", ""))

    def test_empty_values_mask_nothing(self):
        status, masks, _, _ = source_mask()
        self.assertEqual((status, masks), (0, set()))


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


def family_section():
    entry = {"cases": 40, "insufficient": False, "metrics": {"R@5": 0.5, "MRR": 0.4},
             "ci": {"R@5": [0.4, 0.6]}, "case_coverage": {"share_of_scored": 1.0, "with_line_ranges": 3}}
    return {"assignment": "retrieval_diagnostics.routing.task_family", "min_cases": 34, "scored_cases": 40,
            "unassigned_cases": 0, "families": {"issue_to_code": entry}}


class ReduceBenchmarkReportTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)

    def tearDown(self):
        self.tmp.cleanup()

    def reduce(self, report):
        path = self.dir / "holdout.json"
        path.write_text(json.dumps(report))
        self.assertEqual(reduce_report.main([str(path)]), 0)
        return json.loads(path.read_text())

    def test_drops_rows_and_unlisted_fields_and_counts_cases(self):
        report = {
            "label": "holdout", "metrics": {"R@5": 0.5}, "ci": {"R@5": [0.4, 0.6]}, "median_secs": 1.0,
            "yield_budgets": [4000], "coverage_line": "1 of 1 files indexed", "coverage_error": None,
            "coverage": {"discovered": 3, "indexed": 2, "skipped": {"hidden": 1},
                         "policy_excluded_dirs": {"some-dir": 1},
                         "by_language": {"go": {"discovered": 2, "indexed": 2, "sample": ["x.go"]}}},
            "by_task_family": family_section(),
            "rows": [{"sha": "s", "query": "q", "top": ["p"], "rank": 1}, {"sha": "t", "err": "e"}],
            "future_per_case_field": [{"path": "p"}],
        }
        reduced = self.reduce(report)
        self.assertNotIn("rows", reduced)
        self.assertNotIn("future_per_case_field", reduced)
        self.assertEqual((reduced["cases_scored"], reduced["cases_errored"]), (1, 1))
        self.assertEqual(reduced["metrics"], report["metrics"])
        self.assertEqual(reduced["by_task_family"], family_section())
        self.assertNotIn("policy_excluded_dirs", reduced["coverage"])
        self.assertEqual(reduced["coverage"]["by_language"], {"go": {"discovered": 2, "indexed": 2}})
        # Idempotent: a reduced report keeps its counts.
        self.assertEqual(self.reduce(reduced), reduced)

    def test_listed_fields_with_another_shape_are_dropped(self):
        cases = [{"sha": "s", "query": "q"}]
        hash_like_key = "".join(format(i % 16, "x") for i in range(3, 12))
        section = family_section()
        section["families"]["general"] = dict(section["families"]["issue_to_code"], metrics={"R@5": cases})
        reduced = self.reduce({
            "label": "holdout", "metrics": {"R@5": cases}, "ci": {hash_like_key: [0.1, 0.2]},
            "yield_budgets": cases, "median_secs": "slow", "coverage_line": ["a", "list"],
            "by_task_family": section,
        })
        for key in ("metrics", "ci", "yield_budgets", "median_secs", "coverage_line"):
            self.assertNotIn(key, reduced)
        self.assertEqual(list(reduced["by_task_family"]["families"]), ["issue_to_code"])
        self.assertEqual(self.reduce({"label": "src/Main.java"}), {})

    def test_coverage_file_and_absent_paths(self):
        coverage = self.dir / "coverage.json"
        coverage.write_text(json.dumps({"discovered": 1, "indexed": 1, "policy_excluded_dirs": {"d": 1}}))
        self.assertEqual(reduce_report.main([str(self.dir / "missing.json"), "--coverage", str(coverage)]), 0)
        self.assertEqual(json.loads(coverage.read_text()), {"discovered": 1, "indexed": 1})

    def test_unreadable_report_fails(self):
        bad = self.dir / "dev.json"
        bad.write_text("{not json")
        self.assertEqual(reduce_report.main([str(bad)]), 1)


def run_scripts(text):
    """The text of every `run:` value in a workflow, block scalars included."""
    lines = text.splitlines()
    scripts = []
    i = 0
    while i < len(lines):
        match = re.match(r"^(\s*)(- )?run:\s*(.*)$", lines[i])
        if not match:
            i += 1
            continue
        indent = len(match.group(1)) + len(match.group(2) or "")
        value = match.group(3)
        i += 1
        if value.startswith(("|", ">")):
            body = []
            while i < len(lines) and (not lines[i].strip() or len(lines[i]) - len(lines[i].lstrip()) > indent):
                body.append(lines[i])
                i += 1
            scripts.append("\n".join(body))
        else:
            scripts.append(value)
    return scripts


# `ok index` as a subcommand, after any global flags; `ok ... semantic index` is not it.
OK_INDEX = re.compile(r"/ok(?:\s+--?[\w-]+(?:\s+(?!index\b)[^\s-]\S*)?)*\s+index\b")


class WorkflowHygieneTests(unittest.TestCase):
    def setUp(self):
        self.texts = {path.name: path.read_text() for path in WORKFLOWS}
        self.scripts = {name: run_scripts(text) for name, text in self.texts.items()}

    def test_run_scripts_were_found(self):
        for name, scripts in self.scripts.items():
            self.assertGreaterEqual(len(scripts), 8, name)

    def test_no_expression_text_inside_run_scripts(self):
        for name, scripts in self.scripts.items():
            for script in scripts:
                self.assertNotIn("${{", script, f"{name}: pass the value through env: instead")
                self.assertNotRegex(script, r"github\.event\.inputs|\binputs\.", name)

    def test_ok_index_output_goes_to_files_never_to_the_log(self):
        for name, scripts in self.scripts.items():
            calls = [line for script in scripts for line in script.splitlines() if OK_INDEX.search(line)]
            self.assertTrue(calls, f"{name}: no ok index call found")
            for line in calls:
                self.assertNotRegex(line, r"(?<!\|)\|(?!\|)", f"{name}: ok index output is piped")
                self.assertRegex(line, r'>\s*"\$RUNNER_TEMP/[\w.]+"', name)
                self.assertRegex(line, r'2>\s*"\$RUNNER_TEMP/[\w.]+"', name)

    def test_the_clone_is_masked_first_and_removed(self):
        for name, scripts in self.scripts.items():
            clones = [s for s in scripts if "git clone" in s]
            self.assertEqual(len(clones), 1, name)
            script = clones[0]
            self.assertLess(script.index(". scripts/mask-corpus-identity.sh"), script.index("git clone"), name)
            self.assertIn("--quiet", script.split("git clone", 1)[1].splitlines()[0], name)
            self.assertIn("trap 'rm -rf source", script, name)
            self.assertIn("git -C source remote remove origin", script, name)
            self.assertIn("commit-derived-cases.py source --quiet", script, name)

    def test_mapping_lines_keep_a_space_after_the_key(self):
        # A text-level guard for the YAML the parser-free tests above read: `ext:.go` is not a mapping.
        for name, text in self.texts.items():
            self.assertNotRegex(text, r"(?m)^\s+(?:- )?[A-Za-z_][\w-]*:[^\s:]", name)

    def test_every_secret_lookup_names_a_non_empty_secret(self):
        for name, text in self.texts.items():
            self.assertNotRegex(text, r"secrets\[\s*(''|\"\")\s*\]", name)
            self.assertNotIn("paths_secret", text, name)
            for key in set(re.findall(r"secrets\[\s*matrix\.corpus\.(\w+)\s*\]", text)):
                values = [v.strip("'\"") for v in re.findall(rf"(?m)^\s*{key}:\s*(.*?)\s*$", text)]
                self.assertEqual(len(values), 4, f"{name}: {key} is not set on every matrix entry")
                self.assertTrue(all(values), f"{name}: {key} is empty on a matrix entry")
        bench = self.texts["commit-derived-bench.yml"]
        self.assertEqual(bench.count("matrix.corpus.name == 'java-a' && secrets.BENCH_JAVA_A_PATHS || ''"), 2)

    def test_no_subtree_list_is_checked_in(self):
        self.assertNotRegex(self.texts["commit-derived-bench.yml"], r"(?m)^\s*prefixes:")
        for path in sorted((REPO / "benchmarks" / "commit-derived").glob("*-a-*.json")):
            provenance = json.loads(path.read_text())["provenance"]
            self.assertNotIn("path_prefixes", provenance, path.name)
            self.assertIsInstance(provenance.get("path_prefix_count"), int, path.name)


if __name__ == "__main__":
    unittest.main()
