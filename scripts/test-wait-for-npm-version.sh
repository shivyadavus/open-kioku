#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

cat > "$TMP/npm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

STATE_FILE="${FAKE_NPM_STATE_FILE:?}"
VISIBLE_AFTER="${FAKE_NPM_VISIBLE_AFTER:?}"
EXPECTED="${FAKE_NPM_EXPECTED_VERSION:?}"
count=0
if [[ -f "$STATE_FILE" ]]; then
  count="$(cat "$STATE_FILE")"
fi
count=$((count + 1))
printf '%s\n' "$count" > "$STATE_FILE"
if (( count >= VISIBLE_AFTER )); then
  printf '%s\n' "$EXPECTED"
  exit 0
fi
exit 1
EOF
chmod +x "$TMP/npm"

STATE="$TMP/state"
FAKE_NPM_STATE_FILE="$STATE" \
FAKE_NPM_VISIBLE_AFTER=3 \
FAKE_NPM_EXPECTED_VERSION=9.8.7 \
NPM_BIN="$TMP/npm" \
NPM_VIEW_ATTEMPTS=4 \
NPM_VIEW_DELAY_SECONDS=0 \
  bash "$ROOT/scripts/wait-for-npm-version.sh" open-kioku 9.8.7

test "$(cat "$STATE")" = "3"

rm -f "$STATE"
set +e
FAKE_NPM_STATE_FILE="$STATE" \
FAKE_NPM_VISIBLE_AFTER=10 \
FAKE_NPM_EXPECTED_VERSION=9.8.7 \
NPM_BIN="$TMP/npm" \
NPM_VIEW_ATTEMPTS=2 \
NPM_VIEW_DELAY_SECONDS=0 \
  bash "$ROOT/scripts/wait-for-npm-version.sh" open-kioku 9.8.7 >"$TMP/exhaust.out" 2>"$TMP/exhaust.err"
status=$?
set -e
if [[ "$status" -eq 0 ]]; then
  echo "expected npm propagation verification to fail closed on exhaustion" >&2
  exit 1
fi
grep -q "Timed out waiting for open-kioku@9.8.7" "$TMP/exhaust.err"
test "$(cat "$STATE")" = "2"

set +e
NPM_VIEW_ATTEMPTS=0 bash "$ROOT/scripts/wait-for-npm-version.sh" open-kioku 9.8.7 >"$TMP/invalid.out" 2>"$TMP/invalid.err"
status=$?
set -e
if [[ "$status" -ne 2 ]]; then
  echo "expected invalid attempt configuration to exit 2, got $status" >&2
  exit 1
fi
grep -q "NPM_VIEW_ATTEMPTS must be a positive integer" "$TMP/invalid.err"

echo "npm propagation verification tests passed"
