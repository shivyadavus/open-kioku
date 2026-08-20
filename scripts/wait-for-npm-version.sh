#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "Usage: scripts/wait-for-npm-version.sh <package> <version>" >&2
  exit 2
fi

PACKAGE="$1"
EXPECTED_VERSION="$2"
ATTEMPTS="${NPM_VIEW_ATTEMPTS:-30}"
DELAY_SECONDS="${NPM_VIEW_DELAY_SECONDS:-10}"
NPM_BIN="${NPM_BIN:-npm}"

if [[ ! "$ATTEMPTS" =~ ^[1-9][0-9]*$ ]]; then
  echo "NPM_VIEW_ATTEMPTS must be a positive integer; got '$ATTEMPTS'" >&2
  exit 2
fi
if [[ ! "$DELAY_SECONDS" =~ ^[0-9]+$ ]]; then
  echo "NPM_VIEW_DELAY_SECONDS must be a non-negative integer; got '$DELAY_SECONDS'" >&2
  exit 2
fi

for ((attempt = 1; attempt <= ATTEMPTS; attempt++)); do
  actual="$($NPM_BIN view "${PACKAGE}@${EXPECTED_VERSION}" version 2>/dev/null || true)"
  if [[ "$actual" == "$EXPECTED_VERSION" ]]; then
    echo "Verified ${PACKAGE}@${EXPECTED_VERSION} on npm registry (attempt ${attempt}/${ATTEMPTS})."
    exit 0
  fi

  if (( attempt < ATTEMPTS )); then
    echo "${PACKAGE}@${EXPECTED_VERSION} not visible yet (attempt ${attempt}/${ATTEMPTS}); retrying in ${DELAY_SECONDS}s..."
    sleep "$DELAY_SECONDS"
  fi
done

echo "Timed out waiting for ${PACKAGE}@${EXPECTED_VERSION} to become visible on npm after ${ATTEMPTS} attempts." >&2
exit 1
