#!/usr/bin/env python3
"""Validate and freeze the commit-derived baselines under benchmarks/commit-derived/.

    scripts/commit-derived-baselines.py check [--dir benchmarks/commit-derived]
    scripts/commit-derived-baselines.py freeze --run-json RUN.json --download DIR \
        [--jobs-json JOBS.json --accept-regression REASON] [--dir benchmarks/commit-derived]

`check` validates every split baseline in the directory and exits 1 naming each problem.

`freeze` rewrites all eight split baselines from one `commit-derived-bench` run:
- `RUN.json` is `gh run view <run> --json databaseId,workflowName,status,conclusion,headSha`.
  The freeze refuses unless the run is a completed, successful `commit-derived-bench` run with a
  full source commit.
- `--accept-regression REASON` freezes a completed run whose conclusion is `failure` only when
  its baseline comparison failed and nothing else did. `JOBS.json` is
  `gh api --paginate --slurp repos/{owner}/{repo}/actions/runs/<run>/jobs`. Every job of the run
  must have completed, one `bench (<code>)` job must exist per corpus, and every step must have
  succeeded except `Compare against the frozen baseline`, at least one of which failed. The
  non-empty, single-line reason and the run id are written to each baseline's
  `provenance.accepted_regression`; a freeze without the flag removes that record.
- `DIR/<code>/<split>.json` are that run's artifacts, already reduced to aggregates by
  `scripts/reduce-benchmark-report.py`.
Only an explicit allow-list of report fields is copied into each baseline (never
`baseline.update(report)`). Every new baseline is built and validated before any file is written.
Each file is written to a temporary file in the same directory and renamed over the baseline,
and if any write or rename fails, every baseline already replaced is restored, so the directory
holds either all new baselines or all old ones.

A baseline is valid when it holds no case data (rows, queries, ranked paths, or a full commit
hash other than `provenance.source_commit`), and, when it carries `by_task_family`, when:
- the section has its required fields, and `scored_cases` equals the baseline's `cases`;
- the family case counts plus `unassigned_cases` equal `scored_cases`;
- every family carries its required fields, the watched metrics, and a `membership_fingerprint`;
- `provenance` names `frozen_from`, `frozen_on`, and a full 40-hex `source_commit`.
See docs/retrieval-benchmark.md, "Per-task-family breakdown".
"""
import argparse
import datetime
import importlib.util
import json
import os
import re
import sys
import tempfile
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent


def _load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


score = _load("score-context-cases")
compare = _load("compare-commit-derived-report")

CORPORA = ("java-a", "go-a", "ts-a", "py-a")
SPLITS = ("holdout", "dev")
WORKFLOW = "commit-derived-bench"

CASE_DATA_KEYS = {"rows", "query", "top", "sha"}
FULL_HASH = re.compile(r"(?<![0-9a-f])[0-9a-f]{40}(?![0-9a-f])")
FULL_COMMIT = re.compile(r"[0-9a-f]{40}")
FINGERPRINT = re.compile(r"sha256:[0-9a-f]{64}")
FAMILY_ENTRY_KEYS = {"cases", "insufficient", "metrics", "ci", "case_coverage", "membership_fingerprint"}
FAMILY_COUNTS = ("min_cases", "scored_cases", "unassigned_cases")
# The one step of `.github/workflows/commit-derived-bench.yml` an accepted regression may have failed.
COMPARE_STEP = "Compare against the frozen baseline"
REASON_LIMIT = 500


class FreezeRefused(Exception):
    """The freeze cannot proceed; nothing has been written."""


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


def is_count(value):
    return isinstance(value, int) and not isinstance(value, bool)


def baseline_problems(baseline):
    """What is wrong with a frozen baseline; empty when it is well formed.

    Holds before and after a per-family freeze: a baseline without `by_task_family` only has to
    carry no case data; one with the section must also carry its fields and provenance.
    """
    if not isinstance(baseline, dict):
        return ["a baseline is a JSON object"]
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
    for key in FAMILY_COUNTS:
        if not is_count(section.get(key)):
            problems.append(f"by_task_family.{key} is not an integer")
    if section.get("scored_cases") != baseline.get("cases"):
        problems.append("by_task_family.scored_cases differs from the baseline's cases")
    counted = 0
    for family, entry in section["families"].items():
        if not isinstance(entry, dict):
            problems.append(f"{family} is not an object")
            continue
        missing = FAMILY_ENTRY_KEYS - set(entry)
        if missing:
            problems.append(f"{family} lacks {sorted(missing)}")
            continue
        if not is_count(entry["cases"]):
            problems.append(f"{family} cases is not an integer")
            continue
        counted += entry["cases"]
        if set(compare.WATCHED) - set(entry["metrics"]):
            problems.append(f"{family} lacks watched metrics")
        if entry["insufficient"] != (entry["cases"] < section.get("min_cases", 0)):
            problems.append(f"{family} insufficient flag disagrees with its case count")
        if not (isinstance(entry["membership_fingerprint"], str)
                and FINGERPRINT.fullmatch(entry["membership_fingerprint"])):
            problems.append(f"{family} membership_fingerprint is not sha256: and 64 hex digits")
    unassigned = section.get("unassigned_cases")
    if is_count(unassigned) and is_count(section.get("scored_cases")) and counted + unassigned != section["scored_cases"]:
        problems.append(f"by_task_family family cases ({counted}) plus unassigned_cases ({unassigned}) "
                        f"differ from scored_cases ({section['scored_cases']})")
    provenance = baseline.get("provenance") if isinstance(baseline.get("provenance"), dict) else {}
    for key in ("frozen_from", "frozen_on", "source_commit"):
        if not provenance.get(key):
            problems.append(f"provenance.{key} missing")
    if not FULL_COMMIT.fullmatch(str(provenance.get("source_commit", ""))):
        problems.append("provenance.source_commit is not a full Open Kioku commit")
    if "accepted_regression" in provenance:
        problems += accepted_regression_problems(provenance)
    return problems


def reason_problem(reason):
    """Why an accepted-regression reason is unusable, or None."""
    if not isinstance(reason, str) or not reason.strip():
        return "the accepted-regression reason is empty"
    if reason != reason.strip() or len(reason) > REASON_LIMIT or any(ord(c) < 32 for c in reason):
        return f"the accepted-regression reason must be one trimmed line of at most {REASON_LIMIT} characters"
    return None


def accepted_regression_problems(provenance):
    accepted = provenance["accepted_regression"]
    if not isinstance(accepted, dict):
        return ["provenance.accepted_regression is not an object"]
    problems = []
    problem = reason_problem(accepted.get("reason"))
    if problem:
        problems.append(f"provenance.accepted_regression: {problem}")
    run_id = accepted.get("source_run")
    if not is_count(run_id) or run_id <= 0:
        problems.append("provenance.accepted_regression.source_run is not a run id")
    elif f"run {run_id} " not in f"{provenance.get('frozen_from', '')} ":
        problems.append("provenance.accepted_regression.source_run differs from the run in frozen_from")
    return problems


def baseline_files(directory):
    """Every split baseline in `directory`, recognised by its content, as (path, data)."""
    for path in sorted(Path(directory).glob("*.json")):
        data = json.loads(path.read_text())
        if isinstance(data, dict) and {"split", "metrics"} <= set(data):
            yield path, data


def check_run(run, accept_regression=None):
    """(run id, source commit) of a completed commit-derived-bench run that may be frozen; else refuse.

    Without `accept_regression` the run must have succeeded. With it the run must have failed;
    `check_jobs` then decides whether the baseline comparison was the only failure.
    """
    if not isinstance(run, dict):
        raise FreezeRefused("the run description is not a JSON object")
    if run.get("workflowName") != WORKFLOW:
        raise FreezeRefused(f"the run is not a {WORKFLOW} run")
    expected = "success" if accept_regression is None else "failure"
    if run.get("status") != "completed" or run.get("conclusion") != expected:
        if accept_regression is None:
            raise FreezeRefused(f"the run did not succeed (status {run.get('status')!r}, "
                                f"conclusion {run.get('conclusion')!r}); only a successful run can be frozen "
                                "without --accept-regression")
        raise FreezeRefused(f"--accept-regression needs a completed run whose conclusion is failure (status "
                            f"{run.get('status')!r}, conclusion {run.get('conclusion')!r})")
    run_id = run.get("databaseId")
    if not is_count(run_id) or run_id <= 0:
        raise FreezeRefused("the run description has no run id")
    source_commit = run.get("headSha")
    if not isinstance(source_commit, str) or not FULL_COMMIT.fullmatch(source_commit):
        raise FreezeRefused("the run description has no full Open Kioku source commit")
    return run_id, source_commit


def job_list(jobs):
    """Jobs from the jobs API: a `--slurp` list of pages, one page, or a plain list of jobs."""
    if isinstance(jobs, dict):
        jobs = [jobs]
    if not isinstance(jobs, list):
        raise FreezeRefused("the jobs description is not a JSON list or object")
    out = []
    for item in jobs:
        if isinstance(item, dict) and isinstance(item.get("jobs"), list):
            out += item["jobs"]
        elif isinstance(item, dict) and "steps" in item:
            out.append(item)
        else:
            raise FreezeRefused("the jobs description holds something other than jobs")
    return out


def check_jobs(run_id, jobs):
    """Refuse unless every step of the run succeeded except baseline comparisons, one of which failed.

    Reads step conclusions, never the run's overall conclusion: a failed build, index, score,
    reduction, or upload step must not be frozen however the flag is worded.
    """
    jobs = job_list(jobs)
    if not jobs:
        raise FreezeRefused("the run has no jobs")
    names = []
    compare_failures = 0
    for job in jobs:
        name = job.get("name")
        if job.get("run_id") != run_id:
            raise FreezeRefused(f"job {name!r} belongs to another run")
        if job.get("status") != "completed":
            raise FreezeRefused(f"job {name!r} has not completed")
        steps = job.get("steps")
        if not isinstance(steps, list) or not steps:
            raise FreezeRefused(f"job {name!r} lists no steps")
        names.append(name)
        is_bench = isinstance(name, str) and name.startswith("bench (")
        compared = False
        for step in steps:
            if not isinstance(step, dict):
                raise FreezeRefused(f"job {name!r} lists a step that is not an object")
            if step.get("status") != "completed":
                raise FreezeRefused(f"job {name!r} step {step.get('name')!r} has not completed")
            conclusion = step.get("conclusion")
            if is_bench and step.get("name") == COMPARE_STEP:
                compared = True
                if conclusion == "failure":
                    compare_failures += 1
                    continue
            if conclusion != "success":
                raise FreezeRefused(f"job {name!r} step {step.get('name')!r} concluded {conclusion!r}; only the "
                                    "baseline comparison may fail under --accept-regression")
        if is_bench and not compared:
            raise FreezeRefused(f"job {name!r} has no {COMPARE_STEP!r} step")
    missing = [code for code in CORPORA if f"bench ({code})" not in names]
    if missing:
        raise FreezeRefused(f"the run has no bench job for {', '.join(missing)}")
    if not compare_failures:
        raise FreezeRefused("no baseline comparison failed; freeze without --accept-regression")


def r4(value):
    if isinstance(value, float):
        return round(value, 4)
    if isinstance(value, dict):
        return {k: r4(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [r4(v) for v in value]
    return value


def frozen_baseline(baseline, report, run_id, source_commit, today, accept_regression=None):
    """The baseline rewritten from one reduced report through an explicit allow-list."""
    baseline = json.loads(json.dumps(baseline))
    # An explicit allow-list. Never baseline.update(report).
    baseline.update({
        "cases": report["cases_scored"],
        "label": report["label"],
        "median_secs": report["median_secs"],
        "metrics": r4(report["metrics"]),
        "ci": r4(report["ci"]),
        "yield_budgets": report["yield_budgets"],
        "by_task_family": r4(report["by_task_family"]),
    })
    provenance = baseline.setdefault("provenance", {})
    provenance["frozen_from"] = f"{WORKFLOW} run {run_id} (ubuntu-latest)"
    provenance["source_commit"] = source_commit
    provenance["frozen_on"] = today
    provenance.pop("yield_note", None)
    # A record from an earlier accepted regression never carries over to a later freeze.
    provenance.pop("accepted_regression", None)
    if accept_regression is not None:
        provenance["accepted_regression"] = {"reason": accept_regression, "source_run": run_id}
    return baseline


def plan_freeze(run, download, directory, today=None, jobs=None, accept_regression=None):
    """[(path, old text, new text)] for every split baseline, validated; refuses on any problem.

    `accept_regression` is the reason a failed comparison is accepted; it requires `jobs`.
    """
    if accept_regression is not None:
        problem = reason_problem(accept_regression)
        if problem:
            raise FreezeRefused(problem)
        if jobs is None:
            raise FreezeRefused("--accept-regression needs --jobs-json, the run's jobs and step conclusions")
    run_id, source_commit = check_run(run, accept_regression)
    if accept_regression is not None:
        check_jobs(run_id, jobs)
    today = today or datetime.date.today().isoformat()
    plan = []
    for code in CORPORA:
        for split in SPLITS:
            path = Path(directory) / f"{code}-{split}.json"
            source = Path(download) / code / f"{split}.json"
            try:
                old = path.read_text()
                report = json.loads(source.read_text())
                new = frozen_baseline(json.loads(old), report, run_id, source_commit, today, accept_regression)
            except (OSError, ValueError, KeyError, TypeError, AttributeError) as err:
                raise FreezeRefused(f"{code} {split}: cannot build the baseline ({type(err).__name__})") from None
            problems = baseline_problems(new)
            if problems:
                raise FreezeRefused(f"{code} {split}: " + "; ".join(problems))
            plan.append((path, old, json.dumps(new, indent=2) + "\n"))
    return plan


def _write_temp(path, text):
    """Write `text` to a new temporary file beside `path` and return the temporary path."""
    fd, tmp = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise
    return Path(tmp)


def write_all(plan):
    """Replace every file in `plan` or none: temporary files first, then renames, rolled back on failure."""
    temps = []
    replaced = []
    try:
        for path, _old, new in plan:
            temps.append((path, _write_temp(path, new)))
        for path, tmp in temps:
            os.replace(tmp, path)
            replaced.append(path)
    except BaseException:
        originals = {path: old for path, old, _new in plan}
        for path in replaced:
            restore = _write_temp(path, originals[path])
            os.replace(restore, path)
        for path, tmp in temps:
            tmp.unlink(missing_ok=True)
        raise


def check(directory):
    failures = 0
    files = list(baseline_files(directory))
    if not files:
        print(f"no split baselines under {directory}", file=sys.stderr)
        return 1
    for path, data in files:
        problems = baseline_problems(data)
        if problems:
            failures += 1
            print(f"{path.name}: " + "; ".join(problems), file=sys.stderr)
        else:
            print(f"{path.name}: valid")
    return 1 if failures else 0


def read_json(path, what):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError) as err:
        raise FreezeRefused(f"cannot read the {what} ({type(err).__name__})") from None


def freeze(run_json, download, directory, jobs_json=None, accept_regression=None):
    try:
        run = read_json(run_json, "run description")
        jobs = None if jobs_json is None else read_json(jobs_json, "jobs description")
        plan = plan_freeze(run, download, directory, jobs=jobs, accept_regression=accept_regression)
    except FreezeRefused as refused:
        print(f"freeze refused; no baseline written: {refused}", file=sys.stderr)
        return 1
    try:
        write_all(plan)
    except Exception as err:  # noqa: BLE001 - the rollback already ran; report without file content
        print(f"freeze failed and was rolled back; no baseline changed ({type(err).__name__})", file=sys.stderr)
        return 1
    for path, _old, _new in plan:
        print(f"froze {path}" + (" (accepted regression)" if accept_regression is not None else ""))
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="command", required=True)
    check_cmd = sub.add_parser("check", help="validate every split baseline")
    check_cmd.add_argument("--dir", default="benchmarks/commit-derived", type=Path)
    freeze_cmd = sub.add_parser("freeze", help="rewrite every split baseline from one successful run")
    freeze_cmd.add_argument("--run-json", required=True, type=Path)
    freeze_cmd.add_argument("--download", required=True, type=Path)
    freeze_cmd.add_argument("--jobs-json", type=Path, default=None,
                            help="the run's jobs: gh api --paginate --slurp repos/{owner}/{repo}/actions/runs/<run>/jobs")
    freeze_cmd.add_argument("--accept-regression", metavar="REASON", default=None,
                            help="freeze a run whose baseline comparison was its only failure, recording REASON")
    freeze_cmd.add_argument("--dir", default="benchmarks/commit-derived", type=Path)
    args = ap.parse_args(argv)
    if args.command == "check":
        return check(args.dir)
    return freeze(args.run_json, args.download, args.dir, args.jobs_json, args.accept_regression)


if __name__ == "__main__":
    sys.exit(main())
