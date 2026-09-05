# Vector Index

`open-kioku-vector` provides two local backends:

- `ExactFlatVectorIndex` stores normalized `f32` vectors and performs deterministic exact cosine search. It is intentionally simple: it is the correctness oracle that other backends are measured against.
- `UsearchHnswVectorIndex` is a persistent approximate index for corpora too large for an exhaustive scan.

Selection is controlled by `semantic.backend`, which accepts `exact-flat` (the default), `auto`, `usearch-hnsw-f32`, and `usearch-hnsw-bf16`. Under `auto`, the HNSW backend is chosen once the vector count reaches `semantic.ann_min_rows` and exact-flat is used below it, so small repositories keep exact results and large ones stay usable.

## Guarantees

- stable `VectorId` values are derived from target identity, target kind, embedding model, and dimensions
- vector ID collisions are detected
- allowlist search only returns IDs from the supplied allowlist
- target-kind filters can restrict search to chunks, symbols, or future target classes
- persisted indexes load from `.ok/vectors/current/index.json`

## Atomic Promotion

Semantic indexing writes a complete build under `.ok/vectors/builds/build-<run-id>` and promotes it to `.ok/vectors/current` only after manifest, ids, cache, stats, and index files are complete. If promotion fails, the previous `current` directory is restored.
