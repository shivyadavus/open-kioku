# Semantic Search

Open Kioku semantic search is local-first. `ok semantic index` builds an exact-flat cosine vector index under `.ok/vectors/current` using the local hash embedding provider by default. Repository source is not sent to a hosted embedding API unless an external provider is explicitly configured and allowed.

## Commands

```sh
ok semantic status
ok semantic index
ok semantic rebuild
ok semantic clean
ok semantic clean --include-cache
ok search --semantic "session token flow"
ok search --hybrid --explain-ranking "session token flow"
```

`ok semantic status --json` reports backend, provider, model, dimensions, vector count, stale count, failed count, disk usage, and whether `.ok/vectors/current` is ready, missing, stale, corrupt, or disabled.

## Storage

Semantic artifacts are written atomically:

```text
.ok/vectors/
  current/
    manifest.json
    index.json
    ids.json
    embeddings.cache
    stats.json
  builds/
```

Builds are written to a temporary build directory and promoted only after manifest, ids, cache, stats, and index files are complete. If promotion fails, the previous `current` index is restored.

## Ranking

Hybrid search combines lexical candidates with semantic vector candidates and fuses them through the same explainable ranking pipeline as normal search. Semantic-only evidence is labeled with `semantic_similarity`; exact symbol/reference evidence remains a separate stronger signal for identifier-like queries.

## Privacy

The default provider is local. External providers fail unless `semantic.external_provider_allowed = true` is set in `ok.toml`. Semantic indexing respects indexed file metadata and skips vendor/generated/secret-like paths.
