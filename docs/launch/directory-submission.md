# Directory Submission

## Name

Open Kioku

## Tagline

Local-first code intelligence MCP for AI coding agents.

## Short Description

Open Kioku indexes a repository locally and exposes code search, symbol lookup, impact analysis, test hints, context packs, and pre-edit planning to Claude, Cursor, and other MCP clients.

## Longer Description

Open Kioku helps coding agents ask the repo for facts before editing. It builds local SQLite and Tantivy indexes under `.ok/`, serves a read-only MCP tool surface over stdio by default, and gives agents evidence-backed workflows such as `search_code`, `get_definition`, `impact_analysis`, `find_tests_for_change`, `build_context_pack`, and `plan_change`.

The default path does not require a hosted index, source upload, or embeddings API. Semantic search is opt-in and experimental. Write tools are gated and disabled by default.

## Install

```sh
npm install -g open-kioku
ok demo --force
ok mcp install cursor --repo "$PWD/open-kioku-demo"
```

## Proof

```sh
ok prove ./open-kioku-demo --task token
```

The proof report is shareable because it includes metrics and redacted path shapes, not source snippets.

## Links

- GitHub: `https://github.com/shivyadavus/open-kioku`
- Demo: `https://shivyadavus.github.io/open-kioku/`
- npm: `https://www.npmjs.com/package/open-kioku`

## Categories

- Developer Tools
- MCP Server
- AI Coding Agents
- Code Search
- Local-First

## Compatibility

- Claude and Cursor through MCP stdio config.
- Other MCP clients that support local stdio servers.

## Safety Notes

- Local index under `.ok/`.
- Read-only MCP server by default.
- No source upload in the default workflow.
- `apply_patch` is experimental and only exposed when write mode is explicitly enabled.

