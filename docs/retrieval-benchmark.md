# Repository Context Retrieval Benchmark

Open Kioku's repository retrieval benchmark measures one question separately from patch generation: **given a software-engineering task, did retrieval surface the files that contain the evidence an agent needs?**

The benchmark is intentionally local, deterministic, source-safe, and small enough to run in CI. It is a regression harness for Open Kioku's retrieval stack, not a claim of parity with large public research benchmarks.

## Frozen corpus

The versioned corpus lives in `benchmarks/retrieval-cases.json`. Version `open-kioku-retrieval-v1` contains 30 cases across Java, TypeScript, Python, Go, and Rust. It covers five task families:

- `issue_to_code`
- `code_to_test`
- `trace_to_code`
- `comment_to_context`
- `edit_to_ripple`

The corpus includes 20 development cases and 10 holdout cases. Five cases are natural **no-gold** tasks: the requested capability does not exist in the fixture and retrieval should ideally avoid presenting unrelated code as relevant.

Every case points to the same bundled fixture revision by a SHA-256 digest. The benchmark recomputes the digest before scoring; a source change to the fixture therefore invalidates the corpus until the revision is deliberately re-frozen.

Corpus loading is intentionally strict: unknown JSON fields are rejected, every case must use a syntactically valid SHA-256 revision, duplicate gold paths are invalid, and every declared gold file must exist under its fixture. Benchmark-data mistakes therefore fail setup rather than silently degrading a score.

The fixture contains live implementations, tests, and deliberately difficult same-domain distractors such as migration and reporting code. Distractors share vocabulary with the live path so exact keyword overlap alone is not sufficient for consistently strong ranking.

## Development and holdout discipline

Use the **development split** to design retrieval features and tune parameters. Treat the **holdout split** as a regression and release gate.

Do not repeatedly tune an algorithm against individual holdout failures. If the corpus itself needs a material change, create a new corpus revision, re-freeze the fixture digest, publish the old and new measurements, and update the version-controlled threshold contract explicitly.

This separation matters because repository-retrieval research consistently shows that retrieval quality is sensitive to task construction and repository state. The benchmark therefore records both corpus identity and exact fixture content identity.

## Strategies

The initial benchmark records two deterministic baselines over the same candidate pool:

- **lexical** — existing baseline reranking;
- **fusion** — Open Kioku's current Fusion ranking mode.

These are baselines, not a declaration that the current Fusion implementation is the final hybrid retrieval architecture. Context Compiler V2 work adds independent evidence streams and more principled fusion incrementally, with each change measured against this harness.

## Metrics

Positive cases report macro-averaged:

- Recall@1, Recall@5, Recall@10, Recall@20;
- Precision@1, Precision@5, Precision@10, Precision@20;
- mean reciprocal rank (MRR);
- file F1@10;
- gold-file yield under 2K, 4K, and 8K estimated-token budgets.

No-gold cases are **not** folded into positive recall or MRR. They report a separate no-gold false-positive rate. This is deliberate: natural no-gold behavior is a distinct product problem and must not be hidden inside an aggregate retrieval score.

Reports also include per-language, per-task-family, development/holdout, and observational p50/p95 retrieval latency.

### File-level definitions

For a positive case with gold file set `G` and the first `k` unique ranked files `R_k`:

- `Recall@k = |G ∩ R_k| / |G|`
- `Precision@k = |G ∩ R_k| / k`
- `F1@10` is the harmonic mean of Precision@10 and Recall@10
- MRR uses the rank of the first retrieved gold file.

The fixed `k` denominator for Precision@k intentionally penalizes result sets that require a wide context window to recover a small gold set.

### Token-budget yield

For each configured budget, ranked results are packed in order using deterministic first-fit selection. The current estimator is versioned as `unicode_chars_div_4_plus_metadata_v1`: Unicode character count from the result snippet, path, and qualified symbol identity is divided by four and augmented by a small metadata allowance.

This is an approximation, not a model-specific tokenizer. Its purpose in corpus v1 is stable relative comparison. If Open Kioku adopts a production tokenizer for Context Compiler budgeting, introduce a new estimator version rather than silently changing historical numbers.

The compact v1 fixture currently fits all gold evidence within 2K tokens for the measured baseline. That means token-budget yield is a regression guard in this corpus, not yet a discriminating optimization metric. Larger-repository/token-pressure evaluation should be added as a later benchmark dimension rather than artificially padding this fixture.

## Latency and determinism

The complete JSON/Markdown report records observed retrieval latency. Latency is environment-sensitive, so it is intentionally excluded from the checked-in deterministic quality baseline.

`benchmarks/retrieval-baseline.json` contains only deterministic quality values, fixture digests, corpus identity, and breakdowns. CI runs the pinned corpus twice and compares the resulting quality baselines structurally to detect nondeterminism.

## Regression policy

`benchmarks/retrieval-thresholds.json` is the version-controlled holdout quality contract. It initially prevents regressions in:

- Fusion Recall@5 and Recall@10;
- Fusion MRR;
- Fusion file F1@10;
- 2K gold-file yield;
- natural no-gold false-positive rate.

Threshold changes are product changes and should be reviewed explicitly. Do not lower a threshold merely to make CI green. If an intentional tradeoff is valuable, document the measured benefit and update the contract in the same PR.

The current no-gold rate leaves substantial room for improvement; that is expected and is a target for calibrated abstention work. The threshold protects against making it worse while preserving an honest baseline.

## Activating calibrated abstention at runtime

`ok retrieval-bench --write-abstention-activation .ok/abstention-policy.json` writes a
runtime activation artifact, but only when the calibrated policy passes the fail-closed
activation-readiness gate on untouched holdout cases (no positive holdout case may be
suppressed; at least half of the no-gold holdout cases must be caught). On any blocker
the command fails and nothing is written.

When a valid artifact with `readiness_passed = true` exists in a repository's `.ok`
directory, `ok context`/`plan` and the MCP `build_context_pack` path apply the calibrated
policy after selection: packs that fail the calibrated evidence gates carry an explicit
`calibrated_cc6_abstention` reason and caveat. Note that this annotates the pack — it
records the reason and lowers the stated confidence, but it does not remove the selected
files, so an abstaining pack still returns results.

The *signal derivation* is shared between the benchmark and the runtime
(`open_kioku_core::abstention`). The *application* of the calibrated policy is not: the
two gated strategies never build a `ContextPack` at all, and the routed strategy builds
one without attaching the policy. Calibration is measured; the deployed decision is not
measured anywhere, so the two can drift. Closing that gap is tracked under CC6.

Anything invalid, unready, or missing deactivates the feature; exact evidence and
deterministic routing blockers always take precedence.

`benchmarks/retrieval-dimension-thresholds.json` extends the contract to the measured
per-language, per-task-family, and per-query-shape dimensions, so a regression confined to
one language or task family cannot ship silently behind a healthy aggregate. It starts in
`advisory` mode (violations surface as CI warnings via
`scripts/check-retrieval-dimension-thresholds.py` without failing the run); flipping a
dimension to `blocking` is a reviewed contract change, exactly like the holdout thresholds.
The initial floors were derived from the frozen baseline with 10% relative slack and are
themselves subject to review.

## Reproduce locally

From the repository root:

```sh
cargo run -p open-kioku-cli -- \
  --json retrieval-bench . \
  --cases-file benchmarks/retrieval-cases.json \
  --min-cases 30 \
  --write-json artifacts/benchmarks/retrieval-report.json \
  --write-markdown artifacts/benchmarks/retrieval-report.md \
  --write-baseline /tmp/retrieval-baseline.json
```

The first run indexes the bundled fixture. A second deterministic pass can reuse it:

```sh
cargo run -p open-kioku-cli -- \
  --json retrieval-bench . \
  --cases-file benchmarks/retrieval-cases.json \
  --no-index \
  --min-cases 30 \
  --write-baseline /tmp/retrieval-baseline-second.json
```

Compare the two parsed JSON baselines and the checked-in baseline. They should be identical.

## Interpreting the v1 baseline

The hardened v1 corpus intentionally does not produce perfect top-k scores. At the initial freeze, Fusion holdout retrieval has meaningful headroom in Recall@5, MRR, and natural no-gold behavior, while Recall@20 remains complete. This is a healthier optimization target than a corpus that is already saturated at small `k`.

Open Kioku should optimize toward **the smallest evidence set that is correct and sufficient**, not toward retrieving more files. Exact semantic evidence remains authoritative; retrieval streams may surface candidates but may not manufacture semantic certainty.
