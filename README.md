# Open Kioku (`ok`)

![Build Status](https://img.shields.io/badge/build-passing-success)
![Rust Version](https://img.shields.io/badge/rust-stable-orange)
![License](https://img.shields.io/badge/license-MIT-blue)

**Open Kioku** (記憶 - Japanese for "Memory") is a blazing-fast, local-first code intelligence platform. It gives AI agents, IDEs, and CLI tools a persistent, evidence-backed memory of your repository. 

> **The Core Philosophy:** The LLM is not the source of truth. The indexed repository facts are.

---

## ⚡ Why Open Kioku?

When you pass an entire codebase to an LLM, you are trusting the model to guess the architecture. **Open Kioku** changes the paradigm by building a high-speed local graph of your codebase *first*.

- **Blazing Fast BM25 Index:** Backed by `tantivy`, it instantly finds relevant chunks without relying on expensive cloud embeddings.
- **Tree-sitter Precision:** Automatically extracts symbols, imports, and definitions across Rust, Java, Python, TypeScript, and Go.
- **Model Context Protocol (MCP):** Connects directly to Claude Desktop so your AI can instantly navigate your local repositories.
- **Security First:** 100% local, read-only by default, and actively blocks secret paths (`.env`, `.aws`, `.ssh`).

---

## 📦 Installation

Install `ok` directly from source using Cargo:

```sh
# Clone the repository
git clone https://github.com/shivyadavus/open-kioku.git
cd open-kioku

# Install the `ok` binary globally
cargo install --path crates/open-kioku-cli
```

---

## 🤖 Claude Desktop Integration (MCP)

Give Claude the ability to read your mind (and your code). Open Kioku fully implements the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/).

Add `ok` to your Claude Desktop configuration file (on macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "open-kioku": {
      "command": "ok",
      "args": [
        "mcp",
        "serve",
        "--repo",
        "/absolute/path/to/your/repository",
        "--read-only"
      ]
    }
  }
}
```

> **Pro Tip:** Make sure the `ok` binary is in your system's `PATH`, or replace `"ok"` with the absolute path to your binary (e.g., `/Users/yourname/.cargo/bin/ok`).

---

## 💻 CLI Quick Start

You don't need Claude to use Open Kioku. It acts as an incredibly powerful standalone CLI tool for navigating massive codebases.

### 1. Build the Memory
```sh
# Initialize and index the current repository
ok init
ok index .
ok status
```

### 2. Query the Memory
```sh
# Search the local BM25/Tantivy index for specific business logic
ok search "jwt validation token expiration"

# Find precise symbol definitions and references via Tree-sitter
ok symbol find AuthMiddleware

# Generate an AI-ready context bundle (JSON) for a complex refactor
ok context "Migrate the deprecated auth middleware to use the new JWT validation service" --json

# Analyze the blast radius of modifying a core file
ok impact --file src/auth/jwt_middleware.rs

# Find test targets related to changed files
ok tests --changed src/auth/jwt_middleware.rs
```

---

## 🏗️ Architecture Under the Hood

When you run `ok index .`, Open Kioku scans the repository, fingerprints files, detects languages, and extracts symbols/chunks into a highly optimized SQLite metadata graph under `.ok/`. Simultaneously, it builds a disk-backed Tantivy BM25 search index under `.ok/search/tantivy`.

The repository is structured as a modular Cargo workspace containing 38 highly-specialized crates:
- **Core Engine:** `open-kioku-core`, `open-kioku-storage-sqlite`, `open-kioku-search-tantivy`
- **Parsing:** `open-kioku-tree-sitter`, `open-kioku-scip`
- **Integrations:** `open-kioku-mcp`, `open-kioku-github`, `open-kioku-jira`

For a deep dive into the internal data flow, see [docs/architecture.md](docs/architecture.md).

---

## 🔒 Security Posture

Open Kioku operates on a principle of least privilege:
- **Read-Only by Default:** No file writes or shell executions are permitted without explicit overrides.
- **Zero Network:** All indexing and BM25 searching happens 100% locally on your machine.
- **Secret Scanning:** Hardcoded rules deny indexing of sensitive paths.

## 🤝 Contributing
Open Kioku is open-source and modular by design. Pull requests for new Tree-sitter grammars, SCIP indexers, or MCP tools are highly welcome! Check out `CONTRIBUTING.md` to get started.
