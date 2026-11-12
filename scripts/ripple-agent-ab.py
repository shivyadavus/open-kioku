#!/usr/bin/env python3
"""Does the ripple hook change what a coding agent leaves behind?

A task is a real commit that changed a function's signature and then its callers. The checkout
is the commit's parent. The prompt states the signature change and nothing else about where
its callers are. Two arms, the same prompt and tools:

  plain  - a clone at the parent.
  hooks  - the same clone after `ok gate install claude --apply`, with `ok` on PATH. Every
           default applies: no configuration, no index built beforehand (the gate starts one
           in the background), nothing in the prompt about Open Kioku.

When the agent stops, `ok ripple` is run on the checkout by the harness, in both arms: the
number of call sites that no longer fit is the result. No project code is run, by the agent or
by the harness. Each run is a real Claude Code session and is billed as one.
"""
import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

TOOLS = ["Read", "Grep", "Glob", "Edit", "Write", "MultiEdit",
         "Bash(ls:*)", "Bash(rg:*)", "Bash(grep:*)", "Bash(cat:*)", "Bash(head:*)",
         "Bash(wc:*)", "Bash(find:*)"]
HOOK_TOOLS = ["Bash(ok:*)"]
DENY = ["Agent", "Task", "WebFetch", "WebSearch", "NotebookEdit"]

PROMPT = """In `{path}`, change `{name}` so that its signature is

    {after}

It is `{before}` now. Update whatever else in this repository has to change with it. \
You cannot run code here: do not run tests or builds, and stop when the change is complete."""


def run(args, cwd=None, check=True, env=None):
    out = subprocess.run(args, cwd=cwd, capture_output=True, text=True, env=env, errors="replace")
    if check and out.returncode != 0:
        raise RuntimeError(f"{args[:5]} failed: {out.stderr[-400:]}")
    return out.stdout


def prepare(source, work, task, arm, ok_bin):
    dest = work / f"{task['id']}-{arm}"
    if dest.exists():
        return dest
    sha = task["sha"]
    run(["git", "clone", "-q", "--no-hardlinks", source, str(dest)])
    git = ["git", "-C", str(dest), "-c", "user.email=ab@example.invalid", "-c", "user.name=ab"]
    run(git + ["checkout", "-q", "--detach", f"{sha}^"])
    # No ref and no object may hold the answer.
    run(git + ["remote", "remove", "origin"])
    for ref in run(git + ["for-each-ref", "--format=%(refname)"]).split():
        run(git + ["update-ref", "-d", ref])
    run(git + ["reflog", "expire", "--expire=now", "--all"])
    run(git + ["gc", "-q", "--prune=now"])
    if subprocess.run(git + ["cat-file", "-e", f"{sha}^{{commit}}"], capture_output=True).returncode == 0:
        raise RuntimeError(f"{dest} still holds {sha}")
    exclude = dest / ".git" / "info" / "exclude"
    exclude.write_text(exclude.read_text() + "\n.ok/\n.claude/\n")
    if arm == "hooks":
        run([ok_bin, "gate", "install", "claude", "--apply"], cwd=str(dest))
    return dest


def agent(arm, checkout, prompt, out_dir, ok_dir, max_turns, budget):
    env = dict(os.environ)
    env["RUST_BACKTRACE"] = "0"
    tools = list(TOOLS)
    if arm == "hooks":
        env["PATH"] = f"{ok_dir}:{env['PATH']}"
        tools += HOOK_TOOLS
    args = ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
            "--max-turns", str(max_turns), "--max-budget-usd", str(budget),
            "--permission-mode", "acceptEdits", "--strict-mcp-config",
            "--disallowedTools", *DENY, "--allowedTools", *tools]
    started = time.time()
    transcript = out_dir / f"{arm}.transcript.jsonl"
    if transcript.exists() and transcript.stat().st_size > 0:
        stdout = transcript.read_text()
    else:
        out = subprocess.run(args, cwd=str(checkout), capture_output=True, text=True, env=env,
                             stdin=subprocess.DEVNULL, errors="replace")
        stdout = out.stdout
        transcript.write_text(stdout)
        (out_dir / f"{arm}.stderr.txt").write_text(out.stderr)
    record, tools_used, notices, holds = {}, {}, 0, 0
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        if event.get("type") == "result":
            record = event
        text = json.dumps(event)
        notices += text.count("elsewhere no longer fit what it changed")
        holds += text.count("that no longer fit what it changed.")
        message = event.get("message")
        content = message.get("content") if isinstance(message, dict) else None
        for block in (content if isinstance(content, list) else []):
            if isinstance(block, dict) and block.get("type") == "tool_use":
                name = block.get("name") or "?"
                tools_used[name] = tools_used.get(name, 0) + 1
    return {
        "arm": arm,
        "answer_tail": (record.get("result") or "")[-500:],
        "is_error": record.get("is_error"),
        "turns": record.get("num_turns"),
        "cost_usd": round(record.get("total_cost_usd") or 0, 4),
        "duration_s": round((record.get("duration_ms") or 0) / 1000, 1),
        "wall_s": round(time.time() - started, 1),
        "tools": tools_used,
        "ripple_notices_in_transcript": notices,
        "ripple_holds_in_transcript": holds,
    }


def judge(source, checkout, task, ok_bin):
    sha = task["sha"]
    report = json.loads(run([ok_bin, "ripple", "--json", "--no-index"], cwd=str(checkout)))
    target = [c for c in report["contracts"] if c["name"] == task["name"]]
    changed = set(run(["git", "-C", str(checkout), "diff", "--name-only", "HEAD"]).split())
    gold = set(run(["git", "-C", source, "diff", "--name-only", f"{sha}^", sha]).split())
    return {
        "signature_now": target[0].get("after") if target else None,
        "effect": target[0]["effect"] if target else "the declaration was not changed",
        "broken_sites_left": report["broken_sites"],
        "broken_left": [f"{s['path']}:{s['line']}  {s['problem']}"
                        for c in report["contracts"] for s in c["broken"]][:10],
        "calls_fitting": sum(c.get("sites_fitting", 0) for c in report["contracts"]),
        "possible_sites": sum(len(c.get("possible", [])) for c in report["contracts"]),
        "files_changed": sorted(changed),
        "files_of_the_commit_also_changed": f"{len(changed & gold)} of {len(gold)}",
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--repos", required=True, help="directory holding a clone per task['repo']")
    ap.add_argument("--work", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--ok-bin", required=True, help="a binary named `ok`")
    ap.add_argument("--tasks", required=True,
                    help="JSON: [{id, repo, sha, path, name, before, after}]")
    ap.add_argument("--arms", nargs="+", default=["plain", "hooks"])
    ap.add_argument("--max-turns", type=int, default=40)
    ap.add_argument("--budget", type=float, default=2.0)
    args = ap.parse_args()
    work, out = Path(args.work), Path(args.out)
    work.mkdir(parents=True, exist_ok=True)
    out.mkdir(parents=True, exist_ok=True)
    ok_bin = str(Path(args.ok_bin).resolve())
    rows = []
    for index, task in enumerate(json.loads(Path(args.tasks).read_text())):
        source = str(Path(args.repos) / task["repo"])
        prompt = PROMPT.format(**task)
        row = {**task, "arms": {}}
        arms = args.arms if index % 2 == 0 else list(reversed(args.arms))
        for arm in arms:
            task_out = out / task["id"]
            task_out.mkdir(exist_ok=True)
            done = task_out / f"{arm}.result.json"
            if done.exists():
                row["arms"][arm] = json.loads(done.read_text())
                continue
            checkout = prepare(source, work, task, arm, ok_bin)
            record = agent(arm, checkout, prompt, task_out, str(Path(ok_bin).parent),
                           args.max_turns, args.budget)
            record.update(judge(source, checkout, task, ok_bin))
            done.write_text(json.dumps(record, indent=2))
            row["arms"][arm] = record
            print(f"{task['id']} {arm:5} broken_left={record['broken_sites_left']} "
                  f"fitting={record['calls_fitting']} effect={record['effect']} "
                  f"turns={record['turns']} cost={record['cost_usd']} t={record['duration_s']}s "
                  f"notices={record['ripple_notices_in_transcript']} "
                  f"holds={record['ripple_holds_in_transcript']}", flush=True)
        rows.append(row)
        (out / "results.json").write_text(json.dumps(rows, indent=2) + "\n")
    print("done")


if __name__ == "__main__":
    sys.exit(main())
