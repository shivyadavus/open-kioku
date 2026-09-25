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
errors instead of crashing the stdio server. A failure caused by the caller's
arguments returns `-32602` (invalid params):

- a required argument that is missing, or an argument of the wrong JSON type;
- an unknown enumerated value: `mode` on `search_code`, `kind` on `get_references`,
  `detail` on `repo_status` or `plan_change`, and `format` on a contract or
  verification explanation;
- a selector given none or several of its inputs (`task`, `plan`, `plan_json` with
  `persist: true`; `contract_id`, `contract`, `contract_json`), and
  `write_attestation` without a stored `contract_id`;
- a `plan`, `plan_json`, `contract`, `contract_json`, `verification`,
  `verification_json` or `validation_attestations` value that does not decode;
- a blank `search_code` query, a blank `regex_search` pattern, and a pattern that
  does not parse;
- `verify_change` given no changed file, or a diff that names none;
- an identifier Open Kioku issued that the repository does not hold: a
  `retrieve_context` handle, or a `verify_change` `contract_id`;
- `plan_change` with `persist: true` given a `plan` or `plan_json` the contract
  builder rejects (no evidence references, or no primary context or allowed files).

A plan `plan_change` generated itself that cannot become a contract is the tool's
failure and returns `-32000`, as does any failure of the index, the store, or the
filesystem. The line between the two: an argument of the wrong shape, and an
identifier Open Kioku issued that does not exist, are `-32602`; a well-formed lookup
of repository content that finds nothing, such as a `get_definition` or
`get_references` name or a `dependency_path` endpoint the index does not hold,
reports what the index contains (which may be stale) and is `-32000`, or the empty
result the tool already gives. The CLI exits with code 2 for the same mistakes: clap rejects unknown
option values and missing arguments that way, and `ok search`,
`ok retrieve-context`, `ok verify`, `ok verify-boundary` and the `ok contract`
commands check the rest; a plan or contract file that cannot be read still exits 1.
`query_evidence_graph` reports a query that does not parse inside its result, as
before. A dispatch that times out returns `-32001`; other tool failures return `-32000`.

A tool call refused because of the index's state rather than its arguments returns
`-32000` with the message it has always carried, plus a `data` object naming the
state and the next step, so a client can branch without matching the message:

```json
{"code": -32000, "message": "repository is not indexed: …", "data": {"state": "not_indexed", "next_step": "ok index /absolute/path/to/repo"}}
```

| `data.state` | When | `next_step` |
| --- | --- | --- |
| `not_indexed` | No index database, or one with no published manifest and no live writer | `ok index <repo>` |
| `indexing_in_progress` | A live `ok index` or `ok watch` holds `.ok/index.lock` and has not published the manifest | wait for it to publish the index, then retry |
| `index_unavailable` | The index database exists and cannot be opened or read | run `ok doctor <repo>`; `ok index <repo>` rebuilds it |
| `index_newer_than_binary` | The database schema or the manifest was written by a newer Open Kioku | upgrade Open Kioku, or run `ok index <repo>` |

A retired or unknown tool name is not a refusal of this kind and carries no `data`. Each tool dispatch is bounded by a
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
| `repo_status` | Is this repository indexed, how much does the index cover, and what is in it? | `detail` (`summary` default, `full`) |
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
| `query_evidence_graph` | Anything the fifteen above do not answer. | `query` (omit for the schema, whose `syntax`, `examples` and `unsupported` describe the query language) |

Notes on the ones whose behaviour is not obvious from the name:

- `repo_status` returns the index manifest plus `indexed: true`, `analysis_semantics_status`, `generation_id`, `semantic_lifecycle`, `languages`, `coverage`, and, whenever `coverage` is present, `coverage_gaps`. On a repository that has never been indexed it returns `{indexed: false, index_path, message, next_step}` instead, where `next_step` is `ok index <repo>`; every other tool then fails with that same message and `data.state: "not_indexed"` rather than answering from nothing, `initialize` and `tools/list` still work, and the server creates no `.ok` directory or database. When the index has rows but `ok watch` withdrew its manifest because an incremental update failed after replacing them, that object also carries `reason`, which names the failure and says the next change or `ok index` rebuilds the index; the field is absent otherwise and cleared when a manifest is published. When the index exists but cannot be served — a live `ok index`, `ok watch` or `ok snapshot import` holds `.ok/index.lock` and has not published the manifest (`indexing in progress`), the database cannot be opened, or the manifest was written by a newer Open Kioku — `initialize` and `tools/list` still answer, and every tool, `repo_status` included, returns a `-32000` error carrying that reason and a `data.state` of `indexing_in_progress`, `index_unavailable` or `index_newer_than_binary` (see Protocol Hardening) instead of `indexed: false`, so an agent is not sent to start a second `ok index`. The session continues and probes again on the next request; a session already serving an index probes again as soon as its manifest is withdrawn, which a full `ok index` does for its graph and search stages and `ok snapshot import` does before it replaces the database. `ok --json status` returns the same object. `semantic_lifecycle` is the whole semantic index status the retired `semantic_status` tool returned — state, readiness, staleness, provider, backend, model, model artifact hash, dimensions, distance, vector and stale counts, rebuild reasons — so an agent can tell which model produced the vectors before trusting a `mode: "semantic"` result. `ok semantic status` is the CLI equivalent. `coverage` is the same object `ok --json status` exposes: source files `discovered` versus `indexed`, `generated`, `skipped` counts per skip reason, `by_language` with the same fields per language, two blind-spot counts the ratio cannot include — `pruned_dirs` (directories cut from the walk by name, such as `build` or `dist`) and `walk_errors` (directory reads that failed) — and, for files a policy excluded (`hidden`, `ignored`, `denied`, `secret_policy`, `vendor`, `generated`, `fast_mode`, `symlink_policy`), `policy_excluded_by_source`, `policy_excluded_by_language` (the same source counts per language), and `policy_excluded_dirs` (count per top-level directory). The ratio's denominator is `discovered` minus the policy-excluded count, per language and overall: a git-ignored worktree under a hidden directory is reported, not counted as missing. An agent can see how much of the repository the evidence covers before trusting an absence. It is `null` when the index predates coverage recording; re-run `ok index` to record it. `languages` is taken from `coverage.by_language` when it exists and from a file scan otherwise. `coverage_gaps` lists the programming languages whose missing source changes what an absence means, each with `language`, `cause` (`git_ignore`, `excluded_by_policy` or `omitted`), `missing_files`, `language_files`, `reason` and, when one governs it, `governing_setting`. It is `[]` when coverage is complete, and it is the verdict `build_context_pack` and `plan_change` price as the `index_coverage` confidence signal; thresholds and how ignored directories are counted are in `docs/ranking.md`, "Index coverage gaps". Definitions of what is counted: `docs/indexing-pipeline.md`, "Coverage". `quality.quality_notes` is `{total, by_kind, sample}` and `quality.skipped_paths` is `{total, by_reason, sample}` by default, each `sample` at most 20 entries drawn round-robin across every kind or reason, because the full lists were 1.4 MB of a 1.5 MB payload on a 380-file repository; `detail: "full"` returns every entry as the manifest stores it, and `ok --json status --full` is the CLI equivalent. An unknown `detail` is an invalid-params error (`-32602`), whether or not the repository is indexed. Every note carries a `kind` assigned by its producer (`discovery`, `scip`, `exact_references`, `index_mode`, `import_resolver_caveat`, `import_resolver_cap`, `symbol_registry_caveat`, `symbol_registry_unresolved`, `relationship_resolution`, `git_history`; `unclassified` for notes from a manifest written before kinds existed). `quality.redacted_files` is the number of data, config, and prose files indexed with secret-like values replaced by `[REDACTED]` (`docs/security-model.md`, "Secret-value redaction"); it is `null` when the index predates redaction, which means those files were stored as read, so a client must treat null as unknown rather than as zero. `quality.pending_pre_redaction_compaction` is true while the bytes such an index stored as read are still to be cleared from the database, its write-ahead log and the semantic vector store; the next `ok index` retries that work, and `ok doctor` reports it. `search_code` and `regex_search` carry a caveat naming how many files hold redacted values, because a redacted value cannot be found by searching for it; `ok search` and `ok search --regex` carry the same sentence, from the same function.
- `list_files` without `path` is a paginated inventory of what the index holds. With one `path` it returns `{path, file, chunks, caveats}` — the indexed file record and every code chunk covering it, with line ranges. A path the index does not hold returns a null `file` and a caveat naming that, never an empty success.
- `search_code` is the single entry point for "find where X is handled". `mode=code` ranks lexical BM25 candidates from indexed chunks and file paths; `mode=graph` searches indexed graph-node documents; `mode=semantic` ranks candidates from the local vector index; `mode=hybrid` ranks lexical and semantic candidates together, and a path's result carries its semantic evidence. Every mode answers through the same function as `ok search` (`open_kioku_context::search::ranked_search`), so a query returns the same ordered paths and line ranges on both surfaces, and a page at `offset` is a slice of that list. `code`, `semantic` and `hybrid` rerank with the repository's `[ranking]` weights and return one result per path. The server reads those weights from `ok.toml` when `ok mcp serve` starts, so restart it after changing them. `graph` keeps the index's order, as `ok search --kind graph` does. Each source is asked for `4 × (offset + limit)` candidates, clamped to 100..500, never fewer than the `offset + limit + 1` it was asked for before, but a file counts once, so a deep page can hold fewer results than the raw index would. `limit: 0` returns an empty page with `has_more`, where `ok search --limit 0` prints every unique path in the candidate pool. When a source filled its window and the page ends the results, the response sets `truncated` with a warning. Semantic and hybrid report `semantic_status` and fall back to lexical-only results when the vector index is not ready, so their extra recall is never assumed. Every result already carries `score_breakdown` and `evidence_refs`; there is no separate explanation step, and none of the modes performs AST or structural matching. An unknown `mode` is an invalid-params error (`-32602`), and so is a blank `query`, with the message `ok search` gives for it, `search requires a non-empty query`: an empty result would read as "nothing matches". A `query` that is absent, or that is not a string, is reported as ``missing required string argument `query` ``, the way every tool reports a forgotten argument, so a caller can tell which mistake it made.
- `regex_search` compiles the caller's pattern once and evaluates it line by line over indexed chunk text, file by file in path order, returning exact single-line hits at confidence 1.0 with `regex match` as the match reason. Results are not ranked and the walk stops at `limit`. Only indexed chunk text is searched, so regions the indexer never chunked cannot match; every response carries that caveat with the number of files scanned. Two separate limits are disclosed as `truncated` with a warning: the 20,000-file walk budget, and the shared `MAX_MCP_FETCH` candidate cap that a deep `offset` runs into. An unparseable pattern is an invalid-params error (`-32602`) rather than an empty result. `ok search <pattern> --regex` is the equivalent CLI surface and returns the same `results`, `truncated`, `warnings`, and `caveats` fields.
- `search_symbols` filters the indexed symbol table by case-insensitive substring against name and qualified name, ordered by qualified name, and pages through everything when `query` is omitted. It is not fuzzy matching and the results are not ranked, so a name sharing no substring with the query does not match.
- `get_definition` returns the indexed definition record: file, line range, kind, qualified name, confidence, provenance. With `include_body: true` it also joins the symbol back to the indexed chunk text covering it, returning the definition body with the line range it spans plus up to ten indexed lines above and below, verbatim. Documentation comments appear in `leading_lines` only when the indexer chunked them — the usual case for a definition that follows another, never the case for the first symbol in a file, whose preamble falls outside every chunk. Nothing is parsed out or reconstructed. A body that cannot be recovered at all, and one recovered only in part, both come back with an explicit caveat naming the gap. `ok symbol context <name>` is the equivalent CLI surface.
- `get_references` carries three kinds of evidence in one response and keeps them apart. `references` returns indexed occurrences under `occurrences`, each with its own `provenance` and `confidence` — a `lexical` occurrence at low confidence is the name-match fallback used when the index holds no resolved occurrence. `callers` and `callees` return persisted CALLS graph edges under `nodes` and `edges`, with `direction`. `implementations` returns verified implementation sites from persisted IMPLEMENTS facts under `implementations`, with parser provenance. `kind: "all"` returns every section at once. Each section names its own `evidence_source` (`symbol_occurrences`, `sqlite_graph_store`, `persisted_implements_facts`) and carries its own caveats, because absence does not mean the same thing in each: no occurrence is a different claim from no persisted IMPLEMENTS fact. `kind: "implementations"` does not require the target symbol to be indexed, since IMPLEMENTS facts are keyed by target name; the other kinds do, and an unresolvable name is an error for them. An unknown `kind` is an invalid-params error (`-32602`).
- `dependency_path` traces the shortest dependency or reference path between two nodes from the persisted graph. Omit `to` and it returns `from`'s direct dependency neighbours instead of a route. Both shapes report `evidence_source: "sqlite_graph_store"`. A `from` or `to` that resolves to no indexed file, symbol, or graph node is a tool error (`-32000`) naming it, not an empty edge list that would read as "unconnected". `ok path` resolves its arguments through the same function (`open_kioku_graph::resolve_graph_node`), so both surfaces mean the same nodes.
- `build_context_pack` assembles primary files, extracted symbols, dependency edges, tests, architecture policy when configured, and patch boundaries into one `ContextPack`, rendered as Markdown by default. With `compress: true` it stores the original snippets under `.ok` and returns compact handles instead; `retrieve_context` expands one, and a handle the repository does not hold is an invalid-params error (`-32602`) rather than a null value. That is the only path that writes, and `format` then defaults to `json` (`toon` is also produced; `markdown` is not).
- `plan_change` builds the evidence-backed pre-edit plan: primary context, architecture policy when configured, evidence quality, impact candidates, validation candidates, edit boundaries, and recommended tool calls. `detail: "preflight"` returns instead a concise, schema-stable start decision — verdict, confidence, confirmed edit files, likely affected files, validation commands, risks, caveats, evidence references, and evidence quality — in `json` (default), `markdown`, `html`, or `text`. `detail: "patch"` returns a patch plan that writes no files. `persist: true` builds a `ChangeContractV1` from the plan and stores it under `.ok/contracts` (set `store: false` for a transient one); it accepts an inline `plan` or `plan_json` so a plan you already hold becomes a contract without re-planning. An unknown `detail` is an invalid-params error (`-32602`), checked before the index is read. So is a `plan` or `plan_json` given with `persist: true` that the contract builder rejects (no evidence references, or no primary context or allowed files); a plan `plan_change` generated itself that cannot become a contract is a `-32000` tool failure.
- `verify_change` has three inputs and one verb. With `plan` or `plan_json` it checks the actual diff or changed file list against the saved plan's boundary and expected files. API surface (`check_api_surface`) and dependency-delta checks run as part of contract verification: with a contract, or with a plan that converts into one because it has primary context or allowed boundary files; a plan that does not convert is verified without them. A renamed file is checked at both its previous and new path, and a copy's source against the forbidden rules; `previous_paths` in the report pairs them. Copy pairs come only from a supplied `diff`: `since_plan` asks git to detect renames, not copies. With `check_api_surface`, a public item that moves unchanged with a renamed file warns as `api_surface_moved` instead of failing as removed, unless the contract forbids removals in the previous path's scope or additions in the new path's scope, which fails the move. With `contract_id`, `contract`, or `contract_json` it verifies against a change contract; a stored id appends a verification record under `.ok/contracts`, stale evidence quality warns by default and fails under strict traceability, and missing validation attestations warn as pending. Add `explain: true` to get the decision, changed files with their rename and copy pairs, boundary failures, warnings, dependency deltas, validation attestations, and recommended tests instead of the raw report. With `verification` or `verification_json` it explains a report you already hold and verifies nothing. `run_commands: true` executes the plan's or contract's validation commands on the local machine; `write_attestation: true` persists timestamped records. With all flags false it is read-only.
- `find_tests_for_change` returns ranked test file paths with relevance scores from naming conventions, import relationships, and co-change history. Omit `path` for the repository-wide stored test evidence. It executes nothing, and a ranked test is a candidate, not proof of coverage.
- `query_evidence_graph` executes a read-only query in a constrained Cypher-like DSL — not full Cypher — returning `columns` and `rows` with the usual pagination metadata, plus an opaque local `continuation`, an `expires_at` Unix timestamp, and a `next` object when more rows exist. Called with no `query`, it returns the versioned evidence schema instead: node types, edge types, property specs, feature flags, evidence source types, query features, `syntax` (one sentence per clause of the query language), `examples` (queries that parse and run), `unsupported` (rejected forms, each with an example and the accepted alternative), optional evidence, and the Tier-1 relationship-semantic capability matrix. A query that does not parse returns `error.kind: "parse_error"` with a message naming the accepted form: an unknown node or edge type lists the schema's types, a MATCH without an edge pattern shows one, and property access, a function or DISTINCT in RETURN says RETURN accepts only variables. A filter on a field its variable does not take (`qualified_name` on a File node, `file_path` on a `ConfigKey`, `confidence` on a node rather than a bound edge) is also a `parse_error`, listing the fields that variable takes; the schema's `syntax` lists the fields per node type. `file_path` on a symbol node is the path of the File node that defines it, `qualified_name` is a symbol node's label, and `evidence_source` (the pass that recorded the evidence), `evidence_source_type` and `confidence` are read from the evidence of a one-hop edge bound in MATCH (`MATCH (a:Function)-[c:CALLS]->(b:Function) WHERE c.confidence >= 0.85 RETURN a, b`), with `<`, `<=`, `>` and `>=` comparing the band's score as a number. When a matched node carries no `file_path` or `qualified_name` (a `Module` built from an import, a symbol no single File node defines), its row is excluded and a caveat reports at least how many scanned rows that cost, since the scan stops once the requested page is filled. An edge's source node is the node variable on its left and its type is written in the pattern, so `source` and `source_type` are not fields on any variable: either is a `parse_error` naming both readings. An `evidence_source` value that names a node rather than the pass that recorded the evidence is a `parse_error` too, rather than an empty result. An untyped one-hop edge or an untyped multi-hop source node is also reported as `parse_error` when the query runs. A query that parses but fails validation or execution returns `query_rejected` (such as a hop range above 5 or a duplicate RETURN variable), `unbound_variable`, `depth_limit_exceeded` (a hop range above the depth cap of 3) or `timeout`. None of those names an alternative: `query_rejected` states the rule that failed (such as `max_hops cannot exceed hard limit of 5`), `unbound_variable` names the variable, `depth_limit_exceeded` gives only the requested depth (`requested 4 exceeds limit`), and `timeout` says only that the query timed out. A type may be written as the schema names it or in its serialized form (`DatabaseTable` or `database_table`, `DependsOn` or `DEPENDS_ON`), case-insensitively.

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
