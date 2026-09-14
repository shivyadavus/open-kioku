#!/usr/bin/env bash
# Register GitHub Actions log masks for a benchmark corpus's identity.
#
#     REPO_URL=... BASE_SHA=... bash scripts/mask-corpus-identity.sh
#
# Both values come from repository secrets, which the runner already masks verbatim. This
# masks the forms a log can still show on their own: the URL without a trailing slash or
# `.git`, `owner/name`, the owner, the name, and the 7-, 8-, 10- and 12-character forms of
# the base commit. Run it in the step that receives the secrets, before any command that
# could print them, so corpus identity stays out of public logs. An empty value masks nothing.
set -euo pipefail

mask() {
  if [ -n "$1" ]; then
    printf '::add-mask::%s\n' "$1"
  fi
}

url="${REPO_URL:-}"
stem="${url%/}"
stem="${stem%.git}"
mask "$url"
mask "$stem"
case "$stem" in
  */*)
    name="${stem##*/}"
    rest="${stem%/*}"
    owner="${rest##*[/:]}"
    mask "$name"
    mask "$owner"
    if [ -n "$owner" ] && [ -n "$name" ]; then
      mask "$owner/$name"
    fi
    ;;
esac

base="${BASE_SHA:-}"
mask "$base"
for width in 7 8 10 12; do
  mask "${base:0:$width}"
done
