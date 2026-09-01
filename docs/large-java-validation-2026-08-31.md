# Open Kioku main (pre-release) — anonymized large-Java validation, 2026-08-31

This document records the methodology and aggregate evidence for a validation
run of Open Kioku `main` at
`3959fdfb6ca27d0c279b635fca7fc1b7935d4889`, after the 2026-08-31 performance
work. The canonical machine-readable record is
[`demo/proof/large-java-2026-08-31-main.json`](../demo/proof/large-java-2026-08-31-main.json).
It follows the same protocol as the published
[3.0.4 record](large-java-validation-3.0.4.md) on the **same host**, so the
two records are directly comparable; it accompanies the `v3.1.0` release
(whose tag additionally includes the authority/abstention/generations changes
of PR #330, covered by the release CI gates).

## Evidence boundary

The repository name, revision, paths, and source are intentionally withheld.
This is an **auditable scale record**, not an exactly reproducible corpus. The
corpus is a large, permissively licensed public Java repository at a pinned
revision: 16,537 tracked files, 12,580 Java files.

## Build identity

- Open Kioku: `3.1.0` release lineage
- Source: `3959fdfb6ca27d0c279b635fca7fc1b7935d4889`

## Host profile

Identical to the 3.0.4 record: macOS 13.7.6 (`22H625`), x86-64,
Intel Core i5-1038NG7 @ 2.00 GHz, 8 logical CPUs, 16 GiB RAM, local APFS
workspace. Timings captured with `/usr/bin/time -lp`; query measurements used
fresh CLI processes.

## Workload and results

| Measurement | Observed result |
|---|---:|
| Tracked source files / Java files | 16,537 / 12,580 |
| Indexed files / symbols / chunks | 13,607 / 247,499 / 248,107 |
| Graph nodes / edges | 402,844 / 1,522,135 |
| Tests / imports | 84,504 / 181,966 |
| Cold full structural index | 19m 28s (peak ~8.0 GB RSS) |
| Repeat full structural index (existing store) | 27m 49s |
| Exact class lookup, fresh process | 0.05s / 0.02s |
| Exact references query, fresh process | 0.74s |
| Lexical search, fresh process | 0.24s |
| Exact-flat semantic build | 495,606 vectors in 58.8s; 0 failures |
| Persistent HNSW semantic build | 495,606 vectors in 10m 19s; 0 failures |
| Disk after all phases | ~15 GB (`index.sqlite` ~10.5 GB, lexical ~0.6 GB) |

The semantic provider was the deterministic local-hash implementation at 384
dimensions.

## Measured improvement over the prior build

The same corpus was measured on this host against the pre-optimization build
(`main` @ `c96f61a`):

| Measurement | Prior build | This build |
|---|---:|---:|
| Fixed per-command startup | ~14s | sub-second |
| Exact class lookup | 13.9s, returned `symbol not found` (incorrect) | 0.02s, correct class |
| Lexical search | 13.7s | 0.24s |
| Cold full structural index | 40m 40s | 19m 28s |

The startup cost was a migration backfill that rescanned the graph tables on
every store open; the lookup was an unindexable substring scan that could
truncate the true exact match out of its candidate window; the indexing win
came from bulk graph loading with index drop/rebuild. Each fix is landed on
`main` with the measurement in its commit message.

## Procedure

1. Full cold structural index into an empty local store.
2. Repeat full structural index; file, symbol, chunk, test, and import totals
   compared for identity.
3. Fresh-process exact class definitions, exact references, and lexical
   search.
4. Four concurrent graph readers against the completed index.
5. Local semantic rebuild with `semantic.backend = "exact-flat"`, then
   `usearch-hnsw-f32`.

Query text and class identities are withheld because they would identify the
repository.

## Quality observations

- The repeat full structural run reproduced identical file, symbol, chunk,
  test, and import totals.
- Four concurrent graph readers completed with zero lock failures.
- The exact class lookup returned the correct class; on the prior build the
  same query returned `symbol not found`.
- Both semantic builds completed with zero failed vectors.
- Missing optional SCIP evidence was reported rather than inferred.

## Caveats

- Single-workstation observations, not latency guarantees.
- The 495,606-vector persistent HNSW build sits above the ~300K-vector range
  where Open Kioku's own measured ANN scale evidence
  ([`benchmarks/cc5-ann-scale-evidence`](../benchmarks/cc5-ann-scale-evidence/README.md))
  shows recall degradation for the current production profile. The exact-flat
  oracle remains the correctness path at this scale until the scale-profile
  decision (issue #328) lands. Keeping that caveat beside the timing is the
  point of this record.
- The repeat index rebuilds into the existing multi-gigabyte store file, so it
  runs slower than the cold build on an empty store and grows the file; space
  reclamation is tracked with the indexing-architecture work (#242, #329).
- Peak indexing memory (~8 GB RSS on this corpus) is concentrated in the
  resolution/analysis phase (#329).
