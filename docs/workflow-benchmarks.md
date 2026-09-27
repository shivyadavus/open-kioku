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
  "expected_impact": [
    "crates/open-kioku-cli/src/commands/mod.rs",
    "crates/open-kioku-mcp/src/lib.rs"
  ],
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

## Changing a case

The cases are a frozen regression suite: a behaviour change is proven by adding a
case, not by editing an existing one until it passes. An existing expectation is
changed only when it is wrong about the repository, and the reason is recorded
here and in the pull request that changes it. An `expected_impact` entry must be
a file that can actually be affected by the change: a file in a crate that
depends on the changed crate and uses the changed API. A file the changed crate
itself depends on is upstream of the change, and a match on it is a false
positive that the case would otherwise reward.

Recorded changes:

- `plan-engine` (#557): `expected_impact` was `crates/open-kioku-context/src/lib.rs`,
  but `open-kioku-plan` depends on `open-kioku-context`, not the reverse. It is now
  `crates/open-kioku-cli/src/commands/mod.rs` (the `ok plan` and `ok preflight`
  commands build a `PlanEngine`) and `crates/open-kioku-mcp/src/lib.rs` (the MCP
  plan tools build one).
- `ingest-history` (#557): `expected_impact` was `crates/open-kioku-git/src/lib.rs`,
  but `open-kioku-ingest` depends on `open-kioku-git`, not the reverse. It is now
  `crates/open-kioku-cli/src/commands/index.rs` and `crates/open-kioku-watch/src/lib.rs`,
  the only callers of the `Indexer` entry points that return Git history
  (`index_repo_with_history_mode_and_progress` and `index_repo_with_history`).

Both old expectations were met only by lexical matches that moved across the
case limit when unrelated test names changed (#537, #548). On the build before
the change they were no longer met at all: both cases scored an impact recall of
0.0, and the suite's `impact_recall_at_k` was 0.900. With the corrected
expectations `ingest-history` scores 0.5 (`index.rs` ranks third; `watch/src/lib.rs`
is not in the plan's impact list) and `plan-engine` stays at 0.0: neither
corrected file appears anywhere in its plan's 31-file impact list. That is a
measured gap in impact analysis for this change, not a case to adjust further.
The suite's `impact_recall_at_k` moves from 0.900 to 0.925 only because the
corrected `ingest-history` expectation names a file the plan already returned;
impact analysis itself did not change. Impact recall is reported but not gated.

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
