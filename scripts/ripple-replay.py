#!/usr/bin/env python3
"""Replay `ok ripple` over a repository's merged history.

For each commit, two questions:

  half   Apply only the file that declares a changed function (the commit's version of that one
         file on top of the parent). `ok ripple` lists the call sites that half-change leaves
         untouched. How many of them did the real commit go on to change? Those are sites the
         author had to update, named before the author updated them.

  whole  Apply the whole commit. Whatever `ok ripple` still lists is a site the merged commit
         left as it was: a false alarm, or a call the commit really did leave behind.

No index is built and no project code is run. The clone is checked out at many revisions, so
point this at a scratch clone, never at a working copy.
"""
import argparse
import json
import subprocess
import sys
from pathlib import Path

CODE = (".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".go", ".java", ".cs")


def run(args, cwd, check=True):
    out = subprocess.run(args, cwd=cwd, capture_output=True, text=True, errors="replace")
    if check and out.returncode != 0:
        raise RuntimeError(f"{' '.join(args[:6])} failed: {out.stderr[-400:]}")
    return out.stdout


def ripple(ok, repo, base, files=()):
    args = [ok, "ripple", "--no-index", "--json", "--base", base]
    for path in files:
        args += ["--file", path]
    out = subprocess.run(args, cwd=repo, capture_output=True, text=True, errors="replace")
    if out.returncode != 0:
        return None
    return json.loads(out.stdout)


def old_hunks(repo, parent, commit, path):
    """Line ranges of `path` at `parent` that `commit` changed or deleted lines next to."""
    text = run(["git", "diff", "-U0", "--no-color", parent, commit, "--", path], repo, check=False)
    ranges = []
    for line in text.splitlines():
        if not line.startswith("@@"):
            continue
        old = line.split(" ")[1]
        start, _, count = old[1:].partition(",")
        start, count = int(start), int(count or "1")
        ranges.append((start, start + max(count, 1) - 1))
    return ranges


def call_end(repo, parent, path, line, cache):
    """Last line of the call that starts on `line` of `path` at `parent`."""
    if path not in cache:
        cache[path] = run(["git", "show", f"{parent}:{path}"], repo, check=False).splitlines()
    lines, depth, opened = cache[path], 0, False
    for index in range(line - 1, min(line + 40, len(lines))):
        for ch in lines[index]:
            if ch == "(":
                depth, opened = depth + 1, True
            elif ch == ")":
                depth -= 1
        if opened and depth <= 0:
            return index + 1
        if not opened:
            return line
    return line


def breaking(report):
    return [c for c in report["contracts"] if c["effect"] == "breaking"]


def key(contract):
    return (contract["path"], contract.get("owner"), contract["name"], contract["change"])


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ap.add_argument("--repo", required=True, help="a scratch clone; it is checked out repeatedly")
    ap.add_argument("--ok-bin", required=True)
    ap.add_argument("--commits", type=int, default=300, help="newest non-merge commits to replay")
    ap.add_argument("--rev", default="HEAD")
    ap.add_argument("--out", required=True)
    ap.add_argument("--label", default=None)
    args = ap.parse_args()
    repo, ok = str(Path(args.repo).resolve()), str(Path(args.ok_bin).resolve())
    head = run(["git", "rev-parse", args.rev], repo).strip()
    shas = run(["git", "log", "--no-merges", f"-{args.commits}", "--format=%H", head], repo).split()

    rows = []
    totals = {
        "commits": 0, "commits_changing_code": 0, "commits_with_breaking_change": 0,
        "breaking_changes": 0, "in_a_file_the_commit_moved": 0,
        "half_sites_listed_as_possible_only": 0, "moved_or_shared_name": 0, "half_changes_with_a_listed_site": 0,
        "half_sites_listed": 0, "half_sites_the_commit_then_changed": 0,
        "half_changes_where_a_listed_site_was_then_changed": 0,
        "whole_changes_with_a_site_left": 0, "whole_sites_left": 0,
        "commits_with_a_site_left": 0,
    }
    for index, sha in enumerate(shas):
        parents = run(["git", "rev-list", "--parents", "-n", "1", sha], repo).split()[1:]
        if len(parents) != 1:
            continue
        parent = parents[0]
        totals["commits"] += 1
        status = run(["git", "diff", "--name-status", "--no-renames", parent, sha], repo)
        changed = [line.split("\t") for line in status.splitlines()]
        code = [path for kind, path in changed if kind in ("M", "D") and path.endswith(CODE)]
        if not code:
            continue
        totals["commits_changing_code"] += 1
        run(["git", "checkout", "-q", "-f", "--detach", sha], repo)
        run(["git", "clean", "-q", "-fd"], repo)
        whole = ripple(ok, repo, parent)
        if whole is None:
            continue
        whole_breaking = breaking(whole)
        if not whole_breaking:
            continue
        totals["commits_with_breaking_change"] += 1
        row = {"commit": sha[:10], "subject": run(["git", "log", "-1", "--format=%s", sha], repo).strip()[:90],
               "changes": []}
        left_in_commit = 0
        by_file = {}
        for contract in whole_breaking:
            by_file.setdefault(contract["path"], []).append(contract)
        for path, contracts in by_file.items():
            if subprocess.run(["git", "cat-file", "-e", f"{parent}:{path}"], cwd=repo,
                              capture_output=True).returncode != 0:
                # The commit moved the file here: there is no parent version to change alone.
                totals["breaking_changes"] += len(contracts)
                totals["in_a_file_the_commit_moved"] += len(contracts)
                continue
            # The half change: the parent, with this one file as the commit left it.
            run(["git", "checkout", "-q", "-f", "--detach", parent], repo)
            run(["git", "clean", "-q", "-fd"], repo)
            target = Path(repo) / path
            if (kind := next((k for k, p in changed if p == path), "M")) == "D":
                target.unlink(missing_ok=True)
            else:
                target.write_text(run(["git", "show", f"{sha}:{path}"], repo), errors="replace")
            half = ripple(ok, repo, parent, [path])
            half_by_key = {key(c): c for c in breaking(half)} if half else {}
            hunks, texts = {}, {}
            for contract in contracts:
                totals["breaking_changes"] += 1
                if "another declaration is also named" in (contract.get("note") or ""):
                    # Moved, or one of several of the name: applying one file of the commit
                    # makes a move look like a removal, so the half question does not apply.
                    totals["moved_or_shared_name"] += 1
                    continue
                listed = half_by_key.get(key(contract), {})
                sites = listed.get("broken", [])
                confirmed = []
                for site in sites:
                    if site["path"] == path:
                        continue
                    if site["path"] not in hunks:
                        hunks[site["path"]] = old_hunks(repo, parent, sha, site["path"])
                    end = call_end(repo, parent, site["path"], site["line"], texts)
                    if any(a <= end and b >= site["line"] for a, b in hunks[site["path"]]):
                        confirmed.append(site)
                other = [s for s in sites if s["path"] != path]
                totals["half_sites_listed_as_possible_only"] += (
                    len(listed.get("possible", [])) + listed.get("possible_omitted", 0))
                left = len(contract["broken"]) + contract.get("broken_omitted", 0)
                totals["half_sites_listed"] += len(other)
                totals["half_sites_the_commit_then_changed"] += len(confirmed)
                totals["half_changes_with_a_listed_site"] += bool(other)
                totals["half_changes_where_a_listed_site_was_then_changed"] += bool(confirmed)
                totals["whole_changes_with_a_site_left"] += bool(left)
                totals["whole_sites_left"] += left
                left_in_commit += left
                row["changes"].append({
                    "path": path, "name": contract["name"], "owner": contract.get("owner"),
                    "change": contract["change"], "renamed_to": contract.get("renamed_to"),
                    "before": contract.get("before"), "after": contract.get("after"),
                    "half_sites_in_other_files": len(other),
                    "half_sites_the_commit_then_changed": len(confirmed),
                    "whole_sites_left": left,
                    "whole_left_examples": [f"{s['path']}:{s['line']}  {s['text']}  <- {s['problem']}" for s in contract["broken"][:3]],
                    "half_examples": [f"{s['path']}:{s['line']}  {s['text']}  <- {s['problem']}" for s in other[:3]],
                    "note": contract.get("note"),
                })
        totals["commits_with_a_site_left"] += bool(left_in_commit)
        rows.append(row)
        if index % 25 == 0:
            print(f"{index}/{len(shas)} {sha[:10]} {totals}", flush=True)
    run(["git", "checkout", "-q", "-f", "--detach", head], repo)
    result = {"repository": args.label or Path(repo).name, "head": head[:10],
              "commits_requested": args.commits, "totals": totals, "commits": rows}
    Path(args.out).write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(totals, indent=2))


if __name__ == "__main__":
    sys.exit(main())
