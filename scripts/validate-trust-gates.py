#!/usr/bin/env python3
"""Validate that release-trust gates remain wired into CI, release, and docs."""

from __future__ import annotations

import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def text(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def require_contains(errors: list[str], path: str, required: list[str]) -> None:
    body = text(path)
    for needle in required:
        if needle not in body:
            errors.append(f"{path} missing {needle!r}")


def require_exists(errors: list[str], path: str) -> None:
    if not (ROOT / path).exists():
        errors.append(f"{path} is missing")


def main() -> int:
    errors: list[str] = []
    for path in [
        "ci/ignored-tests-allowlist.txt",
        "scripts/check-no-ignored-tests.py",
        "scripts/generate-release-trust-artifacts.sh",
        "docs/release-trust.md",
        "THIRD_PARTY_NOTICES.md",
    ]:
        require_exists(errors, path)

    require_contains(
        errors,
        ".github/workflows/ci.yml",
        [
            "cargo fmt --all -- --check",
            "cargo clippy --all-targets --all-features -- -D warnings",
            "cargo test --all",
            "cargo test -p open-kioku-tests",
            "scripts/check-no-ignored-tests.py",
            "scripts/validate-release-metadata.py",
            "scripts/validate-trust-gates.py",
            "cargo audit",
            "cargo deny check",
            "golden_mcp_protocol_snapshots_are_stable",
            "redacts_secret_like_message_tokens",
            "network_denied_by_default",
            "snapshot_export_import_round_trip_rebuilds_search_and_bootstraps_index",
            "disabled_config_returns_no_provider",
        ],
    )
    require_contains(
        errors,
        ".github/workflows/bench.yml",
        [
            "cargo bench --workspace",
            "ok bench",
            "workflow-bench",
            "architecture bench",
            "contract-bench",
            "history similar-bench",
            "snapshot export --quality fast",
            "snapshot import",
            "graph query --dsl",
            "actions/upload-artifact@v4",
            "benchmark-reports",
        ],
    )
    require_contains(
        errors,
        ".github/workflows/release.yml",
        [
            "scripts/generate-release-trust-artifacts.sh",
            "SHA256SUMS",
            "SBOM.cargo-metadata.json",
            "PROVENANCE.json",
            "THIRD_PARTY_NOTICES.md",
            ".sha256",
            "actions/attest-build-provenance",
            "softprops/action-gh-release",
        ],
    )
    require_contains(
        errors,
        "scripts/verify-release-readiness.sh",
        [
            "plan token --format html",
            "prove \"$SMOKE_REPO\" --task token --limit 8 --html",
            "ui --task token --format html",
            "architecture overview",
            "verify --plan",
            "--format html",
        ],
    )
    require_contains(
        errors,
        "docs/workflow-benchmarks.md",
        [
            "indexing time by phase",
            "memory usage",
            "graph node/edge counts",
            "graph query latency",
            "search latency",
            "test selection quality",
            "plan quality",
            "verification false positives/negatives",
            "snapshot export/import time",
            "token savings",
        ],
    )
    require_contains(
        errors,
        "docs/release-trust.md",
        [
            "checksums",
            "SHA256SUMS",
            "SBOM.cargo-metadata.json",
            "PROVENANCE.json",
            "THIRD_PARTY_NOTICES.md",
            "local processing threat model",
            "install audit",
            "attestations",
            "scripts/verify-release-readiness.sh",
        ],
    )
    require_contains(
        errors,
        "docs/release-checklist.md",
        [
            "scripts/check-no-ignored-tests.py",
            "scripts/validate-release-metadata.py",
            "scripts/validate-trust-gates.py",
            "docs/release-trust.md",
            "SHA256SUMS",
            "SBOM.cargo-metadata.json",
            "PROVENANCE.json",
            "THIRD_PARTY_NOTICES.md",
        ],
    )

    if errors:
        print("Release trust gate validation failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print("Release trust gates are wired into CI, release, and docs.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
