# Changelog

All notable changes to Open Kioku are documented in this file.
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.1.0] — 2026-05-25

### Added
- **MCP server** (`ok mcp serve`) — full Model Context Protocol implementation over stdio with 35+ tools covering search, symbol navigation, impact analysis, architecture detection, and patch planning
- **BM25 / Tantivy search index** — disk-backed full-text search across all indexed code chunks (`search_code`, `regex_search`, `semantic_search`)
- **Tree-sitter parser** — precise symbol extraction for Rust, Java, Python, TypeScript, and Go (`get_definition`, `get_references`, `get_callers`, `get_callees`, `get_implementations`)
- **SQLite metadata graph** — file manifest, symbol table, and dependency graph stored under `.ok/` (`impact_analysis`, `dependency_path`, `module_dependencies`)
- **Architecture detector** — infers high-level component boundaries from file paths (`detect_architecture`, `architecture_violations`)
- **Context pack builder** — assembles AI-ready bundles of primary files, symbols, and tests for a task (`build_context_pack`)
- **Patch planner** — plans code changes without writing files (`propose_patch`, `review_patch`, `validate_patch`)
- **Security posture** — read-only by default; secret paths (`.env`, `.aws`, `.ssh`) blocked from indexing; `apply_patch` gated behind `allow_write: true`
- **Claude Code marketplace manifest** (`.claude-plugin/plugin.json` and `skills/open-kioku/SKILL.md`)
- **Cursor marketplace manifest** (`.cursor-plugin/plugin.json` and `.cursor-plugin/skills/open-kioku.mdc`)
- **CLI** (`ok init`, `ok index`, `ok search`, `ok symbol`, `ok context`, `ok impact`, `ok tests`, `ok status`)

### Fixed
- `serverInfo.name` in MCP `initialize` response corrected to `open-kioku`
- `repository` URL in `Cargo.toml` corrected to `https://github.com/shivyadavus/open-kioku`
- `claude_plugin.json` updated to use `${workspaceFolder}` instead of hardcoded `.`
- LICENSE copyright holder updated to Shiv Yadav
- Added `NOTICE` file as required by Apache License 2.0

[0.1.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v0.1.0
