# Contributor Guide

This guide is the source of truth for adding fixtures, benchmark cases, signals,
and smoke coverage without needing private project context.

## Architecture Map

Open Kioku is a local-first Rust workspace. The main flow is:

1. `open-kioku-cli` parses CLI commands and renders reports.
2. `open-kioku-ingest` scans the repository and builds indexed facts.
3. `open-kioku-parse`, `open-kioku-tree-sitter`, `open-kioku-scip`, and
   `open-kioku-git` produce symbols, occurrences, static facts, exact refs, and
   history facts.
4. `open-kioku-storage-sqlite` persists metadata, graph rows, tests, and
   analysis facts under `.ok/index.sqlite`.
5. `open-kioku-search-tantivy` stores BM25 chunks under `.ok/search/tantivy`.
6. `open-kioku-context`, `open-kioku-impact`, `open-kioku-tests`, and
   `open-kioku-plan` turn indexed facts into context packs, impact reports,
   test recommendations, boundaries, and plans.
7. `open-kioku-patch` verifies actual changed files or diffs against saved
   plans.
8. `open-kioku-mcp` exposes the same capabilities to Claude, Cursor, Codex, and
   other MCP clients.

See `docs/architecture.md`, `docs/indexing-pipeline.md`,
`docs/context-pack-spec.md`, and `docs/graph-model.md` for deeper model docs.

## Crate Selection

Use the narrowest crate that owns the behavior:

| Change type | Start here |
| --- | --- |
| CLI flags, output, JSON reports | `open-kioku-cli` |
| Config defaults and validation | `open-kioku-config` |
| Index scanning and manifest quality | `open-kioku-ingest` |
| Language parsing or static facts | `open-kioku-parse`, `open-kioku-tree-sitter` |
| SCIP import or exact references | `open-kioku-scip` |
| Git co-change facts | `open-kioku-git` |
| SQLite persistence | `open-kioku-storage-sqlite` |
| Search ranking or signal weights | `open-kioku-ranking` |
| Context packs and boundaries | `open-kioku-context` |
| Impact analysis | `open-kioku-impact` |
| Test selection | `open-kioku-tests` |
| Plan assembly/rendering | `open-kioku-plan`, `open-kioku-format` |
| Post-edit verification | `open-kioku-patch` |
| MCP tool schema/behavior | `open-kioku-mcp` |
| Security policy gates | `open-kioku-actions`, `open-kioku-sandbox` |

Create a new crate only when the capability has a distinct dependency surface or
ownership boundary. Otherwise, prefer adding a focused module inside the owning
crate.

## Fixture Conventions

Fixture repos live under `crates/open-kioku-tests/fixtures/`.

Current fixture categories:

| Fixture | Purpose |
| --- | --- |
| `rust-fixture` | Rust indexing, symbol lookup, search, bench lifecycle |
| `typescript-fixture` | TypeScript parsing and search lifecycle |
| `python-fixture` | Python parsing and search lifecycle |
| `go-fixture` | Go parsing and search lifecycle |

When adding a fixture:

- Keep it tiny; fixture repos should index in seconds.
- Include one obvious search term and one expected path.
- Add it to `crates/open-kioku-tests/tests/integration.rs`.
- Avoid generated files, dependency folders, or network calls.
- Keep fixture assertions deterministic across macOS and Linux.

## Benchmark Authoring

Workflow benchmark cases live in `benchmarks/workflow-cases.json` and are
documented in `docs/workflow-benchmarks.md`.

Add a case when a feature changes plan -> edit -> verify behavior:

1. Pick a task prompt that a real agent would receive.
2. Add expected primary context paths.
3. Add expected impact files when the change should surface blast radius.
4. Add expected tests by stable test name when test recall matters.
5. Add expected boundary paths and forbidden paths.
6. Add `changed_files` or `unified_diff`.
7. Set `expected_verdict` to the actual verification contract:
   - `pass`: no boundary violations, warnings, missing tests, or changed impact.
   - `warn`: allowed but needs follow-up evidence, tests, or impact review.
   - `fail`: forbidden or out-of-boundary change.
8. Run:

```sh
ok --json workflow-bench . \
  --cases-file benchmarks/workflow-cases.json \
  --limit 10 \
  --min-cases 20
```

CI runs the same suite and enforces the committed case count.

## Adding A New Signal

Signals must be persisted or traceable; LLM narration is not a source of truth.

Checklist:

1. Define the source in `EvidenceSourceType` or reuse an existing source.
2. Persist facts in SQLite when the signal must survive indexing.
3. Add score components with stable `signal` names and evidence IDs.
4. Surface the signal in context, plan, impact, tests, or verify only where it
   changes behavior.
5. Add negative evidence when absence of the signal should lower confidence.
6. Add ranking/eval ablation support if it affects search ordering.
7. Cover persistence, scoring, and one CLI/MCP workflow smoke.
8. Document the signal in `docs/ranking.md` or the relevant model doc.

## Label Taxonomy

Use these issue labels consistently:

| Label | Meaning |
| --- | --- |
| `P0` | Release-blocking correctness, security, or data-loss risk |
| `P1` | Core product behavior or quality gate |
| `P2` | Adoption, docs, examples, or contributor experience |
| `bug` | Broken existing behavior |
| `enhancement` | New capability |
| `docs` | Documentation-only or documentation-led change |
| `quality` | Benchmark, eval, ranking, or confidence improvement |
| `integration` | MCP/client/plugin/runtime integration |
| `release` | Packaging, versioning, or publishing work |

## Smoke Expectations

Minimum local bundle for code changes:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Add focused smokes based on the touched surface:

| Surface | Smoke |
| --- | --- |
| CLI output | `cargo test -p open-kioku-cli --test cli_smoke` |
| Fixtures | `cargo test -p open-kioku-tests` |
| Workflow quality | `ok workflow-bench . --cases-file benchmarks/workflow-cases.json --no-index` |
| MCP clients | `examples/cursor/smoke.sh`, `examples/claude/smoke.sh` |
| Large-repo behavior | Run `ok --repo /Users/shivyadav/dev/elasticsearch ...` against the target index |
| Docs counts | `scripts/validate-docs.sh` |

## Fixture Matrix

| Behavior | Unit/fixture coverage | Workflow coverage |
| --- | --- | --- |
| Ranking | `open-kioku-ranking` tests | `benchmarks/workflow-cases.json` |
| Impact | `open-kioku-impact` tests | workflow `expected_impact` |
| Tests | `open-kioku-tests` tests and fixtures | workflow `expected_tests` |
| Verify | `open-kioku-patch` tests and CLI smoke | workflow `expected_verdict` |
| MCP | `open-kioku-mcp` tests and client smokes | Cursor/Claude examples |

