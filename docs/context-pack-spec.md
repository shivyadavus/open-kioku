# Context Pack Spec

A Context Pack is the agent-ready bundle returned before edits:

```json
{
  "task": "Add retry handling for failed API imports",
  "intent": "code_change",
  "primary_files": [],
  "primary_symbols": [],
  "supporting_files": [],
  "dependency_edges": [],
  "runtime_signals": [
    {
      "id": "runtime-auth-endpoint",
      "kind": "endpoint",
      "message": "runtime endpoint observed in local trace artifact: POST /login",
      "file_range": {
        "path": "src/auth.rs",
        "line_range": { "start": 3, "end": 5 }
      },
      "occurred_at": null,
      "confidence": "high"
    }
  ],
  "test_candidates": [],
  "risk_report": {},
  "recommended_change_boundary": {
    "allowed_files": [],
    "caution_files": [],
    "forbidden_files": [],
    "allowed_symbols": [],
    "allowed_rules": [
      {
        "path": "src/auth.rs",
        "reason": "primary context matched the requested edit intent",
        "evidence_refs": ["search:src/auth.rs:3-5:0"],
        "symbols": ["src::auth::issue_token"]
      }
    ],
    "caution_rules": [
      {
        "path": "src/lib.rs",
        "reason": "impact analysis linked this file to the primary edit candidates",
        "evidence_refs": ["search:src/lib.rs:7-10:0"],
        "symbols": []
      }
    ],
    "forbidden_rules": [
      {
        "pattern": "vendor/**",
        "reason": "vendored dependencies require a separate explicit change",
        "evidence_refs": ["boundary:default-forbidden"]
      }
    ],
    "expansion_requirements": [
      {
        "reason": "Any edit outside allowed_files must cite concrete evidence from search, impact, references, tests, architecture, ownership, or co-change analysis.",
        "required_evidence_refs": []
      }
    ]
  },
  "validation_plan": {},
  "evidence": [],
  "negative_evidence": [
    {
      "query": "Add retry handling for failed API imports",
      "scope": "runtime",
      "inspected_sources": ["runtime_signals", "search_result.score_breakdown"],
      "reason": "no runtime trace, incident, or error artifact corroborated the selected context",
      "confidence": 0.75,
      "suggested_next_probe": "Import or configure runtime artifacts, then rerun `ok plan`."
    }
  ],
  "confidence_summary": "",
  "confidence_breakdown": {
    "overall_enum": "medium",
    "overall_score": 0.62,
    "components": [
      {
        "signal": "exact_references",
        "raw_value": 0.25,
        "normalized_value": 0.25,
        "weight": 0.2,
        "contribution": 0.05,
        "evidence_ids": [],
        "rationale": "selections backed by exact-authority retrieval, indexed symbol references, or SCIP evidence; a plan also counts proven cross-file dependents"
      }
    ],
    "blockers": [],
    "caveats": ["runtime corroboration is absent"]
  }
}
```

## Confidence

`confidence_breakdown` is computed by `ConfidenceBreakdown::from_signals` in `open-kioku-core` from typed inputs only; no input is derived by scanning result prose, and `ok context` and `ok plan` feed the same functions the same way, except that a plan also counts structurally proven cross-file dependents toward `exact_reference_count` (below) and a context pack does not.

- `overall_enum` labels the weighted score: `Low` below 0.55, `Medium` from 0.55, `High` from 0.75, `Exact` from 0.95. **`Exact` is a provenance claim, not a score band**: it is reachable only when `exact_reference_count > 0`, and otherwise the label caps at `High` whatever the score. Without exact evidence the score itself is capped at 0.74, so the two rules agree; the label gate is stated so a weight change cannot reintroduce an `Exact` label over heuristic evidence.
- `exact_reference_count` counts selections backed by exact provenance: a primary unit whose retrieval trace carries `authority: exact` (the same set `retrieval_diagnostics.selection.exact_evidence_count` reports), a supporting or impact result carrying `exact_reference_provenance` (set by the impact engine on a result produced from a SCIP, tree-sitter or LSP symbol occurrence, and kept when a higher-scoring lexical duplicate on the same range is merged into it), or an evidence record whose `source_type` is `scip`, `tree_sitter` or `lsp`. A plan also counts each structurally proven dependent in `impact.proven_impact` whose `path` differs from `impact.target`, whose `authority` is `authoritative`, which is not `ambiguous`, and whose `edge_type` references a symbol: `REFERENCES`, `USES_TYPE`, `CALLS`, `IMPLEMENTS`, or `EXTENDS`. A same-file proven edge, such as `USES_TYPE` between two of the target's own symbols, is the target referring to itself and does not count; neither does an ambiguous edge, nor an `IMPORTS` edge, which a glob import (`use fx::*;`) proves from `import_binding` alone without referring to any symbol in the target. When a plan counts exact references, it drops the `exact_references` negative evidence item it would otherwise copy from its context pack. A lexical hit whose evidence line quotes a query variant containing `scip` or `exact` is not one.
- `negative_evidence_count`, and the blocker `N negative evidence signal(s) lowered confidence`, is the number of items in the pack's own `negative_evidence` list whose scope is `primary_context` or `anchor`. Items in the `exact_references`, `validation`, and `runtime` scopes are still reported, and their absence is priced by the `exact_references`, `validation_availability`/`test_coverage`, and `runtime_corroboration` components and their caps rather than counted a second time here. Items in the `history` and `boundary` scopes are reported, not priced: history contributes only positive score components in plans, and the boundary item classifies what matched (docs and tests only) rather than naming missing evidence. The `coverage` item is reported, not counted here; a majority coverage gap lowers confidence only through the `index_coverage` caps.
- Named identifiers in the task (`IssueTokenService`, `reticulate_splines`, `X-Request-Id`, `--allow-network`; a capital after the first character, an `_`, digits with a capital, or a `-` separator in a token the next bullet does not classify as a weak anchor; sentence-initial capitals such as `Reap` and ticket references are excluded) that none of the top five primary results spells are `anchor` negative evidence carrying the names, and that item is counted: the 0.60 cap and the count blocker apply whenever any named identifier is unmatched. When every named identifier is unmatched the score additionally caps at 0.50 and a blocker names them; when some are, a caveat names the missing ones.
- An all-lowercase token whose only separator is `-` (`re-index`, `best-effort`, `get-or-load`, `serde-json`) is a weak anchor, because a compound word and a lowercase kebab-case name are spelled the same, unless the task marks it as code: quoted in backticks (`` `get-or-load` ``), written as a flag (`--allow-network`), or joined to a path, module, scope, or assignment by `/`, `::`, `@`, `=`, or a `.` with a word on its other side (`crates/open-kioku-cor/src/lib.rs`, `drive-by.rs`); a sentence-final `.` does not count. A capital, a digit, or an `_` in the token also makes it a named identifier (`X-Request-Id`, `retry-v2`), and a task with an odd number of backticks has no weak anchors, because its quoted spans cannot be told apart. A bare package name such as `serde-json` stays weak. An unmatched weak anchor is listed in the same `anchor` negative evidence item, so the 0.60 cap and the count blocker still apply, and is named in the caveat `N hyphenated task word(s) appear in no selected context`. It is not a named identifier: it is excluded from `named_anchor_count`, never produces the all-unmatched blocker or its 0.50 cap, and never appears in that blocker. The `anchor` item's reason names the two kinds separately, `task identifier(s) spelled by no selected context: …` and `hyphenated task word(s) spelled by no selected context: …`, and a plan's `low confidence:` risk reason splits the same way.
- When the index manifest records a coverage gap (`IndexCoverage::gaps`: a programming language git ignore rules mostly excluded, one under the doctor's 98% or 20-file rule, or every language when policy left no programming-language source to consider), the pack and plan always report it: one `coverage` negative evidence item listing each gap's `coverage:<language>:<cause>` evidence id, a caveat per gap naming the missing share and reason (`index coverage: 25 of 27 rust source files (92.6%) are not indexed (git-ignore); an absence among them is not evidence`), and the zero-weight `index_coverage` component. A gap alone changes no score or label, and coverage caveats are exempt from the 0.94 caveat cap. Beside a majority gap (at least half of the language's judged files missing), an unmatched named identifier or an empty primary context caps the score at 0.50 with the blocker `the task may name code in source the index excluded: …`, and the `anchor` item's next probe no longer says the name does not exist; a primary context in the gap's language caps at 0.74 with the blocker `the selected context is in a language the index mostly excluded: …`, whether or not the task named an identifier the context matched. An index that published no coverage record at all is reported instead as the caveat `index coverage is unrecorded: …` with its own `coverage` negative evidence item, and caps at 0.94 like any other caveat. Thresholds and how ignored directories are counted: `docs/ranking.md`, "Index coverage gaps".
- `evidence_density` measures distinct evidence records over twice the selected primary files; `validation_availability` is whether at least one validation target was selected; `test_coverage` is whether at least one selected target carries a runnable command. They describe completeness of the pack, not its relevance to the task, which `task_relevance` measures.
- In a plan, `evidence_quality.caveats` are attached after scoring through `ConfidenceBreakdown::add_caveats`, under the same rules as the breakdown's own caveats: any caveat caps the score at 0.94 and the label is re-derived through the `Exact` gate. `evidence_quality.exact_reference_available` comes from the manifest's SCIP count; when the plan counted exact references through another typed source, the flag is set and the `exact symbol/reference evidence is unavailable` caveat is withdrawn, so a plan never reports `exact_references 1.00` beside it.

In the Markdown rendering, `Exact-authority selections: N` under Retrieval is the count of primary units with exact retrieval authority, and `Retrieval confidence: <label>` on the next line is the pack's `overall_enum`. They are different things: the first is provenance of individual units, the second the label of the whole pack, and a pack with `Exact-authority selections: 0` can never read `Retrieval confidence: Exact`.

## Selection and region widening

`retrieval_diagnostics.selection` is the pack's cost ledger. `selected_units` lists every unit the pack presents, in presentation order, each with its `path`, `line_range`, `estimated_tokens` (a deterministic four-characters-per-token estimate of what the unit shows, plus fixed overhead), `authority`, `evidence_refs`, and a `rationale`; `estimated_tokens_selected` and `per_file_tokens` are the sums. The list has three kinds of entry:

- **Primary units selected under the budget.** Selection walks the task-ranked candidates and picks units by value per token within `ContextBudget` (`max_tokens` less the instruction and validation reserves, `max_per_file`, `max_primary_files`). The CLI and MCP default path is the file-limit budget, which selects the ranked prefix. Ordering is decided here and nothing after it reorders the pack.
- **Region widening of the top files.** Selection units are chunk-sized (one chunk per symbol start), and on the 626 locally derived line-range-annotated cases from four large public repositories they covered 3–22% of the lines the real change touched even when the file was right, identical at 4k, 8k and 16k tokens, with median tokens-to-first-gold of zero: the cut was wrong, not the order. After selection, for the first `region_files` distinct files in selection order (default 3), each selected unit is widened to its smallest enclosing symbol, the file's other task-ranked units are re-admitted in rank order, and physically adjacent chunks are absorbed, until the file reaches `region_tokens_per_file` estimated tokens (default 1,200, counting the units it already had) or the budget selection left over is spent. Widening runs after selection and only grows or appends units, so nothing selection chose is removed or reordered; that, not the leftover-budget guard, is what keeps a lower-ranked file's first unit in the pack. On the file-limit budget the CLI and MCP default path uses, `max_tokens` is effectively unbounded, so `region_tokens_per_file` is the bound that actually binds; under `ContextBudget::default()` the spendable ceiling is 6,000 tokens (8,000 less the two 1,000-token reserves) and widening stops there - note that `region_files` x `region_tokens_per_file` is 3,600 of those 6,000, over half the ceiling before selection's own units are counted. Every step is an evidence ref on the unit — `region:enclosing-symbol:<symbol id>`, `region:ranked-unit:<task rank>`, `region:adjacent-unit:<path>:<start>-<end>` — and is named in the unit's `rationale`; the retrieval trace keyed on the unit follows its widened identity, so source attribution and `unattributed_selected_file_count` are unaffected. A widening the cap or budget refused is recorded in `omitted_due_to_caps` or `omitted_due_to_budget` with the symbol and its cost, and an omission that widening reversed (a unit the per-file cap dropped and widening re-admitted) is withdrawn from the omission lists. The widened `line_range` is clipped to the lines the snippet actually shows.
- **Supporting files.** Impact expansion lists them; they are not selected under the budget. They appear last, costed at their listing size (path and reason, not the impact snippet), so that a yield measured top-down against the ledger counts every file the pack returns. Every unit carries a `kind` — `primary` for what selection chose (including any widening) and `supporting` for these — because the two are costed differently: a measurement of what retrieval selected must split on that field rather than on the rationale prose.

`scripts/score-context-cases.py` walks `selected_units` in order until the next unit would overflow a token budget and reports `gold_file_yield@B` and `gold_line_yield@B` against commit-derived gold files and line ranges (`docs/retrieval-benchmark.md`, "Gold yield at a token budget").

The builder classifies the task, searches indexed chunks, resolves symbols, estimates impact, recommends tests, and emits a conservative edit boundary. Semantic search may contribute only when enabled; it is never authoritative. Confidence is computed from deterministic evidence signals, not from language-model wording.

`PlanReport` extends this provenance with `runtime_signals`, optional `architecture_policy`, `evidence_quality`, and `evidence_by_section`, mapping sections such as `primary_context`, `validation`, `impact`, `boundary`, and `negative_evidence` to stable evidence IDs. `evidence_quality` records index mode, freshness, exact-reference/runtime/history/coverage availability, skipped paths, unresolved imports, ambiguous edges, failed optional passes, and caveats. When a repository architecture policy is configured, context, plan, and impact JSON include the active `PolicyCheckReport`; otherwise the field is omitted. Context, validation items, runtime signals, and boundary rules also expose `evidence_refs` or stable IDs so downstream MCP tools can audit why each item was selected. On every primary and supporting result, `evidence_refs` pairs one to one with `evidence`: the i-th ref names the fact the i-th line states, and no ref repeats. On a primary result, and in the selection ledger's `selected_units`, a retrieval line's ref is `search:<path>:<range>:<index>`, the id of the pack record that carries that line; supporting results publish no records, so their retrieval refs are the impact report's own ids. A line from another producer cites that producer's id (a graph edge, symbol, document section, region step, runtime signal or history fact), and a line backed by several facts cites the first, with all of them listed on the result's score component. A plan lists its boundary refs, its rules' refs, and every `evidence_by_section` list in file order: by path, then numeric line range, then evidence line, with refs of other shapes in string order. A plan's allowed and caution rules cite their own path's refs, at most ten; every cap here chooses what to keep by authority - refs of exact-reference results, then exact `symbol:` anchors, then direct graph edges (`edge:`), then the remaining refs in file order - and lists what it keeps in file order; a rule whose path has no evidence of its own cites three refs of the plan's `recommended_change_boundary.evidence_refs` (itself capped at 50) instead. Either way, `evidence_refs_omitted` counts the refs the cap left out (omitted when zero), and every ref a plan derives for a rule is listed in `evidence_by_section`. A rule carried over from the context boundary keeps its own refs under the same cap. A plan's `validation` holds at most eight targets; when more plausible targets survive the test predicate and the per-file suite preference, `validation_omitted` counts the ones that bound dropped and `validation_omitted_ids` lists them (both omitted when zero), and `risk.reasons` states the same count; the disclosure does not change the plan's confidence or risk score. `ok verify` still reports each of them as a `missing_test` finding, with a reason saying the plan's validation cap omitted it.

Saved JSON plans can be enforced with `ok verify --plan plan.json --changed <path>`. Allowed files pass, caution files are surfaced with reasons, forbidden generated/vendor/security-sensitive paths fail, and edits outside the saved boundary require explicit `--evidence-ref` values.

Post-edit verification uses `ok verify --plan plan.json --diff patch.diff`, `ok verify --plan plan.json --git`, or explicit `--changed <path>` values. It parses changed files from unified diffs and computes changed symbols from hunk line ranges when a diff is supplied, file granularity otherwise: a symbol is listed when its indexed range overlaps a hunk's post-edit or pre-edit lines, and only the innermost overlapping symbol per hunk, so a module or class cannot stand in for the whole file; a symbol whose whole range one side of the hunk covers is listed as well, so a hunk that replaces an entire `impl` or class names that container beside its methods. A hunk side that holds no lines (`-41,0` for a pure insertion, `+9,0` for a pure deletion) contributes no range, so a symbol that merely ends at the anchor line is not listed. A hunk no symbol range covers is reported as `<path>:<start>-<end>` under `changed_regions_without_symbol` rather than dropped, and the text and HTML reports list those regions too. When a path has no hunk ranges, every symbol in the file is listed and a `symbol_granularity` warning names the path and says why: no diff was supplied for it (only `--changed <path>`, or `changed_files` over MCP), or the supplied diff names it without hunk ranges (a binary or mode-only entry). That warning does not change the verdict. Verification checks both sides of a rename. `changed_files` lists a renamed file's previous path beside its new one, as it would list the deletion and addition git reports without rename detection. `previous_paths` pairs the two as `{path, previous_path, kind}`, where `kind` is `rename` or `copy`. A previous path that is forbidden or outside the boundary fails the way an edit to it would, and the finding names both paths. A copy's source is not a changed file, but a source that matches a forbidden rule fails. Each hunk of a rename is attributed by side: its pre-edit lines to the previous path and its post-edit lines to the new path, so changed symbols stay hunk-scoped on both sides. A side of a text rename that the diff changes no lines of lists no symbols and carries no `symbol_granularity` warning; a binary rename is still listed at file granularity. With `--check-api-surface`, a public item whose kind, name and signature are unchanged across a rename is reported as `api_surface_moved`, a warning that names both paths. When the contract's `api_surface_constraints` forbid removals in the previous path's scope or additions in the new path's scope, the move fails instead, because callers' import path changes. An item missing from the new path still fails as removed, and an item whose signature changed still fails. A `diff --git` entry is read as a rename when git marks it (`rename from`/`rename to`, or differing `---`/`+++` paths with `similarity index`) or when its pre-edit and post-edit paths otherwise differ. Each hunk's content ends where the counts in its `@@ -a,b +c,d @@` header say (an omitted count is 1), so a removed `-- x` or added `++ y` line is never taken for a path, and a plain `---`/`+++` entry that follows a `diff --git` entry in the same diff is still read. A plain unified diff keeps only its post-edit path, and rename and copy paths from a CRLF diff carry no trailing `\r`. `--git` and `--since-plan` run git with `--no-color --find-renames --src-prefix=a/ --dst-prefix=b/`, so neither `diff.renames`, a local path-prefix setting nor `color.diff` changes what is parsed. They do not ask git to detect copies, so `copy` pairs come only from a supplied diff. Verification then checks evidence-backed boundaries, recomputes impact and validation recommendations, reports evidence-quality and confidence caveats, optionally runs saved validation commands with `--run-commands`, and returns `pass`, `warn`, or `fail`. Stale evidence quality warns by default and fails under strict traceability policy. When a repository architecture policy is configured, verification also checks dependency deltas against that policy by default; `--check-deps` remains available for explicit dependency-delta checks in repositories without policy. Add `--write-attestation` with `--run-commands` to persist validation ledgers under `.ok/contracts/validation/` and attach attestation summaries to contract verification records. MCP exposes the same behavior through `verify_change` with `plan`/`plan_json`, `diff` or `changed_files`, optional `evidence_refs`, optional `run_commands`, and optional `write_attestation`. Supplying no change at all is caller input, not a configuration problem: `ok verify` requires one of `--git`, `--diff`, `--changed`, or `--since-plan` and exits 2 without one (and exits 2 with `invalid input: …` when the supplied diff names no file), and `verify_change` returns JSON-RPC `-32602`.
