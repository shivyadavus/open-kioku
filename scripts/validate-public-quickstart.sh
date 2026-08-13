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
    echo "public quickstart advertises an unsupported command: $unexpected" >&2
    exit 1
  fi
}

for command in \
  'npm install -g open-kioku' \
  'ok init .' \
  'ok index .' \
  'ok plan "change token expiration"' \
  'ok mcp install cursor --repo /work/acme-api' \
  'ok mcp install claude --repo /work/acme-api'; do
  require_site_text "$command"
done

reject_site_text 'ok setup agent '
reject_site_text 'ok preflight '

expected_copy_button=$'data-copy="npm install -g open-kioku\nok init .\nok index .\nok plan &quot;change token expiration&quot;"'
copy_button_count="$(python3 - "$site" "$expected_copy_button" <<'PY'
import sys
from pathlib import Path

print(Path(sys.argv[1]).read_text(encoding="utf-8").count(sys.argv[2]))
PY
)"
if [[ "$copy_button_count" != "2" ]]; then
  echo "public quickstart must expose exactly two matching copyable command blocks; found $copy_button_count" >&2
  exit 1
fi

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

for stale_claim in 'explicit write controls' 'Patch and command paths are opt-in'; do
  reject_site_text "$stale_claim"
done

require_site_text 'Open Kioku does not upload source or edit source files.'
require_site_text 'Source edits stay in your normal editor. Command execution is opt-in and policy-controlled.'

if [[ "$static_only" == true ]]; then
  echo "public quickstart static contract passed"
  exit 0
fi

ok_bin="${OK_BIN:-target/debug/ok}"
if [[ ! -x "$ok_bin" ]]; then
  echo "public quickstart validation requires an executable CLI at $ok_bin" >&2
  exit 1
fi

"$ok_bin" init --help >/dev/null
"$ok_bin" index --help >/dev/null
"$ok_bin" plan --help >/dev/null
"$ok_bin" mcp install cursor --help >/dev/null
"$ok_bin" mcp install claude --help >/dev/null

echo "public quickstart command contract passed"
