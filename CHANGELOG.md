# Changelog

All notable changes to Open Kioku are documented in this file.
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

---

## [2.0.1] — 2026-06-05

### Added
- Added styled GitHub star call-to-action cards and buttons on the landing page and npm README to bridge package discovery and GitHub conversions.
- Added subtle, action-oriented post-install success prints to `ok init`, `ok demo`, and `ok prove` commands.
- Synced metadata repositories, homepages, and bugs fields for all sub-packages in the workspace.

### Changed
- Bumped workspace crates and manifests to version 2.0.1 to publish patch updates.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [2.0.0] — 2026-06-05

### Added
- Added a README motion demo and copy-paste 60-second quickstart that runs `ok demo`, generates an evidence-backed plan, and verifies a bounded edit.
- Added reproducible demo scripts: `scripts/quickstart-demo.sh` runs the flow and `scripts/render-quickstart-demo.py` regenerates the GIF asset.
- Added local vector index and hybrid semantic search.
- Added visual crate map showing codebase architecture and dependency layers.
- Added Elastic License 2.0 FAQ and STABILITY.md documentation.
- Added workflow benchmark regression suite.
- Added git co-change history signals and runtime evidence integration.
- Added integration test coverage for Java fixtures and CLI smoke tests.

### Changed
- Bumped all crates and workspace packages to version 2.0.0.
- Evolved homepage to highlight plan-before-edit paradigm and show real Elasticsearch proof numbers.
- Upgraded domain routing for openkioku.com.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [1.0.4] — 2026-06-04

### Fixed
- Re-published the 1.0.3 release candidate as 1.0.4 so crates.io can resolve all internal Open Kioku packages against the corrected static/runtime analysis APIs.
- Kept the GitHub release, npm packages, Cursor manifest, Claude manifest, and crates.io package versions aligned.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [1.0.3] — 2026-06-04

### Added
- Added repo-scoped memory facts with local append-only storage and MCP/CLI recall.
- Added reversible compressed context handles with local original retrieval.
- Added optional TOON output for context packs, compressed context packs, and pre-edit plans.
- Added language-specific static analysis facts for imports, inheritance, implementations, routes, config reads, and table mappings.
- Added optional local runtime evidence ingestion from repository-owned JSONL artifacts under `.ok/runtime/` or `.ok/analysis/runtime/`.
- Added release-readiness smoke coverage for status, setup audit, TOON planning, proof reports, and MCP installer output.
- Added large-repo proof documentation for a local Elasticsearch validation run.

### Changed
- Improved task-anchor planning, impact evidence, test selection, and low-confidence risk reporting.
- Updated MCP tool schemas and docs for memory, compressed context, and TOON prompt handoff.
- Strengthened Gradle Java validation command selection and setup/status quality reporting.

---

## [1.0.1] — 2026-06-04

### Changed
- Added crates.io publishing metadata and versioned internal workspace dependencies.
- Reduced README duplication and focused the getting-started path on install, index, verify, and MCP setup.
- Updated npm, Cursor, and demo package metadata for the 1.0.1 release.

## [1.0.0] — 2026-06-04

### Added
- Added phase-level indexing progress for CLI indexing, benchmark, and proof flows.
- Added an index writer lock to prevent concurrent SQLite/Tantivy writers from corrupting or racing index updates.
- Added bounded context and planning paths that reuse persisted Tantivy search results for large repositories.
- Added fast validation-target selection for large repositories.

### Changed
- Replaced heuristic reference expansion with exact definition occurrences plus SCIP-imported occurrences when available.
- Optimized graph construction, Tantivy rebuilds, symbol definition lookup, context building, planning, and test selection for large repositories.
- Expanded default excludes for dependency, build, generated, and internal index paths.

### Fixed
- Fixed indexing blowups caused by highly repeated method and property names in large repositories.
- Fixed JSON and YAML files emitting every key as a symbol.
- Fixed duplicate chunk and symbol records around same-line symbol boundaries.
- Fixed `patch review --json` to return structured JSON.
- Fixed `symbol definition` ranking so exact class/interface definitions beat lower-quality prefix matches.
- Documented the recommended MCP pre-edit routine for Claude Code, Cursor, and other MCP clients.

## [0.1.4] — 2026-05-26

### Fixed
- Added npm package READMEs for the main wrapper package and platform-specific binary packages.

## [0.1.3] — 2026-05-26

### Fixed
- Fixed release packaging for cross-compiled Linux arm64 binaries by skipping host `strip` on incompatible targets.
- Synced Cursor and npm package manifests with the canonical workspace version.
- Extended version validation so CI catches npm wrapper and platform package drift before release.

## [0.1.0] — 2026-05-25

### Added
- **Enhanced health checks** via `ok doctor` with Rust toolchain, Tree-sitter parsers, and MCP initialize checks
- **Signed release binaries** via GitHub Actions with SHA256 checksums and cross-compilation for musl/darwin
- **Fixture repositories** (Rust, TypeScript, Python, Go) and integration tests under `open-kioku-tests`
- **Search evidence wiring** — search results now provide explanatory evidence strings and normalized confidence scores
- **Experimental tool labeling** — `tools/list` differentiates stable vs experimental tools with `--hide-experimental` flag
- **Write safety** — `apply_patch` handler gated behind `OPEN_KIOKU_ALLOW_WRITE=1` environment variable
- **Context export formats** — `build_context_pack` supports JSON, Markdown, and PromptText formats
- **Performance benchmarks** — `ok bench` CLI command and criterion benchmarks under `benches/`
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

[2.0.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.0.1
[2.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.0.0
[1.0.4]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.4
[1.0.3]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.3
[1.0.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.1
[1.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.0
[0.1.4]: https://github.com/shivyadavus/open-kioku/releases/tag/v0.1.4
[0.1.3]: https://github.com/shivyadavus/open-kioku/releases/tag/v0.1.3
[0.1.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v0.1.0
