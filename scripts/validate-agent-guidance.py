#!/usr/bin/env python3
"""Hold every shipped agent-guidance file to the tool surface the server answers.

Four documents tell an agent which Open Kioku tools to call, and one of them —
the rule `ok setup agent --apply` writes — lands in the user's own repository,
where a retired name fails on first use with nothing to point at the fix. The
retired-tool table in `open-kioku-mcp` is the source of truth, so this cannot
drift from the code the way prose does.

The prose under `docs/` is held to the same bar: two documents described
retired tools as live for a release cycle after they were removed. The one
document that records the retirements themselves is exempt.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

MCP_SOURCE = Path("crates/open-kioku-mcp/src/lib.rs")

GUIDANCE_FILES = [
    Path("crates/open-kioku-cli/src/commands/onboarding.rs"),
    Path("skills/open-kioku/SKILL.md"),
    Path(".cursor-plugin/skills/open-kioku/SKILL.md"),
    Path(".cursor-plugin/skills/open-kioku.mdc"),
]

# `docs/mcp-tools.md` is the migration table: it names every retired tool on
# purpose, beside where its capability went.
DOCS_EXEMPT = {Path("docs/mcp-tools.md")}

# Retired tool names that are also live response-field names. `semantic_status`
# is the field `search_code` reports its fallback in, so the substring cannot
# tell a documented field from a retired tool; those names are skipped for
# `docs/` only, where the guidance files above never had the collision.
DOCS_FIELD_NAME_COLLISIONS = {"semantic_status"}


def retired_tool_names() -> set[str]:
    source = MCP_SOURCE.read_text(encoding="utf-8")
    marker = "const RETIRED_TOOLS: &[(&str, &str)] = &["
    start = source.index(marker) + len(marker)
    end = source.index("\n];", start)
    names = set(re.findall(r'"([a-z0-9_]+)",', source[start:end]))
    # The table pairs each name with guidance prose, which also quotes
    # replacement tool names; keep only the entries that open a tuple.
    names = {
        name
        for name in names
        if re.search(rf'\(\s*"{re.escape(name)}",', source[start:end])
    }
    if len(names) < 30:
        raise SystemExit(
            f"could not read RETIRED_TOOLS from {MCP_SOURCE} (found {len(names)})"
        )
    return names


def guidance_files() -> list[Path]:
    files = list(GUIDANCE_FILES)
    files.extend(sorted(Path(".cursor-plugin/rules").glob("*.mdc")))
    files.extend(sorted(Path(".cursor-plugin/agents").glob("*.md")))
    return files


def docs_files() -> list[Path]:
    return [
        path
        for path in sorted(Path("docs").rglob("*.md"))
        if path not in DOCS_EXEMPT
    ]


def main() -> int:
    retired = retired_tool_names()
    failures: list[str] = []

    for path in guidance_files():
        if not path.exists():
            failures.append(f"{path}: agent guidance file is missing")
            continue
        text = path.read_text(encoding="utf-8")
        for name in sorted(retired):
            # `onboarding.rs` names retired tools inside the regression
            # assertion that keeps them out of the installed rule. Only the
            # guidance strings themselves, which quote tools in backticks,
            # are the contract here.
            needle = f"`{name}`" if path.name == "onboarding.rs" else name
            if needle in text:
                failures.append(f"{path}: names the retired MCP tool `{name}`")

    docs = docs_files()
    for path in docs:
        text = path.read_text(encoding="utf-8")
        for name in sorted(retired - DOCS_FIELD_NAME_COLLISIONS):
            if name in text:
                failures.append(f"{path}: describes the retired MCP tool `{name}`")

    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1

    print(
        f"agent guidance validated: {len(guidance_files())} shipped files and "
        f"{len(docs)} docs carry no retired MCP tool name"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
