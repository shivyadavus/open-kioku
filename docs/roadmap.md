# Roadmap

Open Kioku should win by making AI coding agents stop guessing. The roadmap is ordered by the path from first install to trusted daily use.

## 1. Onboarding and Distribution

- Ship the binary as `ok`.
- Provide `ok doctor` for local health checks.
- Provide `ok mcp install <client>` to print copy-paste MCP config for Claude and Cursor.
- Publish signed release binaries for macOS and Linux.
- Add Homebrew and `cargo binstall` installation paths.

## 2. Trust and Regression Coverage

- Add smoke tests for `ok init`, `ok index`, `ok search`, `ok status`, and MCP tool listing.
- Add fixture repos for Rust, TypeScript, Python, and Go.
- Add golden snapshots for important MCP responses.
- Keep CI running format, clippy, tests, audit, and deny on Linux and macOS.

## 3. Core Intelligence Quality

- Improve ranked snippets for `search_code`.
- Strengthen symbol definition/reference accuracy using tree-sitter plus SCIP when available.
- Return consistent evidence, confidence, and match reasons from every result.
- Add quality benchmarks for precision on fixture repos.

## 4. Tool Surface Maturity

- Split tools into stable and experimental groups.
- Hide or clearly label unsupported integrations so agents do not treat stubs as authoritative.
- Keep the stable default tool set small, sharp, and reliable.

## 5. Daily Workflow

- Make watch mode keep `.ok/` current while editing.
- Add `ok demo` or a bundled demo fixture for fast evaluation.
- Add context-pack export formats for JSON, Markdown, and compact prompt text.
- Add benchmark output for index time, files per second, and search latency.

## 6. Advanced Integrations

- Harden LSP and SCIP import paths.
- Add runtime error mapping for Sentry only after local code intelligence is dependable.
- Add optional semantic search providers without making cloud calls part of the default path.
