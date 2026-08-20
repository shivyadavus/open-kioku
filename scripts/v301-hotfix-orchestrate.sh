#!/usr/bin/env bash
set -euo pipefail

REPO="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
GH_TOKEN="${GH_TOKEN:?GH_TOKEN is required}"
RELEASE_BRANCH="release/v3.0.1"
RELEASE_TAG="v3.0.1"
FIX_SHA="e6f09f04d4000bec618f00aac426191aff4a76b5"

api() {
  local method="$1"
  local path="$2"
  local body="${3:-}"
  if [[ -n "$body" ]]; then
    curl --fail-with-body --silent --show-error \
      -X "$method" \
      -H 'Accept: application/vnd.github+json' \
      -H "Authorization: Bearer $GH_TOKEN" \
      -H 'X-GitHub-Api-Version: 2022-11-28' \
      -H 'Content-Type: application/json' \
      "https://api.github.com/repos/$REPO$path" \
      -d "$body"
  else
    curl --fail-with-body --silent --show-error \
      -X "$method" \
      -H 'Accept: application/vnd.github+json' \
      -H "Authorization: Bearer $GH_TOKEN" \
      -H 'X-GitHub-Api-Version: 2022-11-28' \
      "https://api.github.com/repos/$REPO$path"
  fi
}

git fetch origin \
  '+refs/heads/main:refs/remotes/origin/main' \
  '+refs/heads/release/v3.0.1:refs/remotes/origin/release/v3.0.1' \
  --tags
git checkout -B "$RELEASE_BRANCH" "origin/$RELEASE_BRANCH"

if git rev-parse -q --verify "refs/tags/$RELEASE_TAG" >/dev/null; then
  echo "$RELEASE_TAG already exists locally; skipping candidate mutation"
else
  current_version="$(python3 - <<'PY'
import tomllib
print(tomllib.load(open('Cargo.toml','rb'))['workspace']['package']['version'])
PY
)"

  if [[ "$current_version" == "3.0.0" ]]; then
    test "$(git rev-parse HEAD)" = "$FIX_SHA"

    # Keep product source pinned to the tested fix while carrying only release
    # orchestration and release-metadata changes on the patch branch.
    git show origin/main:.github/workflows/release.yml > .github/workflows/release.yml
    sed -i 's/Generate release trust artifacts/Generate SHA256SUMS SBOM.cargo-metadata.json PROVENANCE.json THIRD_PARTY_NOTICES.md/' .github/workflows/release.yml

    python3 - <<'PY'
from pathlib import Path
path = Path('Cargo.toml')
text = path.read_text(encoding='utf-8')
old = 'version = "3.0.0"'
if old not in text:
    raise SystemExit('release branch is not at workspace version 3.0.0')
path.write_text(text.replace(old, 'version = "3.0.1"', 1), encoding='utf-8')
PY

    scripts/sync-version.sh

    python3 - <<'PY'
from pathlib import Path
path = Path('CHANGELOG.md')
text = path.read_text(encoding='utf-8')
if '## [3.0.1]' not in text:
    section = '''## [3.0.1] — 2026-08-19

### Fixed
- Fixed repository discovery so nested `.gitignore` rules stay scoped to their declaring directory instead of excluding unrelated source trees.
- Made Git authoritative for ignore decisions in Git repositories, preserving tracked-file semantics, nested negation, and standard Git excludes.
- Scoped nested `.okignore` rules correctly, protected large ignore batches from pipe deadlocks, and added a warning when filtering collapses a supported-source index to zero.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

'''
    marker = '## [3.0.0] — 2026-08-17\n'
    if marker not in text:
        raise SystemExit('3.0.0 changelog marker missing')
    text = text.replace(marker, section + marker, 1)
link = '[3.0.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.1'
if link not in text:
    text = text.rstrip() + '\n' + link + '\n'
path.write_text(text, encoding='utf-8')
PY

    cargo metadata --format-version 1 >/dev/null
    scripts/validate-versions.sh
    cargo test -p open-kioku-ingest git_is_authoritative_for_nested_scope_and_tracked_files
    cargo test -p open-kioku-ingest batched_check_drains_large_git_output_without_deadlocking
    cargo test -p open-kioku-ingest --test ignore_discovery

    git config user.name 'github-actions[bot]'
    git config user.email '41898282+github-actions[bot]@users.noreply.github.com'
    git add -A
    git commit -m 'release: prepare 3.0.1 ignore hotfix'
    git push origin HEAD:"$RELEASE_BRANCH"
  elif [[ "$current_version" == "3.0.1" ]]; then
    echo "$RELEASE_BRANCH is already prepared at 3.0.1; validating before retry"
    scripts/validate-versions.sh
  else
    echo "unexpected release branch version: $current_version" >&2
    exit 1
  fi
fi

before_file="$(mktemp)"
api GET '/actions/workflows/release.yml/runs?event=workflow_dispatch&per_page=30' \
  | python3 -c 'import json,sys; [print(r["id"]) for r in json.load(sys.stdin).get("workflow_runs", [])]' \
  > "$before_file"

api POST '/actions/workflows/release.yml/dispatches' '{"ref":"release/v3.0.1"}' >/dev/null

run_id=''
run_url=''
for _ in $(seq 1 60); do
  sleep 3
  payload="$(api GET '/actions/workflows/release.yml/runs?event=workflow_dispatch&per_page=30')"
  result="$(PAYLOAD="$payload" python3 - "$before_file" <<'PY'
import json, os, sys
before = set(open(sys.argv[1], encoding='utf-8').read().split())
data = json.loads(os.environ['PAYLOAD'])
fresh = [r for r in data.get('workflow_runs', []) if str(r['id']) not in before]
if fresh:
    run = max(fresh, key=lambda r: r['id'])
    print(f"{run['id']} {run['html_url']}")
PY
)"
  if [[ -n "$result" ]]; then
    read -r run_id run_url <<<"$result"
    break
  fi
done

if [[ -z "$run_id" ]]; then
  echo 'release dispatch succeeded but no workflow run appeared' >&2
  exit 1
fi

echo "release run: $run_url"
for _ in $(seq 1 480); do
  payload="$(api GET "/actions/runs/$run_id")"
  status="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))' <<<"$payload")"
  if [[ "$status" == 'completed' ]]; then
    conclusion="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("conclusion") or "unknown")' <<<"$payload")"
    echo "release conclusion: $conclusion"
    [[ "$conclusion" == 'success' ]] || exit 1
    exit 0
  fi
  sleep 15
done

echo "release run did not complete in time: $run_url" >&2
exit 1
