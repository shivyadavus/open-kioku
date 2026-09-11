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
   its first instance: on a Go application (~800 files), "release: bump module versions for the X cut" was a third of its
   holdout with the same gold file every time, so one pattern decided the corpus.
   The change lives in the future, never in the index, so a query cannot retrieve its
   own diff. Each gold file also records the line ranges the commit modified (a fifth
   TSV column, `26-33,36-44|1-1`, base side of `git diff -U0 <parent> <sha>`); the scorer
   tolerates its absence, so older four-column corpora still score.
3. Split chronologically — older cases are the development set, newer ones the holdout.

```sh
scripts/commit-derived-cases.py ~/src/java-service --base <base-commit> --after 3800 \
    --path-prefix libs/ --path-prefix modules/ --path-prefix server/ --out cases.tsv
scripts/score-context-cases.py --ok target/release/ok --repo ./corpus-at-base \
    --cases cases-holdout.tsv --label holdout --out holdout.json
scripts/compare-commit-derived-report.py holdout.json benchmarks/commit-derived/java-a-holdout.json
```

`score-context-cases.py` drives `ok context --json`, the same builder `ok plan` and the MCP
`build_context_pack` tool use, and ranks files in the order the pack presents them. It
reports Recall@k and MRR with 95% bootstrap intervals. Frozen baselines live under
`benchmarks/commit-derived/`; `.github/workflows/commit-derived-bench.yml` re-derives four
corpora from four large public repositories nightly as a matrix — a 10k-file Java service (`libs/ modules/ server/`
indexed), a Go application (~800 files), a TypeScript standard library (~900 files), and
a Python ML library (~4k files) — and fails when a watched metric falls more
than 0.03 below its baseline. The repositories are not named here; the baseline files are keyed by
language (the Java baseline is `java-a-holdout.json`, the Go one `go-a-holdout.json`, and so on), and
the workflow reads each repository URL from a repository variable. The baselines were frozen from a hosted-runner matrix run on
2026-09-08, after generated files began to be indexed and ranked below hand-written source and a
commit scope's directory entry file became a candidate (each file records its run and commit under
`provenance`); earlier freezes are in each file's git history:

| Corpus | Split | Cases | R@5 | R@20 | MRR |
|---|---|---|---|---|---|
| Java (10k files) | dev | 262 | 0.599 | 0.759 | 0.478 |
| Java (10k files) | holdout | 113 | 0.566 | 0.699 | 0.504 |
| Go (~800 files) | dev | 196 | 0.561 | 0.765 | 0.396 |
| Go (~800 files) | holdout | 84 | 0.679 | 0.809 | 0.535 |
| TypeScript (~900 files) | dev | 385 | 0.810 | 0.946 | 0.645 |
| TypeScript (~900 files) | holdout | 166 | 0.825 | 0.874 | 0.658 |
| Python (~4k files) | dev | 462 | 0.600 | 0.725 | 0.490 |
| Python (~4k files) | holdout | 199 | 0.663 | 0.759 | 0.545 |

The Go application was the hardest of the four while a third of its holdout was one repeated release-bump
commit; with one case per repeated subject it sits between the others. 21% of its gold files
are `_test.go` benchmarks for tasks that never say "test", and its commit subjects are terse. Read the per-corpus numbers, not
an average; a change that helps Java and hurts Go is a regression on Go. The frozen baselines are what an accuracy change is judged
against; a change that helps one language and hurts another shows up as one failing matrix
entry rather than a blended average. Queries are commit subjects, so absolute numbers are not comparable with
published benchmarks that use issue text; compare a change against the frozen baseline,
not against the literature.

## Gold yield at a token budget

Recall@k and MRR say whether the right *file* is in the pack. They do not say whether the
agent can afford to read it. Across the four commit-derived holdouts (Java, 10k files; Go,
~800 files; TypeScript, ~900 files; Python, ~4k files) a gold file is 168–844 lines at the
median (381–1,368 mean) and the commit modifies 2–3 of them at the median — 0.4–1.8% of
the file, 1.8–8.4% on average — so a pack that names the right file but spends the budget
on the wrong region still costs the agent a file read. The yield metrics score the pack the
way an agent consumes it, top-down under a token budget.

For a case, `scripts/score-context-cases.py` takes the pack's selected units in
presentation order (`retrieval_diagnostics.selection.selected_units`, each with `path`,
`line_range`, and the builder's own `estimated_tokens`) and accumulates them until the next
unit would overflow the budget `B`. Everything before that point is what an agent reading
the pack sees within `B` tokens. Over that prefix:

- `gold_file_yield@B` — the fraction of the case's gold files that have at least one
  selected unit inside the budget, averaged over **every scored case**. A pack that selects
  no units delivered no gold lines, so it scores 0 rather than dropping out of the
  denominator; excluding those cases would inflate the mean and would not be comparable with
  `gold_recall@20`, which averages over every case.
- `gold_line_yield@B` — where the case carries modified line ranges, the fraction of those
  lines that the in-budget units' `line_range`s cover, averaged over the cases that have
  ranges to measure against. Whether a case has ranges is a property of the case file; a case
  that has them and selects nothing still scores 0.
- `tokens_to_first_gold` — the tokens consumed before the first gold unit appears, stored
  per case and reported as a median over the cases that reach a gold unit at all.

`B` is reported at 4,000, 8,000 (the default, matching `ContextBudget::default().max_tokens`)
and 16,000; each yield carries a 95% bootstrap interval like the other metrics. The token
cost is the builder's own estimate — the same accounting the selector uses when it enforces
a budget — so the metric judges the pack against the arithmetic that built it, not against a
tokenizer it never saw.

Two caveats travel with the numbers:

- **The line ranges are a proxy.** They are numbered on the commit's parent, and the index
  is at the base commit `B`, which may be hundreds of commits earlier; a file that moved
  between `B` and the parent shifts its ranges. The parent side is the nearest thing to `B`
  that the change is expressed against, and it is used as the best available proxy. Line
  yield is therefore an underestimate on files that drifted between the two, exact on files
  that did not.
- **A unit's `line_range` is a region of the file, widened for the top-ranked files.** The
  production `ok context` path selects the reranked prefix of up to 20 units under the
  file-limit budget, which sets `max_tokens` to a sentinel and so enforces no token ceiling
  at all; `ContextBudget::default()`, used by callers that pass a real budget, allows 6,000
  tokens (8,000 less the two 1,000-token reserves). A selected unit starts as a search hit
  whose `line_range` is a few lines around the match, and the first `region_files` files
  then have their regions widened to the enclosing symbol, their other ranked units, and
  adjacent chunks, up to `region_tokens_per_file` (see `docs/context-pack-spec.md`,
  "Selection and region widening"). Supporting files appear in the same ledger, costed at
  their listing size.

  Because the ledger now holds two kinds of entry, every report carries two yield families:
  `gold_file_yield@B` / `gold_line_yield@B` over all ledger units, and
  `gold_file_yield_primary@B` / `gold_line_yield_primary@B` over the primary units alone.
  The primary-only family is the one to compare across versions when the question is what
  retrieval put in front of the caller: a change to which files impact expansion appends
  must not be credited to region selection, or vice versa. `gold_line_yield@B` remains
  bounded by how much of the modified region the selected units cover.

  Pack sizes move with region widening, so any figure here is a record of a build, not a
  property of the tool: measure the arm under test rather than quoting these. On the
  four commit-derived holdouts, primary units that were 836-1,152 estimated tokens at the
  median before widening were 2,753-3,612 after, with the 95th percentile moving from
  1,549-5,600 to 4,322-7,629 (measured at `48e64c9`, local workstation, `--workers 2`;
  both arms' reports are in `benchmarks/commit-derived/region-widening-ab.json`). A p95 above
  6,000 is reachable only on the file-limit path; a caller that passes
  `ContextBudget::default()` is bounded by that ceiling instead. No case's yield differed
  between the 4,000 and 16,000 budgets in either arm, so on this path region granularity,
  not the budget, is still what limits how much of a change the pack shows.

`scripts/compare-commit-derived-report.py` prints the yields as informational and does not
gate on them; a baseline frozen before the metric existed compares without it.

Every report also carries the index's coverage line (`coverage_line`, from
`ok --json status` after indexing: source files discovered versus indexed, with skip
reasons), the job summary shows it next to R@5, R@20, and MRR for each corpus, and the
baseline compare prints both sides. It is informational, not a gate: a ranking number
read without knowing that a tenth of the corpus was never indexed is not a number.

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
