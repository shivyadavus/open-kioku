# Cursor Starter

Use this starter to verify that Cursor can reach Open Kioku before an agent edits.

## One-command setup

```sh
ok setup agent cursor --repo /absolute/path/to/repo --apply
```

The default command is a dry run; `--apply` indexes the repo, adds only the
Open Kioku entry to `.cursor/mcp.json`, installs a managed rule, and validates
the local MCP server. Use `--check` later to verify readiness.

## First Invocation

Ask Cursor:

```text
Use Open Kioku before editing. Build a plan for changing token handling, inspect
impact, find tests, and verify the intended edit boundary.
```

Expected MCP tool sequence:

1. `plan_change` with `task: "token"`
2. `impact_analysis` with `path: "src/auth.rs"`
3. `find_tests_for_change` with `path: "src/auth.rs"`
4. `verify_change` with the saved plan and changed file list

## Smoke Test

```sh
examples/cursor/smoke.sh
```

Set `OK_BIN=/path/to/ok` to test a specific binary.
