# Open Kioku (`ok`)

[![CI](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml/badge.svg)](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Elastic--2.0-blue)](LICENSE)
[![npm](https://img.shields.io/npm/v/open-kioku)](https://www.npmjs.com/package/open-kioku)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org)

Open Kioku is a local-first code intelligence MCP for AI coding agents. It indexes a repository on your machine and gives Claude, Cursor, and other MCP clients fast code search, symbol lookup, impact analysis, test hints, and context packs.

No hosted index. No embeddings API required. No source upload.

```sh
npm install -g open-kioku
ok demo --force
ok --repo ./open-kioku-demo plan token --format markdown
```

The fastest way to see it is the hosted demo: https://shivyadavus.github.io/open-kioku/

## Why It Exists

AI coding agents are strongest when they can ask the codebase for facts before editing. Without indexed context, they burn tokens on repeated file crawling, infer references from text matches, and often pick tests only after a change has already gone wrong.

Open Kioku gives agents a pre-edit routine:

1. Search indexed code and files.
2. Resolve symbols and references.
3. Build an evidence-backed pre-edit plan with likely impact and validation targets.
4. Serve those capabilities through MCP over local stdio.

## Install

### npm

```sh
npm install -g open-kioku
ok --version
```

The `open-kioku` npm package is a JavaScript wrapper that installs the native `ok` binary through platform-specific optional dependencies.

Published platform packages:

- `@open-kioku/darwin-x64`
- `@open-kioku/darwin-arm64`
- `@open-kioku/linux-x64`
- `@open-kioku/linux-arm64`
- `@open-kioku/win32-x64`

### Homebrew

```sh
brew install shivyadavus/open-kioku/open-kioku
ok --version
```

The Homebrew formula installs the native release binary for macOS or Linux from GitHub Releases.

### cargo-binstall

```sh
cargo binstall open-kioku-cli
ok --version
```

`open-kioku-cli` includes cargo-binstall metadata for the same native release binaries. If a binary is unavailable for your platform, use the source install path below.

### GitHub Releases

Tagged releases publish native binaries and SHA-256 checksums for:

- Linux x86_64 musl
- Linux arm64 musl
- macOS x86_64
- macOS arm64
- Windows x86_64

Download from https://github.com/shivyadavus/open-kioku/releases, put `ok` on your `PATH`, then run:

```sh
ok --help
```

### From Source

```sh
git clone https://github.com/shivyadavus/open-kioku.git
cd open-kioku
cargo install --path crates/open-kioku-cli
ok --help
```

Requires a stable Rust toolchain.

## Quick Start

Create and index the built-in sample repo:

```sh
ok demo --force
```

This creates `./open-kioku-demo`, writes `ok.toml`, builds `.ok/index.sqlite`, builds `.ok/search/tantivy`, and prints starter commands.

Try the same verified flow used by the hosted demo:

```sh
ok --repo ./open-kioku-demo search token --limit 5
ok --repo ./open-kioku-demo symbol find issue_token
ok --repo ./open-kioku-demo impact --file src/auth.rs
ok --repo ./open-kioku-demo context token --format markdown
ok --repo ./open-kioku-demo plan token --format markdown
ok mcp install cursor --repo ./open-kioku-demo
```

Use a custom demo path when needed:

```sh
ok demo --path /tmp/open-kioku-demo --force
```

## Index Your Repo

```sh
ok init /path/to/repo
ok index /path/to/repo
ok watch /path/to/repo
ok doctor /path/to/repo
ok status /path/to/repo
```

Open Kioku stores local index data inside the target repository:

- `.ok/index.sqlite` stores files, symbols, chunks, imports, occurrences, and graph facts.
- `.ok/search/tantivy` stores the local BM25 search index.
- `ok.toml` controls indexing, security, MCP mode, and command allowlists.

`ok doctor` checks the repo path, config, SQLite index, Tantivy index, and running binary, then prints concrete next steps for anything missing.

`ok watch` performs an initial local index and then keeps `.ok/index.sqlite` and `.ok/search/tantivy` current with a debounced reindex when repository files change.

## CLI Commands

```sh
# Search indexed code
ok --repo /path/to/repo search "token expiration handler"

# Symbol lookup
ok --repo /path/to/repo symbol find PolicyGate
ok --repo /path/to/repo symbol definition PolicyGate
ok --repo /path/to/repo symbol refs PolicyGate

# Impact and validation
ok --repo /path/to/repo impact --file crates/open-kioku-mcp/src/lib.rs
ok --repo /path/to/repo tests --changed crates/open-kioku-core/src/lib.rs

# Context pack for an agent
ok --repo /path/to/repo context "update MCP tool list docs" --format markdown

# Pre-edit plan for an agent
ok --repo /path/to/repo plan "update MCP tool list docs" --format markdown

# Benchmark indexing and search
ok bench /path/to/repo

# Keep the local index current while editing
ok watch /path/to/repo
```

Current top-level commands:

`init`, `index`, `watch`, `status`, `doctor`, `demo`, `search`, `symbol`, `explain`, `impact`, `path`, `tests`, `context`, `plan`, `bench`, `architecture`, `patch`, and `mcp`.

## MCP Setup

Open Kioku runs as a local MCP server over stdio.

Print a Claude config snippet:

```sh
ok mcp install claude --repo /absolute/path/to/repo
```

Print a Cursor config snippet:

```sh
ok mcp install cursor --repo /absolute/path/to/repo
```

Those commands print JSON. They do not modify your editor configuration.

Example server command:

```sh
ok mcp serve --repo /absolute/path/to/repo --read-only
```

Write mode is disabled by default. To expose write tools, the server must be started with `--allow-write`, and patch application still requires explicit approval and `OPEN_KIOKU_ALLOW_WRITE=true`.

## MCP Tools

Stable tools include:

- Repo inventory: `repo_status`, `list_files`, `list_languages`
- Symbols: `list_symbols`, `search_symbols`, `get_definition`, `get_references`, `get_symbol_context`
- Search: `search_code`, `search_files`, `regex_search`
- Graph and impact: `dependency_path`, `impact_analysis`, `module_dependencies`
- Context and planning: `build_context_pack`, `plan_change`, `explain_file`, `explain_symbol`, `summarize_architecture`
- Tests: `find_tests_for_change`, `recommend_validation_plan`, `explain_test_coverage`
- Architecture: `detect_architecture`, `architecture_boundaries`, `architecture_violations`
- Patch planning: `propose_patch`, `review_patch`, `validate_patch`

Experimental tools are present for early workflows and may use heuristic or fallback behavior:

- `semantic_search`
- `structural_search`
- `get_implementations`, `get_callers`, `get_callees`
- `explain_flow`
- Runtime mapping tools such as `map_stacktrace_to_code`, `find_errors_for_symbol`, and `find_recent_failures`
- `apply_patch`, only when write mode is enabled

Every tool returned by `tools/list` includes a `maturity` field. Start the server with `--hide-experimental` when you want agents to see only the stable surface.

Full tool notes: [`docs/mcp-tools.md`](docs/mcp-tools.md).

Verified command output: [`docs/proof.md`](docs/proof.md).

Local usefulness proof: [`docs/usefulness-proof.md`](docs/usefulness-proof.md). The proof harness runs `ok plan` against real local repositories, scores whether the returned context, impact, validation, risk, and agent tool calls are grounded in the indexed repo, and intentionally omits source snippets from the report.

## What Is Local

Open Kioku's default path is local:

- Tree-sitter extracts symbols and chunks from supported source files.
- SQLite stores metadata and dependency graph rows under `.ok/`.
- Tantivy stores BM25 lexical search data under `.ok/search/tantivy`.
- MCP uses stdio to talk to the local `ok` process.
- Semantic search is not required for the default workflow.

The MCP server is designed to be read-only unless write mode is explicitly enabled.

## Language Support

Tree-sitter parsing currently covers Rust, Python, TypeScript, TSX, JavaScript, Go, and Java. Files in other languages can still be indexed as files/chunks where supported by the ingest pipeline, but symbol quality depends on available grammar support.

## Security Model

- Read-only by default.
- No hosted index or cloud search service.
- No embeddings API required for default search, symbol, impact, and context workflows.
- Secret-like paths such as `.env`, `.aws/`, and `.ssh/` are blocked by policy.
- Command execution and patch application are policy-gated.
- Network denial is part of the MCP security config.

See [`docs/security-model.md`](docs/security-model.md) for more detail.

Operational security notes: [`SECURITY.md`](SECURITY.md).

## Repository Layout

This is a 38-crate Cargo workspace. Important crates:

- `open-kioku-cli`: the `ok` binary.
- `open-kioku-mcp`: JSON-RPC MCP server over stdio.
- `open-kioku-ingest`: repository indexing pipeline.
- `open-kioku-tree-sitter`: syntax parsing and symbol extraction.
- `open-kioku-storage-sqlite`: SQLite metadata and graph storage.
- `open-kioku-search-tantivy`: disk-backed BM25 search.
- `open-kioku-context`: task context pack builder.
- `open-kioku-impact`: file impact analysis.
- `open-kioku-tests`: validation target selection.

Architecture docs: [`docs/architecture.md`](docs/architecture.md)

Roadmap: [`docs/roadmap.md`](docs/roadmap.md)

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo test -p open-kioku-cli --test cli_smoke
```

CI also runs audit and dependency policy checks.

## Contributing

Issues and PRs are welcome, especially for parser quality, fixture coverage, MCP tool quality, and distribution improvements.

See [`CONTRIBUTING.md`](CONTRIBUTING.md).
