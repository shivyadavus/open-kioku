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

if ! grep -Eq "workspace is composed of ${crate_count} focused crates" CONTRIBUTING.md; then
  echo "CONTRIBUTING.md crate count is stale; expected ${crate_count}" >&2
  exit 1
fi

if ! grep -Eq "contains ${workflow_case_count}[[:space:]]+cases" docs/workflow-benchmarks.md; then
  echo "docs/workflow-benchmarks.md workflow case count is stale; expected ${workflow_case_count}" >&2
  exit 1
fi

echo "docs validated: ${crate_count} crates, ${workflow_case_count} workflow cases"
