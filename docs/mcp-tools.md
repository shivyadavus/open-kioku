# MCP Tools

The MCP server runs over stdio:

```sh
ok mcp serve --repo . --read-only
```

Write mode requires explicit opt-in:

```sh
ok mcp serve --repo . --allow-write --approval-required --allow-command "cargo test" --deny-network
```

## Read Tools

The read-only tools allow language-agnostic code exploration and AI-ready context aggregation. Some highlighted tools:

- `build_context_pack`: Combines primary files, extracted symbols, dependency edges, tests, and patch boundaries for an AI task into a single compressed `ContextPack`.
- `plan_change`: Builds an evidence-backed pre-edit plan with primary context, impact candidates, validation candidates, edit boundaries, and recommended MCP tool calls.
- `impact_analysis`: Evaluates a file's impact based on lexical references and symbol usage, providing direct and indirect dependent files and an overall risk score.
- `search_code`: Searches exact code text or symbols efficiently using an in-memory or persisted index.
- `architecture_violations`: Detects and reports architecture boundary violations based on package and module heuristics.

Each tool returned by `tools/list` includes a `maturity` field. Stable tools are intended for default agent use. Experimental tools are exposed for early workflows but may rely on heuristic or fallback behavior.

Stable read-only tools:

- `repo_status`, `list_files`, `list_languages`, `list_symbols`
- `detect_architecture`, `architecture_boundaries`, `architecture_violations`, `summarize_architecture`
- `search_code`, `search_files`, `search_symbols`, `regex_search`
- `get_definition`, `get_references`, `get_symbol_context`
- `dependency_path`, `impact_analysis`, `module_dependencies`
- `build_context_pack`, `plan_change`, `explain_file`, `explain_symbol`
- `find_tests_for_change`, `recommend_validation_plan`, `explain_test_coverage`
- `propose_patch`, `review_patch`, `validate_patch`

Experimental tools:

- `semantic_search`: falls back to lexical search while semantic search is disabled.
- `structural_search`: currently searches indexed symbols and chunks, not a full structural query language.
- `get_implementations`, `get_callers`, `get_callees`: graph-backed heuristics until language-specific call resolution is stronger.
- `explain_flow`: currently returns architecture summary data.
- `map_stacktrace_to_code`, `find_errors_for_symbol`, `find_recent_failures`: return low-confidence empty results unless runtime integrations are configured.

## Write Tools

`apply_patch` is experimental and omitted unless write mode is enabled (`--allow-write`). The patches MUST first be generated using `propose_patch` and user approval should be requested before actually executing `apply_patch` for safety.

Every response is JSON and includes evidence where indexed facts are available. Result limits are capped to avoid unbounded responses.
