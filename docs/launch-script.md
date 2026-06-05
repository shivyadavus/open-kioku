# Launch Script

Use this for the short launch video, GIF, or Show HN post.

## Hook

Coding agents should not edit a repo before they know the files, symbols, impact, and tests.

## Demo Flow

```sh
npm install -g open-kioku
ok demo --force
ok --repo ./open-kioku-demo plan token --format markdown
ok prove ./open-kioku-demo --task token
ok mcp install cursor --repo ./open-kioku-demo
```

## Voiceover

Most coding agents start by crawling files and guessing which tests matter.

Open Kioku indexes the repo locally, then gives the agent an evidence-backed pre-edit plan: primary files, relevant symbols, likely impact, validation candidates, and the MCP tool calls to use next.

For launch proof, `ok prove` generates a shareable report with task scores and redacted path shapes, without source snippets. The larger proof is a local Elasticsearch run: 36,640 files, 495,919 symbols, 509,665 chunks, 159,483 tests, 36,363 static analysis facts, and 1,015,502 graph edges.

It runs over local stdio, is read-only by default, and does not require a hosted index or embeddings API.

## Caption

Stop letting coding agents guess your repo.

```sh
npm install -g open-kioku
ok demo --force
```

## Links

- Demo: https://openkioku.com/
- npm: https://www.npmjs.com/package/open-kioku
- Repo: https://github.com/shivyadavus/open-kioku
