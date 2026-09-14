#!/usr/bin/env python3
"""Read the JSON document an `ok --json` command wrote to a file, and print only what a public
log or artifact may carry.

    scripts/extract-ok-json.py index-counts INDEX.json
        one counts-only line from `ok --json index`: files, symbols, chunks, discovered and indexed
    scripts/extract-ok-json.py coverage STATUS.json
        the `coverage` object from `ok --json status`, as JSON on stdout

The CLI's tracing writes warnings to stdout, so the file can hold log lines around the document.
The last complete JSON object that starts a line and carries the expected key is used. When none
parses, a fixed message goes to stderr and the exit status is 1; the file's contents are never
printed, so corpus identity stays out of public logs.
"""
import json
import sys

REQUIRED_KEY = {"index-counts": "file_count", "coverage": "coverage"}


def last_document(text, key):
    """The last JSON object in `text` that starts a line and has `key`, or None."""
    decoder = json.JSONDecoder()
    found = None
    for start, char in enumerate(text):
        if char != "{" or (start and text[start - 1] != "\n"):
            continue
        try:
            value, _ = decoder.raw_decode(text, start)
        except ValueError:
            continue
        if isinstance(value, dict) and key in value:
            found = value
    return found


def count(document, key):
    value = document.get(key) if isinstance(document, dict) else None
    return value if isinstance(value, int) and not isinstance(value, bool) else 0


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 2 or argv[0] not in REQUIRED_KEY:
        print(__doc__, file=sys.stderr)
        return 2
    mode, path = argv
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            text = handle.read()
    except OSError:
        text = ""
    document = last_document(text, REQUIRED_KEY[mode])
    if document is None:
        print(f"no readable ok --json document for {mode}; the output is withheld", file=sys.stderr)
        return 1
    if mode == "index-counts":
        quality = document.get("quality") if isinstance(document.get("quality"), dict) else {}
        coverage = quality.get("coverage") if isinstance(quality.get("coverage"), dict) else {}
        print(f"indexed {count(document, 'file_count'):,} files, {count(document, 'symbol_count'):,} symbols, "
              f"{count(document, 'chunk_count'):,} chunks; {count(coverage, 'indexed'):,} of "
              f"{count(coverage, 'discovered'):,} discovered files indexed")
    else:
        json.dump(document.get("coverage"), sys.stdout, indent=1)
        sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
