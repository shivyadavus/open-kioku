#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PYTHON="${PYTHON:-python3}"
PACKAGE_DIR="$ROOT/packages/npm"

version="$($PYTHON - <<'PY' "$ROOT/Cargo.toml"
import sys
import tomllib
from pathlib import Path

cargo = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
print(cargo["workspace"]["package"]["version"])
PY
)"

pack_json="$(cd "$PACKAGE_DIR" && npm pack --dry-run --json)"
PACK_JSON="$pack_json" EXPECTED_VERSION="$version" "$PYTHON" - <<'PY'
import json
import os

pack = json.loads(os.environ["PACK_JSON"])
if len(pack) != 1:
    raise SystemExit("expected exactly one npm pack result")
entry = pack[0]
if entry.get("name") != "open-kioku":
    raise SystemExit(f"unexpected npm package name: {entry.get('name')!r}")
if entry.get("version") != os.environ["EXPECTED_VERSION"]:
    raise SystemExit("npm pack version does not match Cargo workspace version")
files = {item["path"] for item in entry.get("files", [])}
required = {"package.json", "README.md", "bin/ok.js"}
missing = sorted(required - files)
if missing:
    raise SystemExit(f"npm pack is missing required files: {', '.join(missing)}")
if entry.get("filename") != f"open-kioku-{entry['version']}.tgz":
    raise SystemExit("npm pack filename does not match package version")
PY

echo "npm package dry-run passed for open-kioku@$version"
