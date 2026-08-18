# Frozen relationship conformance benchmark

Open Kioku treats authoritative repository relationships as structural truth, not ranking hints. The RI3 relationship benchmark is the release gate that checks whether those relationships remain precise, reproducible, and proof-backed across the Tier-1 language surface.

## Release corpus

`benchmarks/relationship-cases.json` is the frozen V3 corpus. It contains 336 cases across Rust, TypeScript, JavaScript, Python, Java, and Go: 56 cases per language and eight cases in every language × relationship cohort for `CALLS`, `REFERENCES`, `USES_TYPE`, `IMPLEMENTS`, `EXTENDS`, `IMPORTS`, and `DEPENDS_ON`.

More than 40% of cases are negative, ambiguous, fail-closed, or `MustNotEmit` probes. The corpus includes same-name collisions, unrelated receivers, alias/import ambiguity, lexical shadowing, test/production collisions, constructor/function and static/instance collisions, unknown receivers, dynamic dispatch, overload and inheritance collisions, local/import shadowing, multiple exact reference sites, unresolved external targets, generated/vendor skipped paths, malformed/partial source, and deterministic metamorphic variants.

`benchmarks/relationship-ci-cases.json` is the compact one-case-per-cohort subset used by normal CI. It does not replace the full release corpus.

## Capability contract

Cases explicitly record one of three capability states:

- `authoritative`: the cohort may emit structural truth when the central proof policy is satisfied;
- `corroborating`: evidence can improve retrieval/diagnostics but must not become authoritative structural truth;
- `unsupported`: the language adapter does not claim the relationship capability.

The benchmark never upgrades a broad language capability merely because an easy fixture passes. Corroborating and unsupported cohorts are required to fail closed. This keeps the benchmark aligned with `open-kioku-resolution`'s versioned language capability descriptors while leaving the centralized relationship-proof policy as the authority decision point.

## Live observation path

Use `--observations @live` to execute the corpus through a real temporary repository, Full indexing, the Shadow-mode proof-gated resolver, and graph construction. Semantic retrieval and history are disabled so structural relationship conformance is isolated from heuristic ranking systems.

Exact `REFERENCES` fixtures use deterministic SCIP-equivalent symbol occurrences injected after parser symbolization. This is deliberate: the hermetic benchmark validates the exact-occurrence proof and graph-authority path without requiring an external language-specific SCIP binary or network access. Separate SCIP import/parser tests continue to validate the external artifact ingestion contract.

The live producer retains exact source ranges, proof kinds, resolver strategies, candidate cardinality, and authoritative/corroborating outcomes for scoring and diagnostics.

## Metamorphic determinism

Every language × relationship cohort has a metamorphic group. Variants are indexed independently from the same logical source state. Hardened variants add unrelated source and reverse indexed evidence vectors before graph construction, exercising order independence rather than comparing only final pass/fail verdicts.

The scorer canonicalizes the complete authoritative relationship identity, including endpoints, proof kinds, exact source ranges, and resolver strategies. Metamorphic equivalence therefore means the structural truth and its proof identity are identical, not merely that two cases both passed.

A dedicated watch/index regression also compares relationship graph output after an incremental update with a clean rebuild from the same final source state, so partial-index persistence cannot silently diverge from clean RI3 relationship truth.

## Release thresholds

`benchmarks/relationship-thresholds.json` is strict and versioned. The V3 release contract requires at least:

- 300 frozen cases and 50 cases per Tier-1 language;
- 8 cases per language × relationship cohort;
- 40% negative/ambiguous/fail-closed cases;
- 99.5% overall authoritative precision;
- 99.0% minimum precision for every authoritative production cohort;
- no more than 0.5% `MustNotEmit` false positives;
- zero false negatives for authoritative cohorts;
- 100% exact-range, proof, expected-outcome, and metamorphic-equivalence compliance;
- at least one metamorphic group in every cohort;
- required reproducibility metadata and frozen corpus status.

A cohort that cannot meet the precision contract must remain corroborating/unsupported or be fixed. The release gate must never be weakened automatically to improve recall.

## Reproduce

Compact CI gate:

```bash
cargo run -p open-kioku-cli -- --json relationship-bench \
  --corpus benchmarks/relationship-ci-cases.json \
  --observations @live \
  --write /tmp/relationship-ci-report.json
```

Full release gate:

```bash
cargo build --release -p open-kioku-cli
./target/release/ok --json relationship-bench \
  --corpus benchmarks/relationship-cases.json \
  --observations @live \
  --policy benchmarks/relationship-thresholds.json \
  --enforce-gates \
  --write artifacts/benchmarks/relationship-report.json
```

The `--write` path also emits deterministic Markdown and capability companion reports.

## Approved baseline

Open Kioku 2.x did not have a frozen relationship-conformance baseline, so V3 must not invent a historical comparison. `benchmarks/relationship-baseline.json` is the first approved relationship baseline and is created only after the full frozen corpus passes the release policy on the reviewed implementation.

`./scripts/validate-relationship-baseline.py` compares the deterministic projection of a new report against that checked-in baseline. It intentionally excludes commit-specific run metadata while retaining corpus/schema identity, the observation digest, all quality/cohort metrics, proof/strategy distributions, capability results, and metamorphic equivalence. A baseline change is therefore an explicit reviewed product decision rather than an automatic CI update.

## Failure diagnostics

Wrong-target and false-positive diagnostics include the case ID, source/target identities, candidate cardinality, proof kinds, resolver strategies, and expected outcome. The full JSON report remains the source of truth for investigation; Markdown and capability outputs are summaries for humans and release review.
