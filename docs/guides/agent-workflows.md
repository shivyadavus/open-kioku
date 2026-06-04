# Agent Workflows

Open Kioku is the local evidence layer an agent should use before it edits. The default workflow is:

1. Confirm readiness with `ok status --markdown --write ok-status.md`.
2. Confirm install posture with `ok setup audit`.
3. Ask the MCP client to call `repo_status`, `search_code`, `search_symbols`, `get_definition`, and `get_references`.
4. Use `impact_analysis` and `find_tests_for_change` before changing code.
5. Use `plan_change` as the pre-edit plan and treat low-confidence plans as a stop signal.
6. Use `search_memory` only as supporting context; indexed code and exact references outrank memory.

For large repositories, prefer a short task anchor:

```sh
ok --repo /path/to/repo plan "copy behavior from ExistingType into NewType" --format markdown
ok --repo /path/to/repo context "copy behavior from ExistingType into NewType" --compressed --format toon
```

Before sharing a repo state with another agent or teammate:

```sh
ok status /path/to/repo --markdown --write ok-status.md
ok setup audit /path/to/repo --markdown --write ok-setup.md
ok prove /path/to/repo --task "the workflow being changed"
```

The status and setup files are safe handoff artifacts: they include counts, checks, commands, and redacted guidance, not source snippets.

## Validation Quality

Open Kioku ranks validation candidates from multiple evidence layers:

- indexed tests and path proximity
- exact symbol overlap when SCIP or another occurrence provider is available
- build-aware command derivation for Gradle Java tests
- language-specific static graph facts such as imports, inheritance, routes, config reads, and table mappings
- opt-in runtime facts from local trace/span JSONL artifacts under `.ok/runtime/` or `.ok/analysis/runtime/`
- optional advanced artifacts such as coverage, JUnit history, LSP, BSP, and CodeQL only when they are already present

For Gradle Java repositories, test commands are scoped to the nearest Gradle project and class filter when the test file path is indexed. That keeps plans actionable on large repos where `./gradlew test` is too broad.
