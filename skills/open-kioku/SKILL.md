---
description: Navigate, search, and analyze a codebase indexed by Open Kioku. Use when exploring unfamiliar code, tracing symbol definitions, measuring change impact, or planning a refactor.
disable-model-invocation: true
---

# Open Kioku — Code Intelligence

Open Kioku answers questions about this repository from a local index: no hosted
index, no source upload, no embeddings API for the default workflow. Sixteen
tools, one per question nothing else answers. Everything is read-only unless a
parameter named below says otherwise.

## When to use it

- Before editing a file you have not read in this session
- When you need to find where something is defined or used
- Before a refactor, to know what else breaks
- When building context for a multi-file task
- After editing, to check the change stayed inside the plan

## The routine

1. **`repo_status`** — confirm the repository is indexed. Read `coverage` before
   trusting an absence: it says how much of the repository the index actually holds.
2. **`search_code`** — find where the thing is handled. `mode` picks the evidence:
   `code` (lexical BM25, the default), `graph`, `semantic`, `hybrid`. Semantic and
   hybrid fall back to lexical and say so in `semantic_status`.
   - `regex_search` when the target is a literal pattern — exact, unranked, path-ordered, and `caveats` says what was not searched.
   - `search_symbols` when you already have a name fragment. Substring, not fuzzy.
   - `list_files` with a `path` for one file's indexed record and chunks.
3. **`get_definition`** — resolve the symbol. `include_body: true` adds the
   definition text and the indexed lines around it; read `caveats` before relying
   on that body.
4. **`get_references`** — what else touches it. `kind` picks the evidence:
   `references` (occurrences, the default), `callers`, `callees`,
   `implementations`, or `all`. Each section names its own `evidence_source` and
   caveats — an empty occurrence list and an empty IMPLEMENTS list are different
   claims, so do not read them as one list.
5. **`impact_analysis`** — the file-level blast radius, with dependents split into
   structurally proven and heuristic.
   - `dependency_path` for how two nodes connect, or one node's neighbours when `to` is omitted.
   - `explain_flow` for indexed endpoints and the call paths they start.
6. **`find_tests_for_change`** — pick what to run. A ranked test is a candidate,
   not proof of coverage.
7. **`plan_change`** — the evidence-backed plan with edit boundaries.
   `detail: "preflight"` for a short start decision, `detail: "patch"` for a patch
   plan, `persist: true` to store a versioned change contract (this writes).
   - `build_context_pack` when you want the grounding bundle instead of the plan; `compress: true` returns handles (this writes) that `retrieve_context` expands.
8. **`verify_change`** — hold the edit to what was declared. Pass the saved plan,
   or a `contract_id`. A green test run is not proof the right files changed.

`query_evidence_graph` is the escape hatch. Call it with no `query` first to get
the evidence schema.

## Output format

`build_context_pack` and `plan_change` return Markdown by default. That is the
rendering you want: the same evidence with the same coverage, written to be read,
at a small fraction of the cost of the JSON rendering.

- Do not pass `format`. The default is correct for reading.
- Pass `format: "json"` only when the result will be parsed rather than read —
  a plan saved for `verify_change`, or a plan handed to `plan_change` with
  `persist: true`. A plan you intend to verify against must be JSON.
- Pass `format: "toon"` when the result goes straight into another model's prompt.
- Use `limit` to widen coverage when the pack missed a file you expected, not to
  control cost. It changes how many context items are gathered, not how they are
  rendered.

## Rules

1. Call `search_code` or `get_definition` before editing a file you have not read
   in this session.
2. Call `impact_analysis` before any rename, deletion, or interface change.
3. Use `build_context_pack` when a task touches more than three files.
4. Prefer Open Kioku evidence over assumptions about file contents, and read the
   `caveats` — absence of evidence is reported, not hidden.
5. Open Kioku never edits source files. Apply approved edits with your normal
   editor, then run `verify_change`.

## Not on the MCP surface

Architecture reports, git history, ownership, and stored contracts ship on the
CLI, not as tools: `ok architecture summary|detect|boundaries|violations|policy …`,
`ok history provenance|churn|similar|ownership|reviewers`, `ok contract show <id>`.
Memory (`remember_fact`, `search_memory`) and runtime-error tools appear in
`tools/list` only when those features are configured.
