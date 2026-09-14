#!/usr/bin/env bash
# Normalise a benchmark corpus's identity and register GitHub Actions log masks for it.
#
#     . scripts/mask-corpus-identity.sh
#
# Source it in the step that receives the secrets, before any command that could print them.
# It reads REPO_URL, BASE_SHA and, when set, CORPUS_PATHS (space-separated subtrees), and
# replaces them in the calling shell with normalised values: whitespace is removed from the URL
# and the base commit, the base commit is lowercased, and the subtrees are re-joined with single
# spaces.
#
# The runner masks secrets verbatim. This masks the forms a log can still show on their own:
# - the URL as given, and without a trailing slash or `.git`, in its given and lowercase spelling;
# - `owner/name`, the owner, and the name;
# - the base commit as given, and every 7- to 40-character prefix of its lowercase form;
# - each subtree with and without its trailing slash.
# So corpus identity stays out of public logs.
#
# A subtree must be a plain relative path: letters, digits, `.`, `_` and `-`, separated by `/`,
# not starting with `-` and without `..`. Anything else fails with a message that does not repeat
# the value. An empty value masks nothing.

__ok_mask() {
  if [ -n "$1" ]; then
    printf '::add-mask::%s\n' "$1"
  fi
}

__ok_lower() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

__ok_mask_corpus_identity() {
  local url stem form name rest owner given base width prefix normalised
  local -a parts

  REPO_URL="${REPO_URL:-}"
  REPO_URL="${REPO_URL//[[:space:]]/}"
  url="$REPO_URL"
  __ok_mask "$url"
  __ok_mask "$(__ok_lower "$url")"
  stem="${url%/}"
  stem="${stem%.git}"
  for form in "$stem" "$(__ok_lower "$stem")"; do
    __ok_mask "$form"
    case "$form" in
      */*)
        name="${form##*/}"
        rest="${form%/*}"
        owner="${rest##*[/:]}"
        __ok_mask "$name"
        __ok_mask "$owner"
        if [ -n "$owner" ] && [ -n "$name" ]; then
          __ok_mask "$owner/$name"
        fi
        ;;
    esac
  done

  given="${BASE_SHA:-}"
  given="${given//[[:space:]]/}"
  base="$(__ok_lower "$given")"
  BASE_SHA="$base"
  __ok_mask "$given"
  __ok_mask "$base"
  for ((width = 7; width < ${#base}; width++)); do
    __ok_mask "${base:0:$width}"
  done

  read -r -a parts <<< "${CORPUS_PATHS:-}"
  normalised=""
  for prefix in "${parts[@]+"${parts[@]}"}"; do
    if [[ ! "$prefix" =~ ^[A-Za-z0-9._][A-Za-z0-9._-]*(/[A-Za-z0-9._][A-Za-z0-9._-]*)*/?$ ]] || [[ "$prefix" == *..* ]]; then
      echo "::error::a corpus path prefix is not a plain relative path; the value is not repeated here"
      return 1
    fi
    __ok_mask "${prefix%/}"
    __ok_mask "${prefix%/}/"
    normalised="${normalised:+$normalised }$prefix"
  done
  CORPUS_PATHS="$normalised"
}

if __ok_mask_corpus_identity; then
  unset -f __ok_mask __ok_lower __ok_mask_corpus_identity
else
  unset -f __ok_mask __ok_lower __ok_mask_corpus_identity
  return 1 2>/dev/null || exit 1
fi
