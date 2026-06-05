# Open Kioku

Local-first code intelligence for AI coding agents.

Open Kioku indexes a repository on your machine and exposes fast code search, symbol navigation, impact analysis, context packs, and MCP tools through the `ok` CLI.

## Install

```sh
npm install -g open-kioku
```

Then verify the installed binary:

```sh
ok --version
```

## Quick Start

### Index Your Repository

```sh
npm install -g open-kioku
ok init /absolute/path/to/repo
ok index /absolute/path/to/repo
ok doctor /absolute/path/to/repo
```

Verify that the local index has useful evidence:

```sh
ok --repo /absolute/path/to/repo search "auth flow" --limit 5
ok --repo /absolute/path/to/repo plan "change auth flow" --format markdown
```

Connect the same indexed repo to your LLM client:

```sh
ok mcp install claude --repo /absolute/path/to/repo
ok mcp install cursor --repo /absolute/path/to/repo
```

Paste the printed MCP config snippet into Claude Code, Cursor, or another MCP-compatible client. Open Kioku runs locally over stdio and is read-only by default.

Ask the agent to use the index before editing:

```text
Use Open Kioku before editing. Check repo_status, search_code, get_definition,
get_references, impact_analysis, and find_tests_for_change. Build a plan first.
```

Keep the index fresh while you work:

```sh
ok watch /absolute/path/to/repo
```

### Try The Demo

Create and index a sample repository:

```sh
ok demo --force
```

Search and inspect the demo:

```sh
ok --repo ./open-kioku-demo search token
ok --repo ./open-kioku-demo symbol find issue_token
ok --repo ./open-kioku-demo impact --file src/auth.rs
ok --repo ./open-kioku-demo plan token --format markdown
ok prove ./open-kioku-demo --task token
```

## Package Layout

The `open-kioku` npm package is a small JavaScript wrapper. It installs one platform-specific optional dependency containing the native `ok` binary for your operating system and CPU architecture.

Supported packages:

- `@open-kioku/darwin-x64`
- `@open-kioku/darwin-arm64`
- `@open-kioku/linux-x64`
- `@open-kioku/linux-arm64`
- `@open-kioku/win32-x64`

## Links

- Repository: https://github.com/shivyadavus/open-kioku
- Releases: https://github.com/shivyadavus/open-kioku/releases
- Demo: https://openkioku.com/
- Security: https://github.com/shivyadavus/open-kioku/blob/main/SECURITY.md
