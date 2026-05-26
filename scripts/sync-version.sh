#!/usr/bin/env bash
# sync-version.sh
# Reads the canonical version from Cargo.toml [workspace.package]
# and writes it into every marketplace manifest.
# Safe to run repeatedly (idempotent).

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
CARGO="$ROOT/Cargo.toml"

# ── Read canonical version ────────────────────────────────────────────────────
VERSION=$(grep -m1 '^version' "$CARGO" | sed 's/version = "//;s/"//')

if [[ -z "$VERSION" ]]; then
  echo "ERROR: could not read version from $CARGO" >&2
  exit 1
fi

echo "Syncing version $VERSION to marketplace manifests..."

# ── Cursor: .cursor-plugin/plugin.json ───────────────────────────────────────
PLUGIN_JSON="$ROOT/.cursor-plugin/plugin.json"
if [[ -f "$PLUGIN_JSON" ]]; then
  # Replace the "version": "x.y.z" line in-place
  sed -i.bak "s/\"version\": \"[0-9]*\.[0-9]*\.[0-9]*\"/\"version\": \"$VERSION\"/g" "$PLUGIN_JSON"
  rm -f "${PLUGIN_JSON}.bak"
  echo "  ✓ .cursor-plugin/plugin.json"
fi

# ── Cursor: .cursor-plugin/marketplace.json ──────────────────────────────────
MARKETPLACE_JSON="$ROOT/.cursor-plugin/marketplace.json"
if [[ -f "$MARKETPLACE_JSON" ]]; then
  sed -i.bak "s/\"version\": \"[0-9]*\.[0-9]*\.[0-9]*\"/\"version\": \"$VERSION\"/g" "$MARKETPLACE_JSON"
  rm -f "${MARKETPLACE_JSON}.bak"
  echo "  ✓ .cursor-plugin/marketplace.json"
fi

# ── Claude: claude-plugin.json (if present) ───────────────────────────────────
CLAUDE_JSON="$ROOT/claude-plugin.json"
if [[ -f "$CLAUDE_JSON" ]]; then
  sed -i.bak "s/\"version\": \"[0-9]*\.[0-9]*\.[0-9]*\"/\"version\": \"$VERSION\"/g" "$CLAUDE_JSON"
  rm -f "${CLAUDE_JSON}.bak"
  echo "  ✓ claude-plugin.json"
fi

echo "Done. All manifests are at $VERSION."
