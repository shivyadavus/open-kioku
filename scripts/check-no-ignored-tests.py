#!/usr/bin/env python3
"""Fail CI when Rust tests are ignored without an explicit allowlist entry."""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ALLOWLIST = ROOT / "ci/ignored-tests-allowlist.txt"
RUST_GLOBS = ("crates/**/*.rs", "benches/**/*.rs", "tests/**/*.rs")


def load_allowlist() -> set[str]:
    if not ALLOWLIST.exists():
        return set()
    entries: set[str] = set()
    for raw in ALLOWLIST.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        entries.add(line.split("--", 1)[0].strip())
    return entries


def test_name(lines: list[str], start: int) -> str:
    for line in lines[start + 1 : start + 12]:
        match = re.search(r"\bfn\s+([A-Za-z0-9_]+)\s*\(", line)
        if match:
            return match.group(1)
    return "<unknown>"


def ignored_tests() -> list[str]:
    findings: list[str] = []
    for pattern in RUST_GLOBS:
        for path in sorted(ROOT.glob(pattern)):
            if "target" in path.parts:
                continue
            lines = path.read_text(encoding="utf-8").splitlines()
            for index, line in enumerate(lines):
                if "#[ignore" not in line:
                    continue
                rel = path.relative_to(ROOT).as_posix()
                findings.append(f"{rel}:{index + 1}:{test_name(lines, index)}")
    return findings


def main() -> int:
    allowlist = load_allowlist()
    findings = ignored_tests()
    unexpected = [finding for finding in findings if finding not in allowlist]
    stale = [entry for entry in allowlist if entry not in findings]

    if unexpected or stale:
        print("Ignored-test policy failed:", file=sys.stderr)
        for finding in unexpected:
            print(f"  - unallowlisted ignored test: {finding}", file=sys.stderr)
        for entry in stale:
            print(f"  - stale ignored-test allowlist entry: {entry}", file=sys.stderr)
        print(
            f"Add intentional ignores to {ALLOWLIST.relative_to(ROOT)} with a reason.",
            file=sys.stderr,
        )
        return 1

    print(f"Ignored-test policy passed ({len(findings)} ignored tests allowlisted).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
