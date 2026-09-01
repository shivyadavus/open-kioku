#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

python3 - <<'PY'
import json
import re
from pathlib import Path

proof_path = Path("demo/proof/large-java-2026-08-31-main.json")
proof = json.loads(proof_path.read_text(encoding="utf-8"))

required_top_level = {
    "schema_version",
    "proof_kind",
    "recorded_at",
    "release",
    "privacy",
    "host_profile",
    "workload",
    "method",
    "configuration",
    "measurements",
    "quality_checks",
    "limitations",
}
missing = sorted(required_top_level - proof.keys())
if missing:
    raise SystemExit(f"public proof is missing top-level fields: {missing}")

release = proof["release"]
if release["version"] != "3.1.0" or release["tag"] != "v3.1.0":
    raise SystemExit("public proof does not identify release v3.1.0")
if not re.fullmatch(r"[0-9a-f]{40}", release["source_sha"]):
    raise SystemExit("public proof source_sha must be a full Git commit")

privacy = proof["privacy"]
if privacy["repository_identity"] != "withheld" or privacy["repository_revision"] != "withheld":
    raise SystemExit("anonymous public proof leaked repository identity or revision")
if privacy["source_paths_included"] or privacy["source_snippets_included"]:
    raise SystemExit("anonymous public proof must not include paths or source snippets")
if "cannot reproduce" not in privacy["reproducibility_scope"]:
    raise SystemExit("anonymous public proof must state its replay limitation")

workload = proof["workload"]
measurements = proof["measurements"]
quality = proof["quality_checks"]
positive_counts = [
    workload["tracked_source_files"],
    workload["java_files"],
    workload["indexed_files"],
    workload["symbols"],
    workload["chunks"],
    workload["graph_nodes"],
    workload["graph_edges"],
    workload["semantic_vectors"],
]
if any(not isinstance(value, int) or value <= 0 for value in positive_counts):
    raise SystemExit("public proof workload counts must be positive integers")
if measurements["exact_flat_failed_vectors"] != 0 or measurements["hnsw_failed_vectors"] != 0:
    raise SystemExit("README/site zero-vector-failure claim no longer matches proof")
if not quality["repeat_structural_totals_identical"]:
    raise SystemExit("README/site repeat-index claim no longer matches proof")
if quality["parallel_graph_lock_failures"] != 0:
    raise SystemExit("README/site lock-failure claim no longer matches proof")
if not proof["limitations"]:
    raise SystemExit("public proof must publish limitations")

surfaces = {
    "README.md": Path("README.md").read_text(encoding="utf-8"),
    "demo/index.html": Path("demo/index.html").read_text(encoding="utf-8"),
    "docs/large-java-validation-2026-08-31.md": Path(
        "docs/large-java-validation-2026-08-31.md"
    ).read_text(encoding="utf-8"),
}
expected = [
    release["version"],
    release["source_sha"][:7],
    f'{workload["tracked_source_files"]:,}',
    f'{workload["symbols"]:,}',
    f'{workload["graph_edges"]:,}',
    f'{workload["semantic_vectors"]:,}',
]
for name, content in surfaces.items():
    absent = [value for value in expected if value not in content]
    if absent:
        raise SystemExit(f"{name} is missing public proof values: {absent}")

readme = surfaces["README.md"]
site = surfaces["demo/index.html"]
for content, name in ((readme, "README.md"), (site, "demo/index.html")):
    if "large-java-2026-08-31-main.json" not in content:
        raise SystemExit(f"{name} does not link the machine-readable proof")
    if "large-java-validation-2026-08-31" not in content:
        raise SystemExit(f"{name} does not link the methodology")

print("public proof contract passed")
PY
