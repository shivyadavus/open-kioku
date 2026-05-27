# Reddit Draft

Use a title appropriate to the community. Examples:

```text
I built a local-first MCP server that gives coding agents repo memory before they edit
```

```text
Open Kioku: local code search, impact analysis, and pre-edit plans for Claude/Cursor
```

Post:

```text
I built Open Kioku, a local-first code intelligence MCP for AI coding agents.

It indexes your repo locally and gives Claude, Cursor, and other MCP clients tools for search, symbols, impact analysis, likely tests, context packs, and pre-edit planning.

Why I built it:

- Agents waste context crawling the same files repeatedly.
- They often infer references from text matches.
- They usually pick tests after a bad edit instead of before the edit.

The default workflow is local:

- SQLite metadata index under .ok/
- Tantivy BM25 search under .ok/search/tantivy
- MCP over local stdio
- Read-only by default
- No hosted index or embeddings API required

Try the demo:

npm install -g open-kioku
ok demo --force
ok --repo ./open-kioku-demo plan token --format markdown
ok prove ./open-kioku-demo --task token

`ok prove` generates a shareable proof report with metrics and redacted path shapes, not source snippets, so you can evaluate it on private repos without posting code.

GitHub: https://github.com/shivyadavus/open-kioku
Demo: https://shivyadavus.github.io/open-kioku/

I am looking for feedback on which MCP tools are most useful before editing and which experimental tools should be hardened next.
```

