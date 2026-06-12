# Cross-Harness Setup

Open Kioku serves the same local MCP tool surface across supported clients. Generate the client-specific snippet instead of hand-writing config:

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

Run this first:

```sh
ok init /absolute/path/to/repo
ok index /absolute/path/to/repo
ok doctor /absolute/path/to/repo
ok setup audit /absolute/path/to/repo
```

The setup audit separates default quality signals from optional advanced providers.

Default signals are local and free: build systems detected from repository metadata, indexed tests, indexed imports, language-specific static analysis facts, and validation-command readiness. SCIP is the primary precision provider when an `index.scip` is available.

Runtime analysis is optional local evidence. Put JSONL trace/span artifacts under `.ok/runtime/` or `.ok/analysis/runtime/` with source file paths plus fields such as `http.route`, `http.request.method`, or `db.statement`; `ok index` will turn matching entries into graph facts. Open Kioku does not install or run runtime agents by default.

Advanced providers are opportunistic. BSP descriptors, CodeQL database artifacts, matching LSP commands, coverage reports, and JUnit-style reports are shown only when the corresponding local artifact is detected. Missing advanced providers are not readiness failures.

Client matrix:

| Client | Generated shape | Verify |
| --- | --- | --- |
| Claude | `mcpServers` JSON | Restart Claude and inspect MCP logs. |
| Cursor | Cursor MCP JSON entry | Open Cursor MCP settings and confirm `open-kioku`. |
| Codex | TOML `[mcp_servers.open-kioku]` | Run `/mcp` and confirm the server is listed. |
| Gemini CLI | `settings.json` `mcpServers` | Run `/mcp` and confirm connection status. |
| OpenCode | `opencode.json` local MCP entry | Prompt OpenCode to use the `open-kioku` MCP. |
| Zed | `settings.json` `context_servers` | Confirm the server is active in Agent Panel settings. |
| Windsurf | Windsurf MCP JSON entry | Confirm `open-kioku` is enabled in Windsurf MCP settings. |
| Trae | Trae MCP JSON entry | Confirm `open-kioku` is enabled in Trae MCP settings. |

Keep the server read-only for normal use:

```sh
ok mcp serve --repo /absolute/path/to/repo --read-only
```

Only enable write mode for a deliberately bounded workflow with approvals and command allowlists.
