#!/usr/bin/env python3
from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def load(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def save(path: str, text: str) -> None:
    (ROOT / path).write_text(text, encoding="utf-8")


def replace_once(path: str, old: str, new: str) -> None:
    text = load(path)
    if new in text:
        return
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one anchor, found {count}")
    save(path, text.replace(old, new, 1))


def remove_once(path: str, block: str) -> None:
    text = load(path)
    count = text.count(block)
    if count != 1:
        raise SystemExit(f"{path}: expected one removable block, found {count}")
    save(path, text.replace(block, "", 1))


def metadata_and_npm() -> None:
    metadata_path = ROOT / "release-metadata.json"
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    metadata["npm"]["platform_packages"] = [
        value for value in metadata["npm"]["platform_packages"]
        if value != "@open-kioku/darwin-x64"
    ]
    metadata["artifacts"] = [
        artifact for artifact in metadata["artifacts"]
        if artifact["name"] not in {"ok-macos-x86_64", "ok-macos-x86_64.sha256"}
    ]
    metadata_path.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")

    wrapper_path = ROOT / "packages/npm/package.json"
    wrapper = json.loads(wrapper_path.read_text(encoding="utf-8"))
    wrapper["optionalDependencies"].pop("@open-kioku/darwin-x64", None)
    wrapper_path.write_text(json.dumps(wrapper, indent=2) + "\n", encoding="utf-8")

    launcher_anchor = "function getBinaryPath() {\n    const osType = OS_MAP[os.platform()];\n"
    launcher_guard = "function getBinaryPath() {\n    if (os.platform() === 'darwin' && os.arch() === 'x64') {\n        console.error('Open Kioku 3.x on macOS requires Apple Silicon. Intel Mac users can install Open Kioku 2.4.x.');\n        process.exit(1);\n    }\n\n    const osType = OS_MAP[os.platform()];\n"
    replace_once("packages/npm/bin/ok.js", launcher_anchor, launcher_guard)

    remove_once("packages/npm/README.md", "- `@open-kioku/darwin-x64`\n")
    replace_once(
        "packages/npm/README.md",
        "Supported packages:\n\n",
        "Supported packages:\n\n> Open Kioku 3.x on macOS requires Apple Silicon. Intel macOS remains supported by the 2.4.x release line.\n\n",
    )


def binstall_and_formula() -> None:
    cargo = "crates/open-kioku-cli/Cargo.toml"
    for block in [
        '''[package.metadata.binstall.overrides.x86_64-unknown-linux-musl]\npkg-url = "{ repo }/releases/download/v{ version }/ok-linux-x86_64"\n\n''',
        '''[package.metadata.binstall.overrides.aarch64-unknown-linux-musl]\npkg-url = "{ repo }/releases/download/v{ version }/ok-linux-arm64"\n\n''',
        '''[package.metadata.binstall.overrides.x86_64-apple-darwin]\npkg-url = "{ repo }/releases/download/v{ version }/ok-macos-x86_64"\n\n''',
    ]:
        remove_once(cargo, block)

    old_formula = '''  on_macos do\n    if Hardware::CPU.arm?\n      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.0/ok-macos-arm64"\n      sha256 "85922cbad9f623ff8f6f85fba4c0670e6ab9fb0b6d3b46e612d96232a2e8c82d"\n    else\n      url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.0/ok-macos-x86_64"\n      sha256 "abde42f14789cbc01d4eaa9dc84d44a885d6b92df84bbe0b21a48921fb60bb96"\n    end\n  end\n'''
    new_formula = '''  on_macos do\n    depends_on arch: :arm64\n    url "https://github.com/shivyadavus/open-kioku/releases/download/v3.0.0/ok-macos-arm64"\n    sha256 "85922cbad9f623ff8f6f85fba4c0670e6ab9fb0b6d3b46e612d96232a2e8c82d"\n  end\n'''
    replace_once("Formula/open-kioku.rb", old_formula, new_formula)


def validator_and_docs() -> None:
    replace_once(
        "scripts/validate-release-metadata.py",
        '''    expected_binaries = {\n        "ok-macos-arm64",\n        "ok-macos-x86_64",\n        "ok-linux-arm64",\n        "ok-linux-x86_64",\n    }\n''',
        '''    expected_binaries = {\n        "ok-macos-arm64",\n        "ok-linux-arm64",\n        "ok-linux-x86_64",\n    }\n''',
    )
    replace_once(
        "scripts/validate-release-metadata.py",
        '''    expected = {\n        "ok-linux-x86_64",\n        "ok-linux-arm64",\n        "ok-macos-x86_64",\n        "ok-macos-arm64",\n    }\n''',
        '''    expected = {\n        "ok-linux-x86_64",\n        "ok-linux-arm64",\n        "ok-macos-arm64",\n    }\n''',
    )

    remove_once("docs/release-checklist.md", "- `ok-macos-x86_64`\n- `ok-macos-x86_64.sha256`\n")
    replace_once("docs/release-checklist.md", "five binary artifacts", "four binary artifacts")

    remove_once("CHANGELOG.md", "- `ok-macos-x86_64`\n- `ok-macos-x86_64.sha256`\n")
    anchor = "- V3 Linux release binaries target GNU/glibc on x86_64 and ARM64 because the local neural runtime does not provide supported MUSL prebuilts; npm Linux platform packages declare `libc: glibc` accordingly.\n"
    note = "- V3 macOS binaries require Apple Silicon. The local ONNX Runtime used by neural embeddings no longer provides Intel macOS (`x86_64-apple-darwin`) prebuilts; Intel Mac users can remain on the 2.4.x release line.\n"
    replace_once("CHANGELOG.md", anchor, anchor + note)


def main() -> int:
    metadata_and_npm()
    binstall_and_formula()
    validator_and_docs()
    print("V3 platform compatibility surface updated.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
