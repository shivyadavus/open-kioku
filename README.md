# Open Kioku (`ok`)

[![CI](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml/badge.svg)](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](https://www.rust-lang.org)

**Open Kioku** (記憶, Japanese for “Memory”) is a local MCP server that gives Claude and Cursor a precise, queryable index of your codebase — built from Tree-sitter ASTs and a BM25 full-text engine, running entirely on your machine.

No cloud. No embeddings API. No guessing from a context window.

---

## The problem it solves

When you drop a large codebase into an AI chat, the model has to infer architecture, symbol locations, and call graphs from raw text. It hallucinates file paths, misses callers, and proposes changes that break unrelated modules.

Open Kioku inverts this: **index first, query precisely**. The agent calls a tool instead of guessing.

---

## What it does

| Tool | What it returns |
|---|---|
| `search_code` | BM25-ranked chunks matching a query across all indexed files |
| `get_definition` | Exact file + line range where a symbol is defined (Tree-sitter) |
| `get_references` | Every call site for a symbol |
| `impact_analysis` | All files that transitively depend on a given file |
| `find_tests_for_change` | Tests that cover a file, derived from the dependency graph |
| `build_context_pack` | A JSON bundle of relevant files + symbols for a described task |
| `detect_architecture` | Inferred project structure (monorepo, service boundaries, etc.) |

Full tool list: [`skills/open-kioku/SKILL.md`](skills/open-kioku/SKILL.md)

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

Tagged releases build Linux and macOS `ok` binaries on GitHub Actions. Download the archive for your platform from the Releases page, put `ok` on your `PATH`, then run:

```sh
ok doctor /path/to/your/repo
```

---

## Index your repo

```sh
ok index /path/to/your/repo
ok status
ok doctor /path/to/your/repo
```

The index is stored under `.ok/` in the target repo (SQLite + Tantivy). It is incremental: re-running `ok index` only processes changed files.

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

## CLI usage

```sh
# Full-text search
ok search "token expiration handler"

# Symbol lookup
ok symbol find PolicyGate

# Blast radius before a change
ok impact --file crates/open-kioku-mcp/src/lib.rs

# Tests that cover a file
ok tests --changed crates/open-kioku-core/src/index.rs

# Context bundle for a task (outputs JSON for piping to an LLM)
ok context "refactor the BM25 scorer to support phrase queries" --json
```

---

## Security

- **Read-only by default.** `apply_patch` and write tools require `--allow-write` explicitly.
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

## Contributing

PRs welcome, especially for new Tree-sitter grammars and SCIP indexer support. See [`CONTRIBUTING.md`](CONTRIBUTING.md).
