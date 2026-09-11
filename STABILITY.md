# Stability Policy

This document describes the stability guarantees for Open Kioku.

---

## Stable CLI Commands

The following `ok` commands have stable interfaces. Their flags, output format,
and exit codes will not change in backward-incompatible ways without a major
version bump.

| Command | Description |
|---|---|
| `ok init` | Initialize an Open Kioku project |
| `ok index` | Index the repository |
| `ok search` | Full-text and symbol search |
| `ok symbol` | Look up symbol definitions and references |
| `ok impact` | Analyze the blast radius of a change |
| `ok tests` | Find tests affected by a change |
| `ok plan` | Generate a multi-step change plan |
| `ok verify` | Verify a plan or change |
| `ok status` | Show project and index status |
| `ok doctor` | Diagnose configuration and environment issues |
| `ok demo` | Run the interactive demo |
| `ok mcp install` | Install MCP server configuration for a client |
| `ok watch` | Start file-watching for incremental re-indexing |
| `ok prove` | Run proof workflows |
| `ok bench` | Run benchmarks |
| `ok eval` | Run evaluation suites |

## Stable MCP Tools

The following MCP tools have stable JSON-RPC interfaces. Their input schemas,
output schemas, and error codes will not change in backward-incompatible ways
without a major version bump.

These are the sixteen tools `tools/list` advertises. `docs/mcp-tools.md` is the reference;
the count is derived from the tool table in `crates/open-kioku-mcp/src/lib.rs` and enforced
against this list by `scripts/validate-docs.sh`.

| Tool | Description |
|---|---|
| `repo_status` | Repository, index, coverage, language and semantic-lifecycle status |
| `list_files` | Indexed file inventory, or per-path detail |
| `search_code` | Ranked search over indexed code (`mode`: `code`, `graph`, `semantic`, `hybrid`) |
| `regex_search` | Exact regular-expression line matching over indexed chunk text |
| `search_symbols` | Symbol inventory, optionally filtered by name |
| `get_definition` | Look up a symbol's definition, optionally with its body |
| `get_references` | References to a symbol (`kind`: `references` (default), `callers`, `callees`, `implementations`, `all`) |
| `dependency_path` | Dependency path between two modules, or one module's dependencies |
| `impact_analysis` | Blast-radius analysis for a change |
| `explain_flow` | Explain a control or data flow through the graph |
| `build_context_pack` | Build a ranked, token-budgeted context pack (`compress` to persist it) |
| `retrieve_context` | Retrieve a previously persisted context pack |
| `plan_change` | Generate a change plan (`detail` for preflight or patch, `persist` for a contract) |
| `verify_change` | Verify changed files against a plan or contract |
| `find_tests_for_change` | Identify tests affected by a change, or the repository's test evidence |
| `query_evidence_graph` | Query the evidence graph; with no `query`, return its schema |

Five further tools are advertised **in addition** to the sixteen, but only when the
corresponding feature is configured; all five stay dispatchable either way. `remember_fact` and
`search_memory` require `[memory] enabled = true`; `map_stacktrace_to_code`,
`find_errors_for_symbol` and `find_recent_failures` require `[runtime]` to name an enabled
provider. They carry no stability guarantee here — the runtime three are `experimental`.

`get_references` changed shape in 4.0.0: it returns an object whose sections each name their own
`evidence_source`, not a bare occurrence array. See the 4.0.0 entry in `CHANGELOG.md`.

Capabilities that reached 3.x through MCP and now ship only on the CLI — `ok architecture …`,
`ok history …`, and `ok contract show` — are covered by the CLI section above only where the
command is named there. A shell-capable agent can still reach them; a pure MCP client cannot.

## Experimental Features

Anything **not listed above** is considered experimental. Experimental commands
and tools:

- May change or be removed in any release
- Are labeled with `[experimental]` in `ok --help` output
- Should not be depended on in automation or scripts

When an experimental feature is promoted to stable, it will be announced in the
[CHANGELOG](CHANGELOG.md) as part of a minor release.

---

## Release Cadence

| Release Type | Frequency | What Changes |
|---|---|---|
| **Patch** (`1.0.x`) | As needed (may be daily) | Bug fixes, performance improvements, documentation. No new stable features. No breaking changes. |
| **Minor** (`1.x.0`) | When new stable features ship | New stable commands or MCP tools, experimental features promoted to stable. No breaking changes to existing stable interfaces. |
| **Major** (`x.0.0`) | Rare | Breaking changes to stable interfaces. Migration guide provided. |

## Semantic Versioning

Open Kioku follows [Semantic Versioning 2.0.0](https://semver.org/):

- **MAJOR**: Incompatible changes to stable CLI commands or MCP tool interfaces.
- **MINOR**: New stable features added in a backward-compatible manner.
- **PATCH**: Backward-compatible bug fixes and improvements.

Experimental features are explicitly excluded from semver guarantees. Changes to
experimental features do not trigger a major version bump.

### What Counts as a Breaking Change

- Removing or renaming a stable CLI command or flag
- Changing the output format of a stable CLI command in a way that breaks parsers
- Changing the input or output schema of a stable MCP tool
- Changing the exit code semantics of a stable CLI command

### What Does NOT Count as a Breaking Change

- Adding a new CLI command or flag
- Adding a new MCP tool
- Adding new fields to MCP tool output (additive changes)
- Changing or removing experimental features
- Performance improvements or internal refactors
