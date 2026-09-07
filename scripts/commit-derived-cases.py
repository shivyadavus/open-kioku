#!/usr/bin/env python3
"""Derive leakage-safe retrieval cases from a repository's own history.

Method (after Agent Retrieval Bench, arXiv 2607.24882): pick a base commit B and
index the repository *at B*. Every case is a later commit whose subject is the
query and whose modified source files are the gold set, kept only if all of them
already existed at B. The change itself lives in the future, never in the index,
so a query cannot retrieve its own diff.

    scripts/commit-derived-cases.py REPO --base B --after 3800 \
        --out cases.tsv [--min-files 1] [--max-files 5] [--ext .java]

Output is TSV: sha, author date, query, gold paths joined by '|'. Split it
chronologically (older = dev, newer = holdout) before tuning anything.
"""
import argparse
import re
import subprocess
import sys

PR_REF = re.compile(r"\(#\d+\)|#\d+")
PATHLIKE = re.compile(r"\S*/\S*")
BACKTICKS = re.compile(r"`([^`]*)`")


def git(repo, *args):
    return subprocess.run(
        ["git", "-C", repo, *args], capture_output=True, text=True, check=True
    ).stdout


def clean_query(subject):
    # Strip PR numbers and explicit paths: both would make the case trivially
    # solvable and neither is how a developer phrases a task.
    subject = PR_REF.sub("", subject)
    subject = BACKTICKS.sub(r"\1", subject)
    subject = PATHLIKE.sub("", subject)
    return " ".join(subject.split()).strip(" .:-")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("repo")
    ap.add_argument("--base", required=True, help="base commit; the repository is indexed here")
    ap.add_argument("--after", type=int, default=3800, help="how many commits after base to consider; pick a count that exists on every machine that derives the corpus, the window is anchored at base")
    ap.add_argument("--min-files", type=int, default=1)
    ap.add_argument("--max-files", type=int, default=5)
    ap.add_argument("--ext", action="append", default=None, help="source extensions to keep (repeatable); default .java")
    ap.add_argument("--min-query-words", type=int, default=3)
    ap.add_argument(
        "--path-prefix", action="append", default=None,
        help="only count files under these prefixes as gold (repeatable); use it when only a subtree of the repository is indexed",
    )
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    exts = tuple(args.ext or [".java"])
    prefixes = tuple(args.path_prefix or [""])

    at_base = set(git(args.repo, "ls-tree", "-r", "--name-only", args.base).splitlines())
    # The first `after` commits after base, oldest first. Anchored at base on purpose:
    # `--max-count` keeps the *newest* N commits before `--reverse` reorders them, so a
    # HEAD-anchored window would yield a different case set every time upstream moves.
    log = git(
        args.repo, "log", f"{args.base}..HEAD", "--reverse", "--first-parent",
        "--format=%x00%H%x1f%as%x1f%s", "--name-only", "--diff-filter=M",
    )
    kept = 0
    with open(args.out, "w") as out:
        for block in log.split("\x00")[1 : args.after + 1]:
            header, _, files = block.partition("\n")
            sha, date, subject = header.split("\x1f")
            paths = [p for p in files.split("\n") if p and p.endswith(exts)]
            if any(not p.startswith(prefixes) for p in paths):
                continue  # a commit that also touches unindexed files is not fully answerable
            if not (args.min_files <= len(paths) <= args.max_files):
                continue
            if any(p not in at_base for p in paths):
                continue  # a gold file that does not exist at B is unanswerable by construction
            query = clean_query(subject)
            if len(query.split()) < args.min_query_words:
                continue
            out.write(f"{sha}\t{date}\t{query}\t{'|'.join(paths)}\n")
            kept += 1
    print(f"{kept} cases written to {args.out} (base {args.base[:12]}, {args.after} commits scanned)", file=sys.stderr)


if __name__ == "__main__":
    main()
