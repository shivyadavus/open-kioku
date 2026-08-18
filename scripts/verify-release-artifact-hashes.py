#!/usr/bin/env python3
"""Verify built release binaries against release-metadata.json SHA-256 values."""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    dist = Path(sys.argv[1] if len(sys.argv) > 1 else "dist")
    if not dist.is_absolute():
        dist = ROOT / dist
    metadata = json.loads((ROOT / "release-metadata.json").read_text(encoding="utf-8"))
    errors: list[str] = []
    checked = 0

    for artifact in metadata.get("artifacts", []):
        expected = artifact.get("sha256")
        if expected is None:
            continue
        binary = dist / artifact["name"]
        if not binary.is_file():
            errors.append(f"missing release binary: {artifact['name']}")
            continue
        actual = sha256(binary)
        checked += 1
        if actual != expected:
            errors.append(
                f"{artifact['name']} sha256 mismatch: metadata={expected} built={actual}"
            )
        sidecar = dist / f"{artifact['name']}.sha256"
        if sidecar.is_file():
            sidecar_hash = sidecar.read_text(encoding="utf-8").split()[0]
            if sidecar_hash != actual:
                errors.append(
                    f"{sidecar.name} records {sidecar_hash}, but built binary is {actual}"
                )

    if checked == 0:
        errors.append("release metadata contained no binary sha256 entries")
    if errors:
        print("Release artifact hash verification failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1
    print(f"Verified {checked} release binary hash(es) against release metadata.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
