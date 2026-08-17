#!/usr/bin/env python3
"""Prepare the Open Kioku 3.0.0 release tree deterministically.

Temporary release-engineering helper. The V3 preparation workflow removes this
file before the release PR is opened.
"""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
VERSION = "3.0.0"


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def write(path: str, text: str) -> None:
    (ROOT / path).write_text(text, encoding="utf-8")


def replace_once(path: str, old: str, new: str) -> None:
    text = read(path)
    if new in text:
        return
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one anchor, found {count}")
    write(path, text.replace(old, new, 1))


def update_workspace_version() -> None:
    path = "Cargo.toml"
    text = read(path)
    pattern = re.compile(
        r'(\[workspace\.package\][\s\S]*?^version = ")[^"]+("$)',
        re.MULTILINE,
    )
    updated, count = pattern.subn(rf"\g<1>{VERSION}\2", text, count=1)
    if count != 1:
        raise SystemExit("Cargo.toml: could not update workspace package version")
    write(path, updated)


def add_internal_version_sync() -> None:
    helper = ROOT / "scripts/sync-internal-cargo-versions.py"
    helper.write_text(
        '''#!/usr/bin/env python3
"""Sync sibling Open Kioku Cargo path dependencies to the workspace version."""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def workspace_version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'\\[workspace\\.package\\][\\s\\S]*?^version = "([^"]+)"$', text, re.MULTILINE)
    if not match:
        raise SystemExit("could not read workspace package version")
    return match.group(1)


def main() -> int:
    version = sys.argv[1] if len(sys.argv) > 1 else workspace_version()
    dependency = re.compile(r'^\\s*open-kioku-[A-Za-z0-9_-]+\\s*=\\s*\\{')
    sibling_path = re.compile(r'path\\s*=\\s*"\\.\\./open-kioku-[^"]+"')
    version_field = re.compile(r'version\\s*=\\s*"[^"]+"')

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
''',
        encoding="utf-8",
    )

    path = "scripts/sync-version.sh"
    text = read(path)
    command = 'python3 "$ROOT/scripts/sync-internal-cargo-versions.py" "$VERSION"\n'
    if command not in text:
        anchor = 'echo "Syncing version $VERSION to marketplace manifests..."\n\n'
        if text.count(anchor) != 1:
            raise SystemExit("scripts/sync-version.sh: insertion anchor mismatch")
        text = text.replace(
            anchor,
            anchor
            + "# Keep publishable sibling Cargo dependencies in lockstep across major releases.\n"
            + command
            + "\n",
            1,
        )
        write(path, text)


def harden_release_validation() -> None:
    path = "scripts/validate-release-metadata.py"
    text = read(path)

    if "def check_internal_cargo_dependencies(" not in text:
        anchor = "def check_formula(metadata: dict, version: str, errors: list[str]) -> None:\n"
        function = '''def check_internal_cargo_dependencies(version: str, errors: list[str]) -> None:
    sections = ("dependencies", "dev-dependencies", "build-dependencies")

    def check_table(path: Path, table: dict) -> None:
        for section in sections:
            for name, spec in table.get(section, {}).items():
                if not name.startswith("open-kioku-") or not isinstance(spec, dict):
                    continue
                if not spec.get("path"):
                    continue
                found = spec.get("version")
                if found != version:
                    fail(
                        f"{path.relative_to(ROOT)} internal dependency {name} has version {found!r}; expected {version}",
                        errors,
                    )

    for path in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        data = tomllib.loads(load_text(path))
        check_table(path, data)
        for target in data.get("target", {}).values():
            if isinstance(target, dict):
                check_table(path, target)


'''
        if text.count(anchor) != 1:
            raise SystemExit(f"{path}: internal-dependency function anchor mismatch")
        text = text.replace(anchor, function + anchor, 1)

    call = "    check_internal_cargo_dependencies(version, errors)\n"
    if call not in text:
        anchor = '    check_git_tag(version, metadata.get("tag"), errors)\n'
        if text.count(anchor) != 1:
            raise SystemExit(f"{path}: internal-dependency call anchor mismatch")
        text = text.replace(anchor, anchor + call, 1)

    required = '        "scripts/verify-release-artifact-hashes.py",\n'
    if required not in text:
        # This exact pair exists only in check_release_workflow.required_steps, not
        # in the release-checklist requirements later in the file.
        anchor = (
            '        "scripts/generate-release-trust-artifacts.sh",\n'
            '        "SHA256SUMS",\n'
        )
        replacement = (
            '        "scripts/generate-release-trust-artifacts.sh",\n'
            + required
            + '        "SHA256SUMS",\n'
        )
        if text.count(anchor) != 1:
            raise SystemExit(f"{path}: release-workflow requirement anchor mismatch")
        text = text.replace(anchor, replacement, 1)

    write(path, text)


def add_release_hash_verifier() -> None:
    path = ROOT / "scripts/verify-release-artifact-hashes.py"
    path.write_text(
        '''#!/usr/bin/env python3
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
''',
        encoding="utf-8",
    )


def harden_release_workflow() -> None:
    path = ".github/workflows/release.yml"
    text = read(path)
    if "Verify release artifact hashes" in text:
        return
    anchor = (
        "      - uses: actions/download-artifact@v4\n"
        "        with:\n"
        "          path: dist\n"
        "          merge-multiple: true\n"
        "\n"
        "      - uses: dtolnay/rust-toolchain@stable\n"
    )
    replacement = (
        "      - uses: actions/download-artifact@v4\n"
        "        with:\n"
        "          path: dist\n"
        "          merge-multiple: true\n"
        "\n"
        "      - name: Verify release artifact hashes\n"
        "        shell: bash\n"
        "        run: python3 scripts/verify-release-artifact-hashes.py dist\n"
        "\n"
        "      - uses: dtolnay/rust-toolchain@stable\n"
    )
    if text.count(anchor) != 1:
        raise SystemExit(f"{path}: publish download anchor mismatch ({text.count(anchor)})")
    write(path, text.replace(anchor, replacement, 1))


def update_changelog() -> None:
    path = "CHANGELOG.md"
    text = read(path)
    if "## [3.0.0]" not in text:
        section = '''## [3.0.0] — 2026-08-17

### Added
- Added evidence-first routed context retrieval with provenance-aware bounded context, explicit blockers, and measured retrieval quality gates.
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

'''
        anchor = "## [2.4.0] — 2026-08-14\n"
        if text.count(anchor) != 1:
            raise SystemExit(f"{path}: 2.4.0 section anchor mismatch")
        text = text.replace(anchor, section + anchor, 1)

    link = "[3.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.0\n"
    if link not in text:
        anchor = "[2.4.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.4.0\n"
        if text.count(anchor) != 1:
            raise SystemExit(f"{path}: release-link anchor mismatch")
        text = text.replace(anchor, link + anchor, 1)
    write(path, text)


def main() -> int:
    update_workspace_version()
    add_internal_version_sync()
    harden_release_validation()
    add_release_hash_verifier()
    harden_release_workflow()
    update_changelog()
    print("Prepared deterministic source edits for Open Kioku 3.0.0.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
