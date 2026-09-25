#!/usr/bin/env python3
"""Reject promotional material and internal references from the tracked tree.

This repository is public and maintainer-led. Promotional drafts, channel post
copy and competitor positioning are working material, not product; once
committed they persist in the history of every tag and release branch that
follows, and a later cleanup cannot fully retract them, because pull request
refs keep the original objects even after a history rewrite. Keeping them out
is much cheaper than removing them.

The check is deliberately narrow so it can stay on by default:

  * Path rules reject directories and files that exist only to hold that
    material.
  * Phrase rules match channel names and positioning idioms that have no
    legitimate use in product documentation. Ordinary words are NOT matched --
    "start the daemon" and "star the repository" are fine.

It fails CLOSED: if it cannot read something it was asked to check, it reports
an error and exits non-zero rather than passing silently.

Usage:
    scripts/check-public-surface.py                 # scan the tracked tree
    scripts/check-public-surface.py --commit-msg F  # scan a commit message
"""
from __future__ import annotations

import os
import re
import subprocess
import sys

# Paths that exist only to hold promotional material. Anchored, not substring:
# docs/launcher.md and docs/relaunch.md are legitimate names and must pass.
FORBIDDEN_PATHS = [
    re.compile(r"^docs/launch([/.\-]|$)", re.I),
    re.compile(r"^docs/marketing([/.\-]|$)", re.I),
    re.compile(r"^docs/gtm([/.\-]|$)", re.I),
]

# High-precision phrases. Each must be something that does not occur in
# legitimate product prose. Keep this list short; a noisy gate gets disabled.
FORBIDDEN_PHRASES = [
    (re.compile(r"\bShow HN\b", re.I), "Show HN post draft"),
    (re.compile(r"\bProduct Hunt\b", re.I), "Product Hunt launch copy"),
    (re.compile(r"\bstar[- ]conversion\b", re.I), "growth/conversion framing"),
    (re.compile(r"\bgo[- ]to[- ]market\b", re.I), "go-to-market strategy"),
    (re.compile(r"\blaunch (?:kit|copy|thread|announcement)\b", re.I),
     "launch material"),
    (re.compile(r"\bwaitlist\b", re.I), "growth funnel copy"),
    # Competitor positioning. Naming a competitor in a factual comparison is
    # fine; pairing it with a superiority claim is not. "cursor" is excluded
    # from the case-insensitive list because it is a first-class technical noun
    # here (SQLite cursors, TreeCursor, pagination cursors) -- it gets a
    # stricter, case-sensitive rule that will not fire on cursor-based or
    # cursor.execute().
    (re.compile(r"\b(?:unlike|better than|faster than|beats)\s+"
                r"(?:sourcegraph|cody|codeium|tabnine|aider|devin|greptile)\b", re.I),
     "competitor disparagement"),
    (re.compile(r"\b(?:Unlike|better than|faster than|beats)\s+Cursor\b(?![-.\w])"),
     "competitor disparagement"),
]

# Internal references that must never reach a public tree. `/Users/` is matched
# case-sensitively: macOS spells it with a capital U, while the lowercase
# /users/ appears in legitimate GitHub URLs (api.github.com/users/octocat).
FORBIDDEN_INTERNAL = [
    (re.compile(r"claude\.ai/code/session"), "assistant session link"),
    (re.compile(r"(?<!\w)/Users/[A-Za-z0-9_.-]+/"), "local home-directory path"),
]

# Exempt from PHRASE rules only. The changelog may legitimately record that a
# promotional template was removed. It is NOT exempt from the internal rules:
# .github/workflows/release.yml slices it into published release notes.
PHRASE_EXEMPT = {"scripts/check-public-surface.py", "CHANGELOG.md"}

TEXT_SUFFIXES = (
    ".md", ".markdown", ".mdc", ".txt", ".rs", ".py", ".sh", ".rb", ".go",
    ".java", ".toml", ".json", ".yml", ".yaml", ".html", ".css", ".js", ".ts",
    ".svg", ".xml", ".tf", ".tape",
)

# Extensionless published surfaces worth scanning by name.
TEXT_BASENAMES = {"Dockerfile", "LICENSE", "NOTICE", "CNAME", "Makefile"}

SCISSORS = re.compile(r"^.{0,4}-{8,} ?>8 ?-{8,}")


def repo_root() -> str:
    return subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()


def comment_char() -> str:
    result = subprocess.run(
        ["git", "config", "core.commentChar"], capture_output=True, text=True
    )
    value = result.stdout.strip()
    if not value or value == "auto":
        return "#"
    return value[0]


def is_text_candidate(path: str) -> bool:
    name = path.rsplit("/", 1)[-1]
    if name in TEXT_BASENAMES:
        return True
    if "." not in name:
        return True
    return path.lower().endswith(TEXT_SUFFIXES)


def scan_line(line: str, phrases_apply: bool) -> list[str]:
    rules = (FORBIDDEN_PHRASES + FORBIDDEN_INTERNAL) if phrases_apply else FORBIDDEN_INTERNAL
    return [reason for pattern, reason in rules if pattern.search(line)]


def check_commit_message(path: str) -> int:
    """Check a commit message. Used by the commit-msg hook.

    A commit message is not a tracked file, so the tree scan never sees it, yet
    it is published just as permanently and cannot be corrected after merge
    without rewriting history.

    Everything from git's scissors marker onward is ignored: `git commit -v`
    writes the diff there uncommented, and the diff is not the author's prose.
    """
    if not path:
        print("check-public-surface: --commit-msg needs a file path", file=sys.stderr)
        return 1
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            lines = handle.readlines()
    except OSError as error:
        print(f"check-public-surface: cannot read {path}: {error}", file=sys.stderr)
        return 1

    char = comment_char()
    failures = []
    for number, line in enumerate(lines, start=1):
        if SCISSORS.search(line):
            break
        if line.startswith(char):
            continue
        for reason in scan_line(line, phrases_apply=True):
            failures.append(f"  line {number}: {reason} -- {line.strip()[:90]}")

    if failures:
        print("Commit message rejected by the public-surface check:\n", file=sys.stderr)
        print("\n".join(failures), file=sys.stderr)
        print(
            "\nCommit messages are public and permanent. Remove the offending line\n"
            "before committing, or amend it out before pushing.",
            file=sys.stderr,
        )
        return 1
    return 0


def main(argv: list[str]) -> int:
    if len(argv) > 1 and argv[1] == "--commit-msg":
        return check_commit_message(argv[2] if len(argv) > 2 else "")
    if len(argv) > 1:
        print(f"check-public-surface: unknown argument {argv[1]!r}", file=sys.stderr)
        return 1

    # git ls-files reports paths relative to cwd; without this the scan silently
    # narrows when run from a subdirectory and no path rule can ever fire.
    os.chdir(repo_root())

    paths = [
        line for line in subprocess.run(
            ["git", "ls-files"], capture_output=True, text=True, check=True
        ).stdout.splitlines() if line
    ]

    failures: list[str] = []
    unreadable: list[str] = []

    for path in paths:
        for rule in FORBIDDEN_PATHS:
            if rule.search(path):
                failures.append(f"{path}: path reserved for promotional material")
                break

        if not is_text_candidate(path):
            continue

        try:
            with open(path, encoding="utf-8", errors="replace") as handle:
                lines = handle.readlines()
        except OSError as error:
            unreadable.append(f"{path}: {error}")
            continue

        phrases_apply = path not in PHRASE_EXEMPT
        for number, line in enumerate(lines, start=1):
            for reason in scan_line(line, phrases_apply):
                failures.append(f"{path}:{number}: {reason} -- {line.strip()[:90]}")

    if unreadable:
        print("Public-surface check could not read these tracked files:\n", file=sys.stderr)
        for item in unreadable:
            print(f"  {item}", file=sys.stderr)
        print("\nRefusing to report a pass on an incomplete scan.", file=sys.stderr)
        return 1

    if failures:
        print("Public-surface check FAILED:\n", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        print(
            "\nThis tree is public. Promotional copy, channel post drafts, competitor\n"
            "positioning and internal references must not be committed -- once they\n"
            "land they persist in every tag and release branch cut afterwards.\n"
            "Keep working material outside the repository.",
            file=sys.stderr,
        )
        return 1

    print(f"Public-surface check passed ({len(paths)} tracked files).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
