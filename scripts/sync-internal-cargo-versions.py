#!/usr/bin/env python3
"""Sync sibling Open Kioku Cargo path dependencies to the workspace version."""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def workspace_version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'\[workspace\.package\][\s\S]*?^version = "([^"]+)"$', text, re.MULTILINE)
    if not match:
        raise SystemExit("could not read workspace package version")
    return match.group(1)


def main() -> int:
    version = sys.argv[1] if len(sys.argv) > 1 else workspace_version()
    dependency = re.compile(r'^\s*open-kioku-[A-Za-z0-9_-]+\s*=\s*\{')
    sibling_path = re.compile(r'path\s*=\s*"\.\./open-kioku-[^"]+"')
    version_field = re.compile(r'version\s*=\s*"[^"]+"')

    changed_files = 0
    for cargo_toml in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        lines = cargo_toml.read_text(encoding="utf-8").splitlines(keepends=True)
        output: list[str] = []
        changed = False
        for line in lines:
            if dependency.match(line) and sibling_path.search(line):
                if version_field.search(line):
                    updated = version_field.sub(f'version = "{version}"', line, count=1)
                else:
                    updated = line.replace("{", f'{{ version = "{version}",', 1)
                changed |= updated != line
                line = updated
            output.append(line)
        if changed:
            cargo_toml.write_text("".join(output), encoding="utf-8")
            changed_files += 1
            print(f"  ✓ {cargo_toml.relative_to(ROOT)} internal dependencies")

    print(f"Internal Cargo dependency versions synchronized to {version} ({changed_files} file(s) changed).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
