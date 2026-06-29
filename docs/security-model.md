# Security Model

Default posture:

- read-only MCP mode
- no shell execution
- no network access
- no file writes
- no hidden-file scanning
- deny `.env`, `.aws/**`, `.ssh/**`, and `**/secrets/**`
- redact-capable output boundary
- patch application requires approval

Policy is enforced by `open-kioku-actions::PolicyGate`. Commands must exactly match configured allowlist entries. `open-kioku-sandbox` captures output and applies timeouts only after policy allows execution.

Contract verification uses the same exact command allowlist before running validation commands. When attestation writing is requested, each executed or denied validation command records cwd, timestamps, exit code, allowlist status, normalized outcome, and bounded stdout/stderr summaries in a validation ledger under `.ok/contracts/validation/`.

Patch planning is available in read-only mode because it produces a plan, evidence, risks, tests, and a boundary. Applying a patch is denied unless write mode and approval are both present.

For the agent-facing threat model, including prompt injection, memory poisoning, MCP over-permissioning, and context-handle handling, see [`docs/guides/security-threat-model.md`](guides/security-threat-model.md).
