# Accuracy & performance plan — 2026-08-31

Goal: make Open Kioku a near-accurate system in the only honest sense available —
**every claim it makes is either proven or explicitly labeled, measured accuracy improves
release over release, and no measured regression can ship silently** — while indexing and
query performance stay fast enough that the accuracy is usable.

All baselines below are measured (large-corpus validation of 2026-08-31, frozen
benchmarks in `benchmarks/`).

## Measured starting point

| Layer | Metric | Today |
|---|---|---:|
| Structural truth | Relationship conformance precision/recall (336 cases) | 1.0 / 1.0, FP 0.0 |
| Structural truth | Proven CALLS coverage on 247k-symbol corpus | 215k proven / 86k ambiguous / 772k unresolved |
| Retrieval | Fusion holdout recall@10 / MRR / file-F1@10 (30 cases) | 0.94 / 0.83 / 0.44 |
| Retrieval | No-gold false-positive rate | **0.75** |
| Semantic ANN | Recall@10 at 1M vectors (current profile) | **0.24–0.39** |
| Performance | Exact lookup / search (fresh process) | 0.02s / 0.24s |
| Performance | Cold structural index (16.5k files) | 19m28s |
| Performance | Peak indexing RSS | ~7–8 GB |

## Workstreams

### A1 — Activate calibrated abstention at runtime (accuracy, highest leverage)

The no-gold FP rate of 0.75 means: when there is nothing relevant, retrieval still
returns confident-looking context three times out of four. The CC6 calibration,
metrics, and fail-closed activation-readiness machinery already exist; nothing applies
the policy at runtime.

- Move the policy and the pack-based case derivation into `open-kioku-core` (the
  benchmark derives its calibration case purely from `ContextPack` diagnostics, so the
  runtime can share the identical derivation — no drift between measured and deployed
  behavior).
- Activation artifact `.ok/abstention-policy.json`: written only by an explicit
  activation flow that passes `evaluate_abstention_activation_readiness` (fail-closed);
  carries the policy, calibration provenance, and constraints.
- The context compiler accepts an optional activated policy (builder-injected by
  CLI/MCP). After selection it derives the runtime case; on `should_abstain` it sets
  `abstention_reason = "calibrated_cc6_abstention: …"` and a first-class caveat —
  keeping the pack inspectable but never presenting it as confident.
- Missing/invalid/unready artifact ⇒ feature silently off (fail-closed), never a
  degraded guess.
- Tests: policy application end-to-end through `build_context_pack`; artifact
  tamper/invalid cases; benchmark abstention metrics remain the acceptance evidence.
- Target: measured no-gold FP with activation ≤ 0.25 on the frozen corpus without
  exceeding the calibrated positive-abstention constraint.

### A2 — Grow the frozen retrieval corpus (statistical trust)

30 cases cannot support percentage claims. Author a v2 corpus revision targeting 150+
cases (per-language ≥ 25, per-task-family ≥ 25, no-gold fraction ≥ 0.3), re-freeze
digests, publish old vs new measurements, recalibrate thresholds explicitly. Until
then, all retrieval percentages carry a small-corpus caveat.

### A3 — Raise proven-relationship recall without touching precision

772k unresolved calls on the reference corpus is the structural-recall frontier.
Sequence: (1) resolution-coverage telemetry in `ok status`/`prove` (make recall
visible and trackable), (2) SCIP auto-setup prominence for Java/TS (compiler-grade
evidence raises proven coverage with zero heuristic risk), (3) targeted Tier-2
resolver improvements measured against the conformance corpus (which blocks any
precision loss at 0.995).

### A4 — ANN honesty and scale profile (issue #328)

Immediate (this session): routing caveat when the persistent ANN backend serves a
population above the measured recall-degradation ceiling (~300K vectors), citing the
checked-in evidence — keep-uncertainty-visible applied to our own backend. Then:
measure a higher-construction profile at 300K–1M via the existing CI harness and make
the profile-vs-exact-flat routing decision from that evidence.

### P1 — RI3.6 index generations (performance + robustness)

Per `docs/ri3-index-generations-design.md`: Phase 1 (layout, adoption, atomic pointer,
status/doctor fields) then Phase 2 (build-into-staging). Kills the repeat-index
slowdown and file growth, gives kill-anywhere safety and instant rollback.

### P2 — Streaming ingestion (the memory target)

Design phase 3: stream extract→resolve→graph through the staging store. Target peak
RSS ≤ 2 GB on the reference corpus (today 7–8 GB), verified by the RSS-timeline
profile harness.

### P3 — Remaining hot phases

After P1/P2 re-measure; graph write (8m17s) remains the largest single phase. Candidate
next steps: batched edge JSON serialization, per-batch transactions sized to the page
cache. Target: cold index < 15 min on the reference corpus.

## Sequencing and verification

1. A1 + A4-immediate (this session) — accuracy first, both fail-closed.
2. P1 (next block) — biggest performance/robustness structural step.
3. A2 corpus v2 + re-baseline; then re-calibrate A1 constraints on the larger corpus.
4. A3 telemetry → SCIP → resolver; P2; P3; A4 profile decision.

Every workstream lands only with: clippy `-D warnings` all-targets, affected suites
green, snapshot gates regenerated intentionally, benchmark gates green, and a measured
number in the commit message. Nothing is committed without explicit review approval.
