# Roadmap

Open Kioku wins by making AI coding agents stop guessing. Everything on the roadmap either sharpens that — proof-carrying context, honest uncertainty, a closed plan-to-verify loop — or gets cut.

**The live roadmap is the GitHub issue tracker.** Epics carry the current sequencing; this page is the index, so there is one place to keep current instead of two.

## Current epics

| Epic | Focus | Open work |
|---|---|---|
| [#204](https://github.com/shivyadavus/open-kioku/issues/204) — Context Compiler V2 | Measured hybrid retrieval for coding agents | [#328](https://github.com/shivyadavus/open-kioku/issues/328) ANN scale profile · [#235](https://github.com/shivyadavus/open-kioku/issues/235) ANN lifecycle under churn · [#210](https://github.com/shivyadavus/open-kioku/issues/210) reranking and calibrated abstention · [#211](https://github.com/shivyadavus/open-kioku/issues/211) retrieval-quality telemetry |
| [#236](https://github.com/shivyadavus/open-kioku/issues/236) — Repository Intelligence V3 | Proof-carrying relationships and coherent index generations | [#242](https://github.com/shivyadavus/open-kioku/issues/242) atomic index generations · [#243](https://github.com/shivyadavus/open-kioku/issues/243) relationship authority enforcement · [#244](https://github.com/shivyadavus/open-kioku/issues/244) bounded exploration envelope |
| [#94](https://github.com/shivyadavus/open-kioku/issues/94) — Evidence graph and trust layer | Long-running integration hardening | see issue |

## Known blockers

- [#329](https://github.com/shivyadavus/open-kioku/issues/329) — indexing peak memory is corpus-multiplied (peak ~8.0 GB RSS on a 16,537-file Java repository, per the [2026-08-31 validation record](large-java-validation-2026-08-31.md)), concentrated in the resolution/analysis phase. Memory is treated as a product requirement, not an optimization: it decides whether large repositories can run Open Kioku at all. The structural fix is tracked as RI3.6 ([#242](https://github.com/shivyadavus/open-kioku/issues/242)).

## What already shipped

Onboarding and distribution, trust and regression coverage, core intelligence quality, tool-surface maturity, the daily watch/demo/context workflow, and optional SCIP, LSP, semantic, and runtime integrations are in place across the 3.x line.

[`CHANGELOG.md`](../CHANGELOG.md) is the authoritative record of what landed and when. Measured results live in [`demo/proof/`](../demo/proof) with methodology in this directory — see [`retrieval-benchmark.md`](retrieval-benchmark.md), [`workflow-benchmarks.md`](workflow-benchmarks.md), and [`relationship-benchmark.md`](relationship-benchmark.md) for the standards a change has to clear.
