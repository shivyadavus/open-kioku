# Social Thread

```text
I built Open Kioku: local repo memory for AI coding agents.

Claude, Cursor, Codex, and other agents are much better when they can ask the repo for facts before editing.
```

```text
Open Kioku indexes your repo locally and exposes MCP tools for:

- code search
- symbol lookup
- impact analysis
- likely tests
- context packs
- pre-edit plans
```

```text
The default path is local-first:

- SQLite metadata under .ok/
- Tantivy BM25 search under .ok/search/tantivy
- MCP over stdio
- read-only by default
- no hosted index
- no embeddings API required
```

```text
Try it:

npm install -g open-kioku
ok demo --force
ok --repo ./open-kioku-demo plan token --format markdown
```

```text
The part I care about most: `ok prove`.

Run it on a repo and it creates a shareable report showing whether Open Kioku returned grounded context, impact, validation, risk, and agent tool calls, without including source snippets.
```

```text
GitHub: https://github.com/shivyadavus/open-kioku
Demo: https://shivyadavus.github.io/open-kioku/
```

