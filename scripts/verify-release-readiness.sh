#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OK_BIN="${OK_BIN:-$ROOT/target/debug/ok}"
SMOKE_REPO="${SMOKE_REPO:-/tmp/open-kioku-release-smoke}"

if [[ ! -x "$OK_BIN" ]]; then
  cargo build -p open-kioku-cli --manifest-path "$ROOT/Cargo.toml"
fi

rm -rf "$SMOKE_REPO"

"$OK_BIN" demo --path "$SMOKE_REPO" --force >/tmp/open-kioku-demo.out

status_md="$("$OK_BIN" status "$SMOKE_REPO" --markdown)"
setup_md="$("$OK_BIN" setup audit "$SMOKE_REPO" --markdown)"
plan_toon="$("$OK_BIN" --repo "$SMOKE_REPO" plan token --format toon --limit 8)"
proof_md="$("$OK_BIN" prove "$SMOKE_REPO" --task token --limit 8)"
mcp_cursor="$("$OK_BIN" mcp install cursor --repo "$SMOKE_REPO")"
mcp_codex="$("$OK_BIN" mcp install codex --repo "$SMOKE_REPO")"

assert_contains() {
  local haystack="$1"
  local needle="$2"
  local label="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "release readiness failed: expected $label to contain: $needle" >&2
    exit 1
  fi
}

assert_contains "$status_md" "# Open Kioku Status" "status markdown"
assert_contains "$status_md" "Static analysis facts" "status markdown"
assert_contains "$setup_md" "## Quality Signals" "setup audit"
assert_contains "$setup_md" "## Advanced Providers" "setup audit"
assert_contains "$plan_toon" "validation[" "toon plan"
assert_contains "$plan_toon" "find_tests_for_change" "toon plan"
assert_contains "$proof_md" "## What Was Checked" "proof report"
assert_contains "$proof_md" "Average proof score" "proof report"
assert_contains "$mcp_cursor" "\"open-kioku\"" "cursor MCP config"
assert_contains "$mcp_cursor" "\"command\": \"ok\"" "cursor MCP config"
assert_contains "$mcp_codex" "[mcp_servers.open-kioku]" "codex MCP config"

echo "release readiness smoke passed for $SMOKE_REPO"
