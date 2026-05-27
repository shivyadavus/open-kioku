# Security

Open Kioku is designed to give coding agents local code intelligence without making write access or cloud upload the default.

## Default Posture

- The MCP server is read-only by default.
- The default workflow does not require embeddings or a hosted index.
- Index data is stored locally under the target repository's `.ok/` directory.
- MCP transport is stdio to the local `ok` process.
- Write tools are not listed unless write mode is explicitly enabled.

## Local Data

Open Kioku writes these local artifacts when a repo is indexed:

- `.ok/index.sqlite`: files, symbols, chunks, imports, occurrences, tests, and graph facts.
- `.ok/search/tantivy`: local BM25 search index.
- `ok.toml`: repo configuration for indexing, MCP mode, security, and command allowlists.

These files are intended to stay on the developer's machine unless the user chooses to commit, copy, or upload them.

## Write Controls

`apply_patch` is gated in multiple places:

- The MCP server must be started with `--allow-write`.
- The server environment must include `OPEN_KIOKU_ALLOW_WRITE=true`.
- Patch application requires an explicit stored approval flow.

Read-only planning tools such as `plan_change`, `build_context_pack`, and `propose_patch` can produce recommendations, but they do not modify source files.

## Path Controls

The policy layer blocks secret-like paths from read access, including:

- `.env`
- `.aws/**`
- `.ssh/**`
- `**/secrets/**`

This filtering is enforced by `open-kioku-actions::PolicyGate`.

## Network Controls

The default security config denies network actions. The local indexing, search, symbol, impact, context, and plan workflows do not require outbound network calls.

## What Open Kioku Does Not Guarantee

- It is not a sandbox for arbitrary commands outside the policy gate.
- It does not prove that a proposed code change is safe.
- It does not replace code review, tests, or dependency security scanning.
- Experimental tools may use heuristic or fallback behavior and should not be treated as authoritative.

## Reporting

Please report vulnerabilities through GitHub Security Advisories when available, or open a minimal public issue that does not include exploit details or private code.
