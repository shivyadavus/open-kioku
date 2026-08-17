#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [[ "${1:-}" == "--static" ]]; then
  static_only=true
elif [[ -n "${1:-}" ]]; then
  echo "usage: $0 [--static]" >&2
  exit 2
else
  static_only=false
fi

site="demo/index.html"
readme="README.md"

require_site_text() {
  local expected="$1"
  if ! grep -Fq -- "$expected" "$site"; then
    echo "public quickstart is missing: $expected" >&2
    exit 1
  fi
}

reject_site_text() {
  local unexpected="$1"
  if grep -Fq -- "$unexpected" "$site"; then
    echo "public quickstart advertises an unsupported or stale command: $unexpected" >&2
    exit 1
  fi
}

# The homepage is product-first: it must expose installation and the real plan
# path, but it does not need to spell out lower-level init/index plumbing.
for command in \
  'npm install -g open-kioku' \
  'ok plan "change token expiration"'; do
  require_site_text "$command"
done

# Keep the homepage off retired/unsupported public flows.
reject_site_text 'ok preflight '
for stale_claim in 'explicit write controls' 'Patch and command paths are opt-in'; do
  reject_site_text "$stale_claim"
done

# The README owns the canonical first-win onboarding contract. `setup agent`
# indexes the repo and installs repository-scoped MCP/guidance in one flow.
readme_quickstart=$'npm install -g open-kioku\nok setup agent cursor --repo . --apply'
if ! grep -Fq -- "$readme_quickstart" "$readme"; then
  echo "README.md first-win commands are stale" >&2
  exit 1
fi

readme_first_win="$(sed -n '/^## First Win:/,/^## /p' "$readme")"
for unsupported in 'ok preflight ' 'preflight_change'; do
  if grep -Fq -- "$unsupported" <<<"$readme_first_win"; then
    echo "README.md first-win documentation advertises an unsupported published command: $unsupported" >&2
    exit 1
  fi
done

# Preserve the actual public security posture without coupling CI to one exact
# sentence that marketing copy may legitimately rewrite.
for claim in \
  'Read-only by default' \
  'No hosted index' \
  'source edits remain in the normal editor and agent workflow.' \
  'network denial'; do
  require_site_text "$claim"
done

# Keep the install CTA copyable without pinning CI to an obsolete four-command
# block. The current homepage intentionally copies installation separately.
require_site_text 'data-copy="npm install -g open-kioku"'

if [[ "$static_only" == true ]]; then
  echo "public quickstart static contract passed"
  exit 0
fi

ok_bin="${OK_BIN:-target/debug/ok}"
if [[ ! -x "$ok_bin" ]]; then
  echo "public quickstart validation requires an executable CLI at $ok_bin" >&2
  exit 1
fi

# Canonical first-win commands must remain executable.
"$ok_bin" setup agent cursor --help >/dev/null
"$ok_bin" setup agent claude --help >/dev/null
"$ok_bin" plan --help >/dev/null

# Lower-level/manual commands remain supported CLI surface, even though the
# homepage no longer has to advertise them as onboarding steps.
"$ok_bin" init --help >/dev/null
"$ok_bin" index --help >/dev/null
"$ok_bin" mcp install cursor --help >/dev/null
"$ok_bin" mcp install claude --help >/dev/null

echo "public quickstart command contract passed"
