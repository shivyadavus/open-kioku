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
