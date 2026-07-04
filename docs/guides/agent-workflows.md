# Agent Workflows

Open Kioku is the local evidence layer an agent should use before it edits. The default workflow is:

1. Confirm readiness with `ok status --markdown --write ok-status.md`.
2. Confirm install posture with `ok setup audit`.
3. Ask the MCP client to call `repo_status`, `search_code`, `search_symbols`, `get_definition`, and `get_references`.
4. Use `impact_analysis` and `find_tests_for_change` before changing code.
5. Use `plan_change` as the pre-edit plan and treat low-confidence plans as a stop signal.
6. Use `search_memory` only as supporting context; indexed code and exact references outrank memory.
7. For governed areas, check local ADRs with `ok adr explain --task "<task>"` and keep edits aligned with linked components, boundaries, files, routes, contracts, and validation rules.

For large repositories, prefer a short task anchor:

```sh
ok --repo /path/to/repo plan "copy behavior from ExistingType into NewType" --format markdown
ok --repo /path/to/repo plan "copy behavior from ExistingType into NewType" --format html
ok --repo /path/to/repo context "copy behavior from ExistingType into NewType" --compressed --format toon
```

Before sharing a repo state with another agent or teammate:

```sh
ok status /path/to/repo --markdown --write ok-status.md
ok setup audit /path/to/repo --markdown --write ok-setup.md
ok prove /path/to/repo --task "the workflow being changed"
ok prove /path/to/repo --task "the workflow being changed" --html
```

The status and setup files are safe handoff artifacts: they include counts, checks, commands, and redacted guidance, not source snippets.

## Trust Workflow Reports

Use the trust workflow commands when the handoff needs architecture, ADR, plan, contract, and verification evidence in one source-safe path:

```sh
ok --repo /path/to/repo architecture overview
ok --repo /path/to/repo architecture clusters
ok --repo /path/to/repo architecture hotspots
ok --repo /path/to/repo architecture boundaries
ok --repo /path/to/repo architecture drift
ok --repo /path/to/repo adr add "API boundary" --component api --file src/api/mod.rs
ok --repo /path/to/repo adr explain --task "change API boundary"
ok --repo /path/to/repo ui --task "change API boundary"
ok --repo /path/to/repo verify --plan plan.json --changed src/api/mod.rs --format html
```

HTML reports include evidence handles, caveats, validation status, and reproduction commands. They omit source snippets by default.

## Validation Quality

Open Kioku ranks validation candidates from multiple evidence layers:

- indexed tests and path proximity
- exact symbol overlap when SCIP or another occurrence provider is available
- build-aware command derivation for Gradle Java tests
- language-specific static graph facts such as imports, inheritance, routes, config reads, and table mappings
- opt-in runtime facts from local trace/span JSONL artifacts under `.ok/runtime/` or `.ok/analysis/runtime/`
- optional advanced artifacts such as coverage, JUnit history, LSP, BSP, and CodeQL only when they are already present

For Gradle Java repositories, test commands are scoped to the nearest Gradle project and class filter when the test file path is indexed. That keeps plans actionable on large repos where `./gradlew test` is too broad.
