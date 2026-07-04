# Workflow Benchmarks

`ok workflow-bench` scores plan -> edit -> verify workflows from JSON cases.
The committed suite lives at `benchmarks/workflow-cases.json` and contains 20 cases that CI runs on every pull request through `.github/workflows/bench.yml`.

Run it locally:

```sh
ok workflow-bench . --cases-file benchmarks/workflow-cases.json --limit 10
```

Use `--json` to inspect per-case hits and rollups:

```sh
ok --json workflow-bench . \
  --cases-file benchmarks/workflow-cases.json \
  --limit 10
```

## Case Format

Each case is a JSON object:

```json
{
  "id": "plan-engine",
  "task": "change plan engine boundary evidence",
  "expected_primary_context": ["crates/open-kioku-plan/src/lib.rs"],
  "expected_impact": ["crates/open-kioku-context/src/lib.rs"],
  "expected_tests": ["plan_surfaces_runtime_signals"],
  "expected_boundary": ["crates/open-kioku-plan/src/lib.rs"],
  "forbidden_paths": ["target/generated.rs"],
  "changed_files": ["crates/open-kioku-plan/src/lib.rs"],
  "expected_verdict": "warn",
  "expected_confidence": true
}
```

Fields:

- `id`: stable identifier used in reports.
- `task`: the user-facing change prompt.
- `expected_primary_context`: files that should appear in plan primary context.
- `expected_impact`: files that should appear in direct or indirect impact.
- `expected_tests`: test names that should be selected.
- `expected_boundary`: files expected in allowed or caution boundaries.
- `forbidden_paths`: paths that should not appear in the selected boundary.
- `changed_files` or `unified_diff`: edit input passed to verification.
- `expected_verdict`: `pass`, `warn`, or `fail`.
- `expected_confidence`: whether the workflow should be treated as successful
  for confidence calibration.

## Metrics

The report includes:

- `context_recall_at_k`: expected primary context found in the plan.
- `impact_recall_at_k`: expected impact files found in impact analysis.
- `test_recall_at_k`: expected tests found in validation.
- `boundary_precision`: selected boundary files that do not match case
  forbidden paths.
- `boundary_recall`: expected boundary files found in allowed or caution lists.
- `confidence_calibration_error`: absolute error between expected success and
  the plan-derived success probability.
- `verification_verdict_accuracy`: expected verification verdict match rate.

The `baseline` section is intentionally simple: lexical search for context and
zeroes for planning-only capabilities such as impact, tests, boundaries, and
verification. The `deltas` section shows how much the full workflow adds over
that baseline.

Benchmark fixture files are excluded from retrieval while scoring so cases do
not answer themselves by being indexed as searchable source.

## CI Benchmark Matrix

`.github/workflows/bench.yml` writes reproducible benchmark artifacts under
`artifacts/benchmarks` and uploads them as the `benchmark-reports` artifact.
Run the same commands locally after `cargo build --release -p open-kioku-cli`.

| Metric | Command | Artifact |
| --- | --- | --- |
| indexing time by phase | `ok bench . --quality-case PlanEngine=crates/open-kioku-plan/src/lib.rs --quality-min-precision-at-1 1.0` | `quality-bench.txt` |
| memory usage | `cargo bench --workspace` | `criterion.txt` |
| graph node/edge counts | `ok bench .` | `quality-bench.txt` |
| graph query latency | `ok --repo . graph query --dsl "MATCH (f:File)-[:DEFINES]->(s:Function) RETURN f, s LIMIT 2" --limit 2` | `graph-query-latency.txt` |
| search latency | `ok bench .` | `quality-bench.txt` |
| test selection quality | `ok workflow-bench . --cases-file benchmarks/workflow-cases.json --limit 10 --min-cases 20` | `workflow-bench.txt` |
| plan quality | `ok workflow-bench . --cases-file benchmarks/workflow-cases.json --limit 10` | `workflow-bench.txt` |
| verification false positives/negatives | `ok workflow-bench . --cases-file benchmarks/workflow-cases.json --min-verification-accuracy 1.0` and `ok contract-bench benchmarks/contract-fixture --cases-file benchmarks/contract-cases.json` | `workflow-bench.txt`, `contract-bench.txt` |
| snapshot export/import time | `ok --repo . snapshot export --quality fast` and `ok --repo . snapshot import` | `snapshot-export.txt`, `snapshot-import.txt` |
| token savings | `ok contract-bench benchmarks/contract-fixture --cases-file benchmarks/contract-cases.json --min-toon-reduction 0.35` | `contract-bench.txt` |

The CI thresholds are intentionally checked in the workflow command line rather
than only described here, so a regression fails the benchmark job.
