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
  "runtime_signals": [],
  "test_candidates": [],
  "risk_report": {},
  "recommended_change_boundary": {
    "allowed_files": [],
    "caution_files": [],
    "forbidden_files": []
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

`PlanReport` extends this provenance with `evidence_by_section`, mapping sections such as `primary_context`, `validation`, `impact`, `boundary`, and `negative_evidence` to stable evidence IDs. Context and validation items also expose `evidence_refs` so downstream MCP tools can audit why each item was selected.
