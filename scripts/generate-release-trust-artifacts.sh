#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="${1:-$ROOT/dist}"

mkdir -p "$DIST"

python3 - "$ROOT" "$DIST" <<'PY'
from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tomllib
from pathlib import Path


root = Path(sys.argv[1])
dist = Path(sys.argv[2])
cargo = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
release_metadata = json.loads((root / "release-metadata.json").read_text(encoding="utf-8"))
version = cargo["workspace"]["package"]["version"]
expected_binaries = [
    artifact["name"]
    for artifact in release_metadata["artifacts"]
    if artifact["name"].startswith("ok-") and not artifact["name"].endswith(".sha256")
]

missing = [name for name in expected_binaries if not (dist / name).is_file()]
if missing:
    raise SystemExit("missing release binary artifact(s): " + ", ".join(missing))

checksums: list[dict[str, str]] = []
with (dist / "SHA256SUMS").open("w", encoding="utf-8") as fh:
    for name in expected_binaries:
        data = (dist / name).read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        checksums.append({"name": name, "sha256": digest})
        fh.write(f"{digest}  {name}\n")

metadata = subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--locked"],
    cwd=root,
    check=True,
    text=True,
    capture_output=True,
).stdout
(dist / "SBOM.cargo-metadata.json").write_text(metadata, encoding="utf-8")

provenance = {
    "schema_version": "open-kioku.release-provenance.v1",
    "generated_at": dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z"),
    "repository": release_metadata["repository"],
    "tag": release_metadata["tag"],
    "version": version,
    "source_ref": os.environ.get("GITHUB_SHA", ""),
    "workflow": os.environ.get("GITHUB_WORKFLOW", ""),
    "run_id": os.environ.get("GITHUB_RUN_ID", ""),
    "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT", ""),
    "builder": "github-actions" if os.environ.get("GITHUB_ACTIONS") else "local",
    "artifacts": checksums,
}
(dist / "PROVENANCE.json").write_text(
    json.dumps(provenance, indent=2, sort_keys=True) + "\n",
    encoding="utf-8",
)

shutil.copyfile(root / "THIRD_PARTY_NOTICES.md", dist / "THIRD_PARTY_NOTICES.md")
shutil.copyfile(root / "release-metadata.json", dist / "release-metadata.json")
PY

echo "generated release trust artifacts in $DIST"
