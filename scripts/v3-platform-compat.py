#!/usr/bin/env python3
from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def write(path: str, text: str) -> None:
    (ROOT / path).write_text(text, encoding="utf-8")


def remove_once(path: str, block: str) -> None:
    text = read(path)
    count = text.count(block)
    if count != 1:
        raise SystemExit(f"{path}: expected one removable block, found {count}")
    write(path, text.replace(block, "", 1))


def update_release_metadata() -> None:
    path = ROOT / "release-metadata.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    data["npm"]["platform_packages"] = [
        p for p in data["npm"]["platform_packages"] if p != "@open-kioku/darwin-x64"
    ]
    data["artifacts"] = [
        a
        for a in data["artifacts"]
        if a["name"] not in {"ok-macos-x86_64", "ok-macos-x86_64.sha256"}
    ]
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


def update_npm_wrapper() -> None:
    path = ROOT / "packages/npm/package.json"
    data = json.loads(path.read_text(encoding="utf-8"))
    data["optionalDependencies"].pop("@open-kioku/darwin-x64", None)
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")

    launcher = "packages/npm/bin/ok.js"
    remove_once(launcher, '  "darwin-x64": "@open-kioku/darwin-x64",\n')

    readme = "packages/npm/README.md"
    remove_once(readme, "- `@open-kioku/darwin-x64`\n")
    text = read(readme)
    anchor = "Supported packages:\n\n"
    note = "Supported packages:\n\n> Open Kioku 3.x on macOS requires Apple Silicon. Intel macOS remains supported by the 2.4.x release line.\n\n"
    if note not in text:
        if text.count(anchor) != 1:
            raise SystemExit("packages/npm/README.md: supported-packages anchor mismatch")
        write(readme, text.replace(anchor, note, 1))


def update_binstall() -> None:
    path = "crates/open-kioku-cli/Cargo.toml"
    for block in [
        '''[package.metadata.binstall.overrides.x86_64-unknown-linux-musl]\npkg-url = "{ repo }/releases/download/v{ version }/ok-linux-x86_64"\n\n''',
        '''[package.metadata.binstall.overrides.aarch64-unknown-linux-musl]\npkg-url = "{ repo }/releases/download/v{ version }/ok-linux-arm64"\n\n''',
        '''[package.metadata.binstall.overrides.x86_64-apple-darwin]\npkg-url = "{ repo }/releases/download/v{ version }/ok-macos-x86_64"\n\n''',
    ]:
        remove_once(path, block)


def update_formula() -> None:
    path = "Formula/open-kioku.rb"
    text = read(path)
    old = '''  on_macos do\n    if Hardware::CPU.arm?\n      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.0/ok-macos-arm64"\n      sha256 "85922cbad9f623ff8f6f85fba4c0670e6ab9fb0b6d3b46e612d96232a2e8c82d"\n    else\n      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.0/ok-macos-x86_64"\n      sha256 "abde42f14789cbc01d4eaa9dc84d44a885d6b92df84bbe0b21a48921fb60bb96"\n    end\n  end\n'''
    new = '''  on_macos do\n    depends_on arch: :arm64\n    url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.0/ok-macos-arm64"\n    sha256 "85922cbad9f623ff8f6f85fba4c0670e6ab9fb0b6d3b46e612d96232a2e8c82d"\n  end\n'''
    if new not in text:
        if text.count(old) != 1:
            raise SystemExit("Formula/open-kioku.rb: macOS platform block mismatch")
        write(path, text.replace(old, new, 1))


def update_validator() -> None:
    path = "scripts/validate-release-metadata.py"
    text = read(path)
    text = text.replace(
        '''    expected_binaries = {\n        "ok-macos-arm64",\n        "ok-macos-x86_64",\n        "ok-linux-arm64",\n        "ok-linux-x86_64",\n    }\n''',
        '''    expected_binaries = {\n        "ok-macos-arm64",\n        "ok-linux-arm64",\n        "ok-linux-x86_64",\n    }\n''',
        1,
    )
    text = text.replace(
        '''    expected = {\n        "ok-linux-x86_64",\n        "ok-linux-arm64",\n        "ok-macos-x86_64",\n        "ok-macos-arm64",\n    }\n''',
        '''    expected = {\n        "ok-linux-x86_64",\n        "ok-linux-arm64",\n        "ok-macos-arm64",\n    }\n''',
        1,
    )
    if '"ok-macos-x86_64"' in re.search(r"def check_formula.*?def check_release_workflow", text, re.S).group(0):
        raise SystemExit("validator still expects Intel macOS release artifact")
    write(path, text)


def update_docs() -> None:
    checklist = "docs/release-checklist.md"
    text = read(checklist)
    text = text.replace("- `ok-macos-x86_64`\n- `ok-macos-x86_64.sha256`\n", "", 1)
    text = text.replace("five binary artifacts", "four binary artifacts", 1)
    write(checklist, text)

    changelog = "CHANGELOG.md"
    text = read(changelog)
    text = text.replace("- `ok-macos-x86_64`\n- `ok-macos-x86_64.sha256`\n", "", 1)
    compatibility_anchor = "- V3 Linux release binaries target GNU/glibc on x86_64 and ARM64 because the local neural runtime does not provide supported MUSL prebuilts; npm Linux platform packages declare `libc: glibc` accordingly.\n"
    mac_note = "- V3 macOS binaries require Apple Silicon. The local ONNX Runtime used by neural embeddings no longer provides Intel macOS (`x86_64-apple-darwin`) prebuilts; Intel Mac users can remain on the 2.4.x release line.\n"
    if mac_note not in text:
        if text.count(compatibility_anchor) != 1:
            raise SystemExit("CHANGELOG.md: V3 compatibility anchor mismatch")
        text = text.replace(compatibility_anchor, compatibility_anchor + mac_note, 1)
    write(changelog, text)


def main() -> int:
    update_release_metadata()
    update_npm_wrapper()
    update_binstall()
    update_formula()
    update_validator()
    update_docs()
    print("V3 platform compatibility surface updated.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
