#!/usr/bin/env python3
"""Real Django fixes, done by a coding agent with and without Open Kioku, judged by the tests.

A task is a commit of django/django that changes three or more source files and its tests. The
checkout is the commit's parent with the commit's test changes applied, as a new repository of
one commit, so no object holds the answer. The prompt is the commit's subject, the instruction
to make the tests pass without touching `tests/`, and the command that runs them. The agent may
run that command. Afterwards the harness restores `tests/`, runs the same test modules itself,
and records whether they pass.

  plain  - Read, Grep, Glob, Edit, Write, read-only shell, and the test command.
  kioku  - the same, plus the Open Kioku plugin as shipped (MCP server, skill, hooks), an index
           built before the session, and `ok` on PATH. The prompt is the same.

Setup, once:

    git clone --depth 500 https://github.com/django/django dj
    python3 -m venv venv && venv/bin/pip install asgiref sqlparse jinja2 pyyaml tblib \
        docutils pillow argon2-cffi bcrypt tzdata

    scripts/agent-large-ab.py --source dj --work work --out out --venv venv \
        --ok-bin target/release/ok --plugin-dir . \
        --tasks benchmarks/agent-ab/large-repository-tasks.json \
        --models default claude-haiku-5-5

`--validate <sha>...` checks candidate commits instead of running sessions: for each, the test
modules the commit changed must fail at its parent with its tests applied and pass at the
commit. It prints the task list `--tasks` reads. The result of the run recorded in this
repository is `benchmarks/agent-ab/large-repository.md`.
"""
import argparse
import concurrent.futures
import json
import os
import re
import shutil
import subprocess
import threading
import time
from pathlib import Path

TOOLS = ["Read", "Grep", "Glob", "Edit", "Write", "MultiEdit",
         "Bash(ls:*)", "Bash(rg:*)", "Bash(grep:*)", "Bash(cat:*)", "Bash(sed:*)",
         "Bash(head:*)", "Bash(tail:*)", "Bash(wc:*)", "Bash(find:*)", "Bash(git diff:*)",
         "Bash(git status:*)", "Bash(python tests/runtests.py:*)"]
KIOKU_TOOLS = ["Bash(ok:*)", "mcp__plugin_open-kioku_open-kioku", "mcp__open-kioku", "Skill"]
DENY = ["Agent", "Task", "WebFetch", "WebSearch", "NotebookEdit"]

PROMPT = """{subject}

This is the Django repository. The tests for this change are already in `tests/` and describe \
the expected behaviour. Change the code under `django/` so that they pass. Do not modify \
anything under `tests/`. Run them with:

    python tests/runtests.py --parallel 1 {labels}

Stop when the change is complete."""


def run(args, cwd=None, check=True, env=None, timeout=None):
    out = subprocess.run(args, cwd=cwd, capture_output=True, text=True, env=env, timeout=timeout)
    if check and out.returncode != 0:
        raise RuntimeError(f"{args} failed: {out.stderr[-500:]}")
    return out.stdout


def hunks(diff_text):
    found, path = {}, None
    for line in diff_text.splitlines():
        if line.startswith("--- "):
            path = line[6:] if line.startswith("--- a/") else None
        elif line.startswith("@@") and path:
            match = re.match(r"@@ -(\d+)(?:,(\d+))? ", line)
            start, count = int(match.group(1)), int(match.group(2) or "1")
            found.setdefault(path, []).append((start, start + max(count, 1) - 1))
    return found


def prepare(source, dest, task, arm, ok_bin):
    sha = task["sha"]
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    archive = subprocess.Popen(["git", "-C", str(source), "archive", f"{sha}^"], stdout=subprocess.PIPE)
    subprocess.run(["tar", "-x", "-C", str(dest)], stdin=archive.stdout, check=True)
    archive.wait()
    for path in run(["git", "-C", str(source), "diff", "--name-only", f"{sha}^", sha]).split():
        if path.startswith("tests/"):
            blob = subprocess.run(["git", "-C", str(source), "show", f"{sha}:{path}"], capture_output=True)
            target = dest / path
            if blob.returncode == 0:
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(blob.stdout)
            elif target.exists():
                target.unlink()
    git = ["git", "-C", str(dest), "-c", "user.email=ab@example.invalid", "-c", "user.name=ab"]
    run(git + ["init", "-q"])
    run(git + ["add", "-A", "-f"])
    run(git + ["commit", "-q", "-m", "baseline: the task's tests"])
    exclude = dest / ".git" / "info" / "exclude"
    exclude.write_text(exclude.read_text() + "\n.ok/\n.claude/\n__pycache__/\n*.pyc\n")
    index_s = None
    if arm == "kioku":
        started = time.time()
        run([ok_bin, "index", "."], cwd=str(dest))
        index_s = round(time.time() - started, 1)
    return index_s


def agent(arm, model, checkout, prompt, out_dir, ok_dir, plugin_dir, venv, max_turns, budget):
    env = dict(os.environ)
    env["RUST_BACKTRACE"] = "0"
    env["PATH"] = f"{venv}/bin:{env['PATH']}"
    env["PYTHONPATH"] = str(checkout)
    env["PYTHONDONTWRITEBYTECODE"] = "1"
    tools = list(TOOLS)
    args = ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
            "--max-turns", str(max_turns), "--max-budget-usd", str(budget),
            "--permission-mode", "acceptEdits", "--strict-mcp-config",
            "--disallowedTools", *DENY]
    if arm == "kioku":
        env["PATH"] = f"{ok_dir}:{env['PATH']}"
        tools += KIOKU_TOOLS
        server = {"mcpServers": {"open-kioku": {
            "command": str(Path(ok_dir) / "ok"), "args": ["mcp", "serve", "--repo", "."]}}}
        args += ["--plugin-dir", plugin_dir, "--mcp-config", json.dumps(server)]
    if model:
        args += ["--model", model]
    args += ["--allowedTools", *tools]
    started = time.time()
    out = subprocess.run(args, cwd=str(checkout), capture_output=True, text=True, env=env,
                         stdin=subprocess.DEVNULL, errors="replace")
    (out_dir / "transcript.jsonl").write_text(out.stdout)
    (out_dir / "stderr.txt").write_text(out.stderr)
    record, tools_used, notices, test_runs, servers = {}, {}, 0, 0, None
    for line in out.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        if event.get("type") == "result":
            record = event
        if event.get("type") == "system" and event.get("subtype") == "init":
            servers = [f"{m.get('name')}:{m.get('status')}" for m in event.get("mcp_servers") or []]
        if event.get("type") == "system" and event.get("subtype") == "hook_response":
            if "no longer fit" in json.dumps(event):
                notices += 1
        message = event.get("message")
        content = message.get("content") if isinstance(message, dict) else None
        for block in (content if isinstance(content, list) else []):
            if isinstance(block, dict) and block.get("type") == "tool_use":
                name = block.get("name") or "?"
                tools_used[name] = tools_used.get(name, 0) + 1
                if name == "Bash" and "runtests.py" in str((block.get("input") or {}).get("command")):
                    test_runs += 1
    usage = record.get("usage") or {}
    return {
        "subtype": record.get("subtype"),
        "is_error": record.get("is_error"),
        "turns": record.get("num_turns"),
        "cost_usd": round(record.get("total_cost_usd") or 0, 4),
        "duration_s": round((record.get("duration_ms") or 0) / 1000, 1),
        "wall_s": round(time.time() - started, 1),
        "output_tokens": usage.get("output_tokens"),
        "tools": tools_used,
        "kioku_tool_calls": sum(n for name, n in tools_used.items() if "open-kioku" in name),
        "test_runs_by_agent": test_runs,
        "ripple_notices": notices,
        "mcp_servers": servers,
        "answer_tail": (record.get("result") or "")[-500:],
    }


def judge(source, checkout, task, venv, ok_bin, out_dir):
    sha = task["sha"]
    git = ["git", "-C", str(checkout)]
    run(git + ["add", "-A", "-N"], check=False)
    changed = run(git + ["diff", "--name-only", "HEAD"]).split()
    tests_modified = [p for p in changed if p.startswith("tests/")]
    (out_dir / "change.diff").write_text(run(git + ["diff", "HEAD", "--", "django"]))
    gold = [p for p in run(["git", "-C", str(source), "diff", "--name-only", f"{sha}^", sha]).split()
            if p.startswith("django/")]
    gold_hunks = hunks(run(["git", "-C", str(source), "diff", "-U0", f"{sha}^", sha, "--", "django"]))
    mine = hunks(run(git + ["diff", "-U0", "HEAD", "--", "django"]))
    total = sum(len(v) for v in gold_hunks.values())
    touched = sum(1 for path, spans in gold_hunks.items() for start, end in spans
                  if any(a <= end + 3 and b >= start - 3 for a, b in mine.get(path, [])))
    ripple = None
    try:
        report = json.loads(run([ok_bin, "ripple", "--json"], cwd=str(checkout), timeout=120))
        ripple = sum(len(c.get("broken") or []) for c in report.get("contracts") or [])
    except Exception as error:  # the count is a side measurement
        ripple = f"error: {str(error)[:80]}"
    # The verdict is the harness's own run of the task's test modules on the task's tests.
    if tests_modified:
        run(git + ["checkout", "-q", "HEAD", "--", "tests"], check=False)
    env = dict(os.environ, PYTHONPATH=str(checkout), PYTHONDONTWRITEBYTECODE="1")
    try:
        out = subprocess.run([f"{venv}/bin/python", "tests/runtests.py", "--parallel", "1", "-v", "0",
                              *task["labels"]], cwd=str(checkout), capture_output=True, text=True,
                             env=env, timeout=1200)
        passed, tail = out.returncode == 0, (out.stderr + out.stdout).strip().splitlines()[-1:]
    except subprocess.TimeoutExpired:
        passed, tail = False, ["timeout"]
    return {
        "tests_pass": passed,
        "tests_tail": tail,
        "changed_files": sorted(changed),
        "tests_modified_by_agent": tests_modified,
        "gold_files_touched": f"{len([p for p in gold if p in changed])} of {len(gold)}",
        "gold_hunks_touched": f"{touched} of {total}",
        "files_outside_gold": [p for p in sorted(changed) if p not in gold and not p.startswith("tests/")],
        "broken_sites_left_per_ripple": ripple,
    }


def labels_of(source, sha):
    found = []
    for path in run(["git", "-C", str(source), "diff", "--name-only", f"{sha}^", sha]).split():
        name = Path(path).name
        if path.startswith("tests/") and path.endswith(".py") and (name.startswith("test") or name == "tests.py"):
            found.append(path[len("tests/"):-3].replace("/", "."))
    return sorted(set(found))


def validate(source, work, venv, shas):
    """Each commit's test modules fail at parent-with-tests and pass at the commit."""
    tasks = []
    for sha in shas:
        task = {"sha": sha, "subject": run(["git", "-C", str(source), "log", "-1", "--format=%s", sha]).strip(),
                "labels": labels_of(source, sha)}
        outcome = {}
        for which in ("base", "gold"):
            tree = work / f"validate-{sha}-{which}"
            if which == "base":
                prepare(source, tree, task, "plain", None)
            else:
                tree.mkdir(parents=True, exist_ok=True)
                archive = subprocess.Popen(["git", "-C", str(source), "archive", sha], stdout=subprocess.PIPE)
                subprocess.run(["tar", "-x", "-C", str(tree)], stdin=archive.stdout, check=True)
                archive.wait()
            env = dict(os.environ, PYTHONPATH=str(tree), PYTHONDONTWRITEBYTECODE="1")
            done = subprocess.run([f"{venv}/bin/python", "tests/runtests.py", "--parallel", "1", "-v", "0",
                                   *task["labels"]], cwd=str(tree), capture_output=True, text=True, env=env)
            outcome[which] = done.returncode == 0
            shutil.rmtree(tree, ignore_errors=True)
        usable = bool(task["labels"]) and outcome["base"] is False and outcome["gold"] is True
        task["status"] = "usable" if usable else "unusable"
        tasks.append(task)
    print(json.dumps(tasks, indent=1))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--out")
    ap.add_argument("--ok-bin")
    ap.add_argument("--plugin-dir")
    ap.add_argument("--venv", required=True)
    ap.add_argument("--tasks")
    ap.add_argument("--validate", nargs="+", metavar="SHA")
    ap.add_argument("--only", nargs="*")
    ap.add_argument("--arms", nargs="+", default=["plain", "kioku"])
    ap.add_argument("--models", nargs="+", default=["default"])
    ap.add_argument("--max-turns", type=int, default=80)
    ap.add_argument("--budget", type=float, default=3.0)
    ap.add_argument("--total-budget", type=float, default=45.0)
    ap.add_argument("--jobs", type=int, default=4)
    args = ap.parse_args()
    args.venv = str(Path(args.venv).resolve())
    if args.validate:
        validate(Path(args.source).resolve(), Path(args.work).resolve(), args.venv, args.validate)
        return
    for needed in ("out", "ok_bin", "plugin_dir", "tasks"):
        if not getattr(args, needed):
            ap.error(f"--{needed.replace('_', '-')} is required to run sessions")
    source, work, out = Path(args.source).resolve(), Path(args.work).resolve(), Path(args.out).resolve()
    args.plugin_dir = str(Path(args.plugin_dir).resolve())
    ok_bin = str(Path(args.ok_bin).resolve())
    tasks = [t for t in json.loads(Path(args.tasks).read_text()) if t.get("status") == "usable"]
    if args.only:
        tasks = [t for t in tasks if any(t["sha"].startswith(sha) for sha in args.only)]
    spent, lock = [0.0], threading.Lock()
    for done in out.glob("*/record.json"):
        spent[0] += json.loads(done.read_text())["agent"]["cost_usd"]

    def cell(task, arm, model):
        name = f"{task['sha']}-{model}-{arm}"
        cell_out = out / name
        if (cell_out / "record.json").exists():
            return json.loads((cell_out / "record.json").read_text())
        with lock:
            if spent[0] >= args.total_budget:
                return None
        cell_out.mkdir(parents=True, exist_ok=True)
        checkout = work / name
        index_s = prepare(source, checkout, task, arm, ok_bin)
        prompt = PROMPT.format(subject=task["subject"], labels=" ".join(task["labels"]))
        record = {"task": task["sha"], "subject": task["subject"], "arm": arm, "model": model,
                  "index_s": index_s}
        record["agent"] = agent(arm, None if model == "default" else model, checkout, prompt, cell_out,
                                str(Path(ok_bin).parent), args.plugin_dir, args.venv,
                                args.max_turns, args.budget)
        record["judge"] = judge(source, checkout, task, args.venv, ok_bin, cell_out)
        with lock:
            spent[0] += record["agent"]["cost_usd"]
            record["spent_so_far"] = round(spent[0], 2)
        (cell_out / "record.json").write_text(json.dumps(record, indent=1))
        shutil.rmtree(checkout, ignore_errors=True)
        print(json.dumps({k: record[k] for k in ("task", "arm", "model")} |
                         {"pass": record["judge"]["tests_pass"], "turns": record["agent"]["turns"],
                          "cost": record["agent"]["cost_usd"], "s": record["agent"]["duration_s"],
                          "kioku_calls": record["agent"]["kioku_tool_calls"],
                          "spent": record["spent_so_far"]}), flush=True)
        return record

    cells = [(task, arm, model) for task in tasks for model in args.models for arm in args.arms]
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        futures = [pool.submit(cell, *c) for c in cells]
        for future in futures:
            try:
                future.result()
            except Exception as error:
                print(json.dumps({"error": str(error)[:300]}), flush=True)
    print("DONE spent", round(spent[0], 2), flush=True)


if __name__ == "__main__":
    main()
