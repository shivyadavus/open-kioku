# Agent Workflows

Open Kioku is the local evidence layer an agent should use before it edits. The default workflow is:

1. Confirm readiness with `ok status --markdown --write ok-status.md`.
2. Confirm install posture with `ok setup audit`.
3. Optionally install local agent hook guidance with `ok hooks install --mode advisory`.
4. Ask the MCP client to call `repo_status`, `search_code`, `search_symbols`, `get_definition`, and `get_references`.
5. Use `impact_analysis` and `find_tests_for_change` before changing code.
6. Use `plan_change` as the pre-edit plan and treat low-confidence plans as a stop signal.
7. Use `search_memory` only as supporting context; indexed code and exact references outrank memory.

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

## Agent Hooks

Open Kioku can install reversible local guidance files that nudge agents toward the evidence workflow before editing:

```sh
ok hooks install --mode advisory /path/to/repo
ok hooks install --mode warn /path/to/repo
ok hooks install --mode enforce /path/to/repo
ok doctor hooks --repo /path/to/repo
ok doctor agents --repo /path/to/repo
ok hooks uninstall /path/to/repo
```

`advisory` is the default. `warn` tells the agent to warn before editing when no fresh plan or contract exists. `enforce` is explicit and requires an enforce-ready `ok.toml` policy gate; read/search/status operations must still fail open. Install and uninstall report exact files changed, support `--dry-run`, and only mutate generated marker blocks.

## Validation Quality

Open Kioku ranks validation candidates from multiple evidence layers:

- indexed tests and path proximity
- exact symbol overlap when SCIP or another occurrence provider is available
- build-aware command derivation for Gradle Java tests
- language-specific static graph facts such as imports, inheritance, routes, config reads, and table mappings
- opt-in runtime facts from local trace/span JSONL artifacts under `.ok/runtime/` or `.ok/analysis/runtime/`
- optional advanced artifacts such as coverage, JUnit history, LSP, BSP, and CodeQL only when they are already present

For Gradle Java repositories, test commands are scoped to the nearest Gradle project and class filter when the test file path is indexed. That keeps plans actionable on large repos where `./gradlew test` is too broad.
