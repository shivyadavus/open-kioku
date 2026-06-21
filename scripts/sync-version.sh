#!/usr/bin/env bash
# sync-version.sh
# Reads the canonical version from Cargo.toml [workspace.package]
# and writes it into every marketplace and release manifest.
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

sync_json_version() {
  local file="$1"
  sed -i.bak "s/\"version\": \"[^\"]*\"/\"version\": \"$VERSION\"/g" "$file"
  rm -f "${file}.bak"
}

# ── Cursor: .cursor-plugin/plugin.json ───────────────────────────────────────
PLUGIN_JSON="$ROOT/.cursor-plugin/plugin.json"
if [[ -f "$PLUGIN_JSON" ]]; then
  sync_json_version "$PLUGIN_JSON"
  echo "  ✓ .cursor-plugin/plugin.json"
fi

# ── Cursor: .cursor-plugin/marketplace.json ──────────────────────────────────
MARKETPLACE_JSON="$ROOT/.cursor-plugin/marketplace.json"
if [[ -f "$MARKETPLACE_JSON" ]]; then
  sync_json_version "$MARKETPLACE_JSON"
  echo "  ✓ .cursor-plugin/marketplace.json"
fi

# ── Claude: claude_plugin.json (if present) ───────────────────────────────────
CLAUDE_JSON_UNDERSCORE="$ROOT/claude_plugin.json"
if [[ -f "$CLAUDE_JSON_UNDERSCORE" ]]; then
  sync_json_version "$CLAUDE_JSON_UNDERSCORE"
  echo "  ✓ claude_plugin.json"
fi

CLAUDE_JSON="$ROOT/claude-plugin.json"
if [[ -f "$CLAUDE_JSON" ]]; then
  sync_json_version "$CLAUDE_JSON"
  echo "  ✓ claude-plugin.json"
fi

# ── Claude plugin folder JSONs ───────────────────────────────────────────────
CLAUDE_DIR_JSON1="$ROOT/.claude-plugin/plugin.json"
if [[ -f "$CLAUDE_DIR_JSON1" ]]; then
  sync_json_version "$CLAUDE_DIR_JSON1"
  echo "  ✓ .claude-plugin/plugin.json"
fi

CLAUDE_DIR_JSON2="$ROOT/.claude-plugin/marketplace.json"
if [[ -f "$CLAUDE_DIR_JSON2" ]]; then
  sync_json_version "$CLAUDE_DIR_JSON2"
  echo "  ✓ .claude-plugin/marketplace.json"
fi

# ── Codex plugin folder JSONs ────────────────────────────────────────────────
CODEX_DIR_JSON="$ROOT/.codex-plugin/plugin.json"
if [[ -f "$CODEX_DIR_JSON" ]]; then
  sync_json_version "$CODEX_DIR_JSON"
  echo "  ✓ .codex-plugin/plugin.json"
fi

# ── NPM packages ──────────────────────────────────────────────────────────────
shopt -s nullglob
for NPM_JSON in "$ROOT"/packages/npm*/package.json; do
  sync_json_version "$NPM_JSON"
  sed -i.bak "s/\"@open-kioku\/\([^\"]*\)\": \"[^\"]*\"/\"@open-kioku\/\1\": \"$VERSION\"/g" "$NPM_JSON"
  rm -f "${NPM_JSON}.bak"
  echo "  ✓ ${NPM_JSON#$ROOT/}"
done
shopt -u nullglob

# ── Release metadata ─────────────────────────────────────────────────────────
RELEASE_JSON="$ROOT/release-metadata.json"
if [[ -f "$RELEASE_JSON" ]]; then
  sync_json_version "$RELEASE_JSON"
  sed -i.bak "s/\"tag\": \"v[^\"]*\"/\"tag\": \"v$VERSION\"/g" "$RELEASE_JSON"
  rm -f "${RELEASE_JSON}.bak"
  echo "  ✓ release-metadata.json"
fi

# ── Homebrew formula version and release URLs ────────────────────────────────
FORMULA="$ROOT/Formula/open-kioku.rb"
if [[ -f "$FORMULA" ]]; then
  sed -i.bak -E "s/version \"[^\"]+\"/version \"$VERSION\"/g; s/releases\\/download\\/v[0-9]+\\.[0-9]+\\.[0-9]+/releases\\/download\\/v$VERSION/g" "$FORMULA"
  rm -f "${FORMULA}.bak"
  echo "  ✓ Formula/open-kioku.rb"
fi

# ── Release checklist ────────────────────────────────────────────────────────
CHECKLIST="$ROOT/docs/release-checklist.md"
if [[ -f "$CHECKLIST" ]]; then
  sed -i.bak -E "s/version is \`[^\`]+\`/version is \`$VERSION\`/g; s/tag \`v[^\`]+\`/tag \`v$VERSION\`/g; s/tag is exactly \`v[^\`]+\`/tag is exactly \`v$VERSION\`/g; s/has a \`[^\`]+\` section/has a \`$VERSION\` section/g; s/matching \`\[[^\`]+\]\`/matching \`\[$VERSION\]\`/g" "$CHECKLIST"
  rm -f "${CHECKLIST}.bak"
  echo "  ✓ docs/release-checklist.md"
fi

echo "Done. All manifests are at $VERSION."
