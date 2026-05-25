# Architecture

Open Kioku is organized as a Rust workspace with strict dependency direction:

1. Interface layer: `open-kioku-cli`, `open-kioku-mcp`, `open-kioku-daemon`.
2. Agent intelligence layer: `open-kioku-context`, `open-kioku-ranking`, `open-kioku-impact`, `open-kioku-tests`, `open-kioku-patch`, `open-kioku-actions`.
3. Code intelligence kernel: `open-kioku-ingest`, `open-kioku-parse`, `open-kioku-languages`, `open-kioku-symbols`, `open-kioku-graph`, `open-kioku-architecture`.
4. Storage layer: `open-kioku-storage`, `open-kioku-storage-sqlite`, `open-kioku-storage-kv`, `open-kioku-search-regex`, `open-kioku-search-tantivy`.
5. Optional integrations: `open-kioku-scip`, `open-kioku-lsp`, `open-kioku-semantic`, `open-kioku-qdrant`, `open-kioku-github`, `open-kioku-jira`, `open-kioku-sentry`, `open-kioku-aws`.

`open-kioku-core` owns shared domain types and has no dependency on CLI or MCP. `open-kioku-errors` owns shared errors. All user and agent interfaces consume the same indexing and storage contracts.

```text
CLI/MCP/Daemon
  -> Context/Impact/Tests/Patch/Policy
  -> Ingest/Parse/Symbol/Search/Graph/Architecture
  -> SQLite/KV/Search Indexes
  -> Repository files and optional external facts
```

## Current Vertical Slice

The implemented path supports `init`, `index`, `status`, tree-sitter-backed extraction, SCIP binary import, persisted SQLite occurrences and graph facts, disk-backed Tantivy lexical search, symbol discovery, file/symbol explanation, graph path lookup, impact reports, test recommendations, context packs, patch plans, and a read-only JSON-RPC MCP server.

LSP, semantic search, Qdrant, daemon watch mode, and runtime integrations are explicit extension crates. They return disabled/unsupported diagnostics unless configured, so callers do not mistake missing capabilities for authoritative facts.
