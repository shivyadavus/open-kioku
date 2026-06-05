#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OK_BIN="${OK_BIN:-$ROOT_DIR/target/debug/ok}"

python3 "$ROOT_DIR/examples/smoke-client.py" cursor --ok-bin "$OK_BIN" "$@"

