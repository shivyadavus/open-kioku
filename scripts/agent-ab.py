#!/usr/bin/env python3
"""With/without comparison: does Open Kioku help a coding agent find where a change goes?

For each task (a real commit of the repository), a clone is checked out at the commit's parent
and indexed, and the same prompt (the commit's subject) is given to Claude Code once per arm:

  plain   - built-in read tools only (Read, Grep, Glob, ls/rg/grep/cat/sed/head/wc/find).
  kioku   - the same, plus the Open Kioku plugin as shipped (MCP server, skill, hooks) and
            `ok` on PATH.
  brief   - the plain tools; the prompt also carries the plan's ranked files, as a prompt hook
            would hand them over (`ok preflight`: edit candidates and linked files).
  impact  - the plain tools; the prompt also carries the files with a proven call, import or
            type use into the two files the plain arm ranked first (`ok impact --file`). Run
            the plain arm first.

The agent edits nothing. It answers with the files it would change, and the answer is scored
against the files the real commit changed. Turns, cost, tokens and wall time are the harness's
own result record; tool calls are counted from the transcript. Each run costs real API money:
`--budget` caps one run, and a finished run is never repeated (delete its `*.result.json` to
run it again).

  scripts/agent-ab.py --source . --work ../ab/work --out ../ab/out \\
      --ok-bin target/release/ok --plugin-dir . --arms plain brief --commits <sha> <sha>
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

READ_TOOLS = [
    "Read", "Grep", "Glob",
    "Bash(ls:*)", "Bash(rg:*)", "Bash(grep:*)", "Bash(cat:*)", "Bash(sed:*)",
    "Bash(head:*)", "Bash(wc:*)", "Bash(find:*)",
]
KIOKU_TOOLS = ["Bash(ok:*)", "mcp__plugin_open-kioku_open-kioku", "mcp__open-kioku", "Skill"]
NO_WRITE = ["Edit", "Write", "MultiEdit", "NotebookEdit", "Agent", "Task", "WebFetch", "WebSearch"]
SKIP_GOLD = ("CHANGELOG.md", "Cargo.lock")

PROMPT = """Task for this repository: {subject}

Do not edit any file. Work out which existing files have to change to carry out this task, the \
way you would before starting the work, and stop as soon as you are confident.

End your answer with a line that starts with `FILES:` followed by a JSON array of \
repository-relative paths, most certain first, at most 12 paths."""


def sh(args, cwd=None, check=True, env=None):
    out = subprocess.run(args, cwd=cwd, capture_output=True, text=True, env=env)
    if check and out.returncode != 0:
        raise RuntimeError(f"{args} failed: {out.stderr[-600:]}")
    return out.stdout


def gold_files(repo, sha):
    names = sh(["git", "-C", repo, "diff", "--name-only", "--diff-filter=AMR", f"{sha}^", sha]).split()
    return [n for n in names if n not in SKIP_GOLD]


def subject(repo, sha):
    text = sh(["git", "-C", repo, "log", "-1", "--format=%s", sha]).strip()
    return re.sub(r"\s*\(#\d+\)", "", text).strip()


def prepare(source, work, sha, ok_bin):
    """A clone at the commit's parent, indexed. Returns its path."""
    dest = work / sha[:10]
    if not dest.exists():
        sh(["git", "clone", "-q", "--no-hardlinks", source, str(dest)])
        git = ["git", "-C", str(dest)]
        sh(git + ["checkout", "-q", "--detach", f"{sha}^"])
        # The clone must not hold the answer: no remote, no branch or tag that reaches the
        # commit, no reflog entry, and the unreachable objects pruned. `git log --all` in the
        # checkout then ends at the parent.
        sh(git + ["remote", "remove", "origin"])
        refs = sh(git + ["for-each-ref", "--format=%(refname)"]).split()
        for ref in refs:
            sh(git + ["update-ref", "-d", ref])
        sh(git + ["reflog", "expire", "--expire=now", "--all"])
        sh(git + ["gc", "-q", "--prune=now"])
        held = subprocess.run(git + ["cat-file", "-e", f"{sha}^{{commit}}"], capture_output=True)
        if held.returncode == 0:
            raise RuntimeError(f"{dest} still holds {sha}: the checkout would leak the answer")
    if not (dest / ".ok" / "index.sqlite").exists():
        started = time.time()
        sh([ok_bin, "index", "."], cwd=str(dest))
        (dest / ".ok" / "index-seconds.txt").write_text(f"{time.time() - started:.1f}\n")
    return dest


BRIEF_HEAD = (
    "Open Kioku (this repository's local index) ranked these starting points for the task. "
    "They are candidates from lexical, graph and history evidence, not a verdict: confirm them "
    "in the code, and expect a file the index could not connect to the task to be missing.")


def brief(checkout, task, ok_bin, edit_max=8, affected_max=8):
    """What a prompt hook would hand the agent: the plan's ranked files, and nothing else."""
    started = time.time()
    out = subprocess.run([ok_bin, "preflight", task, "--format", "json"], cwd=str(checkout),
                         capture_output=True, text=True)
    seconds = time.time() - started
    report = json.loads(out.stdout)

    def line(entry):
        span = entry.get("line_range") or {}
        where = f":{span['start']}-{span['end']}" if span else ""
        return f"- {entry['path']}{where}"

    lines = [BRIEF_HEAD, "Edit candidates, most relevant first:"]
    lines += [line(e) for e in report["edit_files"][:edit_max]]
    affected = report["likely_affected_files"][:affected_max]
    if affected:
        lines.append("Linked by impact analysis or changed together with them before:")
        lines += [line(e) for e in affected]
    lines.append(f"Plan confidence: {report['confidence']} ({report['confidence_score']:.2f}).")
    return "\n".join(lines), seconds


DEPENDENTS_HEAD = (
    "Open Kioku (this repository's local code graph) lists, for the files this change most "
    "likely starts in, the files with a proven call, import or type use into them. A change to "
    "an item there may need a matching change in these dependents; check the ones that use "
    "what you would change.")


def dependents(checkout, files, ok_bin, per_file=10):
    """What a post-edit hook would hand the agent: who provably depends on the edited files."""
    started = time.time()
    lines = [DEPENDENTS_HEAD]
    for path in files:
        out = subprocess.run([ok_bin, "--json", "impact", "--file", path], cwd=str(checkout),
                             capture_output=True, text=True)
        try:
            report = json.loads(out.stdout)
        except json.JSONDecodeError:
            continue
        by_file = {}
        for edge in report.get("proven_impact") or []:
            if edge.get("path") == path:
                continue
            entry = by_file.setdefault(edge["path"], {"edges": 0, "uses": []})
            entry["edges"] += 1
            used = (edge.get("source") or "").rsplit("::", 1)[-1]
            if used and used not in entry["uses"]:
                entry["uses"].append(used)
        if not by_file:
            continue
        omitted = report.get("proven_impact_omitted") or 0
        lines.append(f"Depend on {path}" + (f" ({omitted} more proven edges not listed)" if omitted else "") + ":")
        ranked = sorted(by_file.items(), key=lambda kv: (-kv[1]["edges"], kv[0]))[:per_file]
        for dep, entry in ranked:
            uses = ", ".join(f"`{u}`" for u in entry["uses"][:3])
            lines.append(f"- {dep}" + (f" (uses {uses})" if uses else ""))
    return "\n".join(lines), time.time() - started


def run_arm(arm, checkout, prompt, out_dir, plugin_dir, ok_dir, max_turns, budget):
    env = dict(os.environ)
    env["RUST_BACKTRACE"] = "0"
    tools = list(READ_TOOLS)
    args = ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
            "--max-turns", str(max_turns),
            "--max-budget-usd", str(budget), "--permission-mode", "acceptEdits",
            "--strict-mcp-config", "--disallowedTools", *NO_WRITE]
    if arm == "kioku":
        env["PATH"] = f"{ok_dir}:{env['PATH']}"
        tools += KIOKU_TOOLS
        server = {"mcpServers": {"open-kioku": {
            "command": str(Path(ok_dir) / "ok"), "args": ["mcp", "serve", "--repo", "."]}}}
        args += ["--plugin-dir", plugin_dir, "--mcp-config", json.dumps(server)]
    args += ["--allowedTools", *tools]
    started = time.time()
    out = subprocess.run(args, cwd=str(checkout), capture_output=True, text=True, env=env,
                         stdin=subprocess.DEVNULL)
    elapsed = time.time() - started
    (out_dir / f"{arm}.transcript.jsonl").write_text(out.stdout)
    (out_dir / f"{arm}.stderr.txt").write_text(out.stderr)
    record, tools_used, tool_result_chars, mcp_servers = {}, {}, 0, None
    for line in out.stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") == "result":
            record = event
        elif event.get("type") == "system" and event.get("subtype") == "init":
            mcp_servers = [f"{m.get('name')}:{m.get('status')}" for m in event.get("mcp_servers") or []]
        elif event.get("type") in ("assistant", "user"):
            for block in (event.get("message") or {}).get("content") or []:
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "tool_use":
                    name = block.get("name") or "?"
                    if name == "Bash":
                        command = str((block.get("input") or {}).get("command") or "").split()
                        name = f"Bash:{command[0] if command else ''}"
                    tools_used[name] = tools_used.get(name, 0) + 1
                elif block.get("type") == "tool_result":
                    content = block.get("content")
                    if isinstance(content, list):
                        content = "".join(str(c.get("text") or "") for c in content if isinstance(c, dict))
                    tool_result_chars += len(str(content or ""))
    if not record:
        return {"arm": arm, "error": "no result record", "elapsed_s": round(elapsed, 1)}
    usage = record.get("usage") or {}
    return {
        "arm": arm,
        "tools": tools_used,
        "tool_result_chars": tool_result_chars,
        "mcp_servers": mcp_servers,
        "answer": record.get("result") or "",
        "is_error": record.get("is_error"),
        "subtype": record.get("subtype"),
        "turns": record.get("num_turns"),
        "cost_usd": record.get("total_cost_usd"),
        "duration_s": round((record.get("duration_ms") or 0) / 1000, 1),
        "elapsed_s": round(elapsed, 1),
        "input_tokens": usage.get("input_tokens"),
        "cache_read_tokens": usage.get("cache_read_input_tokens"),
        "cache_creation_tokens": usage.get("cache_creation_input_tokens"),
        "output_tokens": usage.get("output_tokens"),
        "models": sorted((record.get("modelUsage") or {}).keys()),
    }


def parse_files(answer):
    found = None
    for match in re.finditer(r"FILES:\s*(\[.*?\])", answer, flags=re.S):
        found = match.group(1)
    if not found:
        return None
    try:
        paths = json.loads(found)
    except json.JSONDecodeError:
        return None
    cleaned = []
    for path in paths:
        if not isinstance(path, str):
            continue
        path = path.strip()
        while path.startswith("./"):
            path = path[2:]
        cleaned.append(path)
    return cleaned


def score(answered, gold):
    if answered is None:
        return {"answered": None, "hits": 0, "recall": 0.0, "precision": 0.0, "first_hit_rank": None}
    gold_set = set(gold)
    # A file left out of the gold standard is left out of the answer too: naming the
    # changelog is neither a hit nor a miss.
    answered = [p for p in answered if p.rsplit("/", 1)[-1] not in SKIP_GOLD]
    hits = [p for p in answered if p in gold_set]
    first = next((i + 1 for i, p in enumerate(answered) if p in gold_set), None)
    return {
        "answered": answered,
        "hits": len(set(hits)),
        "recall": round(len(set(hits)) / len(gold_set), 3) if gold_set else 0.0,
        "precision": round(len(hits) / len(answered), 3) if answered else 0.0,
        "first_hit_rank": first,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", required=True, help="repository to take commits from")
    ap.add_argument("--work", required=True, help="directory for checkouts")
    ap.add_argument("--out", required=True, help="directory for results")
    ap.add_argument("--ok-bin", required=True)
    ap.add_argument("--plugin-dir", required=True)
    ap.add_argument("--commits", nargs="+", required=True)
    ap.add_argument("--arms", nargs="+", default=["plain", "kioku"])
    ap.add_argument("--max-turns", type=int, default=40)
    ap.add_argument("--budget", type=float, default=1.5)
    args = ap.parse_args()
    work, out = Path(args.work), Path(args.out)
    work.mkdir(parents=True, exist_ok=True)
    out.mkdir(parents=True, exist_ok=True)
    ok_dir = str(Path(args.ok_bin).resolve().parent)
    results = []
    for index, sha in enumerate(args.commits):
        gold = gold_files(args.source, sha)
        task = subject(args.source, sha)
        checkout = prepare(args.source, work, sha, args.ok_bin)
        gold_existing = [g for g in gold if (checkout / g).exists()]
        task_out = out / sha[:10]
        task_out.mkdir(exist_ok=True)
        prompt = PROMPT.format(subject=task)
        # Alternate which arm goes first, so neither always meets a warm or a cold service.
        # The impact arm is seeded from the plain arm's answer, so plain stays ahead of it.
        arms = args.arms if index % 2 == 0 else list(reversed(args.arms))
        if "impact" in arms and "plain" in arms and arms.index("impact") < arms.index("plain"):
            arms = ["plain"] + [arm for arm in arms if arm != "plain"]
        row = {"sha": sha[:10], "task": task, "gold": gold, "gold_existing": gold_existing, "arms": {}}
        for arm in arms:
            done = task_out / f"{arm}.result.json"
            if done.exists():
                row["arms"][arm] = json.loads(done.read_text())
                continue
            # The gate hooks write under .ok/gate; start each run without a session.
            subprocess.run(["rm", "-rf", str(checkout / ".ok" / "gate")])
            arm_prompt, extra = prompt, {}
            if arm == "brief":
                text, seconds = brief(checkout, task, args.ok_bin)
                arm_prompt = f"{prompt}\n\n{text}"
                extra = {"brief_chars": len(text), "brief_seconds": round(seconds, 1)}
                (task_out / "brief.txt").write_text(text + "\n")
            if arm == "impact":
                plain = json.loads((task_out / "plain.result.json").read_text())
                seeds = [p for p in (plain.get("answered") or []) if (checkout / p).is_file()
                         and not p.endswith(".md")][:2]
                text, seconds = dependents(checkout, seeds, args.ok_bin)
                arm_prompt = f"{prompt}\n\n{text}"
                extra = {"brief_chars": len(text), "brief_seconds": round(seconds, 1), "seeds": seeds}
                (task_out / "dependents.txt").write_text(text + "\n")
            record = run_arm(arm, checkout, arm_prompt, task_out, args.plugin_dir, ok_dir,
                             args.max_turns, args.budget)
            record.update(extra)
            record.update(score(parse_files(record.get("answer", "")), gold_existing))
            done.write_text(json.dumps(record, indent=2))
            row["arms"][arm] = record
            print(f"{sha[:10]} {arm:5} recall={record.get('recall')} prec={record.get('precision')} "
                  f"turns={record.get('turns')} cost={record.get('cost_usd')} "
                  f"t={record.get('duration_s')}s err={record.get('is_error')}", flush=True)
        results.append(row)
        (out / "results.json").write_text(json.dumps(results, indent=2))
    print("done")


if __name__ == "__main__":
    sys.exit(main())
