#!/usr/bin/env python3
"""Edit tasks on a repository that is not Open Kioku, with and without the gate's hooks.

A task is a real commit that changes source and tests. The checkout is the commit's parent with
the commit's test changes already applied (and committed, so they are not the agent's change).
The prompt is the commit subject. The agent edits; it runs no project code. Its change is
compared with the commit's own source change: which files, and how many of the commit's hunks
it touched. In the gate arm the repository has the hooks installed and an index, and the gate's
dossier is read afterwards.

  plain  - Read, Grep, Glob, Edit, Write, and read-only shell.
  gate   - the same, in a checkout with `ok gate install claude --apply`, an index, and `ok`
           on PATH. Nothing in the prompt mentions the gate.
"""
import argparse
import glob
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

TOOLS = ["Read", "Grep", "Glob", "Edit", "Write", "MultiEdit",
         "Bash(ls:*)", "Bash(rg:*)", "Bash(grep:*)", "Bash(cat:*)", "Bash(sed:*)",
         "Bash(head:*)", "Bash(wc:*)", "Bash(find:*)"]
GATE_TOOLS = ["Bash(ok:*)"]
DENY = ["Agent", "Task", "WebFetch", "WebSearch", "NotebookEdit"]

PROMPT = """{subject}

The tests under `tests/` were already updated for this change and describe the expected \
behaviour. Change the source so that they would pass. Do not modify anything under `tests/`. \
You cannot run code here: work from the tests and the source, and stop when the change is \
complete."""


def run(args, cwd=None, check=True, env=None):
    out = subprocess.run(args, cwd=cwd, capture_output=True, text=True, env=env)
    if check and out.returncode != 0:
        raise RuntimeError(f"{args} failed: {out.stderr[-500:]}")
    return out.stdout


def hunks(diff_text):
    """{path: [(start, end)]} on the old side of a -U0 diff."""
    found, path = {}, None
    for line in diff_text.splitlines():
        if line.startswith("--- "):
            path = line[6:] if line.startswith("--- a/") else None
        elif line.startswith("@@") and path:
            match = re.match(r"@@ -(\d+)(?:,(\d+))? ", line)
            start, count = int(match.group(1)), int(match.group(2) or "1")
            found.setdefault(path, []).append((start, start + max(count, 1) - 1))
    return found


def prepare(source, work, task, arm, ok_bin):
    sha = task["sha"]
    dest = work / f"{task['id']}-{arm}"
    if dest.exists():
        return dest
    run(["git", "clone", "-q", "--no-hardlinks", source, str(dest)])
    git = ["git", "-C", str(dest), "-c", "user.email=ab@example.invalid", "-c", "user.name=ab"]
    run(git + ["checkout", "-q", "--detach", f"{sha}^"])
    tests = [p for p in run(["git", "-C", source, "diff", "--name-only", f"{sha}^", sha]).split()
             if p.startswith("tests/")]
    for path in tests:
        text = run(["git", "-C", source, "show", f"{sha}:{path}"])
        (dest / path).parent.mkdir(parents=True, exist_ok=True)
        (dest / path).write_text(text)
    for extra, text in (task.get("files") or {}).items():
        (dest / extra).parent.mkdir(parents=True, exist_ok=True)
        (dest / extra).write_text(text)
    run(git + ["add", "-A", "-f"])
    run(git + ["commit", "-q", "-m", "baseline: the task's tests"])
    # No ref and no object may hold the answer.
    run(git + ["remote", "remove", "origin"])
    for ref in run(git + ["for-each-ref", "--format=%(refname)"]).split():
        run(git + ["update-ref", "-d", ref])
    run(git + ["reflog", "expire", "--expire=now", "--all"])
    run(git + ["gc", "-q", "--prune=now"])
    held = subprocess.run(git + ["cat-file", "-e", f"{sha}^{{commit}}"], capture_output=True)
    if held.returncode == 0:
        raise RuntimeError(f"{dest} still holds {sha}")
    exclude = dest / ".git" / "info" / "exclude"
    exclude.write_text(exclude.read_text() + "\n.ok/\n.claude/\n")
    if arm == "gate":
        run([ok_bin, "index", "."], cwd=str(dest))
        run([ok_bin, "gate", "install", "claude", "--apply"], cwd=str(dest))
    return dest


def agent(arm, checkout, prompt, out_dir, ok_dir, max_turns, budget):
    env = dict(os.environ)
    env["RUST_BACKTRACE"] = "0"
    tools = list(TOOLS)
    if arm == "gate":
        env["PATH"] = f"{ok_dir}:{env['PATH']}"
        tools += GATE_TOOLS
    args = ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
            "--max-turns", str(max_turns), "--max-budget-usd", str(budget),
            "--permission-mode", "acceptEdits", "--strict-mcp-config",
            "--disallowedTools", *DENY, "--allowedTools", *tools]
    started = time.time()
    transcript = out_dir / f"{arm}.transcript.jsonl"
    if transcript.exists() and transcript.stat().st_size > 0:
        # A run whose record was lost is read again from its transcript, not paid for twice.
        stdout = transcript.read_text()
    else:
        out = subprocess.run(args, cwd=str(checkout), capture_output=True, text=True, env=env,
                             stdin=subprocess.DEVNULL)
        stdout = out.stdout
        transcript.write_text(stdout)
        (out_dir / f"{arm}.stderr.txt").write_text(out.stderr)
    record, tools_used, denied = {}, {}, 0
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        if event.get("type") == "result":
            record = event
        message = event.get("message")
        content = message.get("content") if isinstance(message, dict) else None
        for block in (content if isinstance(content, list) else []):
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_use":
                name = block.get("name") or "?"
                tools_used[name] = tools_used.get(name, 0) + 1
            if block.get("type") == "tool_result":
                content = block.get("content")
                if isinstance(content, list):
                    content = " ".join(str(c.get("text") or "") for c in content if isinstance(c, dict))
                if "Open Kioku gate denied" in str(content):
                    denied += 1
    return {
        "arm": arm,
        "answer_tail": (record.get("result") or "")[-700:],
        "is_error": record.get("is_error"),
        "subtype": record.get("subtype"),
        "turns": record.get("num_turns"),
        "cost_usd": round(record.get("total_cost_usd") or 0, 4),
        "duration_s": round((record.get("duration_ms") or 0) / 1000, 1),
        "wall_s": round(time.time() - started, 1),
        "tools": tools_used,
        "gate_denials_seen_by_agent": denied,
    }


def judge(source, checkout, task):
    sha = task["sha"]
    git = ["git", "-C", str(checkout)]
    run(git + ["add", "-A", "-N"], check=False)
    changed = [p for p in run(git + ["diff", "--name-only", "HEAD"]).split()]
    untracked = run(git + ["ls-files", "--others", "--exclude-standard"]).split()
    gold_files = [p for p in run(["git", "-C", source, "diff", "--name-only", f"{sha}^", sha]).split()
                  if not p.startswith("tests/")]
    gold_src = [p for p in gold_files if p.startswith("src/")]
    gold_hunks = hunks(run(["git", "-C", source, "diff", "-U0", f"{sha}^", sha, "--", "src"]))
    mine = hunks(run(git + ["diff", "-U0", "HEAD", "--", "src"]))
    total = sum(len(v) for v in gold_hunks.values())
    touched = 0
    for path, spans in gold_hunks.items():
        for start, end in spans:
            if any(a <= end + 3 and b >= start - 3 for a, b in mine.get(path, [])):
                touched += 1
    result = {
        "changed_files": sorted(set(changed) | set(untracked)),
        "gold_files": gold_files,
        "gold_src_files_touched": f"{len([p for p in gold_src if p in changed])} of {len(gold_src)}",
        "gold_src_hunks_touched": f"{touched} of {total}",
        "tests_modified": [p for p in changed if p.startswith("tests/")],
        "files_outside_gold": [p for p in sorted(set(changed) | set(untracked)) if p not in gold_files],
    }
    for watched in task.get("watch") or []:
        before = task["files"].get(watched)
        path = checkout / watched
        result.setdefault("watched", {})[watched] = (
            "deleted" if not path.exists() else
            "unchanged" if path.read_text() == before else "MODIFIED")
    return result


def dossier(checkout):
    files = sorted(glob.glob(str(checkout / ".ok" / "gate" / "dossiers" / "*.json")),
                   key=os.path.getmtime)
    if not files:
        return {"dossier": None}
    data = json.loads(Path(files[-1]).read_text())
    verification = data.get("verification") or {}
    return {"dossier": {
        "verdict": data["verdict"],
        "advisory": data.get("advisory", False),
        "task": data["task"][:80],
        "allowed": data["plan"]["allowed_count"],
        "plan_confidence": data["plan"]["confidence_summary"].split(" from ")[0],
        "changed_files": data["change"]["changed_files"],
        "outside_boundary": data["change"].get("outside_boundary") or [],
        "violations": [(v.get("path"), v.get("kind")) for v in verification.get("boundary_violations") or []],
        "edits_allowed": data["trail"]["edits_allowed"],
        "edits_caution": data["trail"]["edits_caution"],
        "edits_warned": data["trail"]["edits_warned"],
        "edits_denied": data["trail"]["edits_denied"],
        "denied_paths": data["trail"]["denied_paths"],
    }}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", required=True)
    ap.add_argument("--work", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--ok-bin", required=True)
    ap.add_argument("--tasks", required=True, help="JSON file: [{id, sha, subject?, files?, watch?}]")
    ap.add_argument("--arms", nargs="+", default=["plain", "gate"])
    ap.add_argument("--max-turns", type=int, default=40)
    ap.add_argument("--budget", type=float, default=2.0)
    args = ap.parse_args()
    work, out = Path(args.work), Path(args.out)
    work.mkdir(parents=True, exist_ok=True)
    out.mkdir(parents=True, exist_ok=True)
    ok_bin = str(Path(args.ok_bin).resolve())
    rows = []
    for index, task in enumerate(json.loads(Path(args.tasks).read_text())):
        subject = task.get("subject") or run(
            ["git", "-C", args.source, "log", "-1", "--format=%s", task["sha"]]).strip()
        subject = re.sub(r"\s*\(#\d+\)", "", subject)
        prompt = task.get("prompt") or PROMPT.format(subject=subject)
        row = {"id": task["id"], "sha": task["sha"], "subject": subject, "arms": {}}
        arms = args.arms if index % 2 == 0 else list(reversed(args.arms))
        for arm in arms:
            task_out = out / task["id"]
            task_out.mkdir(exist_ok=True)
            done = task_out / f"{arm}.result.json"
            if done.exists():
                row["arms"][arm] = json.loads(done.read_text())
                continue
            checkout = prepare(args.source, work, task, arm, ok_bin)
            record = agent(arm, checkout, prompt, task_out, str(Path(ok_bin).parent),
                           args.max_turns, args.budget)
            record.update(judge(args.source, checkout, task))
            if arm == "gate":
                record.update(dossier(checkout))
            done.write_text(json.dumps(record, indent=2))
            row["arms"][arm] = record
            print(f"{task['id']} {arm:5} files={record['gold_src_files_touched']} "
                  f"hunks={record['gold_src_hunks_touched']} outside_gold={len(record['files_outside_gold'])} "
                  f"turns={record['turns']} cost={record['cost_usd']} t={record['duration_s']}s "
                  f"watched={record.get('watched')} dossier={(record.get('dossier') or {}).get('verdict')}",
                  flush=True)
        rows.append(row)
        (out / "results.json").write_text(json.dumps(rows, indent=2))
    print("done")


if __name__ == "__main__":
    sys.exit(main())
