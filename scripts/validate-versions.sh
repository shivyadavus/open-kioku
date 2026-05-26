#!/usr/bin/env bash
# validate-versions.sh
# Asserts that every marketplace manifest version matches Cargo.toml.
# Run in CI to catch a version mismatch before it reaches the marketplace.

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
CARGO="$ROOT/Cargo.toml"

CARGO_VERSION=$(grep -m1 '^version' "$CARGO" | sed 's/version = "//;s/"//')
EXIT=0

check_version() {
  local file="$1"
  if [[ ! -f "$file" ]]; then
    return
  fi
  local file_version
  file_version=$(grep -m1 '"version"' "$file" | sed 's/.*"version": "//;s/".*//')
  if [[ "$file_version" != "$CARGO_VERSION" ]]; then
    echo "VERSION MISMATCH: $file has $file_version, Cargo.toml has $CARGO_VERSION"
    EXIT=1
  else
    echo "  ✓ $file ($file_version)"
  fi
}

echo "Validating manifest versions against Cargo.toml ($CARGO_VERSION)..."
check_version "$ROOT/.cursor-plugin/plugin.json"
check_version "$ROOT/.cursor-plugin/marketplace.json"
check_version "$ROOT/claude-plugin.json"

if [[ $EXIT -ne 0 ]]; then
  echo ""
  echo "Run scripts/sync-version.sh to fix mismatches."
  exit 1
fi

echo "All manifest versions match."
