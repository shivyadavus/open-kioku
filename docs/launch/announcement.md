# Announcement Draft

# Open Kioku: Local-First Code Intelligence for AI Coding Agents

Open Kioku is a local-first code intelligence MCP for AI coding agents. It indexes a repository on your machine and gives Claude, Cursor, and other MCP clients tools for code search, symbol lookup, impact analysis, likely tests, context packs, and pre-edit planning.

The goal is simple: agents should ask the codebase for evidence before editing.

## Why This Matters

Coding agents often start by crawling files repeatedly. They infer references from text matches, miss hidden impact, and choose tests only after a change fails.

Open Kioku gives them a local pre-edit routine:

1. Search indexed code and files.
2. Resolve symbols and references.
3. Estimate impact.
4. Pick validation targets.
5. Build a context pack or plan before editing.

## Local By Default

The default workflow does not require a hosted index, source upload, or embeddings API.

- Metadata is stored in SQLite under `.ok/index.sqlite`.
- BM25 search is stored under `.ok/search/tantivy`.
- MCP runs over local stdio.
- Write tools are disabled by default.
- Semantic search is opt-in and experimental.

## Try It

```sh
npm install -g open-kioku
ok demo --force
ok --repo ./open-kioku-demo plan token --format markdown
ok prove ./open-kioku-demo --task token
```

`ok prove` generates a shareable report from a real repository. It scores whether Open Kioku returned grounded primary context, existing paths, source context, impact candidates, validation candidates, risk, and agent tool calls. It intentionally omits source snippets.

## Links

- GitHub: `https://github.com/shivyadavus/open-kioku`
- Demo: `https://shivyadavus.github.io/open-kioku/`
- npm: `https://www.npmjs.com/package/open-kioku`

