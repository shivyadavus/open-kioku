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

A no-gold case counts as a false positive when the strategy returned results **and stood behind them**: for the fusion strategy that means the context pack's overall confidence is above `Low` (`returned_confident` in the case report). Lexical search finds some word overlap for almost any prose, so "returned anything" is not the product's abstention signal; a pack that reports `Low` confidence has already told the caller not to trust it. Strategies that build no context pack have no confidence and fall back to "returned anything". Both `returned_any` and `returned_confident` are recorded per case.

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

The thresholds were re-frozen on 2026-09-07 together with the identifier-aware Tantivy tokenizer, the edit-anchor fix, and the confidence-based no-gold definition above. Holdout MRR moved from 0.917 to 0.806 — one of the eighteen holdout cases (`ts-trace-invoice-created`) now ranks its gold third instead of first because a sibling file shares the identifier parts — while on a 490-case commit-derived benchmark over a 10k-file Java repository the same changes lifted lexical MRR from 0.235 to 0.393 (dev) and 0.202 to 0.337 (holdout), and the production context-pack path from R@20 0.42 / MRR 0.22 to 0.73 / 0.44 on the same 60 cases. The no-gold maximum tightened from 0.75 to 0.25 because the product's confidence signal now catches all five no-gold cases. A 39-file fixture cannot arbitrate a change measured at that scale; it exists to catch determinism and gross regressions, and `scripts/commit-derived-cases.py` is how larger corpora are derived.

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

## Commit-derived corpora

The fixture above is 39 files. Three of the defects fixed in September 2026 — tests
displacing source in primary context, sentence-initial capitals acting as edit anchors,
and single-word expansions outranking the whole task — passed it bit-identically and only
showed on a ten-thousand-file repository. Larger corpora are derived from a real
repository's own history, after the Agent Retrieval Bench methodology:

1. Pick a base commit `B` and index the repository (or a subtree) **at `B`**.
2. Every case is a later commit: the query is its subject with PR numbers stripped; the gold
   set is the source files it modified that already existed at `B`. Commits whose subject
   names a path are dropped (kept, the path is the answer; stripped, the subject no longer
   describes the change), and a subject that repeats an earlier one up to numbers keeps only
   its first instance: hugo's "releaser: Bump versions for release of X" was a third of its
   holdout with the same gold file every time, so one pattern decided the corpus.
   The change lives in the future, never in the index, so a query cannot retrieve its
   own diff.
3. Split chronologically — older cases are the development set, newer ones the holdout.

```sh
scripts/commit-derived-cases.py ~/src/elasticsearch --base 1e6d7960 --after 3800 \
    --path-prefix libs/ --path-prefix modules/ --path-prefix server/ --out cases.tsv
scripts/score-context-cases.py --ok target/release/ok --repo ./es-at-base \
    --cases cases-holdout.tsv --label holdout --out holdout.json
scripts/compare-commit-derived-report.py holdout.json benchmarks/commit-derived/elasticsearch-1e6d7960-holdout.json
```

`score-context-cases.py` drives `ok context --json`, the same builder `ok plan` and the MCP
`build_context_pack` tool use, and ranks files in the order the pack presents them. It
reports Recall@k and MRR with 95% bootstrap intervals. Frozen baselines live under
`benchmarks/commit-derived/`; `.github/workflows/commit-derived-bench.yml` re-derives four
corpora nightly as a matrix — Elasticsearch (Java, `libs/ modules/ server/`), hugo (Go),
deno_std (TypeScript), and transformers (Python) — and fails when a watched metric falls more
than 0.03 below its baseline. The baselines were frozen from a hosted-runner matrix run on
2026-09-07 after the commit-scope anchors landed and repeated subjects were dropped from the
derivation (each file records its run and commit under `provenance`); earlier freezes are in
each file's git history:

| Corpus | Split | Cases | R@5 | R@20 | MRR |
|---|---|---|---|---|---|
| elasticsearch-1e6d7960 | dev | 262 | 0.576 | 0.744 | 0.460 |
| elasticsearch-1e6d7960 | holdout | 113 | 0.549 | 0.681 | 0.482 |
| hugo-79da24a0 | dev | 196 | 0.571 | 0.765 | 0.395 |
| hugo-79da24a0 | holdout | 84 | 0.691 | 0.809 | 0.551 |
| deno_std-d93aa7c9 | dev | 385 | 0.797 | 0.893 | 0.636 |
| deno_std-d93aa7c9 | holdout | 166 | 0.753 | 0.801 | 0.621 |
| transformers-7cd9b985 | dev | 462 | 0.606 | 0.732 | 0.512 |
| transformers-7cd9b985 | holdout | 199 | 0.658 | 0.749 | 0.556 |

Hugo was the hardest of the four while a third of its holdout was one repeated release-bump
commit; with one case per repeated subject it sits between the others. 21% of its gold files
are `_test.go` benchmarks for tasks that never say "test", and its commit subjects are terse. Read the per-corpus numbers, not
an average; a change that helps Java and hurts Go is a regression on Go. The frozen baselines are what an accuracy change is judged
against; a change that helps one language and hurts another shows up as one failing matrix
entry rather than a blended average. Queries are commit subjects, so absolute numbers are not comparable with
published benchmarks that use issue text; compare a change against the frozen baseline,
not against the literature.

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
