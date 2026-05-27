# Roadmap

Open Kioku should win by making AI coding agents stop guessing. The roadmap is ordered by the path from first install to trusted daily use.

## 1. Onboarding and Distribution

- Done: Ship the binary as `ok`.
- Done: Provide `ok doctor` for local health checks.
- Done: Provide `ok mcp install <client>` to print copy-paste MCP config for Claude and Cursor.
- Done: Publish release binaries and SHA-256 checksums for macOS, Linux, and Windows.
- Done: Add Homebrew and `cargo binstall` installation paths.
- Next: Add provenance signing or attestations for release artifacts.

## 2. Trust and Regression Coverage

- Done: Add smoke tests for `ok init`, `ok index`, `ok search`, `ok status`, and MCP tool listing.
- Done: Add fixture repos for Rust, TypeScript, Python, and Go.
- Done: Add golden snapshots for important MCP responses.
- Done: Keep CI running format, clippy, tests, audit, and deny on Linux and macOS.
- Next: Expand golden snapshots beyond tool listing to representative tool calls.

## 3. Core Intelligence Quality

- Done: Improve ranked snippets for `search_code`.
- Done: Strengthen symbol definition/reference accuracy using tree-sitter plus SCIP when available.
- Done: Return consistent evidence, confidence, and match reasons from every result.
- In progress: Add quality benchmarks for precision on fixture repos and real local repos.

## 4. Tool Surface Maturity

- Done: Split tools into stable and experimental groups.
- Done: Hide or clearly label unsupported integrations so agents do not treat stubs as authoritative.
- Done: Keep the stable default tool set small, sharp, and reliable.
- Next: Graduate experimental tools only with fixture-backed behavior and snapshots.

## 5. Daily Workflow

- Done: Make watch mode keep `.ok/` current while editing with debounced local reindexing.
- Done: Keep `ok demo` useful as the fastest way to evaluate search, symbols, impact, context packs, planning, and MCP setup.
- Done: Add context-pack export formats for JSON, Markdown, and compact prompt text.
- Done: Add benchmark output for index time, files per second, and search latency.

## 6. Advanced Integrations

- Next: Harden LSP and SCIP import paths.
- Next: Add runtime error mapping for Sentry only after local code intelligence is dependable.
- Next: Add optional semantic search providers without making cloud calls part of the default path.
