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
  "confidence_summary": ""
}
```

The builder classifies the task, searches indexed chunks, resolves symbols, estimates impact, recommends tests, and emits a conservative edit boundary. Semantic search may contribute only when enabled; it is never authoritative.

