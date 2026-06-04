# Open Kioku (`ok`)

[![CI](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml/badge.svg)](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Elastic--2.0-blue)](LICENSE)
[![npm](https://img.shields.io/npm/v/open-kioku)](https://www.npmjs.com/package/open-kioku)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org)

Open Kioku is a local-first code intelligence MCP for AI coding agents. It indexes a repository on your machine and gives Claude, Cursor, and other MCP clients fast code search, symbol lookup, impact analysis, test hints, context packs, repo-scoped memory, and reversible compressed context handles.

No hosted index. No embeddings API required. No source upload.

Semantic search is opt-in. The default MCP path stays lexical and offline; enabling `semantic.enabled = true` uses the built-in local hash provider unless another provider is explicitly supported and configured.

```sh
npm install -g open-kioku
ok init /path/to/your/repo
ok index /path/to/your/repo
ok mcp install cursor --repo /path/to/your/repo
```

The fastest way to see it is the hosted demo: https://shivyadavus.github.io/open-kioku/

## Why It Exists

AI coding agents are strongest when they can ask the codebase for facts before editing. Without indexed context, they burn tokens on repeated file crawling, infer references from text matches, and often pick tests only after a change has already gone wrong.

Open Kioku gives agents a pre-edit routine:

1. Search indexed code and files.
2. Resolve symbols and references.
3. Build an evidence-backed pre-edit plan with likely impact and validation targets.
4. Recall prior repo facts without letting memory outrank indexed code evidence.
5. Compress context into handles that can retrieve the original snippets later.
6. Serve those capabilities through MCP over local stdio.

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

### crates.io

```sh
cargo install open-kioku-cli
ok --version
```

This compiles the CLI from crates.io. Use this path when you already have Rust installed or when native release binaries are not available for your platform.

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

Use this path for a real repository:

```sh
npm install -g open-kioku
ok init /absolute/path/to/repo
ok index /absolute/path/to/repo
ok doctor /absolute/path/to/repo
ok --repo /absolute/path/to/repo search "the feature or bug you care about" --limit 5
ok mcp install cursor --repo /absolute/path/to/repo
ok mcp install claude --repo /absolute/path/to/repo
```

`ok index` writes local data under `.ok/`: SQLite metadata and graph rows in `.ok/index.sqlite`, plus BM25 search data in `.ok/search/tantivy`. Repo memory is append-only under `.ok/memory.sqlite`; compressed context originals are retrievable from `.ok/context.sqlite`. Large indexes report progress phases such as `scan`, `parse`, `occurrences`, `store`, `graph`, `search`, and `complete`.

Paste the printed MCP config snippet into Cursor, Claude Code, or another MCP-compatible agent. The default server is read-only and runs locally over stdio.

Ask your agent to use Open Kioku before editing:

```text
Use Open Kioku before editing. Check repo_status, search_code, get_definition,
get_references, impact_analysis, and find_tests_for_change. Build a plan first,
then edit only after the indexed evidence is clear.
```

Keep the index fresh while editing:

```sh
ok watch /absolute/path/to/repo
```

## Quality Mode

Open Kioku works without external language indexers, but exact references improve search grounding, impact analysis, test selection, and planning. Check what is available:

```sh
ok scip doctor /absolute/path/to/repo
ok scip setup /absolute/path/to/repo
ok index /absolute/path/to/repo --with-scip auto
ok doctor /absolute/path/to/repo
```

Default indexing consumes existing SCIP files such as `index.scip` and `.ok/indexes/*.scip` when present. `--with-scip auto` runs installed indexers for supported repos; it does not install third-party tools. `--with-scip required` fails the index if no SCIP facts can be imported.

Use `ok eval` to protect quality on real workflows:

```sh
ok eval /absolute/path/to/repo \
  --case "fix token expiration=src/auth.rs,tests/auth_flow.rs" \
  --min-recall-at-k 0.8 \
  --min-mrr 0.5
```

## Try The Demo

```sh
ok demo --force
ok --repo ./open-kioku-demo search token --limit 5
ok --repo ./open-kioku-demo plan token --format markdown
ok --repo ./open-kioku-demo plan token --format toon
ok --repo ./open-kioku-demo memory remember "auth flow maps issue_token to tests/auth_flow.rs" --source demo
ok --repo ./open-kioku-demo --json context token --compressed
ok --repo ./open-kioku-demo context token --compressed --format toon
ok prove ./open-kioku-demo --task token
ok mcp install cursor --repo ./open-kioku-demo
ok mcp install claude --repo ./open-kioku-demo
```

`ok demo` creates `./open-kioku-demo`, writes `ok.toml`, and builds the local SQLite and Tantivy indexes. Use `ok demo --path /tmp/open-kioku-demo --force` for a custom path.

## Useful Commands

```sh
ok --repo /path/to/repo search "token expiration handler"
ok --repo /path/to/repo symbol definition PolicyGate
ok --repo /path/to/repo symbol refs PolicyGate
ok --repo /path/to/repo impact --file crates/open-kioku-mcp/src/lib.rs
ok --repo /path/to/repo tests --changed crates/open-kioku-core/src/lib.rs
ok --repo /path/to/repo context "update MCP docs" --format markdown
ok --repo /path/to/repo --json context "update MCP docs" --compressed
ok --repo /path/to/repo context "update MCP docs" --compressed --format toon
ok --repo /path/to/repo memory remember "release workflow uses scripts/publish-crates.sh" --source human
ok --repo /path/to/repo memory search "release workflow"
ok --repo /path/to/repo plan "update MCP docs" --format markdown
ok --repo /path/to/repo plan "update MCP docs" --format toon
ok eval /path/to/repo --case "auth flow=src/auth.rs,tests/auth_flow.rs"
ok prove /path/to/repo --task "auth flow" --task "release workflow"
ok bench /path/to/repo
```

Current top-level commands: `init`, `index`, `watch`, `status`, `doctor`, `demo`, `search`, `symbol`, `explain`, `impact`, `path`, `tests`, `context`, `retrieve-context`, `plan`, `bench`, `prove`, `architecture`, `patch`, `memory`, `mcp`, and `scip`.

Full MCP tool notes: [`docs/mcp-tools.md`](docs/mcp-tools.md). Verified command output: [`docs/proof.md`](docs/proof.md). Local usefulness proof: [`docs/usefulness-proof.md`](docs/usefulness-proof.md).

## What Is Local

Open Kioku's default path is local:

- Tree-sitter extracts symbols and chunks from supported source files.
- SQLite stores metadata and dependency graph rows under `.ok/`.
- Tantivy stores BM25 lexical search data under `.ok/search/tantivy`.
- Repo memory facts are append-only and local under `.ok/memory.sqlite`.
- Reversible compressed context handles store originals locally under `.ok/context.sqlite`.
- TOON is an optional prompt-rendering format for compact LLM handoff; JSON remains the internal and MCP structured data format.
- MCP uses stdio to talk to the local `ok` process.
- Semantic search is not required for the default workflow.

The MCP server is designed to be source-tree read-only unless write mode is explicitly enabled. Memory and compressed-context tools may write local `.ok/` artifacts so their results can be recalled or expanded later.

## Language Support

Tree-sitter parsing currently covers Rust, Python, TypeScript, TSX, JavaScript, Go, and Java. Files in other languages can still be indexed as files/chunks where supported by the ingest pipeline, but symbol quality depends on available grammar support.

## Security Model

- Read-only by default.
- No hosted index or cloud search service.
- No embeddings API required for default search, symbol, impact, and context workflows.
- Optional semantic search can run with the built-in local provider and no network calls.
- Secret-like paths such as `.env`, `.aws/`, and `.ssh/` are blocked by policy.
- Command execution and patch application are policy-gated.
- Network denial is part of the MCP security config.

See [`docs/security-model.md`](docs/security-model.md) for more detail.

Operational security notes: [`SECURITY.md`](SECURITY.md).

## Repository Layout

This is a 41-crate Cargo workspace. Important crates:

- `open-kioku-cli`: the `ok` binary.
- `open-kioku-mcp`: JSON-RPC MCP server over stdio.
- `open-kioku-ingest`: repository indexing pipeline.
- `open-kioku-tree-sitter`: syntax parsing and symbol extraction.
- `open-kioku-storage-sqlite`: SQLite metadata and graph storage.
- `open-kioku-search-tantivy`: disk-backed BM25 search.
- `open-kioku-context`: task context pack builder.
- `open-kioku-context-compress`: reversible context handle compression.
- `open-kioku-format`: prompt-oriented renderers, including TOON.
- `open-kioku-memory`: append-only repo memory and entity-linked recall.
- `open-kioku-impact`: file impact analysis.
- `open-kioku-tests`: validation target selection.

Architecture docs: [`docs/architecture.md`](docs/architecture.md)

Roadmap: [`docs/roadmap.md`](docs/roadmap.md)

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo test -p open-kioku-cli --test cli_smoke
```

CI also runs audit and dependency policy checks.

## Contributing

Issues and PRs are welcome, especially for parser quality, fixture coverage, MCP tool quality, and distribution improvements.

See [`CONTRIBUTING.md`](CONTRIBUTING.md).
