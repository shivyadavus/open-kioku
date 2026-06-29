# MCP Tools

Index the repository before connecting an LLM client:

```sh
ok init /absolute/path/to/repo
ok index /absolute/path/to/repo
ok doctor /absolute/path/to/repo
ok status /absolute/path/to/repo --markdown --write ok-status.md
ok setup audit /absolute/path/to/repo
```

Then print client-specific config and paste it into the client:

```sh
ok mcp install cursor --repo /absolute/path/to/repo
ok mcp install claude --repo /absolute/path/to/repo
ok mcp install codex --repo /absolute/path/to/repo
ok mcp install gemini --repo /absolute/path/to/repo
ok mcp install opencode --repo /absolute/path/to/repo
ok mcp install zed --repo /absolute/path/to/repo
ok mcp install windsurf --repo /absolute/path/to/repo
ok mcp install trae --repo /absolute/path/to/repo
```

Supported install snippets:

| Client | Config shape |
| --- | --- |
| Claude | `mcpServers` JSON |
| Cursor | Cursor MCP JSON |
| Codex | `~/.codex/config.toml` `[mcp_servers.open-kioku]` |
| Gemini CLI | `settings.json` `mcpServers` |
| OpenCode | `opencode.json` `mcp.open-kioku` local server |
| Zed | `settings.json` `context_servers.open-kioku` |
| Windsurf | Windsurf MCP JSON |
| Trae | Trae MCP JSON |

The MCP server runs over stdio:

```sh
ok mcp serve --repo /absolute/path/to/repo --read-only
```

Write mode requires explicit opt-in:

```sh
ok mcp serve --repo /absolute/path/to/repo --allow-write --approval-required --allow-command "cargo test" --deny-network
```

## Recommended Agent Routine

Open Kioku is intended to give Claude Code, Cursor, and other MCP clients a repeatable pre-edit routine. Ask the agent to use Open Kioku before changing files:

```text
Use Open Kioku before editing. Check repo_status, search_code, get_definition,
get_references, impact_analysis, search_memory, and find_tests_for_change.
Build a plan first, then edit only after the indexed evidence is clear.
```

A good default tool sequence is:

1. `repo_status`: confirm the repository is indexed.
2. `search_code` and `search_symbols`: locate candidate files and symbols.
3. `get_definition`, `get_references`, and `get_symbol_context`: resolve the important code facts.
4. `impact_analysis`: identify direct and indirect dependents.
5. `search_memory`: recall prior repo facts, then verify them against indexed code before relying on them.
6. `find_tests_for_change` or `recommend_validation_plan`: select validation targets.
7. `plan_change` or `build_context_pack`: assemble the grounded plan the agent should use before editing. Use `format: "toon"` when the result is going straight into an LLM prompt and `format: "json"` when another tool needs structured data.
8. `build_compressed_context` and `retrieve_context`: use handles when the agent needs compact context with reversible access to originals.

By default these tools are source-tree read-only. Memory and compressed-context tools may write local `.ok/` artifacts so facts and handles can be recalled later. The agent should make source edits with its normal editor tools unless the Open Kioku server was intentionally started with write mode.

## Source-Read Tools

The source-read tools allow language-agnostic code exploration and AI-ready context aggregation. Some highlighted tools:

- `build_context_pack`: Combines primary files, extracted symbols, dependency edges, tests, and patch boundaries for an AI task into a single compressed `ContextPack`.
- `build_compressed_context`: Stores original context snippets locally and returns compact handles that can be expanded with `retrieve_context`. Supports `format: "toon"` for compact prompt handoff.
- `plan_change`: Builds an evidence-backed pre-edit plan with primary context, impact candidates, validation candidates, edit boundaries, and recommended MCP tool calls. Supports `format: "json"`, `format: "markdown"`, and `format: "toon"`.
- `remember_fact` and `search_memory`: Maintain append-only repo memory facts with extracted entity links and provenance.
- `impact_analysis`: Evaluates a file's impact based on lexical references and symbol usage, providing direct and indirect dependent files and an overall risk score.
- `search_code`: Searches exact code text or symbols efficiently using an in-memory or persisted index.
- `architecture_violations`: Detects and reports architecture boundary violations based on package and module heuristics.
- `architecture_policy_check`: Evaluates repository-owned architecture policy dependency rules against indexed imports, references, and calls.
- `architecture_policy_explain`: Explains component matches, public API boundary findings, and exemptions for one indexed file or symbol.

Each tool returned by `tools/list` includes a `maturity` field. Stable tools are intended for default agent use. Experimental tools are exposed for early workflows but may rely on heuristic or fallback behavior.

Stable source-read tools:

- `repo_status`, `list_files`, `list_languages`, `list_symbols`
- `detect_architecture`, `architecture_boundaries`, `architecture_violations`, `architecture_policy_check`, `architecture_policy_explain`, `summarize_architecture`
- `search_code`, `search_files`, `search_symbols`, `regex_search`
- `get_definition`, `get_references`, `get_symbol_context`
- `dependency_path`, `impact_analysis`, `module_dependencies`
- `build_context_pack`, `build_compressed_context`, `retrieve_context`, `plan_change`, `explain_file`, `explain_symbol`
- `remember_fact`, `search_memory`
- `find_tests_for_change`, `recommend_validation_plan`, `explain_test_coverage`
- `propose_patch`, `review_patch`, `validate_patch`, `verify_change`

Experimental tools:

- `history_provenance_lookup`: returns bounded first-seen, last-touched, and recent commit provenance for exactly one `path` or `symbol`, including confidence and uncertainty. `symbol` accepts an exact name, qualified name, or symbol ID.
- `semantic_status`: reports whether `.ok/vectors/current` is disabled, missing, stale, corrupt, or ready.
- `semantic_search`: searches the local semantic vector index and returns explicit semantic status metadata.
- `hybrid_search`: combines lexical and semantic candidates while preserving evidence and ranking signals.
- `explain_search_result`: returns hybrid search details for explaining semantic, lexical, and other score contributions.
- `structural_search`: currently searches indexed symbols and chunks, not a full structural query language.
- `get_implementations`, `get_callers`, `get_callees`: graph-backed heuristics until language-specific call resolution is stronger.
- `explain_flow`: currently returns architecture summary data.
- `map_stacktrace_to_code`, `find_errors_for_symbol`, `find_recent_failures`: return a structured low-confidence disabled response unless a runtime provider such as Sentry is explicitly configured.

## Write Tools

`apply_patch` is experimental and omitted unless write mode is enabled (`--allow-write`). The patches MUST first be generated using `propose_patch` and user approval should be requested before actually executing `apply_patch` for safety.

Every response is JSON and includes evidence where indexed facts are available. Result limits are capped to avoid unbounded responses.
