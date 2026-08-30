#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

crate_count="$(find crates -mindepth 2 -maxdepth 2 -name Cargo.toml | wc -l | tr -d ' ')"
workflow_case_count="$(python3 - <<'PY'
import json
with open("benchmarks/workflow-cases.json", "r", encoding="utf-8") as handle:
    print(len(json.load(handle)))
PY
)"

if ! grep -Eq "This is a ${crate_count}-crate Cargo workspace" README.md; then
  echo "README.md crate count is stale; expected ${crate_count}" >&2
  exit 1
fi



if ! grep -Eq "contains ${workflow_case_count}[[:space:]]+cases" docs/workflow-benchmarks.md; then
  echo "docs/workflow-benchmarks.md workflow case count is stale; expected ${workflow_case_count}" >&2
  exit 1
fi

scripts/validate-public-quickstart.sh --static
scripts/validate-public-proof.sh

python3 - <<'PY'
import re
from pathlib import Path

types = Path("crates/open-kioku-cli/src/types.rs").read_text(encoding="utf-8")
start = types.index("enum Command {")
body_start = types.index("{", start)
depth = 0
body_end = None
for index, character in enumerate(types[body_start:], body_start):
    if character == "{":
        depth += 1
    elif character == "}":
        depth -= 1
        if depth == 0:
            body_end = index
            break

if body_end is None:
    raise SystemExit("could not find the end of the CLI Command enum")

command_body = types[body_start + 1:body_end]
variants = re.findall(
    r"^    ([A-Z][A-Za-z0-9]*)(?:\s*\(|\s*\{|,)", command_body, re.MULTILINE
)
expected = [re.sub(r"(?<!^)([A-Z])", r"-\1", variant).lower() for variant in variants]

readme = Path("README.md").read_text(encoding="utf-8")
match = re.search(r"^Current top-level commands \((\d+)\): (.+)$", readme, re.MULTILINE)
if match is None:
    raise SystemExit("README.md is missing the top-level command inventory")

documented_count = int(match.group(1))
documented = re.findall(r"`([^`]+)`", match.group(2))
if documented_count != len(expected) or documented != expected:
    missing = [command for command in expected if command not in documented]
    extra = [command for command in documented if command not in expected]
    raise SystemExit(
        "README.md top-level command inventory is stale; "
        f"expected {len(expected)} commands, documented {documented_count}; "
        f"missing={missing or 'none'}, extra={extra or 'none'}, "
        "and the documented order must match the CLI"
    )
PY

echo "docs validated: ${crate_count} crates, ${workflow_case_count} workflow cases, CLI command inventory, public quickstart, and public proof"
