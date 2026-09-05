# Semantic Search

Open Kioku semantic search is local-first. Semantic search remains disabled until explicitly enabled. The deterministic local hash provider and exact-flat cosine index remain the zero-download correctness baseline; repository source is not sent to a hosted embedding API unless an external provider is explicitly configured and allowed.

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

`ok semantic status --json` reports the provider, selected model, dimensions, vector count, stale count, failed count, disk usage, resolved backend, `ann_active`, and `ann_profile`. When `semantic.backend = "auto"`, a ready status reports `exact-flat` or `usearch-hnsw-f32` rather than merely echoing `auto`, so CLI and MCP callers can see whether ANN is active and which measured HNSW profile produced the index.

## Vector backend selection

`exact-flat` is the correctness oracle and remains the default backend. It is recommended for small corpora and regression testing because it evaluates every stored vector.

For users who explicitly select `semantic.backend = "auto"`, Open Kioku uses exact-flat below 10,000 vectors and persistent local HNSW at 10,000 vectors or more. The measured production HNSW profile is:

```text
usearch-2.21.1-hnsw-meta3-c32-a256-s1024
connectivity = 32
expansion_add = 256
expansion_search = 1024
```

The checked-in calibration requires both Recall@10 >= 0.98 relative to exact-flat and at least 1.5x p95 query-latency speedup. The production-profile confirmation additionally reports MRR, build time, index size, memory, and host profile.

On the checked-in x86-64 Linux calibration host, the selected profile produced:

| Dimensions | Vectors | Recall@10 | MRR | Exact p95 | ANN p95 | p95 speedup | ANN build |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 384 | 10,000 | 1.0000 | 1.0000 | 6.145 ms | 2.730 ms | 2.25x | 8.31 s |
| 384 | 25,000 | 0.9969 | 0.9844 | 16.584 ms | 6.043 ms | 2.74x | 44.02 s |
| 768 | 10,000 | 1.0000 | 1.0000 | 10.068 ms | 4.149 ms | 2.43x | 17.33 s |
| 768 | 25,000 | 0.9984 | 1.0000 | 25.150 ms | 7.526 ms | 3.34x | 92.99 s |

These measurements support 10,000 vectors as the automatic crossover on the representative fixtures while preserving exact-flat as the default and correctness oracle. They are a reproducible selection guide, not a claim that every CPU or corpus has the same absolute latency.

Explicit `usearch-hnsw-f32` and `usearch-hnsw-bf16` backends are also available for controlled experiments. `auto` resolves to the F32 HNSW backend when the measured row threshold is reached.

The final production profile can be reproduced with:

```sh
cargo run --release -p open-kioku-vector --example ann_profile_confirm
```

The broader construction/search sweep is available with:

```sh
cargo run --release -p open-kioku-vector --example ann_calibrate
```

Environment variables such as `OK_ANN_CONFIRM_SIZES`, `OK_ANN_CONFIRM_DIMS`, `OK_ANN_CONFIRM_QUERIES`, and the corresponding calibration variables can narrow or extend the measurements.

The machine-readable benchmark evidence is checked in at `benchmarks/cc5-ann-calibration.json`.

## Storage and provenance

Semantic artifacts are written atomically:

```text
.ok/vectors/
  current/
    manifest.json
    index.json          # exact-flat
    index.usearch       # HNSW, when selected
    index.meta.json     # HNSW target metadata and graph parameters
    ids.json
    embeddings.cache
    stats.json
  builds/
```

Only the artifact for the resolved backend is required. HNSW metadata stores target identity/filter fields and the exact graph parameters separately from the native index; embedding vectors are not duplicated in metadata.

Builds are written to a temporary build directory and promoted only after manifest, ids, cache, stats, and backend-specific index files are complete. If promotion fails, the previous `current` index is restored.

The semantic manifest records the resolved backend and an index-version identity in addition to embedding provider/model/implementation, model-artifact digest, dimensions, distance metric, and chunker version. The HNSW index-version identity is the measured profile ID. Search also validates the graph parameters loaded from HNSW metadata and refuses an artifact that does not match the production profile, requiring a rebuild instead of silently using stale ANN settings.

## Ranking

Hybrid search combines lexical candidates with semantic vector candidates and fuses them through the same explainable ranking pipeline as normal search. Semantic-only evidence is labeled with `semantic_similarity`; exact symbol/reference evidence remains a separate stronger signal for identifier-like queries.

Agents reach the same behavior through `semantic_status`, `semantic_search`, `hybrid_search`, and `explain_search_result`. Every semantic or hybrid response carries semantic index status metadata, so a stale, disabled, missing, or corrupt vector index is explicit rather than silently degrading into a weaker result.

## Privacy

The default provider is local. Neural models are local and opt-in; their first download requires explicit consent. External providers fail unless `semantic.external_provider_allowed = true` is set in `ok.toml`. Semantic indexing respects indexed file metadata and skips vendor/generated/secret-like paths.
