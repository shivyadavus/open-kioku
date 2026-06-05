# Hybrid Ranking

Hybrid search combines lexical/BM25 candidates with local semantic vector candidates:

```sh
ok search --hybrid --explain-ranking "natural language task"
```

Semantic similarity is one ranking signal named `semantic_similarity`. It does not replace exact symbol/reference evidence, graph evidence, validation proximity, git co-change, memory, or path-quality signals. Identifier-like queries still benefit from exact and lexical matches; semantic-only hits are labeled as semantic evidence.

MCP agents can use:

- `semantic_status`
- `semantic_search`
- `hybrid_search`
- `explain_search_result`

Each semantic or hybrid response includes semantic index status metadata so stale, disabled, missing, or corrupt vector indexes are explicit.
