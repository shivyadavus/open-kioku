# Vector Index

`open-kioku-vector` provides two local backends:

- `ExactFlatVectorIndex` stores normalized `f32` vectors and performs deterministic exact cosine search. It is intentionally simple: it is the correctness oracle that other backends are measured against.
- `UsearchHnswVectorIndex` is a persistent approximate index for corpora too large for an exhaustive scan.

Selection is controlled by `semantic.backend`, which accepts `exact-flat` (the default), `auto`, `usearch-hnsw-f32`, and `usearch-hnsw-bf16`. Under `auto`, the HNSW backend is chosen once the vector count reaches `semantic.ann_min_rows` and exact-flat is used below it, so small repositories keep exact results and large ones stay usable.

## Scale limit

`benchmarks/cc5-ann-scale-evidence` measured the production HNSW parameters against the exact-flat oracle, at the configured production search expansion of 1024, on shared CI runners. The artifacts hold **four** series — clustered and code-shaped, each at 384 and 768 dimensions — and this table reports all of them (`ann-scale-aggregate.json`, `ann-code-shape-aggregate.json`; the README's own table shows three columns and omits 768d code-shaped):

| Vectors | clustered 384d | clustered 768d | code-shaped 384d | code-shaped 768d | range |
|---:|---:|---:|---:|---:|---|
| 50,000 | 0.92 | 0.92 | 0.82 | 0.86 | 0.82 - 0.92 |
| 100,000 | 0.78 | 0.93 | 0.68 | 0.65 | 0.65 - 0.93 |
| 300,000 | 0.51 | 0.55 | 0.39 | 0.41 | 0.39 - 0.55 |
| 1,000,000 | 0.39 | 0.36 | 0.24 | 0.27 | 0.24 - 0.39 |

`semantic.dimensions` is user-configurable, so the 768d rows govern for anyone on a 768-dimension model; at 100,000 vectors that series is the lowest of the four, at 0.6469.

The artifact states its own conclusion as adequate below about 100,000 vectors and not adequate above about 300,000, and it measures no point between those two. That gap is a gap in the evidence, not a plateau.

What follows:

- `exact-flat` stays the default backend. It scores every vector, so its recall does not fall as the corpus grows; its query latency grows linearly with the vector count instead.
- At or above 300,000 vectors, persistent ANN reports best-effort recall in the semantic routing diagnostics (`routing.caveats`), whether `auto` selected the backend or it was configured explicitly. 300,000 is itself a measured point (0.39 - 0.55), which is why it belongs to this band rather than the one below. Routing does not switch to exact-flat at that size: the configured backend answers, and the caveat keeps the measured degradation visible. Threshold: `ANN_MEASURED_RECALL_CEILING_VECTORS`.
- From 100,000 up to 300,000 vectors, recall for this profile is **unmeasured**, and a query there says so in its own caveat rather than passing silently. 100,000 is included because it is where the measured low end has already fallen to 0.65, so an uncaveated answer from there upward would imply an adequacy the evidence does not establish. Threshold: `ANN_MEASURED_RECALL_ADEQUATE_VECTORS`.
- **Both bands key on the size of the graph being traversed, not on the candidates left after filtering.** A filtered ANN search walks the whole graph with a predicate: a path or allowlist scope narrows what may be returned, but not what is traversed, and `expansion_search` is partly spent on ineligible nodes. Keying on the filtered count would let `--path` delete a degradation warning from a million-vector graph. The caveat names both numbers, and the filtered count remains what backend routing uses.
- No higher-construction HNSW profile is added here. Selecting one needs a measurement at these populations that has not been made; this section will name it when it exists.

## Guarantees

- stable `VectorId` values are derived from target identity, target kind, embedding model, and dimensions
- vector ID collisions are detected
- allowlist search only returns IDs from the supplied allowlist
- target-kind filters can restrict search to chunks, symbols, or future target classes
- persisted indexes load from `.ok/vectors/current/index.json`

## Atomic Promotion

Semantic indexing writes a complete build under `.ok/vectors/builds/build-<run-id>` and promotes it to `.ok/vectors/current` only after manifest, ids, cache, stats, and index files are complete. If promotion fails, the previous `current` directory is restored.
