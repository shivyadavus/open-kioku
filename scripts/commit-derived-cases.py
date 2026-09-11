#!/usr/bin/env python3
"""Derive leakage-safe retrieval cases from a repository's own history.

Method (after Agent Retrieval Bench, arXiv 2607.24882): pick a base commit B and
index the repository *at B*. Every case is a later commit whose subject is the
query and whose modified source files are the gold set, kept only if all of them
already existed at B. The change itself lives in the future, never in the index,
so a query cannot retrieve its own diff.

    scripts/commit-derived-cases.py REPO --base B --after 3800 \
        --out cases.tsv [--min-files 1] [--max-files 5] [--ext .java]

Output is TSV: sha, author date, query, gold paths joined by '|', and the modified
line ranges per gold file ("26-33,36-44|1-1", one field per gold path in the same order,
each "start-end" inclusive, from `git diff -U0 <parent> <sha>`; a pure insertion records
its anchor line). Ranges are numbered on the commit's *parent*, the nearest thing to the
indexed base B that the change is expressed against. Split the file chronologically
(older = dev, newer = holdout) before tuning anything.

    scripts/commit-derived-cases.py REPO --annotate cases.tsv --out cases-with-ranges.tsv

re-derives the fifth column for an existing four-column file without changing its cases.
"""
import argparse
import re
import subprocess
import sys

PR_REF = re.compile(r"\(#\d+\)|#\d+")
HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+\d+(?:,\d+)? @@")
NUMBERS = re.compile(r"\d+")
PATHLIKE = re.compile(r"\S*/\S*")
BACKTICKS = re.compile(r"`([^`]*)`")


def git(repo, *args):
    return subprocess.run(
        ["git", "-C", repo, *args], capture_output=True, text=True, check=True
    ).stdout


def parse_hunk_ranges(diff_text):
    """Base-side line ranges per path from a `git diff -U0 --src-prefix=a/` text.

    Returns {path: [(start, end), ...]} with inclusive one-based ranges. A hunk that only
    inserts (`-N,0`) has no base-side lines; it is recorded as the single anchor line N
    (line 1 for an insertion at the top) so the region an editor has to look at still
    counts, without inflating the line total.
    """
    ranges = {}
    current = None
    for line in diff_text.splitlines():
        if line.startswith("--- a/"):
            current = line[len("--- a/"):]
            ranges.setdefault(current, [])
        elif line.startswith("@@") and current is not None:
            hunk = HUNK.match(line)
            if not hunk:
                continue
            start = int(hunk.group(1))
            count = 1 if hunk.group(2) is None else int(hunk.group(2))
            if count == 0:
                start = max(start, 1)
                ranges[current].append((start, start))
            else:
                ranges[current].append((start, start + count - 1))
    return ranges


def changed_ranges(repo, sha, paths):
    """Modified base-side line ranges of `sha` for each of `paths`, in that order."""
    diff = git(
        repo, "diff", "-U0", "--no-color", "--src-prefix=a/", "--dst-prefix=b/",
        f"{sha}^", sha, "--", *paths,
    )
    found = parse_hunk_ranges(diff)
    return [found.get(path, []) for path in paths]


def format_ranges(per_path):
    return "|".join(",".join(f"{a}-{b}" for a, b in ranges) for ranges in per_path)


def clean_query(subject):
    # Strip PR numbers. A subject that names a path is dropped by the caller: kept, the path
    # is the answer; stripped, what remains ("chore: fix regex in") is unanswerable.
    subject = PR_REF.sub("", subject)
    subject = BACKTICKS.sub(r"\1", subject)
    return " ".join(subject.split()).strip(" .:-")


def annotate(repo, cases_path, out_path):
    """Add (or refresh) the modified-line-range column on an existing cases TSV."""
    written = 0
    with open(cases_path) as src, open(out_path, "w") as out:
        for line in src:
            fields = line.rstrip("\n").split("\t")
            if len(fields) < 4:
                continue
            sha, date, query, gold = fields[:4]
            paths = gold.split("|")
            ranges = format_ranges(changed_ranges(repo, sha, paths))
            out.write(f"{sha}\t{date}\t{query}\t{gold}\t{ranges}\n")
            written += 1
    print(f"{written} cases annotated with modified line ranges -> {out_path}", file=sys.stderr)
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("repo")
    ap.add_argument("--base", help="base commit; the repository is indexed here (required unless --annotate)")
    ap.add_argument("--after", type=int, default=3800, help="how many commits after base to consider; pick a count that exists on every machine that derives the corpus, the window is anchored at base")
    ap.add_argument("--min-files", type=int, default=1)
    ap.add_argument("--max-files", type=int, default=5)
    ap.add_argument("--ext", action="append", default=None, help="source extensions to keep (repeatable); default .java")
    ap.add_argument("--min-query-words", type=int, default=3)
    ap.add_argument(
        "--keep-path-subjects", action="store_true",
        help="keep commits whose subject contains a path-like token (default: drop them; the path is either the answer or, once stripped, the subject no longer describes the change)",
    )
    ap.add_argument(
        "--keep-repeated-subjects", action="store_true",
        help="keep every commit whose subject repeats an earlier one up to numbers (default: keep the first only; on one Go corpus a 'release: bump module versions for the X cut' subject was a third of the holdout and every instance had the same gold file, so one pattern decided the corpus)",
    )
    ap.add_argument(
        "--path-prefix", action="append", default=None,
        help="only count files under these prefixes as gold (repeatable); use it when only a subtree of the repository is indexed",
    )
    ap.add_argument(
        "--annotate", metavar="CASES_TSV",
        help="re-derive the modified-line-range column for an existing cases file instead of deriving cases; every other selection flag is ignored",
    )
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    if args.annotate:
        return annotate(args.repo, args.annotate, args.out)
    if not args.base:
        ap.error("--base is required unless --annotate is given")
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
    dropped_repeats = 0
    seen_subjects = set()
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
            if PATHLIKE.search(subject) and not args.keep_path_subjects:
                continue
            query = clean_query(subject)
            if len(query.split()) < args.min_query_words:
                continue
            if not args.keep_repeated_subjects:
                key = " ".join(NUMBERS.sub("#", query).lower().split())
                if key in seen_subjects:
                    dropped_repeats += 1
                    continue
                seen_subjects.add(key)
            ranges = format_ranges(changed_ranges(args.repo, sha, paths))
            out.write(f"{sha}\t{date}\t{query}\t{'|'.join(paths)}\t{ranges}\n")
            kept += 1
    print(f"{kept} cases written to {args.out} (base {args.base[:12]}, {args.after} commits scanned, {dropped_repeats} repeated subjects dropped)", file=sys.stderr)


if __name__ == "__main__":
    sys.exit(main())
