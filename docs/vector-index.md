# Vector Index

Open Kioku's first local vector backend is `ExactFlatVectorIndex` in `open-kioku-vector`. It stores normalized `f32` vectors and performs deterministic exact cosine search. Exact-flat is intentionally simple: it is the correctness backend for local semantic search and future optimized local backends.

## Guarantees

- stable `VectorId` values are derived from target identity, target kind, embedding model, and dimensions
- vector ID collisions are detected
- allowlist search only returns IDs from the supplied allowlist
- target-kind filters can restrict search to chunks, symbols, or future target classes
- persisted indexes load from `.ok/vectors/current/index.json`

## Atomic Promotion

Semantic indexing writes a complete build under `.ok/vectors/builds/build-<run-id>` and promotes it to `.ok/vectors/current` only after manifest, ids, cache, stats, and index files are complete. If promotion fails, the previous `current` directory is restored.
