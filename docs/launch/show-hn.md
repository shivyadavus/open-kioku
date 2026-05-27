# Show HN Draft

Title:

```text
Show HN: Open Kioku - local repo memory for AI coding agents
```

Post:

```text
I built Open Kioku, a local-first code intelligence MCP for AI coding agents.

The problem: coding agents often start by crawling files, guessing references from grep output, and picking tests after an edit goes wrong.

Open Kioku indexes a repo locally and exposes a read-only MCP tool surface for:

- code and file search
- symbol definitions and references
- impact analysis
- likely tests for a change
- context packs
- evidence-backed pre-edit plans

The default path uses local SQLite + Tantivy indexes under .ok/. No hosted index, no source upload, and no embeddings API required.

Try it:

npm install -g open-kioku
ok demo --force
ok --repo ./open-kioku-demo plan token --format markdown
ok prove ./open-kioku-demo --task token

The `ok prove` command generates a shareable report from a real repo without source snippets, so people can check whether it is actually useful on their own code.

GitHub: https://github.com/shivyadavus/open-kioku
Demo: https://shivyadavus.github.io/open-kioku/

I would especially like feedback on the MCP tool surface, what should stay stable vs experimental, and what workflows coding agents should run before editing.
```

