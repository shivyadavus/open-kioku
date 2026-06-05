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

The builder classifies the task, searches indexed chunks, resolves symbols, estimates impact, recommends tests, and emits a conservative edit boundary. Semantic search may contribute only when enabled; it is never authoritative. Confidence is computed from deterministic evidence signals, not from language-model wording.

`PlanReport` extends this provenance with `runtime_signals` and `evidence_by_section`, mapping sections such as `primary_context`, `validation`, `impact`, `boundary`, and `negative_evidence` to stable evidence IDs. Context, validation items, runtime signals, and boundary rules also expose `evidence_refs` or stable IDs so downstream MCP tools can audit why each item was selected.

Saved JSON plans can be enforced with `ok verify --plan plan.json --changed <path>`. Allowed files pass, caution files are surfaced with reasons, forbidden generated/vendor/security-sensitive paths fail, and edits outside the saved boundary require explicit `--evidence-ref` values.

Post-edit verification uses `ok verify --plan plan.json --diff patch.diff`, `ok verify --plan plan.json --git`, or explicit `--changed <path>` values. It parses changed files from unified diffs, computes changed symbols, checks evidence-backed boundaries, recomputes impact and validation recommendations, optionally runs saved validation commands with `--run-commands`, and returns `pass`, `warn`, or `fail`. MCP exposes the same behavior through `verify_change` with `plan`/`plan_json`, `diff` or `changed_files`, optional `evidence_refs`, and optional `run_commands`.
