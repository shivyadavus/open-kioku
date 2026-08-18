#!/usr/bin/env python3
from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def save(path: str, text: str) -> None:
    (ROOT / path).write_text(text, encoding="utf-8")


def replace_once(path: str, old: str, new: str) -> None:
    text = load(path)
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one anchor, found {count}")
    save(path, text.replace(old, new, 1))


def remove_once(path: str, block: str) -> None:
    replace_once(path, block, "")


def update_metadata() -> None:
    path = ROOT / "release-metadata.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    data["npm"]["platform_packages"] = [
        value for value in data["npm"]["platform_packages"]
        if value != "@open-kioku/darwin-x64"
    ]
    data["artifacts"] = [
        artifact for artifact in data["artifacts"]
        if artifact["name"] not in {"ok-macos-x86_64", "ok-macos-x86_64.sha256"}
    ]
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")

    wrapper = ROOT / "packages/npm/package.json"
    package = json.loads(wrapper.read_text(encoding="utf-8"))
    package["optionalDependencies"].pop("@open-kioku/darwin-x64", None)
    wrapper.write_text(json.dumps(package, indent=2) + "\n", encoding="utf-8")


def update_launcher_docs() -> None:
    replace_once(
        "packages/npm/bin/ok.js",
        "function getBinaryPath() {\n    const osType = OS_MAP[os.platform()];\n",
        "function getBinaryPath() {\n    if (os.platform() === 'darwin' && os.arch() === 'x64') {\n        console.error('Open Kioku 3.x on macOS requires Apple Silicon. Intel Mac users can install Open Kioku 2.4.x.');\n        process.exit(1);\n    }\n\n    const osType = OS_MAP[os.platform()];\n",
    )
    remove_once("packages/npm/README.md", "- `@open-kioku/darwin-x64`\n")
    replace_once(
        "packages/npm/README.md",
        "Supported packages:\n\n",
        "Supported packages:\n\n> Open Kioku 3.x on macOS requires Apple Silicon. Intel macOS remains supported by the 2.4.x release line.\n\n",
    )


def update_cargo_formula() -> None:
    remove_once(
        "crates/open-kioku-cli/Cargo.toml",
        '''[package.metadata.binstall.overrides.x86_64-apple-darwin]\npkg-url = "{ repo }/releases/download/v{ version }/ok-macos-x86_64"\n\n''',
    )
    formula = "Formula/open-kioku.rb"
    text = load(formula)
    pattern = re.compile(
        r'''  on_macos do\n    if Hardware::CPU\.arm\?\n      url "([^"]*ok-macos-arm64)"\n      sha256 "([0-9a-f]{64})"\n    else\n      url "[^"]*ok-macos-x86_64"\n      sha256 "[0-9a-f]{64}"\n    end\n  end\n'''
    )
    match = pattern.search(text)
    if not match:
        raise SystemExit("Formula/open-kioku.rb: Intel/ARM macOS block not found")
    replacement = (
        "  on_macos do\n"
        "    depends_on arch: :arm64\n"
        f"    url \"{match.group(1)}\"\n"
        f"    sha256 \"{match.group(2)}\"\n"
        "  end\n"
    )
    save(formula, pattern.sub(replacement, text, count=1))


def update_validators_docs() -> None:
    validator = "scripts/validate-release-metadata.py"
    text = load(validator)
    text, c1 = re.subn(r'\n\s*"ok-macos-x86_64",', '', text)
    if c1 < 1:
        raise SystemExit("validate-release-metadata.py: macOS x64 expectation not found")
    save(validator, text)

    remove_once("docs/release-checklist.md", "- `ok-macos-x86_64`\n- `ok-macos-x86_64.sha256`\n")
    checklist = load("docs/release-checklist.md").replace("five binary artifacts", "four binary artifacts")
    save("docs/release-checklist.md", checklist)

    changelog = load("CHANGELOG.md")
    start = changelog.find("## [3.0.0]")
    if start < 0:
        raise SystemExit("CHANGELOG.md: 3.0.0 section missing")
    end = changelog.find("\n---\n", start)
    if end < 0:
        raise SystemExit("CHANGELOG.md: 3.0.0 section terminator missing")
    section = changelog[start:end]
    intel_artifacts = "- `ok-macos-x86_64`\n- `ok-macos-x86_64.sha256`\n"
    if section.count(intel_artifacts) != 1:
        raise SystemExit("CHANGELOG.md: 3.0.0 Intel artifact block missing or duplicated")
    section = section.replace(intel_artifacts, "", 1)
    anchor = "- V3 Linux release binaries target GNU/glibc on x86_64 and ARM64 because the local neural runtime does not provide supported MUSL prebuilts; npm Linux platform packages declare `libc: glibc` accordingly.\n"
    note = "- V3 macOS binaries require Apple Silicon. The local ONNX Runtime dependency no longer provides Intel macOS (`x86_64-apple-darwin`) prebuilts; Intel Mac users can remain on the 2.4.x release line.\n"
    if section.count(anchor) != 1:
        raise SystemExit("CHANGELOG.md: 3.0.0 Linux compatibility anchor missing")
    section = section.replace(anchor, anchor + note, 1)
    save("CHANGELOG.md", changelog[:start] + section + changelog[end:])


def remove_intel_matrix(path: str) -> None:
    text = load(path)
    patterns = [
        re.compile(r'''\n\s*- os: macos-15-intel\n\s*target: x86_64-apple-darwin\n\s*binary_name: ok-macos-x86_64'''),
        re.compile(r'''\n\s*- os: macos-13\n\s*target: x86_64-apple-darwin\n\s*binary_name: ok-macos-x86_64'''),
    ]
    total = 0
    for pattern in patterns:
        text, count = pattern.subn("", text)
        total += count
    if total != 1:
        raise SystemExit(f"{path}: expected exactly one Intel macOS matrix entry, found {total}")
    save(path, text)


def main() -> int:
    update_metadata()
    update_launcher_docs()
    update_cargo_formula()
    update_validators_docs()
    remove_intel_matrix(".github/workflows/release.yml")
    remove_intel_matrix(".github/workflows/v3-mainline-candidate.yml")
    print("V3 release surface aligned to supported platforms (Apple Silicon macOS only).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
