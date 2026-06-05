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

| Tool | Description |
|---|---|
| `repo_status` | Repository and index status |
| `search_code` | Full-text code search |
| `get_definition` | Look up a symbol's definition |
| `get_references` | Find all references to a symbol |
| `impact_analysis` | Blast-radius analysis for a change |
| `find_tests_for_change` | Identify tests affected by a change |
| `plan_change` | Generate a change plan |
| `build_context_pack` | Build a ranked, token-budgeted context pack |
| `compressed_context` | Compress a context pack for later retrieval |
| `retrieve_compressed_context` | Retrieve a previously compressed context pack |
| `remember_fact` | Store a persistent fact |
| `recall_facts` | Retrieve previously stored facts |

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
