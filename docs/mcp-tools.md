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

Optional command execution and local validation attestations require explicit opt-in:

```sh
ok mcp serve --repo /absolute/path/to/repo --read-only --approval-required --allow-command "cargo test" --deny-network
```

## Recommended Agent Routine

Open Kioku exists to give Claude Code, Cursor, and other MCP clients a repeatable
pre-edit routine. This is the one routine the project ships; `skills/open-kioku/SKILL.md`
and `.cursor-plugin/skills/open-kioku/SKILL.md` carry the same eight steps for the
client-facing skill.

```text
Use Open Kioku before editing. Check repo_status, then search_code and
get_definition to locate the code, get_references and impact_analysis to see what
else it touches, find_tests_for_change to pick validation, and plan_change before
you edit. After editing, verify_change against that plan.
```

The eight steps, in order:

1. `repo_status` — confirm the repository is indexed, see its coverage and languages, and check whether the semantic index is ready. Everything below reads that index; an absence means nothing until you know what it covers.
2. `search_code` — find where the thing is handled. `mode` picks the evidence: `code` (lexical, the default), `graph`, `semantic`, `hybrid`. `regex_search` when the target is a literal pattern, `search_symbols` when you already have a name, `list_files` with a `path` for one file's indexed detail.
3. `get_definition` — resolve the symbol. Add `include_body: true` when the definition text and the lines around it are what the task needs.
4. `get_references` — see what else touches it. `kind` picks the evidence: `references` (occurrences, the default), `callers`, `callees`, `implementations`, or `all`. Read each section's own `evidence_source` and `caveats`: an empty occurrence list and an empty IMPLEMENTS list are different claims.
5. `impact_analysis` — the file-level blast radius, with dependents split into structurally proven and heuristic. `dependency_path` explains how two nodes connect, or lists one node's neighbours when `to` is omitted; `explain_flow` traces indexed endpoints to their call paths.
6. `find_tests_for_change` — pick the validation targets for the changed path.
7. `plan_change` — the evidence-backed pre-edit plan, with edit boundaries. `detail: "preflight"` for a short start decision, `detail: "patch"` for a patch plan, `persist: true` to store a versioned change contract. `build_context_pack` when the task needs the grounding bundle rather than the plan; `compress: true` returns handles that `retrieve_context` expands.
8. `verify_change` — hold the actual edit to what was declared. Pass the saved plan, or a `contract_id`. Exit code 0 from a test runner is not proof the right files changed; this is.

`query_evidence_graph` is the escape hatch for a question none of those answer;
call it with no `query` first to get the evidence schema.

Open Kioku MCP tools do not edit source files. `build_context_pack` with
`compress: true` and `plan_change` with `persist: true` write local `.ok/`
artifacts so handles and contracts can be recalled later; everything else is
read-only. Agents should apply approved source edits with their normal editor
tools, then use Open Kioku to verify the result.

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

For `list_files`, `search_symbols`, `search_code` in every mode, and
`regex_search`, the items are returned under `files`, `symbols`, or `results`
alongside that metadata. `get_references` pages each evidence section
independently, so `returned`, `limit`, and `has_more` sit inside the section
rather than at the top level. `query_evidence_graph` returns `columns` and `rows` with the same
metadata. When graph query results have more rows, the response also includes
an opaque local `continuation`, an `expires_at` Unix timestamp, and a `next`
object with the safe follow-up `offset`.
Search tools use a bounded candidate scan for high offsets and mark the response
with `truncated` plus a warning when callers should narrow the query.

Large `tools/call` text content is truncated before it is placed into the
human-readable `content` field; the response includes a warning when that
happens. JSON results are also returned in `structuredContent` (the declared
`outputSchema`), where they are not capped. Rendered text results (Markdown,
TOON) are sent once, in `content`: `structuredContent` then carries only
`{"rendered_in": "content", "bytes": N, "truncated": bool}`, because repeating
the rendering doubled the size of every such response and the most common
client reads `content` only. A successful `tools/call` result also carries
`"isError": false` explicitly rather than leaving the optional field absent.

Both success envelopes are pinned by golden snapshots in
`crates/open-kioku-mcp/snapshots/mcp/`: `tools_call_json_tool.json` for a JSON
tool and `tools_call_rendered_tool.json` for a Markdown rendering, alongside the
`tool_error.json` failure envelope. Changing the wire shape of a tool response
moves one of those files.

## The Advertised Tools

The server advertises 16 tools. That number is derived from the tool table in
`crates/open-kioku-mcp/src/lib.rs` and checked against this file and `README.md`
by `scripts/validate-docs.sh`, so it cannot drift from the code.

Each of the sixteen answers one question no other tool answers. Names that were
retired in 4.0.0 are gone from `tools/list` and from the dispatch table together,
so a stale name never resolves to a different shape. It is not a bare refusal
either: the error names where the capability went, for example ``` `get_callers`
was retired from the MCP tool surface in 4.0.0: use `get_references` with
`kind: "callers"` ``` or ``` `churn_analysis` was retired from the MCP tool
surface in 4.0.0: moved to the CLI: `ok history churn --path`, `--module`, or
`--symbol` ```.

| Tool | Question it answers | Key parameters |
| --- | --- | --- |
| `repo_status` | Is this repository indexed, how much does the index cover, and what is in it? | — |
| `list_files` | What files does the index hold, and what does it hold about one of them? | `path` for one file's record and chunks |
| `search_code` | Where is this handled? | `mode`: `code`, `graph`, `semantic`, `hybrid` |
| `regex_search` | Where does this literal pattern appear? | `pattern` |
| `search_symbols` | Which indexed symbols match this substring? | `query` (optional) |
| `get_definition` | Where is this symbol defined, and what does it say? | `include_body` |
| `get_references` | What else touches this symbol? | `kind`: `references`, `callers`, `callees`, `implementations`, `all` |
| `dependency_path` | How are these two connected, or what is next to this one? | `from`, optional `to` |
| `impact_analysis` | What breaks if I change this file? | `path` (a path the index does not hold reports `risk_report.level: "unknown"` and is named in `risk_report.reasons`, since no dependents could be measured) |
| `explain_flow` | Which call paths start at an indexed endpoint? | `limit` |
| `build_context_pack` | What do I need in context for this task? | `compress` |
| `retrieve_context` | What was behind this handle? | `handle` |
| `plan_change` | What should I change, and within what boundary? | `detail`, `persist`, `store` |
| `verify_change` | Did the change do what the plan or contract said? | `plan`, `contract_id`, `verification`, `explain` |
| `find_tests_for_change` | What should I run now? | `path` (optional) |
| `query_evidence_graph` | Anything the fifteen above do not answer. | `query` (omit for the schema) |

Notes on the ones whose behaviour is not obvious from the name:

- `repo_status` returns the index manifest plus `indexed: true`, `analysis_semantics_status`, `generation_id`, `semantic_lifecycle`, `languages`, and `coverage`. On a repository that has never been indexed it returns `{indexed: false, index_path, message, next_step}` instead, where `next_step` is `ok index <repo>`; every other tool then fails with that same message rather than answering from nothing, `initialize` and `tools/list` still work, and the server creates no `.ok` directory or database. `ok --json status` returns the same object. `semantic_lifecycle` is the whole semantic index status the retired `semantic_status` tool returned — state, readiness, staleness, provider, backend, model, model artifact hash, dimensions, distance, vector and stale counts, rebuild reasons — so an agent can tell which model produced the vectors before trusting a `mode: "semantic"` result. `ok semantic status` is the CLI equivalent. `coverage` is the same object `ok --json status` exposes: source files `discovered` versus `indexed`, `generated`, `skipped` counts per skip reason, `by_language` with the same fields per language, and two blind-spot counts the ratio cannot include — `pruned_dirs` (directories cut from the walk by name, such as `build` or `dist`) and `walk_errors` (directory reads that failed). An agent can see how much of the repository the evidence covers before trusting an absence. It is `null` when the index predates coverage recording; re-run `ok index` to record it. `languages` is taken from `coverage.by_language` when it exists and from a file scan otherwise. Definitions of what is counted: `docs/indexing-pipeline.md`, "Coverage".
- `list_files` without `path` is a paginated inventory of what the index holds. With one `path` it returns `{path, file, chunks, caveats}` — the indexed file record and every code chunk covering it, with line ranges. A path the index does not hold returns a null `file` and a caveat naming that, never an empty success.
- `search_code` is the single entry point for "find where X is handled". `mode=code` is lexical BM25 over indexed chunks and file paths; `mode=graph` searches indexed graph-node documents; `mode=semantic` searches the local vector index; `mode=hybrid` merges lexical and semantic candidates, deduplicates by path, and re-sorts by combined score. Semantic and hybrid report `semantic_status` and fall back to lexical-only results when the vector index is not ready, so their extra recall is never assumed. Every result already carries `score_breakdown` and `evidence_refs`; there is no separate explanation step, and none of the modes performs AST or structural matching. An unknown `mode` is a tool error, and so is a blank `query`: an empty result for it would read as "nothing matches".
- `regex_search` compiles the caller's pattern once and evaluates it line by line over indexed chunk text, file by file in path order, returning exact single-line hits at confidence 1.0 with `regex match` as the match reason. Results are not ranked and the walk stops at `limit`. Only indexed chunk text is searched, so regions the indexer never chunked cannot match; every response carries that caveat with the number of files scanned. Two separate limits are disclosed as `truncated` with a warning: the 20,000-file walk budget, and the shared `MAX_MCP_FETCH` candidate cap that a deep `offset` runs into. An unparseable pattern is a tool error rather than an empty result. `ok search <pattern> --regex` is the equivalent CLI surface and returns the same `results`, `truncated`, `warnings`, and `caveats` fields.
- `search_symbols` filters the indexed symbol table by case-insensitive substring against name and qualified name, ordered by qualified name, and pages through everything when `query` is omitted. It is not fuzzy matching and the results are not ranked, so a name sharing no substring with the query does not match.
- `get_definition` returns the indexed definition record: file, line range, kind, qualified name, confidence, provenance. With `include_body: true` it also joins the symbol back to the indexed chunk text covering it, returning the definition body with the line range it spans plus up to ten indexed lines above and below, verbatim. Documentation comments appear in `leading_lines` only when the indexer chunked them — the usual case for a definition that follows another, never the case for the first symbol in a file, whose preamble falls outside every chunk. Nothing is parsed out or reconstructed. A body that cannot be recovered at all, and one recovered only in part, both come back with an explicit caveat naming the gap. `ok symbol context <name>` is the equivalent CLI surface.
- `get_references` carries three kinds of evidence in one response and keeps them apart. `references` returns indexed occurrences under `occurrences`, each with its own `provenance` and `confidence` — a `lexical` occurrence at low confidence is the name-match fallback used when the index holds no resolved occurrence. `callers` and `callees` return persisted CALLS graph edges under `nodes` and `edges`, with `direction`. `implementations` returns verified implementation sites from persisted IMPLEMENTS facts under `implementations`, with parser provenance. `kind: "all"` returns every section at once. Each section names its own `evidence_source` (`symbol_occurrences`, `sqlite_graph_store`, `persisted_implements_facts`) and carries its own caveats, because absence does not mean the same thing in each: no occurrence is a different claim from no persisted IMPLEMENTS fact. `kind: "implementations"` does not require the target symbol to be indexed, since IMPLEMENTS facts are keyed by target name; the other kinds do, and an unresolvable name is an error for them. An unknown `kind` is a tool error.
- `dependency_path` traces the shortest dependency or reference path between two nodes from the persisted graph. Omit `to` and it returns `from`'s direct dependency neighbours instead of a route. Both shapes report `evidence_source: "sqlite_graph_store"`. A `from` or `to` that resolves to no indexed file, symbol, or graph node is a tool error naming it, not an empty edge list that would read as "unconnected".
- `build_context_pack` assembles primary files, extracted symbols, dependency edges, tests, architecture policy when configured, and patch boundaries into one `ContextPack`, rendered as Markdown by default. With `compress: true` it stores the original snippets under `.ok` and returns compact handles instead; `retrieve_context` expands one, and a handle the repository does not hold is a tool error rather than a null value. That is the only path that writes, and `format` then defaults to `json` (`toon` is also produced; `markdown` is not).
- `plan_change` builds the evidence-backed pre-edit plan: primary context, architecture policy when configured, evidence quality, impact candidates, validation candidates, edit boundaries, and recommended tool calls. `detail: "preflight"` returns instead a concise, schema-stable start decision — verdict, confidence, confirmed edit files, likely affected files, validation commands, risks, caveats, evidence references, and evidence quality — in `json` (default), `markdown`, `html`, or `text`. `detail: "patch"` returns a patch plan that writes no files. `persist: true` builds a `ChangeContractV1` from the plan and stores it under `.ok/contracts` (set `store: false` for a transient one); it accepts an inline `plan` or `plan_json` so a plan you already hold becomes a contract without re-planning. An unknown `detail` is a tool error.
- `verify_change` has three inputs and one verb. With `plan` or `plan_json` it checks the actual diff or changed file list against the saved plan's boundary, expected files, API surface, and dependency policy. With `contract_id`, `contract`, or `contract_json` it verifies against a change contract; a stored id appends a verification record under `.ok/contracts`, stale evidence quality warns by default and fails under strict traceability, and missing validation attestations warn as pending. Add `explain: true` to get the decision, boundary failures, warnings, dependency deltas, validation attestations, and recommended tests instead of the raw report. With `verification` or `verification_json` it explains a report you already hold and verifies nothing. `run_commands: true` executes the plan's or contract's validation commands on the local machine; `write_attestation: true` persists timestamped records. With all flags false it is read-only.
- `find_tests_for_change` returns ranked test file paths with relevance scores from naming conventions, import relationships, and co-change history. Omit `path` for the repository-wide stored test evidence. It executes nothing, and a ranked test is a candidate, not proof of coverage.
- `query_evidence_graph` executes a read-only query in a constrained Cypher-like DSL — not full Cypher — returning `columns` and `rows` with the usual pagination metadata, plus an opaque local `continuation`, an `expires_at` Unix timestamp, and a `next` object when more rows exist. Called with no `query`, it returns the versioned evidence schema instead: node types, edge types, property specs, feature flags, evidence source types, query features, optional evidence, and the Tier-1 relationship-semantic capability matrix.

`build_context_pack`, `plan_change`, and `impact_analysis` include an
`architecture_policy` report when a repository policy is configured.
`verify_change` loads configured policy automatically and checks dependency
deltas against it by default; `check_dependency_delta` remains available for
explicit checks in repositories without policy. Plans and generated contracts
preserve `evidence_quality` so agents can distinguish fresh exact-reference
evidence from fast-mode, stale, skipped-path, unresolved-import, ambiguous-edge,
missing-runtime, missing-history, or missing-coverage caveats.

Every tool returned by `tools/list` includes a `maturity` field. The sixteen are
stable. Where a tool folds in a capability that rests on heuristic or fallback
evidence — `search_code`'s semantic and hybrid modes, `get_references`'s call and
implementation sections — the response itself says so, in `semantic_status` and
in each section's `evidence_source` and `caveats`, rather than in a tool-level
label the caller would have to remember.

## Config-Gated Tools

Five tools are advertised only where the repository has configured the feature
behind them. They stay dispatchable either way; what the gate removes is a name
an agent would otherwise be taught to reach for and get nothing from.

- `remember_fact` and `search_memory` — advertised when `[memory] enabled = true`. They maintain append-only repo memory facts with extracted entity links and provenance. `ok memory` is the CLI surface.
- `map_stacktrace_to_code`, `find_errors_for_symbol`, and `find_recent_failures` — advertised when the configured runtime provider validates its own configuration, not merely when `enabled = true`. `[runtime]` takes `enabled`, `provider`, `organization`, `project`, and `auth_token_env`; for `provider = "sentry"` the gate is `open_kioku_sentry::ensure_configured`, so an incomplete block advertises nothing. The same check decides the answer: without a validated provider they return the structured low-confidence disabled response, and with one they return `configured: true` and state that this build ships no runtime query implementation — an empty result is never presented as evidence that no runtime errors exist. They are marked `experimental`.

## Capabilities That Moved to the CLI

Thirteen capabilities left the MCP surface in 4.0.0 and ship on the CLI. The
trade is deliberate and is not free: a coding agent with a shell can still reach
them through `ok`, but a pure MCP client with no shell cannot.

| Retired MCP tool | CLI equivalent |
| --- | --- |
| `detect_architecture` | `ok architecture detect` |
| `architecture_boundaries` | `ok architecture boundaries`, `ok architecture summary` |
| `architecture_violations` | `ok architecture violations` |
| `summarize_architecture` | `ok architecture summary` |
| `architecture_policy_validate` | `ok architecture policy validate` |
| `architecture_policy_check` | `ok architecture policy check` |
| `architecture_policy_explain` | `ok architecture policy explain` |
| `history_provenance_lookup` | `ok history provenance --path` / `--symbol` |
| `churn_analysis` | `ok history churn --path` / `--module` / `--symbol` |
| `history_similar_changes` | `ok history similar --task` / `--path` / `--symbol` |
| `ownership_lookup` | `ok history ownership --path` |
| `reviewer_suggestions` | `ok history reviewers --path` |
| `get_change_contract` | `ok contract show <id>` |

`ok architecture summary` is new in 4.0.0 and returns what the retired
`summarize_architecture` returned: detected components, the configured policy,
the evaluated `policy_check`, and its violations. `ok architecture violations`
now reports those evaluated violations rather than heuristic detection output.

## Source Edits

Open Kioku intentionally exposes no MCP source-editing tool. Use `plan_change` (with `detail: "patch"` for a patch plan) to prepare an evidence-backed edit, apply the approved change with the normal editor, then run `verify_change`.

Every response is JSON and includes evidence where indexed facts are available. Result limits are capped to avoid unbounded responses.
