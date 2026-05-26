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

- **Index location:** `.ok/` directory inside the repository you index.
- **What is stored:** file paths, symbol names, BM25 search chunks, dependency graph edges, and import/reference metadata — all derived from your local source files.
- **Where it is stored:** exclusively on your local disk. Nothing is uploaded.

## Network Activity

The `ok mcp serve` process makes **zero outbound network connections**. It communicates only over local stdio with the MCP client (Claude desktop, Cursor, etc.).

The only network activity associated with Open Kioku is:

1. **Installation:** downloading the `ok` binary from GitHub Releases (one-time, user-initiated).
2. **Font/asset loading:** the animated web demo at `shivyadavus.github.io/open-kioku` loads standard web fonts from a CDN. This is a static documentation page only and is not part of the plugin.

## Secret and Sensitive Path Handling

Open Kioku's `PolicyGate` layer explicitly excludes the following paths from indexing regardless of `.gitignore` settings:

- `.env`, `.env.*`
- `.aws/`, `.ssh/`, `.gnupg/`
- Files matching common secret patterns (private keys, credential files)

These paths are never read into the index.

## Write Access

The MCP server is **read-only by default**. Write tools (`apply_patch`) require both the `--allow-write` CLI flag and the `OPEN_KIOKU_ALLOW_WRITE=true` environment variable to be explicitly set by the user.

## Third-Party Services

Open Kioku does not integrate with or transmit data to any third-party service.

## Changes to This Policy

If this policy changes materially, the updated version will be committed to this repository with a new "Last updated" date.

## Contact

For questions about this privacy policy, open an issue at:
[https://github.com/shivyadavus/open-kioku/issues](https://github.com/shivyadavus/open-kioku/issues)
