# Launch Kit

This directory contains copy-ready launch material for Open Kioku. Keep every claim tied to behavior that is available in the current release.

## Core Message

Open Kioku gives AI coding agents local code intelligence so Claude, Cursor, Codex, and other MCP clients can search, resolve symbols, estimate impact, pick tests, and build evidence-backed plans before they edit.

The default path is local: no hosted index, no source upload, and no embeddings API. On a local Elasticsearch validation run, Open Kioku indexed 36,640 files, 495,919 symbols, 509,665 chunks, 159,483 tests, 36,363 static analysis facts, and 1,015,502 graph edges.

## Proof Commands

Run these before posting:

```sh
npm install -g open-kioku
ok demo --force
ok prove ./open-kioku-demo --task token
ok mcp install cursor --repo "$PWD/open-kioku-demo"
ok mcp install claude --repo "$PWD/open-kioku-demo"
```

For release readiness from source:

```sh
scripts/verify-release-readiness.sh
scripts/generate-proof.sh
```

Use `docs/release-checklist.md` for the full package, tag, formula, npm, cargo-binstall, release-note, and artifact consistency checklist.

For a real repository:

```sh
ok init /path/to/repo
ok index /path/to/repo
ok prove /path/to/repo --task "auth flow" --task "release workflow"
```

`ok prove` is the safest public artifact to share because it reports metrics and redacted path shapes, not source snippets.

## Launch Checklist

- Confirm the latest GitHub Actions run is green.
- Confirm npm, GitHub Releases, Homebrew, and cargo-binstall install paths are documented.
- Run `scripts/verify-release-readiness.sh` from a clean checkout.
- Generate fresh `ok prove` output on the demo repo.
- Link `docs/large-repo-proof.md` when making scale or “real repo” claims.
- Include the hosted demo link: `https://shivyadavus.github.io/open-kioku/`.
- Include the GitHub repo link: `https://github.com/shivyadavus/open-kioku`.
- Include one copy-paste install command.
- Avoid claiming exact token savings, production adoption, or semantic accuracy unless separately measured.

## Channels

- Cursor and Claude plugin submissions.
- MCP directories and marketplace-style indexes.
- GitHub topic discovery: `mcp`, `ai-agents`, `code-search`, `local-first`, `developer-tools`.
- Show HN.
- Reddit communities focused on MCP, Cursor, Claude, local AI, and programming.
- Product Hunt once the install and demo flow has been tested from a clean machine.
