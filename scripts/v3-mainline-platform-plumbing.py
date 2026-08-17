#!/usr/bin/env python3
from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text(encoding="utf-8")
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one anchor, found {count}")
    p.write_text(text.replace(old, new, 1), encoding="utf-8")


release = ".github/workflows/release.yml"
replace_once(release, "            dist/ok-macos-x86_64\n", "")
replace_once(release, "          cp dist/ok-macos-x86_64 packages/npm-darwin-x64/ok\n", "")
replace_once(release, "          chmod +x packages/npm-darwin-x64/ok\n", "")

candidate = ".github/workflows/v3-mainline-candidate.yml"
replace_once(
    candidate,
    "          for name in ['ok-macos-arm64','ok-macos-x86_64','ok-linux-arm64','ok-linux-x86_64']:\n",
    "          for name in ['ok-macos-arm64','ok-linux-arm64','ok-linux-x86_64']:\n",
)
