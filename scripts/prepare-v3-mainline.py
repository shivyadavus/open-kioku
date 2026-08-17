#!/usr/bin/env python3
from __future__ import annotations

import json
import re
from pathlib import Path

VERSION = "3.0.0"

cargo = Path("Cargo.toml")
text = cargo.read_text(encoding="utf-8")
text, count = re.subn(
    r'(\[workspace\.package\]\nversion = ")[^"]+("\n)',
    rf'\g<1>{VERSION}\2',
    text,
    count=1,
)
if count != 1:
    raise SystemExit("workspace version anchor not found")
cargo.write_text(text, encoding="utf-8")

changelog = Path("CHANGELOG.md")
text = changelog.read_text(encoding="utf-8")
if f"## [{VERSION}]" not in text:
    anchor = "## [Unreleased]\n\n---\n\n"
    section = """## [3.0.0] — 2026-08-17

### Added
- Added evidence-first routed context retrieval with provenance-aware bounded context, explicit blockers, and measured retrieval quality gates.
- Added task/query-shape routing measurement with frozen labels, adversarial probes, classifier accuracy, task-family × query-shape quality, and latency reporting while preserving the generic blocking retrieval baseline.
- Added quality-tiered local neural embedding profiles alongside the deterministic local-hash baseline, with opt-in local model acquisition and provenance.
- Added persistent local HNSW semantic indexing with exact-flat correctness fallback, persistence, filter parity, and scale calibration.
- Added proof-carrying relationship authority and deterministic proof-gated structural resolution for calls, references, type use, inheritance, and imports.
- Added the `ok relationship-bench` conformance scoring foundation with strict proof/range/outcome/metamorphic threshold policy and reproducibility metadata.

### Changed
- Made `ResolutionMode::Shadow` the default so proof-gated structural relationships are operational while legacy evidence remains available for compatibility; explicit `Legacy` and `V2` modes remain available.
- Made authoritative architecture/context consumers fail closed on unproven structural relationships instead of promoting heuristic confidence into graph truth.
- Refreshed the homepage and README around the evidence-first workflow and real pinned-main dogfood proof.
- Bumped the 43-crate workspace and all release/install channels to 3.0.0, including explicit 3.0.0 requirements for publishable internal Cargo path dependencies.
- Hardened release publishing so built binary SHA-256 values must match checked-in release metadata before GitHub/npm publication.

### Fixed
- Preserved typed authority for uniquely resolved import targets and exact Rust `crate::module::member()` calls without enabling fuzzy structural fallbacks.
- Updated public quickstart validation to treat `ok setup agent ...` as the primary onboarding flow while retaining lower-level `init`, `index`, and manual MCP commands as supported primitives.

### Compatibility
- Reindexing is recommended for 3.0. Existing heuristic structural edges from older indexes are not trusted as authoritative unless reconstructed with proof; relationship counts may decrease when ambiguous evidence correctly fails closed.
- V3 Linux release binaries target GNU/glibc on x86_64 and ARM64 because the local neural runtime does not provide supported MUSL prebuilts; npm Linux platform packages declare `libc: glibc` accordingly.
- The checked-in relationship scorer is a conformance-scoring foundation; the full frozen >=300-case #240 corpus remains follow-up work and is not claimed complete by this release.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

"""
    if anchor not in text:
        raise SystemExit("changelog Unreleased anchor not found")
    text = text.replace(anchor, anchor + section, 1)
    link = "[3.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.0"
    if link not in text:
        text = text.rstrip() + "\n\n" + link + "\n"
    changelog.write_text(text, encoding="utf-8")

for path in [
    Path("packages/npm-linux-x64/package.json"),
    Path("packages/npm-linux-arm64/package.json"),
]:
    data = json.loads(path.read_text(encoding="utf-8"))
    data["libc"] = ["glibc"]
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
