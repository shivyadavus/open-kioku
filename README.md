# Open Kioku (`ok`)

[![CI](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml/badge.svg)](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Elastic--2.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org)

**Open Kioku** (記憶, Japanese for “Memory”) is a local code-intelligence server for AI coding agents. It gives Claude, Cursor, and other MCP clients a precise, queryable index of your codebase — built from Tree-sitter symbols, a SQLite dependency graph, and a Tantivy BM25 search index, running entirely on your machine.

No cloud. No embeddings API. No guessing from a context window.

```sh
# Try it in under a minute
cargo install --path crates/open-kioku-cli
ok demo
```

`ok demo` creates a small sample repo, indexes it, and prints commands for search, symbol lookup, impact analysis, context packs, and MCP setup.

---

## The problem it solves

When you drop a large codebase into an AI chat, the model has to infer architecture, symbol locations, and call graphs from raw text. It hallucinates file paths, misses callers, and proposes changes that break unrelated modules.

Open Kioku inverts this: **index first, query precisely**. The agent calls a tool instead of guessing.

**Think of Open Kioku as the glasses an AI agent puts on to read your specific codebase perfectly.**

---

## Why Agents Use It

Open Kioku turns a repo into an evidence-backed memory layer:

- **Find facts before editing**: exact symbols, snippets, files, and line ranges.
- **Build task context**: package relevant files, symbols, tests, and graph facts for an LLM.
- **Estimate blast radius**: inspect dependencies and likely impacted files before a change.
- **Stay local by default**: no outbound network calls from the MCP server.
- **Expose trust signals**: every search result carries evidence, confidence, and match reason.

---

## What It Does

| Tool | What it returns |
|---|---|
| `search_code` | BM25-ranked chunks matching a query across all indexed files |
| `get_definition` | Exact file + line range where a symbol is defined (Tree-sitter) |
| `get_references` | Indexed references to a symbol |
| `impact_analysis` | Direct and indirect files likely affected by a change |
| `find_tests_for_change` | Test candidates derived from indexed metadata and graph facts |
| `build_context_pack` | A JSON bundle of relevant files + symbols for a described task |
| `detect_architecture` | Inferred project structure (monorepo, service boundaries, etc.) |

Full MCP tool reference: [`docs/mcp-tools.md`](docs/mcp-tools.md). Tools returned by `tools/list` include a `maturity` field (`stable` or `experimental`) so agents can prefer the reliable default surface.

---

## 🔥 Animated Web Demo

Check out our **[Animated Web Demo](https://shivyadavus.github.io/open-kioku/)** to see a hyper-realistic, blazing-fast CLI session of Open Kioku in action!
*(Alternatively, you can generate a gorgeous terminal GIF locally by running `vhs demo.tape` using [Charmbracelet VHS](https://github.com/charmbracelet/vhs))*

---

## Languages supported

Rust, Python, TypeScript, JavaScript, Java, Go. Additional grammars can be added via the `open-kioku-tree-sitter` crate.

---

## Install

```sh
git clone https://github.com/shivyadavus/open-kioku.git
cd open-kioku
cargo install --path crates/open-kioku-cli
ok --help
```

Requires Rust stable (1.78+).

Tagged releases build Linux and macOS `ok` binaries on GitHub Actions. Download the archive for your platform from the Releases page, put `ok` on your `PATH`, then verify:

```sh
ok --help
```

---

## Quickstart

### 1. Try the Built-In Demo

```sh
ok demo
```

This creates `./open-kioku-demo`, writes an `ok.toml`, builds the SQLite and Tantivy indexes, and prints commands like:

```sh
ok --repo ./open-kioku-demo search token
ok --repo ./open-kioku-demo symbol find issue_token
ok --repo ./open-kioku-demo impact --file src/auth.rs
ok --repo ./open-kioku-demo context "change token expiry" --json
ok mcp install claude --repo ./open-kioku-demo
```

Use a specific location or replace an existing demo:

```sh
ok demo --path /tmp/open-kioku-demo --force
```

### 2. Index Your Repo

```sh
ok init /path/to/your/repo
ok index /path/to/your/repo
ok doctor /path/to/your/repo
ok status /path/to/your/repo
```

The index is stored under `.ok/` in the target repo:

- `.ok/index.sqlite` stores files, symbols, chunks, imports, occurrences, and graph facts.
- `.ok/search/tantivy` stores the BM25 lexical search index.
- `ok.toml` controls indexing, security, MCP mode, and command allowlists.

### 3. Check Local Health

```sh
ok doctor /path/to/your/repo
```

`ok doctor` verifies the repo path, config, metadata index, search index, and running binary. It prints concrete next steps when something is missing.

### 4. Query from the CLI

```sh
# Full-text search
ok --repo /path/to/your/repo search "token expiration handler"

# Symbol lookup
ok --repo /path/to/your/repo symbol find PolicyGate

# Blast radius before a change
ok --repo /path/to/your/repo impact --file crates/open-kioku-mcp/src/lib.rs

# Tests that cover a file
ok --repo /path/to/your/repo tests --changed crates/open-kioku-core/src/lib.rs

# Context bundle for an LLM
ok --repo /path/to/your/repo context "refactor the BM25 scorer to support phrase queries" --json
```

---

## Connect to Claude Code

Add to `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or equivalent:

```json
{
  "mcpServers": {
    "open-kioku": {
      "command": "ok",
      "args": ["mcp", "serve", "--repo", "/absolute/path/to/your/repo", "--read-only"]
    }
  }
}
```

Or install from the Claude Code plugin marketplace:

```
/plugin install open-kioku@open-kioku
```

To print the MCP config snippet for your current repo:

```sh
ok mcp install claude --repo /absolute/path/to/your/repo
```

That command does not modify your Claude config. It prints a safe copy-paste JSON snippet.

---

## Connect to Cursor

Add to your Cursor MCP config:

```json
{
  "open-kioku": {
    "command": "ok",
    "args": ["mcp", "serve", "--repo", "${workspaceFolder}", "--read-only"]
  }
}
```

To print the Cursor config snippet:

```sh
ok mcp install cursor --repo /absolute/path/to/your/repo
```

---

## Stable vs Experimental MCP Tools

Open Kioku deliberately labels tool maturity so agents can make conservative choices.

Stable tools include:

- `search_code`, `search_files`, `regex_search`
- `list_files`, `list_symbols`, `search_symbols`
- `get_definition`, `get_references`, `get_symbol_context`
- `dependency_path`, `impact_analysis`, `module_dependencies`
- `build_context_pack`, `explain_file`, `explain_symbol`
- `find_tests_for_change`, `recommend_validation_plan`
- `detect_architecture`, `architecture_boundaries`, `architecture_violations`

Experimental tools are exposed for early workflows but may use heuristic or fallback behavior:

- `semantic_search`
- `structural_search`
- `get_implementations`, `get_callers`, `get_callees`
- runtime integrations such as stacktrace and recent failure mapping

See [`docs/mcp-tools.md`](docs/mcp-tools.md) for the full list.

---

## What Was Recently Added

- `ok` is now the installed binary name.
- `ok doctor` provides local health checks.
- `ok demo` creates and indexes a sample repo for instant evaluation.
- `ok mcp install claude|cursor` prints copy-paste MCP config.
- CLI smoke tests cover the first-run flow.
- Search tests assert snippets, evidence, confidence, symbols, and match reasons.
- MCP `tools/list` now includes `maturity`.
- Tagged releases build Linux and macOS binary archives.

---

## Security

- **Read-only by default.** `apply_patch` and write tools require `--allow-write` explicitly. Additionally, the `apply_patch` MCP tool requires the `OPEN_KIOKU_ALLOW_WRITE=true` environment variable to be set on the MCP server.
- **No network calls.** The MCP server never makes outbound connections.
- **Secret path blocking.** `.env`, `.aws/`, `.ssh/`, and similar paths are excluded from indexing at the policy layer (`PolicyGate`), not just `.gitignore`.

---

## Codebase

34-crate Cargo workspace. CI runs `cargo fmt`, `cargo clippy -D warnings`, `cargo test --all`, `cargo audit`, and `cargo deny` on Ubuntu and macOS on every push.

Key crates:

- `open-kioku-core` — graph, indexer, query planner
- `open-kioku-storage-sqlite` — metadata and dependency graph
- `open-kioku-search-tantivy` — BM25 full-text index
- `open-kioku-tree-sitter` — AST extraction and symbol resolution
- `open-kioku-mcp` — MCP server (JSON-RPC 2.0 over stdio)
- `open-kioku-cli` — the `ok` binary

See [`docs/architecture.md`](docs/architecture.md) for the full data flow.
See [`docs/roadmap.md`](docs/roadmap.md) for the product and engineering roadmap.

---

## Roadmap Highlights

**Recently Completed:**
- Interactive first-run onboarding (`ok demo`, `ok doctor`, MCP setup helpers).
- Comprehensive benchmark output (`ok bench`) for index time, files per second, and BM25/Regex search latency.
- Golden MCP response snapshots and multi-language integration test fixtures.

**Current priorities:**
- Improve symbol/reference quality with stronger Tree-sitter, SCIP, and LSP integration.
- Advance the maturity of experimental semantic search and impact analysis tools.
- Add `cargo binstall` and Homebrew distribution paths.
- Support remote backend indexes (e.g., PostgreSQL / RuVector integrations).

Full plan: [`docs/roadmap.md`](docs/roadmap.md).

---

## Contributing

PRs welcome, especially for new Tree-sitter grammars and SCIP indexer support. See [`CONTRIBUTING.md`](CONTRIBUTING.md).
