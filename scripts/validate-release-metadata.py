#!/usr/bin/env python3
"""Validate release-channel metadata against the canonical workspace version."""

from __future__ import annotations

import json
import os
import re
import sys
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def fail(message: str, errors: list[str]) -> None:
    errors.append(message)


def load_json(path: Path) -> object:
    with path.open("r", encoding="utf-8") as fh:
        return json.load(fh)


def load_text(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def cargo_version() -> str:
    cargo = tomllib.loads(load_text(ROOT / "Cargo.toml"))
    return cargo["workspace"]["package"]["version"]


def artifact_names(metadata: dict) -> list[str]:
    return [artifact["name"] for artifact in metadata["artifacts"]]


def binary_artifacts(metadata: dict) -> dict[str, str]:
    return {
        artifact["name"]: artifact["sha256"]
        for artifact in metadata["artifacts"]
        if "sha256" in artifact
    }


def check_json_version(path: Path, version: str, errors: list[str]) -> None:
    if not path.exists():
        return
    data = load_json(path)
    found = data.get("version")
    if found is None and isinstance(data.get("metadata"), dict):
        found = data["metadata"].get("version")
    if found != version:
        fail(f"{path.relative_to(ROOT)} version {found!r} does not match {version}", errors)


def check_npm_packages(metadata: dict, version: str, errors: list[str]) -> None:
    wrapper_path = ROOT / "packages/npm/package.json"
    wrapper = load_json(wrapper_path)
    if wrapper.get("name") != metadata["npm"]["wrapper"]:
        fail("packages/npm/package.json wrapper name does not match release metadata", errors)
    if wrapper.get("version") != version:
        fail(f"packages/npm/package.json version does not match {version}", errors)

    expected_platforms = set(metadata["npm"]["platform_packages"])
    optional = wrapper.get("optionalDependencies", {})
    if set(optional) != expected_platforms:
        fail("packages/npm/package.json optionalDependencies do not match release metadata", errors)
    for package, dep_version in optional.items():
        if dep_version != version:
            fail(f"packages/npm/package.json dependency {package} is {dep_version}, expected {version}", errors)

    seen_platforms: set[str] = set()
    for package_json in sorted((ROOT / "packages").glob("npm-*/package.json")):
        data = load_json(package_json)
        name = data.get("name")
        seen_platforms.add(name)
        if name not in expected_platforms:
            fail(f"{package_json.relative_to(ROOT)} package name {name!r} is not in release metadata", errors)
        if data.get("version") != version:
            fail(f"{package_json.relative_to(ROOT)} version does not match {version}", errors)

    missing = expected_platforms - seen_platforms
    if missing:
        fail(f"missing npm platform package manifests: {', '.join(sorted(missing))}", errors)


def check_formula(metadata: dict, version: str, errors: list[str]) -> None:
    formula_path = ROOT / metadata["homebrew"]["formula"]
    text = load_text(formula_path)
    if f'version "{version}"' not in text:
        fail(f"{formula_path.relative_to(ROOT)} version does not match {version}", errors)

    expected_binaries = {
        "ok-macos-arm64",
        "ok-macos-x86_64",
        "ok-linux-arm64",
        "ok-linux-x86_64",
    }
    shas = binary_artifacts(metadata)
    for name in expected_binaries:
        url = f"{metadata['repository']}/releases/download/v{version}/{name}"
        if url not in text:
            fail(f"{formula_path.relative_to(ROOT)} missing URL {url}", errors)
        sha = shas.get(name)
        if not sha or not re.fullmatch(r"[0-9a-f]{64}", sha):
            fail(f"release metadata missing sha256 for Homebrew artifact {name}", errors)
        elif sha not in text:
            fail(f"{formula_path.relative_to(ROOT)} missing sha256 for {name}", errors)


def check_binstall(metadata: dict, errors: list[str]) -> None:
    cli_toml = load_text(ROOT / "crates/open-kioku-cli/Cargo.toml")
    expected = {
        "ok-linux-x86_64",
        "ok-linux-arm64",
        "ok-macos-x86_64",
        "ok-macos-arm64",
    }
    for name in expected:
        fragment = f"releases/download/v{{ version }}/{name}"
        if fragment not in cli_toml:
            fail(f"cargo-binstall metadata missing artifact URL fragment {fragment}", errors)

    if "[package.metadata.binstall]" not in cli_toml:
        fail("open-kioku-cli is missing package.metadata.binstall", errors)


def check_release_workflow(metadata: dict, errors: list[str]) -> None:
    workflow = load_text(ROOT / ".github/workflows/release.yml")
    for name in artifact_names(metadata):
        if name.endswith(".sha256"):
            continue
        if name not in workflow:
            fail(f"release workflow does not package artifact {name}", errors)

    required_steps = [
        "scripts/validate-versions.sh",
        "softprops/action-gh-release",
        "npm publish --access public",
    ]
    for step in required_steps:
        if step not in workflow:
            fail(f"release workflow missing {step}", errors)


def current_changelog_section(version: str) -> str:
    changelog = load_text(ROOT / "CHANGELOG.md")
    match = re.search(
        rf"^## \[{re.escape(version)}\].*?(?=^## \[|\Z)",
        changelog,
        flags=re.MULTILINE | re.DOTALL,
    )
    return match.group(0) if match else ""


def check_release_notes(metadata: dict, version: str, errors: list[str]) -> None:
    changelog = load_text(ROOT / "CHANGELOG.md")
    if f"[{version}]: {metadata['repository']}/releases/tag/v{version}" not in changelog:
        fail(f"CHANGELOG.md missing release link for v{version}", errors)

    section = current_changelog_section(version)
    if not section:
        fail(f"CHANGELOG.md missing section for {version}", errors)
        return
    for name in artifact_names(metadata):
        if name not in section:
            fail(f"CHANGELOG.md {version} release notes missing artifact {name}", errors)


def check_release_checklist(metadata: dict, version: str, errors: list[str]) -> None:
    checklist_path = ROOT / "docs/release-checklist.md"
    if not checklist_path.exists():
        fail("docs/release-checklist.md is missing", errors)
        return
    checklist = load_text(checklist_path)
    required = [
        "scripts/validate-versions.sh",
        "scripts/verify-release-readiness.sh",
        "cargo fmt --all -- --check",
        "cargo clippy --all-targets --all-features -- -D warnings",
        "cargo test --all",
        "cargo binstall open-kioku-cli",
        metadata["homebrew"]["install"],
        "npm install -g open-kioku",
        f"v{version}",
    ]
    for item in required:
        if item not in checklist:
            fail(f"release checklist missing {item}", errors)
    for name in artifact_names(metadata):
        if name not in checklist:
            fail(f"release checklist missing artifact {name}", errors)


def check_git_tag(version: str, tag: str, errors: list[str]) -> None:
    if tag != f"v{version}":
        fail(f"release metadata tag {tag!r} does not match v{version}", errors)

    github_ref = os.environ.get("GITHUB_REF_NAME")
    if github_ref and re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", github_ref):
        if github_ref != tag:
            fail(f"GitHub ref tag {github_ref} does not match release metadata tag {tag}", errors)


def main() -> int:
    errors: list[str] = []
    version = cargo_version()
    metadata = load_json(ROOT / "release-metadata.json")

    if metadata.get("version") != version:
        fail(f"release-metadata.json version {metadata.get('version')!r} does not match Cargo.toml {version}", errors)

    check_git_tag(version, metadata.get("tag"), errors)
    check_json_version(ROOT / ".cursor-plugin/plugin.json", version, errors)
    check_json_version(ROOT / ".cursor-plugin/marketplace.json", version, errors)
    check_json_version(ROOT / "claude-plugin.json", version, errors)
    check_npm_packages(metadata, version, errors)
    check_formula(metadata, version, errors)
    check_binstall(metadata, errors)
    check_release_workflow(metadata, errors)
    check_release_notes(metadata, version, errors)
    check_release_checklist(metadata, version, errors)

    if errors:
        print("Release metadata validation failed:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print(f"Release metadata is consistent for {metadata['tag']}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
