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
- `impact_analysis`: Evaluates a file's impact based on lexical references and symbol usage, providing direct and indirect dependent files and an overall risk score.
- `search_code`: Searches exact code text or symbols efficiently using an in-memory or persisted index.
- `architecture_violations`: Detects and reports architecture boundary violations based on package and module heuristics.

List of all standard read-only tools:
- `repo_status`, `list_files`, `list_languages`, `list_symbols`, `detect_architecture`, `search_code`, `search_files`, `search_symbols`, `regex_search`, `semantic_search`, `structural_search`, `get_definition`, `get_references`, `get_implementations`, `get_callers`, `get_callees`, `get_symbol_context`, `dependency_path`, `impact_analysis`, `module_dependencies`, `architecture_boundaries`, `architecture_violations`, `build_context_pack`, `explain_file`, `explain_symbol`, `explain_flow`, `summarize_architecture`, `find_tests_for_change`, `recommend_validation_plan`, `explain_test_coverage`, `propose_patch`, `review_patch`, `validate_patch`, `map_stacktrace_to_code`, `find_errors_for_symbol`, `find_recent_failures`.

## Write Tools

`apply_patch` is omitted unless write mode is enabled (`--allow-write`). The patches MUST first be generated using `propose_patch` and user approval should be requested before actually executing `apply_patch` for safety.

Every response is JSON and includes evidence where indexed facts are available. Result limits are capped to avoid unbounded responses.

