#!/usr/bin/env python3
"""Reject go-to-market material and internal references from the tracked tree.

This repository is public and maintainer-led. Launch copy, channel-specific post
drafts and competitor positioning are working material, not product; once
committed they stay in the history of every tag and release branch that follows,
and a later cleanup cannot fully retract them. Keeping them out is much cheaper
than removing them.

The check is deliberately narrow so it can stay on by default:

  * Path rules reject whole directories that exist only to hold launch material.
  * Phrase rules match channel names and positioning idioms that have no
    legitimate use in product documentation. Ordinary words are NOT matched --
    "launch the daemon" and "star the repository" are fine.

Run directly, or via CI. Exits non-zero and names every offending file:line.
"""
from __future__ import annotations

import re
import subprocess
import sys

# Directories and filenames whose only purpose is go-to-market material.
FORBIDDEN_PATHS = [
    re.compile(r"^docs/launch(/|-|$)"),
    re.compile(r"^docs/marketing(/|$)"),
    re.compile(r"^docs/gtm(/|$)"),
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
    # Competitor positioning: naming a competitor is fine in a factual
    # comparison of protocols; pairing it with a superiority claim is not.
    (re.compile(r"\b(?:unlike|better than|faster than|beats)\s+"
                r"(?:sourcegraph|cody|codeium|tabnine|aider|devin|greptile|cursor)\b", re.I),
     "competitor disparagement"),
]

# Internal references that should never reach a public tree.
FORBIDDEN_INTERNAL = [
    (re.compile(r"claude\.ai/code/session"), "assistant session link"),
    (re.compile(r"/Users/[a-z0-9_.-]+/", re.I), "local home-directory path"),
]

# Files exempt from phrase scanning: this script states the phrases it bans, and
# the changelog may legitimately record that a launch template was removed.
EXEMPT = {"scripts/check-public-surface.py", "CHANGELOG.md"}

TEXT_SUFFIXES = (
    ".md", ".markdown", ".txt", ".rs", ".py", ".sh", ".toml", ".json",
    ".yml", ".yaml", ".html", ".js", ".ts", ".tape",
)


def tracked_files() -> list[str]:
    out = subprocess.run(
        ["git", "ls-files"], capture_output=True, text=True, check=True
    ).stdout
    return [line for line in out.splitlines() if line]


def check_commit_message(path: str) -> int:
    """Check a commit message file. Used by the commit-msg hook.

    A commit message is not a tracked file, so the tree scan never sees it, yet
    it is published just as permanently and cannot be corrected without
    rewriting history.
    """
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            lines = handle.readlines()
    except OSError:
        return 0

    failures = []
    for number, line in enumerate(lines, start=1):
        if line.startswith("#"):
            continue
        for pattern, reason in FORBIDDEN_PHRASES + FORBIDDEN_INTERNAL:
            if pattern.search(line):
                failures.append(f"  commit message line {number}: {reason} -- {line.strip()[:90]}")

    if failures:
        print("Commit message rejected by the public-surface check:\n", file=sys.stderr)
        print("\n".join(failures), file=sys.stderr)
        print(
            "\nCommit messages are public and permanent. Remove the offending line,\n"
            "or amend it out before pushing.",
            file=sys.stderr,
        )
        return 1
    return 0


def main() -> int:
    if len(sys.argv) > 2 and sys.argv[1] == "--commit-msg":
        return check_commit_message(sys.argv[2])

    failures: list[str] = []

    for path in tracked_files():
        for rule in FORBIDDEN_PATHS:
            if rule.search(path):
                failures.append(f"{path}: path reserved for go-to-market material")
                break

        if path in EXEMPT or not path.endswith(TEXT_SUFFIXES):
            continue

        try:
            with open(path, encoding="utf-8", errors="replace") as handle:
                lines = handle.readlines()
        except OSError:
            continue

        for number, line in enumerate(lines, start=1):
            for pattern, reason in FORBIDDEN_PHRASES + FORBIDDEN_INTERNAL:
                if pattern.search(line):
                    failures.append(
                        f"{path}:{number}: {reason} -- {line.strip()[:90]}"
                    )

    if failures:
        print("Public-surface check FAILED:\n", file=sys.stderr)
        for failure in failures:
            print(f"  {failure}", file=sys.stderr)
        print(
            "\nThis tree is public. Launch copy, channel post drafts, competitor\n"
            "positioning and internal references must not be committed -- once they\n"
            "land they persist in every tag and release branch cut afterwards.\n"
            "Keep working material outside the repository.",
            file=sys.stderr,
        )
        return 1

    print(f"Public-surface check passed ({len(tracked_files())} tracked files).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
