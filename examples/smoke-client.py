#!/usr/bin/env python3
"""Smoke a client starter integration against the local Open Kioku MCP server."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def run(cmd: list[str], *, input_text: str | None = None, timeout: int = 90) -> str:
    try:
        result = subprocess.run(
            cmd,
            input=input_text,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        raise SystemExit(f"command timed out after {timeout}s: {' '.join(cmd)}") from exc
    if result.returncode != 0:
        raise SystemExit(
            f"command failed: {' '.join(cmd)}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}"
        )
    return result.stdout


def call_mcp(ok: str, repo: Path, name: str, arguments: dict) -> dict:
    request = {
        "jsonrpc": "2.0",
        "id": name,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }
    raw = run([ok, "--repo", str(repo), "mcp", "serve"], input_text=json.dumps(request) + "\n")
    for line in raw.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        response = json.loads(line)
        if response.get("id") == name:
            if "error" in response:
                raise SystemExit(f"MCP tool {name} failed: {response['error']}")
            return response
    raise SystemExit(f"MCP tool {name} did not return a JSON-RPC response:\n{raw}")


def assert_contains(value: object, needle: str, label: str) -> None:
    text = json.dumps(value, sort_keys=True)
    if needle not in text:
        raise SystemExit(f"{label} did not contain {needle!r}:\n{text[:2000]}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("client", choices=["cursor", "claude"])
    parser.add_argument("--repo", type=Path)
    parser.add_argument("--task", default="token")
    parser.add_argument("--changed-path", default="src/auth.rs")
    parser.add_argument("--ok-bin", default=os.environ.get("OK_BIN", "ok"))
    args = parser.parse_args()

    temp_dir: tempfile.TemporaryDirectory[str] | None = None
    repo = args.repo
    if repo is None:
        temp_dir = tempfile.TemporaryDirectory(prefix=f"open-kioku-{args.client}-")
        repo = Path(temp_dir.name) / "demo"
        run([args.ok_bin, "demo", "--path", str(repo), "--force"])

    install = run([args.ok_bin, "mcp", "install", args.client, "--repo", str(repo)])
    assert_contains(install, "open-kioku", "install snippet")
    if args.client == "cursor":
        assert_contains(install, "Cursor", "Cursor install snippet")
    else:
        assert_contains(install, "Claude", "Claude install snippet")

    plan_json = run([args.ok_bin, "--repo", str(repo), "--json", "plan", args.task])
    json.loads(plan_json)

    plan = call_mcp(
        args.ok_bin,
        repo,
        "plan_change",
        {"task": args.task, "format": "json", "limit": 5},
    )
    assert_contains(plan, "primary_context", "plan_change")

    impact = call_mcp(
        args.ok_bin,
        repo,
        "impact_analysis",
        {"path": args.changed_path, "limit": 5},
    )
    assert_contains(impact, "risk_report", "impact_analysis")

    tests = call_mcp(
        args.ok_bin,
        repo,
        "find_tests_for_change",
        {"path": args.changed_path, "limit": 5},
    )
    assert_contains(tests, "command", "find_tests_for_change")

    verify = call_mcp(
        args.ok_bin,
        repo,
        "verify_change",
        {"plan_json": plan_json, "changed_files": [args.changed_path]},
    )
    assert_contains(verify, "verdict", "verify_change")

    print(f"{args.client} starter smoke passed for {repo}")
    if temp_dir is not None:
        temp_dir.cleanup()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
