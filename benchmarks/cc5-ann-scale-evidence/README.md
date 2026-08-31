# CC5.2 ANN scale evidence (50K → 1M vectors)

Checked-in evidence for issue #232: the persistent ANN backend measured across
the full requested matrix, with the exact-flat oracle as ground truth. This
directory records what was measured; it deliberately selects **no** production
profile — the aggregates' own `selection_policy` states the same.

## Provenance

| Record | Source workflow run | Harness |
|---|---|---|
| `ann-scale-aggregate.json` | `ann-scale.yml` run `33334252445` | `open-kioku-vector/examples/ann_scale_matrix.rs` |
| `ann-code-shape-aggregate.json` | `ann-code-shape-scale.yml` run `33334254465` | `examples/ann_code_shape_validation.rs` |
| `generation-update/*.json` | `ann-update-scale.yml` run `33334256246` | `examples/ann_generation_update.rs` |

All three ran on 2026-08-30 against `main` (`7231a78`), on `ubuntu-latest`
GitHub-hosted runners, backend `usearch-hnsw-f32` with the production HNSW
parameters, oracle `exact-flat`, sizes 50K/100K/300K/1M × 384/768 dimensions,
search expansions 64–1024. Aggregation used
`scripts/aggregate_ann_scale_evidence.py`, which fails closed on missing matrix
points, duplicates, mixed schemas, or non-finite metrics.

## Measured finding

Recall@10 at the maximum measured expansion (1024):

| Vectors | 384d clustered | 768d clustered | 384d code-shaped |
|---:|---:|---:|---:|
| 50,000 | 0.92 | 0.92 | 0.82 |
| 100,000 | 0.78 | 0.93 | 0.68 |
| 300,000 | 0.51 | 0.55 | 0.39 |
| 1,000,000 | 0.39 | 0.36 | 0.24 |

**The current single production HNSW profile holds usable recall through
roughly 100K vectors and degrades sharply beyond 300K on clustered and
code-shaped distributions.** Build cost and memory grow accordingly (1M × 768:
~21 minute build, ~4.8 GB build memory, ~3.3 GB index). The generation-update
records show the same recall ceiling is inherited by fresh-generation rebuilds
under churn.

These are synthetic-distribution measurements on shared CI runners; absolute
latency varies with hardware. The recall trend is the finding, not the exact
timings.

## What follows from this

- Below ~100K vectors the measured profile is adequate; the existing
  `ann_min_rows` crossover remains supported by this data.
- Above ~300K vectors the measured profile is **not** adequate. Either a
  higher-construction profile must be measured and added, or routing should
  prefer the exact-flat oracle (or clearly surface degraded-recall caveats) at
  those scales.
- Regression gates for these dimensions should only be introduced alongside
  that profile decision — gating today's collapsed recall would freeze a bad
  frontier.
