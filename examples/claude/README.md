# Claude Starter

Use this starter to verify that Claude Code can reach Open Kioku before an agent edits.

## One-command setup

```sh
ok setup agent claude --repo /absolute/path/to/repo --apply
```

The default command is a dry run; `--apply` indexes the repo, adds only the
Open Kioku entry to the repository's `.mcp.json`, installs a managed skill, and
validates the local MCP server. Use `--check` later to verify readiness.

## First Invocation

Ask Claude:

```text
Use Open Kioku first. For a token-handling change, build an evidence-backed
plan, inspect impact, find the tests, and verify the changed files against the
saved plan.
```

Expected MCP tool sequence:

1. `plan_change` with `task: "token"`
2. `impact_analysis` with `path: "src/auth.rs"`
3. `find_tests_for_change` with `path: "src/auth.rs"`
4. `verify_change` with the saved plan and changed file list

## Smoke Test

```sh
examples/claude/smoke.sh
```

Set `OK_BIN=/path/to/ok` to test a specific binary.
