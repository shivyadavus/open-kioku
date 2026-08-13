# Roadmap

Open Kioku should win by making AI coding agents stop guessing. The roadmap is ordered by the path from first install to trusted daily use.

## 1. Onboarding and Distribution

- Done: Ship the binary as `ok`.
- Done: Provide `ok doctor` for local health checks.
- Done: Provide `ok mcp install <client>` to print copy-paste MCP config for all supported clients.
- Done: Publish release binaries and SHA-256 checksums for macOS, Linux, and Windows.
- Done: Add `cargo binstall`, npm, crates.io, and GitHub release installation paths.
- Done: Publish GitHub build-provenance attestations for each release binary, alongside checksums, SBOM, and release metadata. See [`docs/release-trust.md`](release-trust.md).

## 2. Trust and Regression Coverage

- Done: Add smoke tests for `ok init`, `ok index`, `ok search`, `ok status`, and MCP tool listing.
- Done: Add fixture repos for Rust, TypeScript, Python, and Go.
- Done: Add golden snapshots for important MCP responses.
- Done: Keep CI running format, clippy, tests, audit, and deny on Linux and macOS.
- Done: Add `ok prove` for shareable local usefulness reports without source snippets.
- Done: Add release-readiness smoke coverage for demo setup, status, setup audit, TOON planning, proof generation, and MCP installer output.
- Done: Keep golden MCP snapshots for representative status, schema, graph-query, ranked-search, pagination, malformed-input, and tool-error calls.

## 3. Core Intelligence Quality

- Done: Improve ranked snippets for `search_code`.
- Done: Strengthen symbol definition/reference accuracy using tree-sitter plus SCIP when available.
- Done: Return consistent evidence, confidence, and match reasons from every result.
- Done: Add quality benchmarks for precision on fixture repos and real local repos.
- Done: Add language-specific static facts and optional runtime facts to the graph so plans can reason about routes, config keys, tables, inheritance, and implementations.
- Done: Add a documented, fixture-backed Java SCIP proof path that generates the standard `index.scip`, requires a successful import, and surfaces the exact-reference count.

## 4. Tool Surface Maturity

- Done: Split tools into stable and experimental groups.
- Done: Hide or clearly label unsupported integrations so agents do not treat stubs as authoritative.
- Done: Keep the stable default tool set small, sharp, and reliable.
- Done: Cover representative history, ownership, and reviewer MCP responses with deterministic fixtures and golden protocol snapshots.
- Done: Cover disabled semantic status, explicit semantic-search unavailability, and hybrid lexical fallback with golden MCP snapshots.
- Done: Cover the disabled-by-default responses for runtime stack-trace and error lookup tools with golden MCP snapshots.
- Done: Cover representative explanatory, structural-candidate, implementation-candidate, and architecture-flow MCP responses with golden snapshots.
- Done: Make experimental caller and callee lookups directional and cover both paths with golden MCP snapshots.
- Next: Finish representative fixture-backed MCP snapshots for experimental tools before graduating any of them to stable.

## 5. Daily Workflow

- Done: Make watch mode keep `.ok/` current while editing with debounced local reindexing.
- Done: Keep `ok demo` useful as the fastest way to evaluate search, symbols, impact, context packs, planning, and MCP setup.
- Done: Add context-pack export formats for JSON, Markdown, and compact prompt text.
- Done: Add benchmark output for index time, files per second, and search latency.
- Done: Add launch kit drafts and directory submission copy grounded in tested commands.

## 6. Advanced Integrations

- Done: Harden LSP and SCIP import paths.
- Done: Add runtime error mapping guardrails for Sentry only after local code intelligence is dependable.
- Done: Add optional semantic search providers without making cloud calls part of the default path.
