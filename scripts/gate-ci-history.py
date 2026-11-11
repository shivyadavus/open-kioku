#!/usr/bin/env python3
"""What would `ok gate ci` have said about a repository's own merged history?

For each of the last N first-parent commits of a branch, a scratch clone is checked out at the
commit and indexed, and `ok gate ci --base <commit>^` judges the commit three ways:

  default        the task is the commit subject; the planned boundary is advisory.
  enforce        the same task with `--mode enforce`: a file outside the boundary fails.
  enforce_body   `--mode enforce` with the subject and the body as the task.

Every commit here was reviewed and merged, so a FAIL is the gate refusing a change the
maintainers accepted. The share of merged commits a mode fails is the price of running that
mode as a required check.

  scripts/gate-ci-history.py --source . --work ../gate-ci-history --ok-bin target/release/ok \\
      --commits 20 --write benchmarks/gate/ci-history.json
"""
import argparse
import json
import subprocess
import sys
import time
from pathlib import Path

LOCKFILES = {"Cargo.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "poetry.lock",
             "uv.lock", "Gemfile.lock", "go.sum", "composer.lock"}
CHANGELOGS = {"CHANGELOG.md", "CHANGES.md", "HISTORY.md", "NEWS.md"}


def sh(args, cwd=None, check=True):
    out = subprocess.run(args, cwd=cwd, capture_output=True, text=True)
    if check and out.returncode != 0:
        raise RuntimeError(f"{args} failed: {out.stderr[-400:]}")
    return out


def kind(path):
    """A coarse class for a changed path, to say what a boundary planned from text misses."""
    name = path.rsplit("/", 1)[-1]
    if name in CHANGELOGS:
        return "changelog"
    if name in LOCKFILES:
        return "lockfile"
    if path.endswith((".md", ".mdx", ".rst")):
        return "documentation"
    if "/tests/" in path or path.startswith("tests/") or "/snapshots/" in path:
        return "tests"
    if path.endswith((".toml", ".json", ".yml", ".yaml")):
        return "manifest or data"
    return "source"


def judge(ok, clone, base, task, mode):
    args = [ok, "--json", "gate", "ci", "--base", base, "--task", task]
    if mode:
        args += ["--mode", mode]
    started = time.time()
    out = sh(args, cwd=clone, check=False)
    try:
        dossier = json.loads(out.stdout)["dossier"]
    except (json.JSONDecodeError, KeyError):
        return {"error": out.stderr.strip()[-300:]}
    verification = dossier.get("verification") or {}
    violations = verification.get("boundary_violations") or []
    return {
        "verdict": dossier["verdict"],
        "exit_status": out.returncode,
        "seconds": round(time.time() - started, 1),
        "plan_confidence": dossier["plan"]["confidence_summary"].split(" from ")[0],
        "allowed": dossier["plan"]["allowed_count"],
        "caution": dossier["plan"]["caution_count"],
        "changed_files": len(dossier["change"]["changed_files"]),
        "violations": [v.get("path") for v in violations],
        "forbidden": [v.get("path") for v in violations if v.get("kind") == "forbidden_boundary"],
        "outside_boundary": dossier["change"].get("outside_boundary") or [],
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--source", required=True, help="repository whose history is replayed")
    ap.add_argument("--work", required=True, help="directory for the scratch clone")
    ap.add_argument("--ok-bin", required=True)
    ap.add_argument("--branch", default="origin/main")
    ap.add_argument("--commits", type=int, default=20)
    ap.add_argument("--write", required=True, help="where the JSON result goes")
    args = ap.parse_args()
    ok = str(Path(args.ok_bin).resolve())
    work = Path(args.work)
    work.mkdir(parents=True, exist_ok=True)
    clone = work / "repo"
    if not clone.exists():
        sh(["git", "clone", "-q", "--no-hardlinks", args.source, str(clone)])
    shas = sh(["git", "-C", args.source, "log", "--first-parent", f"-{args.commits}",
               "--format=%H", args.branch]).stdout.split()
    rows = []
    for sha in shas:
        subject = sh(["git", "-C", args.source, "log", "-1", "--format=%s", sha]).stdout.strip()
        body = sh(["git", "-C", args.source, "log", "-1", "--format=%b", sha]).stdout.strip()
        changed = sh(["git", "-C", args.source, "diff", "--name-only", f"{sha}^", sha]).stdout.split()
        sh(["git", "-C", str(clone), "checkout", "-q", "-f", sha])
        sh(["git", "-C", str(clone), "clean", "-fdq", "-e", ".ok"])
        sh([ok, "index", "."], cwd=str(clone))
        row = {
            "commit": sha[:10],
            "subject": subject,
            "changed_by_kind": {},
            "default": judge(ok, str(clone), f"{sha}^", subject, None),
            "enforce": judge(ok, str(clone), f"{sha}^", subject, "enforce"),
            "enforce_body": judge(ok, str(clone), f"{sha}^", f"{subject}\n\n{body}", "enforce"),
        }
        for path in changed:
            row["changed_by_kind"][kind(path)] = row["changed_by_kind"].get(kind(path), 0) + 1
        row["enforce"]["violations_by_kind"] = {}
        for path in row["enforce"].get("violations") or []:
            by_kind = row["enforce"]["violations_by_kind"]
            by_kind[kind(path)] = by_kind.get(kind(path), 0) + 1
        rows.append(row)
        print(f"{sha[:10]} default={row['default'].get('verdict')} "
              f"enforce={row['enforce'].get('verdict')} "
              f"({len(row['enforce'].get('violations') or [])} of {len(changed)} outside) "
              f"enforce+body={row['enforce_body'].get('verdict')} | {subject[:60]}", flush=True)

        def count(mode, verdict):
            return sum(1 for r in rows if r[mode].get("verdict") == verdict)

        totals = {"changed": {}, "outside_under_enforce": {}}
        for r in rows:
            for k, v in r["changed_by_kind"].items():
                totals["changed"][k] = totals["changed"].get(k, 0) + v
            for k, v in (r["enforce"].get("violations_by_kind") or {}).items():
                totals["outside_under_enforce"][k] = totals["outside_under_enforce"].get(k, 0) + v
        result = {
            "method": "ok gate ci over the last merged first-parent commits of the branch; "
                      "each commit judged against its own subject (and body).",
            "branch": args.branch,
            "head": shas[0][:10],
            "commits": len(rows),
            "verdicts": {mode: {v: count(mode, v) for v in ("pass", "warn", "fail")}
                         for mode in ("default", "enforce", "enforce_body")},
            "files_by_kind": totals,
            "cases": rows,
        }
        Path(args.write).write_text(json.dumps(result, indent=2) + "\n")
    print("done")


if __name__ == "__main__":
    sys.exit(main())
