#!/usr/bin/env python3
"""Validate and freeze the commit-derived baselines under benchmarks/commit-derived/.

    scripts/commit-derived-baselines.py check [--dir benchmarks/commit-derived]
    scripts/commit-derived-baselines.py freeze --run-json RUN.json --download DIR \
        [--dir benchmarks/commit-derived]

`check` validates every split baseline in the directory and exits 1 naming each problem.

`freeze` rewrites all eight split baselines from one `commit-derived-bench` run:
- `RUN.json` is `gh run view <run> --json databaseId,workflowName,status,conclusion,headSha`.
  The freeze refuses unless the run is a completed, successful `commit-derived-bench` run with a
  full source commit.
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
    return problems


def baseline_files(directory):
    """Every split baseline in `directory`, recognised by its content, as (path, data)."""
    for path in sorted(Path(directory).glob("*.json")):
        data = json.loads(path.read_text())
        if isinstance(data, dict) and {"split", "metrics"} <= set(data):
            yield path, data


def check_run(run):
    """(run id, source commit) of a completed, successful commit-derived-bench run; else refuse."""
    if not isinstance(run, dict):
        raise FreezeRefused("the run description is not a JSON object")
    if run.get("workflowName") != WORKFLOW:
        raise FreezeRefused(f"the run is not a {WORKFLOW} run")
    if run.get("status") != "completed" or run.get("conclusion") != "success":
        raise FreezeRefused(f"the run did not succeed (status {run.get('status')!r}, "
                            f"conclusion {run.get('conclusion')!r}); only a successful run can be frozen")
    run_id = run.get("databaseId")
    if not is_count(run_id) or run_id <= 0:
        raise FreezeRefused("the run description has no run id")
    source_commit = run.get("headSha")
    if not isinstance(source_commit, str) or not FULL_COMMIT.fullmatch(source_commit):
        raise FreezeRefused("the run description has no full Open Kioku source commit")
    return run_id, source_commit


def r4(value):
    if isinstance(value, float):
        return round(value, 4)
    if isinstance(value, dict):
        return {k: r4(v) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [r4(v) for v in value]
    return value


def frozen_baseline(baseline, report, run_id, source_commit, today):
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
    return baseline


def plan_freeze(run, download, directory, today=None):
    """[(path, old text, new text)] for every split baseline, validated; refuses on any problem."""
    run_id, source_commit = check_run(run)
    today = today or datetime.date.today().isoformat()
    plan = []
    for code in CORPORA:
        for split in SPLITS:
            path = Path(directory) / f"{code}-{split}.json"
            source = Path(download) / code / f"{split}.json"
            try:
                old = path.read_text()
                report = json.loads(source.read_text())
                new = frozen_baseline(json.loads(old), report, run_id, source_commit, today)
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


def freeze(run_json, download, directory):
    try:
        try:
            run = json.loads(Path(run_json).read_text())
        except (OSError, ValueError) as err:
            raise FreezeRefused(f"cannot read the run description ({type(err).__name__})") from None
        plan = plan_freeze(run, download, directory)
    except FreezeRefused as refused:
        print(f"freeze refused; no baseline written: {refused}", file=sys.stderr)
        return 1
    try:
        write_all(plan)
    except Exception as err:  # noqa: BLE001 - the rollback already ran; report without file content
        print(f"freeze failed and was rolled back; no baseline changed ({type(err).__name__})", file=sys.stderr)
        return 1
    for path, _old, _new in plan:
        print(f"froze {path}")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="command", required=True)
    check_cmd = sub.add_parser("check", help="validate every split baseline")
    check_cmd.add_argument("--dir", default="benchmarks/commit-derived", type=Path)
    freeze_cmd = sub.add_parser("freeze", help="rewrite every split baseline from one successful run")
    freeze_cmd.add_argument("--run-json", required=True, type=Path)
    freeze_cmd.add_argument("--download", required=True, type=Path)
    freeze_cmd.add_argument("--dir", default="benchmarks/commit-derived", type=Path)
    args = ap.parse_args(argv)
    if args.command == "check":
        return check(args.dir)
    return freeze(args.run_json, args.download, args.dir)


if __name__ == "__main__":
    sys.exit(main())
