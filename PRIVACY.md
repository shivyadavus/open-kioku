# Privacy Policy

**Last updated: May 2026**

## Overview

Open Kioku (`ok`) is a local-first code intelligence tool. This privacy policy describes how the plugin handles data when used as an MCP server with Claude or other AI agents.

## Data Collection

**Open Kioku collects no data.**

- No telemetry, analytics, or usage data is collected.
- No information is transmitted to any remote server by the plugin or MCP server.
- No account, registration, or API key is required.

## How Your Code Is Processed

Open Kioku indexes your local codebase and stores the resulting index entirely on your own machine:

- **Index location:** `.ok/` directory inside the repository you index — specifically `.ok/index.sqlite` (metadata and dependency graph) and `.ok/search/tantivy` (BM25 full-text index).
- **What is stored:** file paths, symbol names, BM25 search chunks, dependency graph edges, and import/reference metadata — all derived from your local source files.
- **Where it is stored:** exclusively on your local disk. Nothing is uploaded.

## Network Activity

The `ok mcp serve` process makes **zero outbound network connections**. Network access is denied by default (`deny_network: true` in the default configuration). It communicates only over local stdio with the MCP client (Claude desktop, Cursor, etc.).

The only network activity associated with Open Kioku is:

1. **Installation:** downloading the `ok` binary from GitHub Releases (one-time, user-initiated).
2. **Font/asset loading:** the animated web demo at `shivyadavus.github.io/open-kioku` loads standard web fonts from a CDN. This is a static documentation page only and is not part of the plugin.

## Secret and Sensitive Path Handling

Open Kioku's `PolicyGate` layer explicitly blocks the following path patterns from being read or indexed, by default:

- `.env`
- `.aws/**`
- `.ssh/**`
- `**/secrets/**`

These patterns are enforced via glob matching in code and cannot be bypassed by `.gitignore` configuration. Users may add additional deny patterns in `ok.toml` under `[paths] deny`.

## Write Access

The MCP server is **read-only by default** (`mcp.mode = "read-only"`, `security.allow_write = false`). The `apply_patch` tool requires both:

1. `security.allow_write = true` set in `ok.toml` (or via the `OK_SECURITY_MODE` environment variable), **and**
2. The `OPEN_KIOKU_ALLOW_WRITE=true` environment variable set on the MCP server process.

Both conditions must be true simultaneously. If either is missing, `apply_patch` is denied.

## Third-Party Services

Open Kioku does not integrate with or transmit data to any third-party service. Semantic search is **disabled by default** (`search.semantic = "disabled"`); if enabled by the user, it uses a locally configured provider only.

## Changes to This Policy

If this policy changes materially, the updated version will be committed to this repository with a new "Last updated" date.

## Contact

For questions about this privacy policy, open an issue at:
[https://github.com/shivyadavus/open-kioku/issues](https://github.com/shivyadavus/open-kioku/issues)
