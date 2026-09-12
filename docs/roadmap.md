# Roadmap

Open Kioku is a local repository-intelligence and change-safety layer for AI coding agents: proof-carrying context, reported uncertainty, and a plan-to-verify loop that is checked rather than assumed.

**The live roadmap is the GitHub issue tracker.** This page indexes it; it does not sequence it.

## Epics

| Epic | Focus | Status | Open work |
|---|---|---|---|
| [#204](https://github.com/shivyadavus/open-kioku/issues/204) — Context Compiler V2 | Measured hybrid retrieval for coding agents | Closed 2026-09-08 (completed). [#328](https://github.com/shivyadavus/open-kioku/issues/328) ANN scale profile, [#235](https://github.com/shivyadavus/open-kioku/issues/235) ANN lifecycle under churn, [#210](https://github.com/shivyadavus/open-kioku/issues/210) reranking and calibrated abstention, and [#211](https://github.com/shivyadavus/open-kioku/issues/211) retrieval-quality telemetry closed the same day. | none |
| [#236](https://github.com/shivyadavus/open-kioku/issues/236) — Repository Intelligence V3 | Proof-carrying relationships and coherent index generations | Closed 2026-09-08 (completed). [#244](https://github.com/shivyadavus/open-kioku/issues/244) bounded exploration envelope closed the same day. | [#242](https://github.com/shivyadavus/open-kioku/issues/242) atomic index generations · [#243](https://github.com/shivyadavus/open-kioku/issues/243) relationship authority enforcement |
| [#94](https://github.com/shivyadavus/open-kioku/issues/94) — Evidence graph and trust layer | Integration hardening | Closed 2026-09-08 (completed) | none |

## Known blockers

- [#329](https://github.com/shivyadavus/open-kioku/issues/329) (open) — indexing peak memory is corpus-multiplied (peak ~8.0 GB RSS on a 16,537-file Java repository, per the [2026-08-31 validation record](large-java-validation-2026-08-31.md)), concentrated in the resolution/analysis phase. Memory is treated as a product requirement, not an optimization: it decides whether large repositories can run Open Kioku at all. 4.0.0 lowered peak indexing RSS by 15.3% on a smaller public Java corpus ([`CHANGELOG.md`](../CHANGELOG.md), #363); the issue stays open, and it assigns the streaming-ingestion remainder to [#242](https://github.com/shivyadavus/open-kioku/issues/242).

## The original eight priorities

The first roadmap for this repository named eight priorities. Their status against the current tree:

| Priority | Status | Where it lives |
|---|---|---|
| 1. Plan verification contract | Shipped. Change contracts landed in 2.1.0. | `ok plan`, `ok verify`, `ok verify-boundary`, `ok contract create\|verify\|explain`; [`change-contract.md`](change-contract.md) |
| 2. Architecture policy engine | Shipped in 2.1.0. | `ok architecture policy validate\|check\|explain`, `ok.toml` components; [`architecture-policy.md`](architecture-policy.md) |
| 3. Historical change intelligence | Shipped. Co-change signals landed in 2.0.0, incremental commit-history ingestion in 2.1.0. | `ok history churn\|similar\|provenance`; the `git_cochange` signal in [`ranking.md`](ranking.md) |
| 4. Ownership and reviewer intelligence | Shipped. CLI only since 4.0.0, when the MCP tools for it were retired. | `ok history ownership\|reviewers`; CODEOWNERS and git-log derivation in `crates/open-kioku-git/src/ownership.rs` and `reviewers.rs` |
| 5. Runtime failure evidence | Partially shipped. Local runtime artifacts under `.ok/runtime/` ingest (1.0.3, strengthened in 2.1.0) and feed the `runtime_corroboration` signal. The Sentry provider validates its configuration and answers `configured: true` with no query implementation (4.0.0). No open issue tracks the remainder. | `crates/open-kioku-sentry`; runtime tools in [`mcp-tools.md`](mcp-tools.md) |
| 6. Language-specific precision packs | Shipped for Rust, Java, Python, TypeScript/JavaScript, and Go (tree-sitter parsers in 0.1.0, language-specific analysis facts in 1.0.3). Kotlin is not in the workspace and no open issue tracks it. | `crates/open-kioku-languages/src/{rust,java,python,typescript,go}.rs`, `crates/open-kioku-resolution` |
| 7. Workflow quality benchmark suite | Partially shipped. Workflow benchmarks landed in 2.0.0; the workflow baseline is lexical-only, and grep-only and vector-only arms are not measured. Agent turns, tokens, and success with and without `ok` is open as [#386](https://github.com/shivyadavus/open-kioku/issues/386). | `ok workflow-bench`, `ok retrieval-bench`, `ok contract-bench`, `ok relationship-bench`; [`workflow-benchmarks.md`](workflow-benchmarks.md), [`retrieval-benchmark.md`](retrieval-benchmark.md), [`contract-benchmarks.md`](contract-benchmarks.md), [`relationship-benchmark.md`](relationship-benchmark.md) |
| 8. Agent contract export | Shipped. TOON output landed in 1.0.3, contract export with 2.1.0's change contracts. | `ok contract export --format json\|markdown\|toon`; [`change-contract.md`](change-contract.md), [`guides/compressed-context-and-toon.md`](guides/compressed-context-and-toon.md) |

## What already shipped

Onboarding and distribution, trust and regression coverage, core intelligence quality, tool-surface maturity, the daily watch/demo/context workflow, and optional SCIP, LSP, semantic, and runtime integrations are in place across the 3.x line and 4.0.0.

[`CHANGELOG.md`](../CHANGELOG.md) is the authoritative record of what landed and when. Measured results live in [`demo/proof/`](../demo/proof) with methodology in this directory — see [`retrieval-benchmark.md`](retrieval-benchmark.md), [`workflow-benchmarks.md`](workflow-benchmarks.md), and [`relationship-benchmark.md`](relationship-benchmark.md) for the standards a change has to clear.
