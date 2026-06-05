#!/usr/bin/env bash
# validate-versions.sh
# Asserts that every package, marketplace, release, and install-channel
# manifest matches Cargo.toml. Run in CI to catch drift before release.

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
PYTHON="${PYTHON:-}"
if [[ -z "$PYTHON" ]]; then
  if command -v python3 >/dev/null 2>&1; then
    PYTHON=python3
  elif command -v python >/dev/null 2>&1; then
    PYTHON=python
  else
    echo "ERROR: Python 3.11+ is required to validate release metadata." >&2
    exit 1
  fi
fi

"$PYTHON" - <<'PY'
import sys
if sys.version_info < (3, 11):
    raise SystemExit("ERROR: Python 3.11+ is required to validate release metadata.")
PY

echo "Validating release metadata against Cargo.toml..."
"$PYTHON" "$ROOT/scripts/validate-release-metadata.py"
echo "All release manifests match."
