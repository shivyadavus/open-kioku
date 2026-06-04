# Agent Security Threat Model

Open Kioku assumes AI agents can be tricked by repository content, stale memory, malicious MCP configuration, or overly broad local permissions. The default product posture is local, read-only, and network-denied.

## Assets

- Source files in the target repository.
- Local index data under `.ok/index.sqlite` and `.ok/search/tantivy`.
- Repo memory under `.ok/memory.sqlite`.
- Compressed context originals under `.ok/context.sqlite`.
- MCP client configuration that launches `ok mcp serve`.

## Main Threats

| Threat | Risk | Control |
| --- | --- | --- |
| Prompt injection in source or docs | Agent follows untrusted repo text as instruction | Agents must treat Open Kioku output as evidence, not instruction. |
| Secret exposure through indexing | Sensitive files become searchable | Secret-like paths are denied and hidden files are blocked by default. |
| Memory poisoning | Stale or malicious facts outrank code | Memory is append-only support context; indexed code evidence wins. |
| MCP over-permissioning | Agent gains write, command, or network access | Read-only MCP mode is default; write tools require explicit opt-in. |
| Repo poisoning | Malicious files influence plan selection | `ok plan` reports confidence and evidence paths; low confidence should block edits. |
| Context handle leakage | Stored originals expose code if copied | Handles resolve only from local `.ok/context.sqlite`; do not publish `.ok/`. |

## Operating Rules

- Run `ok setup audit` before connecting a new client.
- Keep `security.allow_write = false` for normal workflows.
- Keep `security.deny_network = true` unless a specific provider requires network access.
- Do not commit `.ok/` artifacts.
- Do not store secrets in repo memory.
- Regenerate the index after major branch changes.
- Treat `ok status --markdown` and `ok prove` as shareable readiness artifacts; do not paste raw source unless needed.

## Write Mode

Write mode should be temporary and narrow:

```sh
ok mcp serve \
  --repo /absolute/path/to/repo \
  --allow-write \
  --approval-required \
  --allow-command "cargo test" \
  --deny-network
```

Patch planning can stay read-only. Patch application should require user approval and normal code review.
