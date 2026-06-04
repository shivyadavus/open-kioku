#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/publish-crates.sh [--dry-run|--publish]

Publishes Open Kioku workspace crates to crates.io in dependency order.

Environment:
  EXPECTED_VERSION       Optional version guard, for example 1.0.1.
  PUBLISH_ALLOW_DIRTY    Set to 1 to permit a dirty worktree during dry-run.
  CARGO_REGISTRY_TOKEN   Required by cargo publish unless cargo is already logged in.
EOF
}

MODE="${1:---dry-run}"
case "$MODE" in
  --dry-run | --publish) ;;
  -h | --help)
    usage
    exit 0
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"
if [[ -n "${EXPECTED_VERSION:-}" && "$VERSION" != "$EXPECTED_VERSION" ]]; then
  echo "Expected version $EXPECTED_VERSION, found $VERSION" >&2
  exit 1
fi

if [[ "$MODE" == "--publish" && -z "${CARGO_REGISTRY_TOKEN:-}" && "${CI:-0}" == "true" ]]; then
  echo "CARGO_REGISTRY_TOKEN is required for --publish in CI." >&2
  exit 1
fi

if [[ "${PUBLISH_ALLOW_DIRTY:-0}" != "1" ]]; then
  if [[ -n "$(git status --porcelain)" ]]; then
    echo "Working tree is dirty. Commit first or set PUBLISH_ALLOW_DIRTY=1 for local dry-runs." >&2
    exit 1
  fi
fi

CARGO_PUBLISH_ARGS=(--locked)
if [[ "${PUBLISH_ALLOW_DIRTY:-0}" == "1" ]]; then
  CARGO_PUBLISH_ARGS+=(--allow-dirty)
fi

ORDER="$(mktemp)"
python3 - <<'PY' > "$ORDER"
import json
import subprocess

metadata = json.loads(
    subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"])
)
workspace_ids = set(metadata["workspace_members"])
packages = {pkg["id"]: pkg for pkg in metadata["packages"] if pkg["id"] in workspace_ids}
by_name = {pkg["name"]: pkg for pkg in packages.values()}
seen = set()
order = []

def visit(package_id):
    if package_id in seen:
        return
    seen.add(package_id)
    package = packages[package_id]
    for dep in package["dependencies"]:
        if dep.get("path") and dep["name"] in by_name:
            visit(by_name[dep["name"]]["id"])
    order.append(package["name"])

for member_id in metadata["workspace_members"]:
    visit(member_id)

print("\n".join(order))
PY

crate_version_exists() {
  local crate="$1"
  local version="$2"
  curl --fail --silent --show-error \
    --user-agent "open-kioku-publish-script" \
    "https://crates.io/api/v1/crates/${crate}/${version}" >/dev/null 2>&1
}

wait_for_crate_version() {
  local crate="$1"
  local version="$2"
  for _ in {1..30}; do
    if crate_version_exists "$crate" "$version"; then
      return 0
    fi
    sleep 10
  done
  echo "Timed out waiting for ${crate} ${version} to appear in crates.io." >&2
  return 1
}

internal_deps_missing_from_registry() {
  local crate="$1"
  python3 - "$crate" "$VERSION" <<'PY'
import json
import subprocess
import sys
import urllib.error
import urllib.request

crate = sys.argv[1]
version = sys.argv[2]
metadata = json.loads(
    subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"])
)
members = {pkg["name"]: pkg for pkg in metadata["packages"] if pkg["id"] in set(metadata["workspace_members"])}
package = members[crate]
missing = []
for dep in package["dependencies"]:
    if dep.get("path") and dep["name"] in members:
        url = f"https://crates.io/api/v1/crates/{dep['name']}/{version}"
        request = urllib.request.Request(url, headers={"User-Agent": "open-kioku-publish-script"})
        try:
            urllib.request.urlopen(request, timeout=10).close()
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                missing.append(dep["name"])
            else:
                raise
if missing:
    print(",".join(missing))
PY
}

while IFS= read -r crate; do
  [[ -n "$crate" ]] || continue

  if crate_version_exists "$crate" "$VERSION"; then
    echo "== ${crate} ${VERSION} already exists on crates.io; skipping =="
    continue
  fi

  if [[ "$MODE" == "--dry-run" ]]; then
    missing="$(internal_deps_missing_from_registry "$crate")"
    if [[ -n "$missing" ]]; then
      echo "== dry-run ${crate}: skipped until dependencies are published (${missing}) =="
      continue
    fi
    echo "== dry-run ${crate} =="
    cargo publish --dry-run "${CARGO_PUBLISH_ARGS[@]}" -p "$crate"
  else
    echo "== publish ${crate} ${VERSION} =="
    cargo publish "${CARGO_PUBLISH_ARGS[@]}" -p "$crate"
    wait_for_crate_version "$crate" "$VERSION"
  fi
done < "$ORDER"

rm -f "$ORDER"
