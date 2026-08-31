# Changelog

All notable changes to Open Kioku are documented in this file.
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Fixed
- Stopped the graph query-column migration backfill from full-scanning (and partially rewriting) the graph tables on every store open. On a 16.5k-file Java corpus this removed ~14 seconds of fixed latency from every CLI command; existing stores migrate once and record completion.
- Fixed exact symbol lookups returning `symbol not found` for symbols that exist: the substring scan ordered by qualified name could truncate the true exact match out of its candidate window. `ok symbol definition` now consults an indexed exact-name path first (13.9s → 0.02s on a 247k-symbol corpus, with the correct result).

### Performance
- Bulk graph replacement drops and rebuilds secondary indexes around a prepared-statement, primary-key-ordered insert under a scoped page cache. Cold structural indexing of a 16.5k-file Java repository improved from 40m40s to 19m28s (graph write 28m37s → 8m17s).
- Ingest releases the parsed corpus in one consuming pass instead of cloning every extracted field, graph nodes are moved rather than cloned twice, and search git-history annotation groups facts by file instead of rescanning the full fact list per result.

### Added
- CC5.3: semantic lifecycle health is now explained by `ok status`, `ok doctor` (semantic-lifecycle check with concrete rebuild reasons), structured `rebuild_required`/`rebuild_reasons`/`last_rebuilt_at`/`stale_ratio` fields on semantic status, and a `semantic_lifecycle` block in MCP `repo_status`.
- RI3.7: impact analysis classifies relationship-edge dependents into `proven_impact` and `possible_impact` through the shared fail-closed `RelationshipUsePolicy` — a heuristic same-name edge can never be presented as structural truth. Wired through the CLI, MCP `impact_analysis`, context compilation, planning, and patch verification.
- `ok --version --json` machine-readable version output and copy-paste examples in `ok status/search/impact --help`.
- CC5.2: measured 50K→1M ANN scale evidence recorded under `benchmarks/cc5-ann-scale-evidence` (recall collapses beyond ~300K vectors on the current HNSW profile; profile decision tracked in #328).

---

## [3.0.4] — 2026-08-23

### Fixed
- Made concurrent readers wait for short-lived SQLite schema writers instead of failing with `database is locked` on large indexes.
- Anchored one-hop graph equality queries through indexed node labels, symbol names, and directional edge endpoints, eliminating full edge scans for exact Java definition, call, and implementation lookups.
- Ranked exact symbol and file-stem identities ahead of broader camel-case prefix matches while preserving natural-language workflow relevance and explicit score provenance.

### Validated
- Indexed a large Java repository: 9,312 indexed files, 136,212 symbols, 136,646 chunks, 211,057 graph nodes, and 707,271 graph edges.
- Reproduced the full structural index deterministically, validated concurrent lexical and graph reads, and built 272,858 semantic vectors with both exact-flat and persistent HNSW backends.

### Changed
- Added machine-readable MCP routing categories alongside titles, detailed usage guidance, schemas, maturity, and safety annotations for all 58 tools, with a regression test that rejects incomplete tool metadata.

### Compatibility
- Existing indexes are upgraded in place with the new graph-label index; no functionality or semantic backend is removed.
- Compiler-grade Java SCIP remains optional and externally provided. Structural Java indexing remains fully operational when an upstream `scip-java` build integration is unavailable.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [3.0.3] — 2026-08-22

### Changed
- Upgraded the persistent lexical search backend from Tantivy 0.22 to Tantivy 0.26.1 while preserving score-ordered BM25 results and compatibility with existing indexes.
- Raised the Rust MSRV to 1.86, the minimum supported by Tantivy 0.26.1.

### Fixed
- Removed large-repository indexing hot spots by pre-indexing legacy call facts and deterministic same-file and lexical-scope symbol candidates instead of repeatedly scanning global fact and symbol collections.
- Bounded human-readable status quality diagnostics to 100 deterministic notes with an explicit omitted count and a pointer to the complete JSON report, preventing multi-megabyte terminal output without hiding totals.
- Preserved complete semantic functionality across deterministic local embeddings, persistent HNSW ANN, and opt-in Jina neural embeddings.

### Compatibility
- Existing Tantivy 0.22 indexes remain searchable, and a normal full index rebuild recreates the lexical index using Tantivy 0.26.1.
- The supported release artifacts remain Linux x86_64/ARM64 GNU, macOS ARM64, and Windows x86_64.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [3.0.2] — 2026-08-20

### Fixed
- Fixed repository discovery so nested `.gitignore` and `.okignore` rules remain correctly scoped, tracked Git files remain indexable, large ignore batches cannot deadlock, and zero-result filtering is reported instead of silently succeeding.
- Kept Context Compiler ambiguity telemetry consistent across top-level and selection-scoped diagnostics without borrowing authority from ambiguous legacy traces.
- Reported exact, graph, and validation candidates as high-value omissions when the context budget has zero remaining capacity.
- Bound downstream Context Compiler authority to the caller-visible primary selection so hidden retrieval candidates cannot widen symbols, dependency seeds, impact anchoring, or allowed edit boundaries.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [3.0.0] — 2026-08-17

### Added
- Added evidence-first routed context retrieval with provenance-aware bounded context, explicit blockers, and measured retrieval quality gates.
- Added task/query-shape routing measurement with frozen labels, adversarial probes, classifier accuracy, task-family × query-shape quality, and latency reporting while preserving the generic blocking retrieval baseline.
- Added quality-tiered local neural embedding profiles alongside the deterministic local-hash baseline, with opt-in local model acquisition and provenance.
- Added persistent local HNSW semantic indexing with exact-flat correctness fallback, persistence, filter parity, and scale calibration.
- Added proof-carrying relationship authority and deterministic proof-gated structural resolution for calls, references, type use, inheritance, and imports.
- Added the `ok relationship-bench` conformance scoring foundation with strict proof/range/outcome/metamorphic threshold policy and reproducibility metadata.

### Changed
- Made `ResolutionMode::Shadow` the default so proof-gated structural relationships are operational while legacy evidence remains available for compatibility; explicit `Legacy` and `V2` modes remain available.
- Made authoritative architecture/context consumers fail closed on unproven structural relationships instead of promoting heuristic confidence into graph truth.
- Refreshed the homepage and README around the evidence-first workflow and real pinned-main dogfood proof.
- Bumped the 43-crate workspace and all release/install channels to 3.0.0, including explicit 3.0.0 requirements for publishable internal Cargo path dependencies.
- Hardened release publishing so built binary SHA-256 values must match checked-in release metadata before GitHub/npm publication.

### Fixed
- Preserved typed authority for uniquely resolved import targets and exact Rust `crate::module::member()` calls without enabling fuzzy structural fallbacks.
- Updated public quickstart validation to treat `ok setup agent ...` as the primary onboarding flow while retaining lower-level `init`, `index`, and manual MCP commands as supported primitives.

### Compatibility
- Reindexing is recommended for 3.0. Existing heuristic structural edges from older indexes are not trusted as authoritative unless reconstructed with proof; relationship counts may decrease when ambiguous evidence correctly fails closed.
- V3 Linux release binaries target GNU/glibc on x86_64 and ARM64 because the local neural runtime does not provide supported MUSL prebuilts; npm Linux platform packages declare `libc: glibc` accordingly.
- The checked-in relationship scorer is a conformance-scoring foundation; the full frozen >=300-case #240 corpus remains follow-up work and is not claimed complete by this release.

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

## [2.4.0] — 2026-08-14

### Added
- Implemented 1-command agent onboarding command (`ok setup agent <agent>`) for Cursor, Claude Code, and Codex.

### Changed
- Scaled symbol-edge resolution for large codebases by pre-indexing file imports, leveraging Rayon chunk parallelization, zero-allocation matching keys, pre-indexing symbol suffixes, and capping fuzzy name scans.
- Improved `publish-crates.sh` with a 15-second crates.io index propagation pause between workspace crate publications.

### Fixed
- Fixed non-UTF8 binary patch parsing in `open-kioku-git`.
- Removed experimental nonfunctional `apply_patch` MCP tool.

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

## [2.3.0] — 2026-07-04

### Added
- Added dedicated setup guides for Claude Code, Cursor, Codex, and Gemini CLI in the `demo/` directory.

### Changed
- Optimized the documentation layout and repository quickstart to focus on local-first onboarding diagnostics.
- Updated the release manifest synchronization script to automate version propagation to demo site resources.

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

## [2.2.3] — 2026-07-04

### Fixed
- Fixed JSON-RPC notification handling in the MCP stdio server so `notifications/initialized` produces no response, matching MCP client expectations and unblocking Glama/mcp-proxy container inspection.
- Hardened release publishing so reruns skip npm package versions that are already published instead of failing with immutable registry conflicts.
- Hardened GitHub Pages demo deployment by canceling stale queued deployments and extending the deployment timeout.

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

## [2.2.2] — 2026-07-02

### Changed
- Comprehensively refactored and enriched MCP tool descriptions and guidance text for all 27 low-scoring tools to achieve A-level ratings on the Glama TDQS rubric.
- Added explicit "Do NOT use when..." instructions, detailed sibling tool alternatives, and clarified data source and side-effect transparency.
- Enriched all tool parameter schemas with default values, value constraints, and explicit semantic descriptions.
- Updated integration test tools list snapshot to reflect the updated tool specifications.

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

## [2.2.1] — 2026-07-02

### Added
- Added root `glama.json` metadata so Glama can associate the MCP listing with the repository maintainer.
- Added MCP `title`, `annotations`, and `outputSchema` metadata to every tool definition.

### Changed
- Expanded MCP tool descriptions with explicit when-to-use guidance, sibling alternatives, and side-effect transparency for better Glama TDQS scoring.
- Marked write-like MCP tools with accurate read/write/destructive/open-world annotations.
- Decomposed the CLI crate into command, benchmark, report, and shared type modules while keeping the binary behavior intact.
- Added GitHub star and npm download badges to the README.
- Reconciled the release line after `v2.2.0` so package registries receive the current `main` MCP tool surface.

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

## [2.1.1] — 2026-06-21

### Fixed
- **SQLite Ingestion & Backfill Performance**:
  - Wrapped SQLite node and edge backfill updates (`backfill_graph_query_columns`) inside transactions. This critical performance fix reduces backfill time on large codebases (such as Elasticsearch with 269k stale edges) from several hours to under 25 seconds.

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

## [2.1.0] — 2026-06-21

### Added
- **Architecture Boundaries & Enforcement**:
  - Implemented architecture policy validation with the new `ok architecture policy check` CLI command and `architecture_policy_check` MCP tool, evaluating dependency rules against graph imports, calls, and references.
  - Implemented configuration-based policy component resolution (`ok.toml` components).
- **Evidence Graph v2 (E1-E19 Series)**:
  - Added complexity analysis and relationship evidence passes (E19).
  - Strengthened validation evidence selection and test-to-code target selection (E18).
  - Strengthened runtime evidence aggregation and ingestion (E17).
  - Promoted service boundary graph facts (E15).
  - Introduced versioned evidence graph schema manifest and mapped SQLite metadata directly to it.
- **Cross-Project Workspace Linking**:
  - Added cross-project workspace linking (E16) to allow multi-repository planning and context packs.
- **Git History & Provenance Tracking**:
  - Implemented incremental git commit history parsing and ingestion to extract co-change metrics.
  - Added file and symbol level historical provenance lookup CLI command and MCP tool.
- **Change Contracts**:
  - Introduced versioned change contracts (`ContractBuilder` and schemas) and contract store persistence to ensure pre-edit plans are verified post-edit.
- **High-Performance Ingestion & Graph Buffer**:
  - Implemented incremental index updates, parsing only modified files for rapid re-indexing.
  - Implemented a high-throughput deduplicating `GraphBuffer` for buffered database writes.
  - Added symbol registry resolution, discovery skip reporting, and import manifest resolution.
  - Supported index snapshot export/import for transferability.
- **Client & Integration Ecosystem**:
  - Added auto-installation and support for **Windsurf** and **Trae** MCP configurations.
  - Scaffolded the repository-scoped **Codex** marketplace and browser plugin.
  - Added Glama verification metadata.

### Changed
- Bumped workspace packages, plugins, manifests, and homebrew formulas to version 2.1.0.

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

[2.4.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.4.0
[2.3.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.3.0
[2.2.3]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.2.3
[2.2.2]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.2.2
[2.2.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.2.1
[2.1.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.1.1
[2.1.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.1.0
[2.0.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.0.1
[2.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.0.0
[1.0.4]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.4
[1.0.3]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.3
[1.0.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.1
[1.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.0
[0.1.4]: https://github.com/shivyadavus/open-kioku/releases/tag/v0.1.4
[0.1.3]: https://github.com/shivyadavus/open-kioku/releases/tag/v0.1.3
[0.1.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v0.1.0

[3.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.0
[3.0.2]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.2
[3.0.3]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.3
[3.0.4]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.4
