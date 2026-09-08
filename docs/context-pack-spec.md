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
      "inspected_sources": ["runtime_signals", "search_result.evidence"],
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
        "rationale": "explicit exact symbol references or SCIP signals"
      }
    ],
    "blockers": [],
    "caveats": ["runtime corroboration is absent"]
  }
}
```

## Selection and region widening

`retrieval_diagnostics.selection` is the pack's cost ledger. `selected_units` lists every unit the pack presents, in presentation order, each with its `path`, `line_range`, `estimated_tokens` (a deterministic four-characters-per-token estimate of what the unit shows, plus fixed overhead), `authority`, `evidence_refs`, and a `rationale`; `estimated_tokens_selected` and `per_file_tokens` are the sums. The list has three kinds of entry:

- **Primary units selected under the budget.** Selection walks the task-ranked candidates and picks units by value per token within `ContextBudget` (`max_tokens` less the instruction and validation reserves, `max_per_file`, `max_primary_files`). The CLI and MCP default path is the file-limit budget, which selects the ranked prefix. Ordering is decided here and nothing after it reorders the pack.
- **Region widening of the top files.** Selection units are chunk-sized (one chunk per symbol start), and on commit-derived holdouts from four large public repositories they covered 3–22% of the lines the real change touched even when the file was right, identical at 4k, 8k and 16k tokens, with median tokens-to-first-gold of zero: the cut was wrong, not the order. After selection, for the first `region_files` distinct files in selection order (default 3), each selected unit is widened to its smallest enclosing symbol, the file's other task-ranked units are re-admitted in rank order, and physically adjacent chunks are absorbed, until the file reaches `region_tokens_per_file` estimated tokens (default 1,200, counting the units it already had) or the budget selection left over is spent. Widening runs after selection and only grows or appends units, so nothing selection chose is removed or reordered; that, not the leftover-budget guard, is what keeps a lower-ranked file's first unit in the pack. On the file-limit budget the CLI and MCP default path uses, `max_tokens` is effectively unbounded, so `region_tokens_per_file` is the bound that actually binds; under `ContextBudget::default()` the spendable ceiling is 6,000 tokens (8,000 less the two 1,000-token reserves) and widening stops there. Every step is an evidence ref on the unit — `region:enclosing-symbol:<symbol id>`, `region:ranked-unit:<task rank>`, `region:adjacent-unit:<start>-<end>` — and is named in the unit's `rationale`; the retrieval trace keyed on the unit follows its widened identity, so source attribution and `unattributed_selected_file_count` are unaffected. A widening the cap or budget refused is recorded in `omitted_due_to_caps` or `omitted_due_to_budget` with the symbol and its cost, and an omission that widening reversed (a unit the per-file cap dropped and widening re-admitted) is withdrawn from the omission lists. The widened `line_range` is clipped to the lines the snippet actually shows.
- **Supporting files.** Impact expansion lists them; they are not selected under the budget. They appear last, costed at their listing size (path and reason, not the impact snippet), with a rationale saying so, so that a yield measured top-down against the ledger counts every file the pack returns.

`scripts/score-context-cases.py` walks `selected_units` in order until the next unit would overflow a token budget and reports `gold_file_yield@B` and `gold_line_yield@B` against commit-derived gold files and line ranges (`docs/retrieval-benchmark.md`, "Gold yield at a token budget").

The builder classifies the task, searches indexed chunks, resolves symbols, estimates impact, recommends tests, and emits a conservative edit boundary. Semantic search may contribute only when enabled; it is never authoritative. Confidence is computed from deterministic evidence signals, not from language-model wording.

`PlanReport` extends this provenance with `runtime_signals`, optional `architecture_policy`, `evidence_quality`, and `evidence_by_section`, mapping sections such as `primary_context`, `validation`, `impact`, `boundary`, and `negative_evidence` to stable evidence IDs. `evidence_quality` records index mode, freshness, exact-reference/runtime/history/coverage availability, skipped paths, unresolved imports, ambiguous edges, failed optional passes, and caveats. When a repository architecture policy is configured, context, plan, and impact JSON include the active `PolicyCheckReport`; otherwise the field is omitted. Context, validation items, runtime signals, and boundary rules also expose `evidence_refs` or stable IDs so downstream MCP tools can audit why each item was selected.

Saved JSON plans can be enforced with `ok verify --plan plan.json --changed <path>`. Allowed files pass, caution files are surfaced with reasons, forbidden generated/vendor/security-sensitive paths fail, and edits outside the saved boundary require explicit `--evidence-ref` values.

Post-edit verification uses `ok verify --plan plan.json --diff patch.diff`, `ok verify --plan plan.json --git`, or explicit `--changed <path>` values. It parses changed files from unified diffs, computes changed symbols, checks evidence-backed boundaries, recomputes impact and validation recommendations, reports evidence-quality and confidence caveats, optionally runs saved validation commands with `--run-commands`, and returns `pass`, `warn`, or `fail`. Stale evidence quality warns by default and fails under strict traceability policy. When a repository architecture policy is configured, verification also checks dependency deltas against that policy by default; `--check-deps` remains available for explicit dependency-delta checks in repositories without policy. Add `--write-attestation` with `--run-commands` to persist validation ledgers under `.ok/contracts/validation/` and attach attestation summaries to contract verification records. MCP exposes the same behavior through `verify_change` with `plan`/`plan_json`, `diff` or `changed_files`, optional `evidence_refs`, optional `run_commands`, and optional `write_attestation`.
