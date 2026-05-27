# Launch Kit

This directory contains copy-ready launch material for Open Kioku. Keep every claim tied to behavior that is available in the current release.

## Core Message

Open Kioku gives AI coding agents local, evidence-backed repo memory so Claude, Cursor, Codex, and other MCP clients stop guessing before they edit.

## Proof Commands

Run these before posting:

```sh
npm install -g open-kioku
ok demo --force
ok prove ./open-kioku-demo --task token
ok mcp install cursor --repo "$PWD/open-kioku-demo"
ok mcp install claude --repo "$PWD/open-kioku-demo"
```

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
- Generate fresh `ok prove` output on the demo repo.
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

