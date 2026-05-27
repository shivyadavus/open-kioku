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
```

Generate MCP client configuration:

```sh
ok mcp install claude --repo /absolute/path/to/repo
ok mcp install cursor --repo /absolute/path/to/repo
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
- Demo: https://shivyadavus.github.io/open-kioku/
- Security: https://github.com/shivyadavus/open-kioku/blob/main/SECURITY.md
