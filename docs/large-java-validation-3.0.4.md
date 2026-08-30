# Open Kioku 3.0.4 — anonymized large-Java validation

This document records the public methodology and aggregate evidence behind the
large-Java figures quoted in the README and on the product site. The canonical
machine-readable record is
[`demo/proof/large-java-3.0.4.json`](../demo/proof/large-java-3.0.4.json).

## Evidence boundary

The repository name, revision, paths, and source are intentionally withheld.
That protects the test target but means this is an **auditable scale record**,
not an exactly reproducible corpus. The published artifact makes the release
identity, host profile, configuration, command shapes, aggregate workload,
measurements, quality checks, and limitations inspectable without claiming that
another repository will produce the same timings.

## Release identity

- Open Kioku: `3.0.4`
- Tag: `v3.0.4`
- Source: `84ca3d09209495f6c842f9ecdf670c0dac945e72`
- Release: <https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.4>

## Host profile

- macOS 13.7.6 (`22H625`), x86-64
- Intel Core i5-1038NG7 @ 2.00 GHz
- 8 logical CPUs
- 16 GiB RAM
- local APFS workspace

Free-space and background-load conditions were not controlled. Timings were
captured with `/usr/bin/time -lp`; query measurements used fresh CLI processes.

## Workload and results

| Measurement | Observed result |
|---|---:|
| Tracked source files / Java files | 11,404 / 9,247 |
| Indexed files / symbols / chunks | 9,312 / 136,212 / 136,646 |
| Graph nodes / edges | 211,057 / 707,271 |
| Tests / imports | 67,810 / 87,148 |
| Cold full structural index | 14m 44s |
| Repeat full structural index | 8m 22s |
| Exact class lookup, fresh process | 3.82s |
| Exact definition / implementation graph query | 2.88s / 4.50s |
| Exact-flat semantic build | 272,858 vectors in 33.33s; 0 failures |
| Persistent HNSW semantic build | 272,858 vectors in 5m 02s; 0 failures |
| Structural / HNSW disk footprint | approximately 4.7 GB / 2.35 GB |

The semantic provider was the deterministic local-hash implementation at 384
dimensions. The correctness run used `exact-flat`; the persistent run used
`usearch-hnsw-f32` over the same vector count.

## Procedure

1. Run a full structural index and record the completed manifest and graph
   totals.
2. Run the same full structural path again and compare file, symbol, chunk,
   node, and edge totals.
3. In fresh processes, time one exact Java class lookup and exact definition
   and implementation graph queries.
4. Rebuild local semantic evidence with `semantic.backend = "exact-flat"`.
5. Rebuild the same local semantic evidence with
   `semantic.backend = "usearch-hnsw-f32"`.
6. Run four graph readers concurrently against the completed structural index.

The JSON artifact includes redacted command templates. Query text and class
identity are withheld because they would make the repository readily
identifiable.

## Quality observations

- The repeat full structural run reproduced the same file, symbol, chunk, node,
  and edge totals.
- Four concurrent graph reads completed with zero SQLite lock failures.
- Exact symbol identity ranked ahead of a broader prefix match.
- Exact graph queries used indexed anchors instead of scanning the complete edge
  table.
- Both semantic builds completed with zero failed vectors.

## Caveats

Compiler-grade Java SCIP evidence was unavailable in this environment.
Structural Java indexing remained operational, and Open Kioku reported the
missing optional evidence rather than raising the authority of heuristic facts.

Absolute latency and storage vary with hardware, filesystem state, background
load, repository shape, Git history, enabled evidence sources, and semantic
model. These measurements establish observed behavior at this scale; they do
not guarantee identical performance elsewhere.
