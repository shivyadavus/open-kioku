# Frozen relationship conformance benchmark

Open Kioku treats authoritative repository relationships as structural truth, not ranking hints. The relationship conformance benchmark is the release gate that checks whether those relationships remain precise, reproducible, and proof-backed across the Tier-1 language surface.

## Release corpus

`benchmarks/relationship-cases.json` is the frozen release corpus. It contains 336 cases across Rust, TypeScript, JavaScript, Python, Java, and Go: 56 cases per language and eight cases in every language × relationship cohort for `CALLS`, `REFERENCES`, `USES_TYPE`, `IMPLEMENTS`, `EXTENDS`, `IMPORTS`, and `DEPENDS_ON`.

More than 40% of cases are negative, ambiguous, fail-closed, or `MustNotEmit` probes. The corpus includes same-name collisions, unrelated receivers, alias/import ambiguity, lexical shadowing, test/production collisions, constructor/function and static/instance collisions, unknown receivers, dynamic dispatch, overload and inheritance collisions, local/import shadowing, multiple exact reference sites, unresolved external targets, generated/vendor skipped paths, malformed/partial source, and deterministic metamorphic variants.

`benchmarks/relationship-ci-cases.json` is the compact subset used by normal CI: one case per cohort plus targeted regression cases. The Rust `CALLS` regressions (`ci-rust-calls-02` to `-36`) write multi-file packages, and the live producer fails a case if any of its `.rs` files was not indexed. `-18` to `-25` cover inline `mod` blocks: an item or type of the file is in scope inside a `mod` block only through `use super::*`, `use super::name` or another import, an item the block declares needs neither, and a same-named item or type elsewhere in the file must not take the edge; `-25` keeps the file's own item reachable through `use super::*` beside a second glob. `-26` and `-27` check that a bare call inside a method never names an associated function of the enclosing `impl`. `-28` to `-33` cover `self::` and `super::` paths written inside inline `mod` blocks: each `super` leaves the innermost block, so `super::target_fn()` in `mod tests` names the file's own item and never the crate root's, `super::super::` climbs past the file to the crate root, a nested block reaches its enclosing block's item, and a path landing on a module that only imports the name emits no authoritative edge. `-34` to `-36` check that a path through `mod name;` continues into that module's file, from the file itself and from a block, and that `self::` in a block reaches an item its `use super::*` brings in. `ci-python-calls-02` and `-03` cover a Python import made inside a `try:` body: alone it binds the call, and with an alternative import under `except ImportError:` the call keeps both candidates and no authoritative edge; the live producer fails either case if one of its `.py` files was not indexed. The Rust `IMPORTS` regressions (`ci-rust-imports-02` to `-04`) write a package whose crate root only declares the modules and re-exports one item: a cross-module item import and a glob must reach the file declaring what they name, and an item reachable only through the crate-root `pub use` must emit no authoritative edge.
- **Must emit:** a call through a cross-module item import, and its grouped, aliased twin in the same metamorphic group; a call through `crate::` inside the importing workspace member, with a same-text module in the other member.
- **Must not emit:**
  - imports that are not in scope at the call: a `mod tests` import from production code, a file-level import inside a `mod` block that globs another module (as a bare call and as a typed call), and a file-level import shadowed by an unresolved block import;
  - paths the module tree does not support: `super::` inside an inline module, a relative import from a file mounted by `#[path]`, a stale file beside `#[path]`, a name that is both a submodule and a function, and `callee.rs` beside `callee/mod.rs` (rustc rejects that layout, but the index still has to fail closed on it);
  - `crate::` reaching another workspace member's same-text module;
  - a receiver typed from a path call whose indexed return type is not the path's type. CI asserts its exact case count, so adding a case means updating that count in `.github/workflows/ci.yml`. It does not replace the full release corpus.

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

A dedicated watch/index regression also compares relationship graph output after an incremental update with a clean rebuild from the same final source state, so partial-index persistence cannot silently diverge from the relationship truth of a clean rebuild.

## Release thresholds

`benchmarks/relationship-thresholds.json` is strict and versioned. The release contract requires at least:

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

Open Kioku 2.x did not have a frozen relationship-conformance baseline, so the 3.x benchmark must not invent a historical comparison. `benchmarks/relationship-baseline.json` is the first approved relationship baseline and is created only after the full frozen corpus passes the release policy on the reviewed implementation.

`./scripts/validate-relationship-baseline.py` compares the deterministic projection of a new report against that checked-in baseline. It intentionally excludes commit-specific run metadata while retaining corpus/schema identity, the observation digest, all quality/cohort metrics, proof/strategy distributions, capability results, and metamorphic equivalence. A baseline change is therefore an explicit reviewed product decision rather than an automatic CI update.

## Failure diagnostics

Wrong-target and false-positive diagnostics include the case ID, source/target identities, candidate cardinality, proof kinds, resolver strategies, and expected outcome. The full JSON report remains the source of truth for investigation; Markdown and capability outputs are summaries for humans and release review.
