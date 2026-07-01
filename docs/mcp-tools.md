# MCP Tools

Index the repository before connecting an LLM client:

```sh
ok init /absolute/path/to/repo
ok index /absolute/path/to/repo
ok doctor /absolute/path/to/repo
ok status /absolute/path/to/repo --markdown --write ok-status.md
ok setup audit /absolute/path/to/repo
```

Then print client-specific config and paste it into the client:

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

Supported install snippets:

| Client | Config shape |
| --- | --- |
| Claude | `mcpServers` JSON |
| Cursor | Cursor MCP JSON |
| Codex | `~/.codex/config.toml` `[mcp_servers.open-kioku]` |
| Gemini CLI | `settings.json` `mcpServers` |
| OpenCode | `opencode.json` `mcp.open-kioku` local server |
| Zed | `settings.json` `context_servers.open-kioku` |
| Windsurf | Windsurf MCP JSON |
| Trae | Trae MCP JSON |

The MCP server runs over stdio:

```sh
ok mcp serve --repo /absolute/path/to/repo --read-only
```

Write mode requires explicit opt-in:

```sh
ok mcp serve --repo /absolute/path/to/repo --allow-write --approval-required --allow-command "cargo test" --deny-network
```

## Recommended Agent Routine

Open Kioku is intended to give Claude Code, Cursor, and other MCP clients a repeatable pre-edit routine. Ask the agent to use Open Kioku before changing files:

```text
Use Open Kioku before editing. Check repo_status, search_code, get_definition,
get_references, impact_analysis, ownership_lookup, reviewer_suggestions,
search_memory, and find_tests_for_change.
Build a plan first, then edit only after the indexed evidence is clear.
```

A good default tool sequence is:

1. `repo_status`: confirm the repository is indexed.
2. `search_code` and `search_symbols`: locate candidate files and symbols.
3. `get_definition`, `get_references`, and `get_symbol_context`: resolve the important code facts.
4. `impact_analysis`: identify direct and indirect dependents.
5. `ownership_lookup`: resolve CODEOWNERS, local git-history, and secondary repo-memory ownership signals for files where edit responsibility matters.
6. `reviewer_suggestions`: rank reviewer candidates from stored review evidence when present, otherwise explicit ownership and author-history inference.
7. `search_memory`: recall prior repo facts, then verify them against indexed code before relying on them.
8. `find_tests_for_change` or `recommend_validation_plan`: select validation targets.
9. `plan_change` or `build_context_pack`: assemble the grounded plan the agent should use before editing. Use `format: "toon"` when the result is going straight into an LLM prompt and `format: "json"` when another tool needs structured data.
10. `create_change_contract`: turn the plan into a stored, versioned contract when the workflow needs a durable pre-edit artifact.
11. `build_compressed_context` and `retrieve_context`: use handles when the agent needs compact context with reversible access to originals.
12. `verify_change_contract` or `verify_change`: verify the final changed files or diff against the stored contract or legacy saved plan.

By default these tools are source-tree read-only. Memory and compressed-context tools may write local `.ok/` artifacts so facts and handles can be recalled later. The agent should make source edits with its normal editor tools unless the Open Kioku server was intentionally started with write mode.

## Protocol Hardening

Open Kioku preserves string and numeric JSON-RPC request IDs in responses,
returns parse errors for malformed JSON, and returns an invalid-request error
when `method` is missing. Tool execution failures return structured JSON-RPC
errors instead of crashing the stdio server. Each tool dispatch is bounded by a
server-side timeout, and idle stdio sessions reopen the local SQLite store after
an inactivity window.

List/search/query responses include standard pagination metadata:

```json
{
  "returned": 20,
  "limit": 20,
  "offset": 0,
  "has_more": true,
  "truncated": false,
  "warnings": [],
  "caveats": []
}
```

For `list_files`, `list_symbols`, `search_symbols`, `search_code`,
`search_files`, `regex_search`, `semantic_search`, and `hybrid_search`, the
items are returned under `files`, `symbols`, or `results` alongside that
metadata. `query_evidence_graph` returns `columns` and `rows` with the same
metadata. When graph query results have more rows, the response also includes
an opaque local `continuation`, an `expires_at` Unix timestamp, and a `next`
object with the safe follow-up `offset`.
Search tools use a bounded candidate scan for high offsets and mark the response
with `truncated` plus a warning when callers should narrow the query.

Large `tools/call` text content is truncated before it is placed into the
human-readable `content` field. The full structured result remains available in
`structuredContent`; the response includes a warning when text truncation
occurs.

## Source-Read Tools

The source-read tools allow language-agnostic code exploration and AI-ready context aggregation. Some highlighted tools:

- `build_context_pack`: Combines primary files, extracted symbols, dependency edges, tests, architecture policy when configured, and patch boundaries for an AI task into a single compressed `ContextPack`.
- `build_compressed_context`: Stores original context snippets locally and returns compact handles that can be expanded with `retrieve_context`. Supports `format: "toon"` for compact prompt handoff.
- `plan_change`: Builds an evidence-backed pre-edit plan with primary context, architecture policy when configured, evidence quality, impact candidates, validation candidates, edit boundaries, and recommended MCP tool calls. Supports `format: "json"`, `format: "markdown"`, and `format: "toon"`.
- `create_change_contract`: Builds a `ChangeContractV1` from a task, inline `PlanReport`, or `plan_json`. It stores the contract in `.ok/contracts` by default, preserves source-plan `evidence_quality`, and supports `format: "json"`, `format: "markdown"`, and `format: "toon"`.
- `get_change_contract`: Retrieves a stored contract by `contract_id` and can export JSON, Markdown, or TOON.
- `verify_change_contract`: Verifies changed files, a diff, or a git range against a stored contract id, inline contract object, or `contract_json`. Stored contract ids append verification records under `.ok/contracts`; stale evidence quality warns by default and fails under strict traceability, while missing validation attestations warn as pending validation.
- `explain_verification`: Summarizes a `ContractVerificationReport` decision, boundary failures, warnings, dependency deltas, validation attestations, and recommended tests.
- `remember_fact` and `search_memory`: Maintain append-only repo memory facts with extracted entity links and provenance.
- `impact_analysis`: Evaluates a file's impact based on lexical references and symbol usage, providing direct and indirect dependent files, active architecture policy when configured, and an overall risk score.
- `ownership_lookup`: Resolves ranked owner suggestions for a path from CODEOWNERS, persisted local git author/touch history, and secondary repo memory facts. The result includes source breakdown, confidence, staleness, component matches, and uncertainty.
- `reviewer_suggestions`: Suggests ranked reviewer candidates for a path. Actual PR-review certainty is used only when stored review/approval evidence exists; normal local clones return explicit inferred or unavailable availability from ownership and git-author signals.
- `search_code`: Searches exact code text or symbols efficiently using an in-memory or persisted index.
- `architecture_violations`: Detects and reports architecture boundary violations based on package and module heuristics.
- `architecture_policy_validate`: Validates the resolved repository architecture policy or an explicit policy TOML path.
- `architecture_policy_check`: Evaluates repository-owned architecture policy dependency rules against indexed imports, references, and calls.
- `architecture_policy_explain`: Explains component matches, public API boundary findings, and exemptions for one indexed file, symbol, or repository scope.

Architecture policy tool schemas:

- `architecture_policy_validate`
  - Request: `{}` or `{"path": ".open-kioku/architecture.toml"}`.
  - Response: `{valid, configured, source, paths, policy, message}`. `source` is `canonical`, `compatibility`, `explicit`, or `null`.
- `architecture_policy_check`
  - Request: `{}`.
  - Response: the stable `PolicyCheckReport` shape with `configured`, edge counts, `violations`, `exemptions`, bounded `unknown_edges`, and `uncertainty`.
- `architecture_policy_explain`
  - Request: exactly one of `{"file": "src/api/internal/session.rs"}`, `{"symbol": "crate::api::handler"}`, or `{"scope": "repo"}`.
  - Response: `{configured, query_kind, query, file_path, symbol, components, violations, exemptions, uncertainty, message}`.

Each tool returned by `tools/list` includes a `maturity` field. Stable tools are intended for default agent use. Experimental tools are exposed for early workflows but may rely on heuristic or fallback behavior.

`build_context_pack`, `build_compressed_context`, `plan_change`, and
`impact_analysis` include an `architecture_policy` report when a repository
policy is configured. `verify_change` loads configured policy automatically and
checks dependency deltas against it by default; `check_dependency_delta` remains
available for explicit dependency-delta checks in repositories without policy.
`create_change_contract` preserves policy evidence from the source plan, and
`verify_change_contract` applies the same configured-policy dependency-delta
default. Plans and generated contracts also preserve `evidence_quality` so
agents can distinguish fresh exact-reference evidence from fast-mode, stale,
skipped-path, unresolved-import, ambiguous-edge, missing-runtime, missing-history,
or missing-coverage caveats.

Stable source-read tools:

- `repo_status`, `list_files`, `list_languages`, `list_symbols`
- `detect_architecture`, `architecture_boundaries`, `architecture_violations`, `architecture_policy_validate`, `architecture_policy_check`, `architecture_policy_explain`, `summarize_architecture`
- `search_code`, `search_files`, `search_symbols`, `regex_search`
- `get_definition`, `get_references`, `get_symbol_context`
- `dependency_path`, `impact_analysis`, `module_dependencies`
- `build_context_pack`, `build_compressed_context`, `retrieve_context`, `plan_change`, `create_change_contract`, `get_change_contract`, `explain_file`, `explain_symbol`
- `remember_fact`, `search_memory`
- `find_tests_for_change`, `recommend_validation_plan`, `explain_test_coverage`
- `propose_patch`, `review_patch`, `validate_patch`, `verify_change`, `verify_change_contract`, `explain_verification`

Experimental tools:

- `history_provenance_lookup`: returns bounded first-seen, last-touched, and recent commit provenance for exactly one `path` or `symbol`, including confidence and uncertainty. `symbol` accepts an exact name, qualified name, or symbol ID.
- `churn_analysis`: returns materialized all-time, 30-day, 90-day, recency-weighted, and hotspot stats for exactly one `path`, `module`, or `symbol`, including confidence and uncertainty. Lookups read persisted summaries instead of scanning raw commit history.
- `history_similar_changes`: returns ranked similar historical commits from task text, paths, symbols, co-change neighborhoods, churn, and commit metadata, with evidence and confidence on every hit.
- `ownership_lookup`: returns ranked owner suggestions for one `path` from CODEOWNERS, local git author/touch history, and secondary repo memory facts. Memory-only suggestions are marked low-confidence and uncorroborated.
- `reviewer_suggestions`: returns ranked reviewer suggestions for one `path` with source type, rationale, confidence, availability, `actual_review_evidence`, and `inferred_from_authors`. It does not call remote PR APIs; absent stored review evidence is reported as inferred or unavailable.

History API examples:

`history_similar_changes` request:

```json
{"task":"fix token expiration","paths":["src/auth/session.rs"],"symbols":["validate_session"],"limit":5}
```

Response shape:

```json
{
  "query": {"task": "fix token expiration", "paths": ["src/auth/session.rs"], "symbols": ["validate_session"]},
  "hits": [
    {
      "change": {
        "commit": {"id": "auth-expiry-fix", "summary": "Fix token expiration in login flow"},
        "touched_paths": ["src/auth/session.rs", "tests/auth_session.rs"],
        "touched_symbols": ["crate::auth::validate_session"],
        "cochange_paths": ["tests/auth_session.rs"],
        "churn_hotspot_score": 0.42
      },
      "score": 1.0,
      "confidence": "high",
      "evidence": [{"source_type": "path", "score": 0.35, "message": "query path matched historical touch"}],
      "uncertainty": []
    }
  ],
  "truncated": false,
  "uncertainty": []
}
```

`ownership_lookup` request:

```json
{"path":"src/auth/session.rs"}
```

Response shape:

```json
{
  "path": "src/auth/session.rs",
  "owners": [
    {
      "owner": {"name": "auth-owner@example.com", "email": "auth-owner@example.com"},
      "source_types": ["codeowners", "git_history"],
      "confidence": "high",
      "score": 0.91,
      "stale": false,
      "evidence": [{"source_type": "codeowners", "source": ".github/CODEOWNERS:1 `src/auth/*`"}]
    }
  ],
  "uncertainty": []
}
```

`reviewer_suggestions` request:

```json
{"path":"src/auth/session.rs"}
```

Response shape:

```json
{
  "path": "src/auth/session.rs",
  "availability": "actual_review_evidence",
  "suggestions": [
    {
      "reviewer": {"name": "reviewer@example.com", "email": "reviewer@example.com"},
      "availability": "actual_review_evidence",
      "source_types": ["review_evidence"],
      "actual_review_evidence": true,
      "inferred_from_authors": false,
      "confidence": "high",
      "score": 0.92
    }
  ],
  "uncertainty": []
}
```

`churn_analysis` request:

```json
{"path":"src/auth/session.rs"}
```

Response shape:

```json
{
  "entity_kind": "file",
  "key": "src/auth/session.rs",
  "path": "src/auth/session.rs",
  "stats": {
    "all_time": 3,
    "last_30d": 3,
    "last_90d": 3,
    "recency_weighted": 2.8,
    "touch_count": 3,
    "hotspot_score": 0.42
  },
  "confidence": "high",
  "uncertainty": []
}
```

`history_provenance_lookup` request:

```json
{"path":"src/auth/session.rs","limit":5}
```

Response shape:

```json
{
  "path": "src/auth/session.rs",
  "first_seen": {"commit": {"id": "auth-intro"}, "change_kind": "added"},
  "last_touched": {"commit": {"id": "auth-hardening"}, "change_kind": "modified"},
  "recent_touches": [
    {"commit": {"id": "auth-hardening"}, "path": "src/auth/session.rs"},
    {"commit": {"id": "auth-expiry-fix"}, "path": "src/auth/session.rs"}
  ],
  "confidence": "exact",
  "truncated": false,
  "uncertainty": []
}
```
- `semantic_status`: reports whether `.ok/vectors/current` is disabled, missing, stale, corrupt, or ready.
- `semantic_search`: searches the local semantic vector index and returns explicit semantic status metadata.
- `hybrid_search`: combines lexical and semantic candidates while preserving evidence and ranking signals.
- `explain_search_result`: returns hybrid search details for explaining semantic, lexical, and other score contributions.
- `structural_search`: currently searches indexed symbols and chunks, not a full structural query language.
- `get_implementations`, `get_callers`, `get_callees`: graph-backed heuristics until language-specific call resolution is stronger.
- `explain_flow`: currently returns architecture summary data.
- `map_stacktrace_to_code`, `find_errors_for_symbol`, `find_recent_failures`: return a structured low-confidence disabled response unless a runtime provider such as Sentry is explicitly configured.

## Write Tools

`apply_patch` is experimental and omitted unless write mode is enabled (`--allow-write`). The patches MUST first be generated using `propose_patch` and user approval should be requested before actually executing `apply_patch` for safety.

Every response is JSON and includes evidence where indexed facts are available. Result limits are capped to avoid unbounded responses.
