#!/usr/bin/env bash
# Replays one real commit of psf/black halfway and prints what Open Kioku says about it.
#
# The commit, 1ed66a8b33 "Add --cache-dir command-line option (#5433)", gave the function
# `get_cache_file` a second required parameter and updated its callers. This script checks
# out the commit's parent, takes from the commit only the file that declares the function,
# and runs `ok ripple` on that half-made change. The commit is its own answer key: the
# script then counts how many of the lines ripple named the commit went on to change.
# Last, it commits the half and judges it the way CI judges a pull request.
#
# Usage: scripts/demo-replay.sh [--check] [--write] [--dir <empty directory>]
#   OK_BIN    the `ok` binary to run (default: `ok` on PATH)
#   --check   compare what was printed with demo/proof/replay-black/ and fail on a difference
#   --write   record what was printed into demo/proof/replay-black/
#   --dir     where to clone (default: a new temporary directory, removed afterwards)
#
# Needs git, python3 and network access to github.com. Nothing is written outside the clone.
set -euo pipefail

REPOSITORY=https://github.com/psf/black
COMMIT=1ed66a8b3385a000508a03158ac3dc2a8a75a209
DECLARING_FILE=src/black/cache.py
TASK="Add --cache-dir command-line option"

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
recorded="$here/demo/proof/replay-black"
ok_bin="${OK_BIN:-ok}"
case "$ok_bin" in */*) ok_bin="$(cd "$(dirname "$ok_bin")" && pwd)/$(basename "$ok_bin")" ;; esac

mode=show
dir=
while [ $# -gt 0 ]; do
  case "$1" in
    --check) mode=check ;;
    --write) mode=write ;;
    --dir) dir="$2"; shift ;;
    -h|--help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
  shift
done

if [ -z "$dir" ]; then
  dir="$(mktemp -d)"
  trap 'rm -rf "$dir"' EXIT
fi
out="$dir/out"
clone="$dir/black"
mkdir -p "$out"

say() { printf '\n== %s ==\n' "$1"; }
run() { printf '$ %s\n' "$*"; }

say "1. The repository as it was before the commit"
run "git clone $REPOSITORY && cd black"
git init -q "$clone"
cd "$clone"
git remote add origin "$REPOSITORY"
fetched=
for attempt in 1 2 3; do
  if git fetch -q --depth 2 origin "$COMMIT"; then fetched=yes; break; fi
  sleep $((attempt * 5))
done
[ -n "$fetched" ] || { echo "could not fetch $COMMIT from $REPOSITORY" >&2; exit 1; }
run "git checkout ${COMMIT:0:10}~1"
git checkout -q --detach "$COMMIT~1"
run "ok index"
"$ok_bin" index > "$out/index.txt" 2>&1
echo "indexed $(git ls-files | wc -l | tr -d ' ') tracked files"

say "2. Half of the commit: only the file that declares the function"
run "git checkout ${COMMIT:0:10} -- $DECLARING_FILE"
git checkout -q "$COMMIT" -- "$DECLARING_FILE"
run "ok ripple"
TIMEFORMAT=%R
{ time "$ok_bin" ripple > "$out/ripple.txt"; } 2> "$out/seconds.txt"
cat "$out/ripple.txt"
echo "($(cat "$out/seconds.txt") s)"

say "3. The answer key: the commit as it landed"
"$ok_bin" ripple --json > "$out/ripple.json"
python3 - "$out/ripple.json" "$COMMIT" > "$out/answer-key.txt" <<'PY'
import json, re, subprocess, sys

report, commit = json.load(open(sys.argv[1])), sys.argv[2]
sites = [(site["path"], site["line"]) for contract in report["contracts"] for site in contract["broken"]]
changed = {}
for path in sorted({path for path, _ in sites}):
    diff = subprocess.run(["git", "diff", "-U0", commit + "~1", commit, "--", path],
                          check=True, capture_output=True, text=True).stdout
    spans = []
    for start, count in re.findall(r"^@@ -(\d+)(?:,(\d+))? ", diff, flags=re.M):
        count = 1 if count == "" else int(count)
        if count:
            spans.append((int(start), int(start) + count - 1))
    changed[path] = spans
for path, line in sites:
    hit = any(low <= line <= high for low, high in changed[path])
    print(f"{path}:{line}  {'changed by the commit' if hit else 'NOT changed by the commit'}")
hits = sum(any(low <= line <= high for low, high in changed[path]) for path, line in sites)
print(f"{hits} of {len(sites)} sites ripple named are on lines the commit went on to change.")
PY
cat "$out/answer-key.txt"

say "4. The same half-made change, judged as a pull request"
git -c user.name=replay -c user.email=replay@example.invalid commit -q -m "half of ${COMMIT:0:10}" "$DECLARING_FILE"
run "ok gate ci --base HEAD~1 --task \"$TASK\""
"$ok_bin" gate ci --base HEAD~1 --task "$TASK" --format markdown > "$out/dossier.md"
awk '/^### Ripple/{on=1} /^### /&&!/^### Ripple/{on=0} on' "$out/dossier.md" > "$out/dossier-ripple.md"
sed -n '1p' "$out/dossier.md"
echo
cat "$out/dossier-ripple.md"

compared="ripple.txt answer-key.txt dossier-ripple.md"
case "$mode" in
  write)
    mkdir -p "$recorded"
    for name in $compared; do cp "$out/$name" "$recorded/$name"; done
    # The whole dossier and the time are recorded for the page; neither is compared, since
    # one carries a session id and a clock and the other depends on the machine.
    cp "$out/dossier.md" "$out/seconds.txt" "$recorded/"
    echo; echo "recorded in ${recorded#"$here"/}"
    ;;
  check)
    status=0
    for name in $compared; do
      if ! diff -u "$recorded/$name" "$out/$name"; then
        echo "differs from the recorded demo/proof/replay-black/$name" >&2
        status=1
      fi
    done
    [ "$status" -eq 0 ] && { echo; echo "matches the recorded output in demo/proof/replay-black"; }
    exit "$status"
    ;;
esac
