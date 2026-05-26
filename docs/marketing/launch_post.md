# Show HN: Open Kioku – Give AI Agents perfect glasses to read your codebase

Hey HN,

If you've tried using Claude Desktop or an AI agent to read a large codebase via MCP, you've probably noticed a massive flaw: it hallucinates function signatures, struggles to follow deep dependency paths, and randomly gets stuck reading 10,000 lines of minified JavaScript when it's supposed to be debugging your Rust app.

This happens because most codebase tools rely on either naive regex `grep` (which doesn't understand syntax) or standard Vector databases (BM25) which split code into meaningless chunks and strip out compiler relationships. 

We built **Open Kioku** to solve this.

Open Kioku is an enterprise-grade, blazing-fast codebase index engine and semantic search MCP. It bridges the gap by building a unified graph of your local codebase utilizing Tree-sitter for AST parsing, SQLite for relationship graphs (who calls what?), and Tantivy for lightning-fast BM25 text search.

**How it works:**
1. You run `npx open-kioku init` or `ok init .` in your repo.
2. It indexes your entire repository locally in seconds (~80 files/sec on average).
3. It exposes a set of intelligent Model Context Protocol (MCP) tools:
   - `search_code`: Instant BM25 query ranked search
   - `get_symbol_context`: Exact file + line range where a symbol lives
   - `dependency_path`: Follow the graph between two files or symbols
   - `impact_analysis`: What will break if I touch this file?
   - `find_tests_for_change`: Instantly map a changed function to the test that covers it.

**Why it matters:**
Instead of your LLM blindly "grepping" and failing, Open Kioku gives the agent a localized, deterministic graph of the code. We wrote the engine in Rust so that it runs silently in the background of your editor or Claude Desktop without hogging memory. 

The demo is entirely interactive right here: https://shivyadavus.github.io/open-kioku/demo/index.html

We just launched the NPM wrapper (`npx open-kioku`), and you can also grab it via `cargo binstall open-kioku` or Homebrew. 

We'd love for you to try it out on your largest, messiest monorepos and let us know what the agent successfully navigates that it previously failed on!

Repo: https://github.com/shivyadavus/open-kioku
