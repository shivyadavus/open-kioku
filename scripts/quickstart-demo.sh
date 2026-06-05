#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OK_BIN="${OK_BIN:-$ROOT_DIR/target/release/ok}"
if [[ ! -x "$OK_BIN" ]]; then
  OK_BIN="${OK_BIN:-ok}"
fi

DEMO_REPO="${DEMO_REPO:-$ROOT_DIR/open-kioku-demo}"
PLAN_JSON="${PLAN_JSON:-/tmp/open-kioku-quickstart-plan.json}"

echo "+ $OK_BIN demo --path $DEMO_REPO --force"
"$OK_BIN" demo --path "$DEMO_REPO" --force

echo
echo "+ $OK_BIN --repo $DEMO_REPO plan token --format markdown --limit 6"
"$OK_BIN" --repo "$DEMO_REPO" plan token --format markdown --limit 6 | sed -n '1,80p'

echo
echo "+ $OK_BIN --repo $DEMO_REPO --json plan token > $PLAN_JSON"
"$OK_BIN" --repo "$DEMO_REPO" --json plan token >"$PLAN_JSON"

echo
echo "+ $OK_BIN --repo $DEMO_REPO --json verify --plan $PLAN_JSON --changed src/auth.rs"
"$OK_BIN" --repo "$DEMO_REPO" --json verify --plan "$PLAN_JSON" --changed src/auth.rs

