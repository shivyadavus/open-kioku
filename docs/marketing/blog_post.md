# Why RAG is Broken for Codebases (And How We Fixed It)

If you've ever watched an AI agent try to debug a complex repository, you've witnessed a tragedy of context. 

An agent generally interacts with your code through two primary methods today:
1. **Naive Regex/Grep Search:** The agent searches for `class User`, gets 500 unranked results back across `tests/`, `dist/`, and `node_modules/`, runs out of context window, and fails.
2. **Vector DB (RAG) Chunks:** The codebase is chunked into 512-token segments and embedded. The agent searches for "database connection", gets a floating chunk of a `try/catch` block with zero context about what file it is in, what dependencies it requires, or who calls it, and hallucinates the rest.

Neither of these approaches respects the fundamental nature of code: **Code is a graph, not a text document.**

### Enter Open Kioku

We built **Open Kioku** because we were tired of agents being blind to our architecture. Open Kioku is a blazing-fast, language-aware codebase indexing engine written in Rust that exposes a rich Model Context Protocol (MCP) server.

Instead of just chunking text, Open Kioku puts on "developer glasses."

#### The Architecture
When you run `ok init .`, Open Kioku does three things in milliseconds:
1. **Tree-sitter Parsing:** It parses your code into Abstract Syntax Trees (ASTs), identifying exact function boundaries, class definitions, and import statements.
2. **SQLite Relationship Graph:** It maps *who calls who*. If `userService.ts` imports `db.ts`, Open Kioku records that edge. 
3. **Tantivy BM25 Indexing:** It builds a massive, lightning-fast text index of the code so that natural language queries map correctly to symbols.

#### What this unlocks for Agents
Instead of a single `search_code` tool, Open Kioku gives your agent a full arsenal:
- `get_symbol_context(query: "authenticateUser")`: Instantly returns the exact file, exact line range, and full function signature without guessing.
- `dependency_path(from: "AuthRouter", to: "Database")`: Maps the architectural jump between two components.
- `impact_analysis(path: "core/auth.ts")`: Tells the agent exactly what other files will break if it refactors the authentication logic.
- `find_tests_for_change(path: "core/auth.ts")`: Immediately links the agent to `auth.spec.ts`.

#### Built in Rust for Local-First Speed
Agents are local, and your index should be too. By building Open Kioku in Rust, we achieved an indexing speed of over **80 files per second**, and a BM25 median search latency of **under 1 millisecond**. It runs silently, sips memory, and integrates into any Claude Desktop or MCP-compatible editor instantly.

**Try it today:**
```bash
npx open-kioku mcp serve
# or
cargo binstall open-kioku
```

Check out the interactive demo and source code on our [GitHub Repository](https://github.com/shivyadavus/open-kioku).
