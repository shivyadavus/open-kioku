# Open Kioku Execution Plan

Repository: `shivyadavus/open-kioku`  
Primary parent epic: `#94`  
Planning goal: sequence all current Open Kioku trust-layer, evidence-graph, history, architecture, and contract issues so the team can work fast without creating duplicate or low-quality implementations.

---

## Executive Summary

Open Kioku should be implemented in this order:

```text
Phase 0: Cleanup completed duplicate issues
Phase 1: Evidence graph foundation
Phase 2: Resolution, indexing quality, and freshness
Phase 3: Architecture policy core
Phase 4: Contract verification core
Phase 5: History intelligence
Phase 6: Evidence expansion: snapshot, cross-project, runtime, tests, risk
Phase 7: Final trust-layer integration and product polish
```

The most important sequencing rule:

```text
Do not start deep architecture/contract/history integration work until the evidence graph foundation is stable.
```

Issues that should wait for graph/schema/identity/resolution foundations:

```text
#68, #69, #74, #79, #80, #114
```

These depend heavily on evidence quality, stable identity, graph storage, graph query, symbol resolution, and confidence/caveat semantics.

---

## Core Principles for All Work

### 1. Extend existing Open Kioku architecture

Do not introduce duplicate systems.

Preserve and extend:

```text
open-kioku-core
open-kioku-graph
open-kioku-storage
open-kioku-storage-sqlite
open-kioku-ingest
open-kioku-search-tantivy
open-kioku-ranking
open-kioku-impact
open-kioku-tests
open-kioku-plan
open-kioku-contract
open-kioku-mcp
open-kioku-cli
open-kioku-watch
open-kioku-architecture
```

### 2. Avoid parallel graph/storage models

Do not create a second graph model or a second SQLite model unless a specific issue explicitly justifies it.

Use:

```text
GraphNode
GraphEdge
GraphNodeType
GraphEdgeType
Evidence
Confidence
IndexManifest
IndexQuality
graph_nodes
graph_edges
analysis_facts
```

### 3. Evidence must be honest

Every plan, contract, verification result, and report should surface:

```text
confidence
freshness
ambiguity
missing evidence
stale evidence
fast-mode caveats
unsupported languages
failed optional passes
skipped paths
runtime/test/history gaps
```

### 4. Default safety posture

Keep the default product posture:

```text
local-first
read-only by default
no hosted source upload
no required embedding API
source-tree writes only when explicitly enabled
agent hooks advisory by default
network denial preserved in MCP security posture
```

---

# Phase 0 — Cleanup Duplicate Issues

## Status

Completed.

The following old duplicate issues are already closed and should not be assigned:

```text
#54 duplicate of #60
#55 duplicate of #61
#57 duplicate of #62
#58 duplicate of #63
#59 duplicate of #65
```

## Assignment instruction

Do not assign Phase 0 issues.

Use the completed detailed replacements as the source of truth:

```text
#60 C1: Create open-kioku-contract and define Change Contract v1
#61 A1: Define architecture policy schema and canonical config loading
#62 H1: Add persistent history schema for commits, touches, co-change, and review evidence
#63 H2: Ingest commit metadata and file-touch history from local git
#64 A2: Resolve files and symbols to policy components and annotate the graph
#65 C2: Build contracts from indexed repo evidence and existing planning output
#66 C3: Persist contracts and verification records under .ok/contracts
#67 H3: Map commit history to files and symbols for provenance lookup
```

## Exit criteria

- Duplicate issues are closed.
- Team knows not to work from #54, #55, #57, #58, #59.
- #60–#67 are treated as completed foundation work.

---

# Phase 1 — Evidence Graph Foundation

## Goal

Establish the durable, queryable, schema-aware graph foundation that all later policy, history, contract, runtime, and verification work can trust.

## Issues

```text
#95  E1  Audit and align evidence graph v2 with current Open Kioku graph/storage/types
#96  E2  Add versioned evidence graph schema manifest over existing graph types
#97  E3  Extend GraphNode, GraphEdge, and Evidence with queryable properties and provenance metadata
#98  E4  Add high-throughput GraphBuffer for deterministic graph construction
#99  E5  Add query-optimized SQLite graph columns, indexes, and GraphStore APIs
#100 E6  Add safe read-only graph query DSL and MCP tool
#101 E7  Extend Tantivy/search/ranking with graph-node identifier search
#102 E8  Centralize stable identity and qualified-name rules
```

## Recommended order inside Phase 1

```text
1. #95
2. #96
3. #97
4. #102
5. #98
6. #99
7. #100
8. #101
```

Slight adjustment from numeric order: do **#102 before #98/#99** if the implementer needs stable identity decisions before graph buffering or persistence changes.

## Why this phase comes first

The old backlog issues around architecture policy, contract verification, history ranking, and evidence quality all depend on stable graph facts. Starting those first risks implementing shallow heuristics that later need to be rewritten.

## Parallelization

Safe parallel work:

```text
Lane A: #95 -> #96 -> #97
Lane B: #102 after #95
Lane C: #98 after #97 and #102
Lane D: #99 after #97 and #102
Lane E: #100 after #96 and #99
Lane F: #101 after #96 and #99
```

## Quality gates

Phase 1 is done only when:

- Existing `GraphNode`, `GraphEdge`, `GraphNodeType`, `GraphEdgeType`, and `Evidence` are extended, not replaced.
- Schema manifest exists and is test-covered.
- Stable identity rules are documented and fixture-tested.
- GraphBuffer is deterministic and does not create duplicate nodes/edges.
- SQLite graph storage can query important fields without scanning JSON blobs.
- Read-only graph query cannot mutate data.
- Graph-node search works through existing search/ranking infrastructure.
- Existing CLI/MCP tools still pass smoke tests.

## PR rules

Every PR in Phase 1 should include:

```text
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
targeted fixture tests for changed graph/storage behavior
```

---

# Phase 2 — Resolution, Indexing Quality, and Freshness

## Goal

Make indexed evidence more precise, observable, mode-aware, and maintainable for large repos.

## Issues

```text
#103 E9   Add manifest, workspace, and path-alias resolver
#104 E10  Add registry-based symbol, call, and type resolution
#105 E11  Add explicit indexing modes, phase reports, and confidence caveats
#106 E12  Harden discovery, ignore policy, and skipped-path reporting
#107 E13  Implement incremental indexing and git-diff range impact
```

## Recommended order

```text
1. #103
2. #104
3. #105
4. #106
5. #107
```

## Dependencies

```text
#103 depends on Phase 1 identity/schema decisions.
#104 depends on #103.
#105 depends on GraphBuffer and storage/query foundations.
#106 depends on #105 so skipped-path reporting can align with modes.
#107 depends on #102, #105, and #106.
```

## Parallelization

After #103 starts, possible lanes:

```text
Lane A: #103 -> #104
Lane B: #105 -> #106
Lane C: #107 after #102, #105, #106
```

## Quality gates

Phase 2 is done only when:

- Imports resolve with confidence and ambiguity metadata.
- Path aliases and workspace/module ownership are represented in graph facts.
- Symbol/call/type resolution never reports high confidence for ambiguous matches.
- Index modes are explicit: full, balanced, fast, cross-project.
- Phase reports include timings, counts, warnings, skipped paths, and caveats.
- Discovery reports skipped paths without leaking sensitive paths.
- Incremental indexing handles modified, added, deleted, renamed, and mode-skipped files.
- Changed-range impact has deterministic tests.

## Assignment warning

Do not let implementers add broad fuzzy reference expansion without caps. That would reduce trust. Ambiguity must be represented as caveats.

---

# Phase 3 — Architecture Policy Core

## Goal

Turn architecture policy from configuration/discovery into reliable graph-backed enforcement and user-facing policy UX.

## Issues

```text
#68 A3  Evaluate allowed and forbidden dependency edges against policy
#72 A4  Add public API boundary rules and explicit exemptions
#78 A5  Ship policy-specific CLI and MCP UX
#82 A7  Build an architecture-violation corpus and benchmark harness
```

## Recommended order

```text
1. #68
2. #72
3. #78
4. #82
```

## Dependencies

Already completed foundation:

```text
#61 A1 completed
#64 A2 completed
```

Should also wait for:

```text
#99 query-optimized graph APIs
#100 graph query DSL
#103 manifest/path resolver
#104 symbol/call/type resolution
```

## Why this waits until Phase 3

Architecture policy quality depends on accurate graph edges. If #68 is implemented before resolution and query foundations, it may become a path-only heuristic instead of a trustworthy policy engine.

## Quality gates

Phase 3 is done only when:

- Allowed and forbidden edges are evaluated against resolved graph evidence.
- Public API rules are explicit and support exemptions.
- Policy violations include rule identity, source, target, severity, and evidence.
- Unmapped inputs are explained, not ignored.
- CLI/MCP UX can validate, explain, and report policy decisions.
- Benchmark corpus includes true positives, false positives, exemptions, and unmapped cases.

## PR rule

Every architecture-policy PR must include fixture repos with both valid and invalid dependency patterns.

---

# Phase 4 — Contract Verification Core

## Goal

Make `ChangeContractV1` the authoritative artifact for planning and verification while preserving backward compatibility with current plan-based workflows.

## Issues

```text
#73 C6  Add first-class evidence traceability and boundary-expansion proof rules
#69 C5  Refactor verification around contracts with backward-compatible plan adapter
#74 C7  Detect API-surface and dependency-graph deltas during verification
#75 C8  Add validation execution ledger and attestation records
#81 C4  Add user-facing CLI and MCP contract commands
#84 C9  Extend benchmark corpus for contract generation and verification
```

## Recommended order

```text
1. #73
2. #69
3. #74
4. #75
5. #81
6. #84
```

## Why #73 before #69

Although #69 has an older implementation order, #73 should come first because evidence traceability prevents contract verification from becoming shallow. Verification should not simply say pass/fail; it should explain what evidence justified the boundary, risk, required tests, and allowed changes.

## Dependencies

Already completed:

```text
#60 C1 completed
#65 C2 completed
#66 C3 completed
```

Should wait for:

```text
#96 schema manifest
#97 queryable graph/evidence metadata
#99 graph storage APIs
#100 graph query
#102 stable identity
```

Should ideally benefit from:

```text
#103 path/module resolver
#104 symbol/call/type resolution
#68 architecture dependency policy
#72 public API boundary rules
```

## Quality gates

Phase 4 is done only when:

- Contract verification is the primary verification path.
- Plan-based verification remains backward-compatible through an adapter.
- Contracts include explicit evidence traceability.
- Boundary expansions require proof.
- API-surface and graph-delta checks are deterministic.
- Required validations can be attested through a durable ledger.
- User-facing CLI/MCP commands can create, show, explain, verify, and export contracts.
- Contract benchmark corpus covers happy paths, violations, missing evidence, stale evidence, and broad boundary expansion.

## Assignment warning

Do not let #81 user-facing commands land before the contract verifier path is stable. Otherwise the UX will freeze immature semantics.

---

# Phase 5 — History Intelligence

## Goal

Use local git history, ownership, churn, hotspots, reviewer signals, and similar-change retrieval as trust-layer evidence.

## Issues

```text
#70 H4  Compute churn and hotspot summaries
#71 H5  Build ownership intelligence from CODEOWNERS, git history, and repo memory
#76 H6  Add reviewer suggestion engine with explicit fallback semantics
#77 H7  Implement similar historical changes retrieval
#80 H8  Feed history signals into ranking, impact, tests, plans, and contracts
#83 H9  Expose history APIs through CLI/MCP and add benchmark corpus
```

## Recommended order

```text
1. #70
2. #71
3. #76
4. #77
5. #80
6. #83
```

## Dependencies

Already completed:

```text
#62 H1 completed
#63 H2 completed
#67 H3 completed
```

Should wait for or align with:

```text
#102 stable identity
#105 indexing modes and caveats
#107 incremental indexing
```

## Parallelization

Safe parallel lanes after #70/#71 foundations:

```text
Lane A: #70 -> #80
Lane B: #71 -> #76
Lane C: #77 after #70 and #102
Lane D: #83 after #76, #77, #80
```

## Quality gates

Phase 5 is done only when:

- Churn/hotspot summaries are persisted or cached efficiently.
- Ownership intelligence has explicit source breakdown.
- Reviewer suggestions are clear about local-history limits.
- Similar historical changes are ranked with transparent evidence.
- History signals feed ranking, impact, tests, plans, and contracts.
- Public CLI/MCP history APIs include confidence, truncation, and caveats.
- Benchmark corpus prevents regressions.

## Assignment warning

Reviewer suggestions must not imply real PR-review evidence unless that evidence exists. Local author/touch history should be labeled accordingly.

---

# Phase 6 — Evidence Expansion

## Goal

Expand evidence coverage beyond static code into snapshots, cross-project linking, runtime behavior, validation evidence, complexity, hot paths, similarity, and semantic relationships.

## Issues

```text
#108 E14  Add compressed index snapshot export/import
#109 E15  Promote route, channel, config, and resource evidence to first-class graph facts
#110 E16  Add cross-project graph linking mode
#111 E17  Strengthen runtime evidence ingestion and aggregation
#112 E18  Strengthen test, coverage, and history evidence for validation selection
#113 E19  Add complexity, hot-path, similarity, and semantic relationship passes
```

## Recommended order

```text
1. #108
2. #109
3. #110
4. #111
5. #112
6. #113
```

## Dependencies

```text
#108 depends on schema/storage stability: #96, #99, #105
#109 depends on graph types/properties: #97, #102, #103, #104
#110 depends on snapshots and first-class route/channel evidence: #108, #109
#111 depends on route/config evidence: #109
#112 depends on resolution/history/runtime foundations: #104, #107, #111
#113 depends on resolution/ranking/evidence-quality foundations: #104, #101, #114 if available
```

## Parallelization

Possible lanes:

```text
Lane A: #108
Lane B: #109 -> #110
Lane C: #111 -> #112
Lane D: #113 after #101/#104
```

## Quality gates

Phase 6 is done only when:

- Snapshots are schema-checked, integrity-checked, atomic, and source-safe.
- Runtime artifacts are local-only and aggregated with confidence.
- Routes/channels/config/resources are first-class graph facts.
- Cross-project linking does not reparse source unnecessarily.
- Test selection uses static graph, coverage, history, runtime, and confidence signals.
- Complexity/hot-path results are explainable.
- Semantic evidence never outranks exact symbol/reference/test/history/runtime evidence.

## Assignment warning

Do not bundle `.ok/memory.sqlite` or `.ok/context.sqlite` into index snapshots by default. Snapshots should be safe for team/CI index sharing, not personal memory/context sharing.

---

# Phase 7 — Final Trust-Layer Integration and Product Polish

## Goal

Turn all evidence into a coherent product workflow: plan, constrain, verify, explain, prove, benchmark, and safely guide agents.

## Issues

```text
#79  A6   Integrate architecture policy into planning, impact, contracts, and verification
#114 E20  Enforce evidence quality gates in plan, contract, and verify
#115 E21  Harden MCP protocol, pagination, and large-result behavior
#116 E22  Add advisory/warn/enforce agent hooks and doctor checks
#117 E23  Upgrade architecture, ADR, UI, and proof reports around the trust workflow
#118 E24  Add CI, security, benchmark, and release-trust gates
```

## Recommended order

```text
1. #115
2. #79
3. #114
4. #116
5. #117
6. #118
```

## Why #115 first

MCP protocol hardening and large-result behavior should land before new product-facing tools generate bigger outputs. This reduces risk for agents using the new graph/query/plan surfaces.

## Why #114 after #79

Evidence quality gates should enforce the combined behavior of graph evidence, architecture policy, history, runtime, tests, and contracts. It is strongest after architecture policy is integrated into planning/impact/contracts/verification.

## Dependencies

#79 should wait for:

```text
#68
#72
#73
#69
#96
#99
#100
#103
#104
```

#114 should wait for:

```text
#79
#111
#112
#113
#73
#69
```

#116 should wait for:

```text
#105
#107
#114
#115
```

#117 should wait for:

```text
#79
#114
#115
```

#118 should be last or near-last.

## Quality gates

Phase 7 is done only when:

- Architecture policy actively informs planning, impact, contracts, and verification.
- Plans and contracts cannot hide stale/missing/ambiguous evidence.
- MCP handles malformed JSON, string IDs, pagination, caps, and large results safely.
- Agent hooks are advisory by default, warn/enforce only by explicit policy.
- UI/reports show the full trust workflow:
  - task
  - context
  - affected files
  - affected symbols
  - evidence
  - tests
  - runtime/history
  - boundaries
  - contract
  - verification
- CI/security/release gates back up the product’s trust-layer claims.

---

# Recommended Team Assignment Model

## Team 1 — Graph and Storage Foundation

Primary issues:

```text
#95
#96
#97
#98
#99
#100
#102
```

Secondary:

```text
#108
#110
```

Skills:

```text
Rust core
SQLite
schema/migrations
graph algorithms
MCP JSON output discipline
```

## Team 2 — Search, Resolution, and Indexing

Primary issues:

```text
#101
#103
#104
#105
#106
#107
```

Secondary:

```text
#113
```

Skills:

```text
Tantivy
ranking
tree-sitter
import resolution
large-repo indexing
incremental processing
```

## Team 3 — Architecture and Contracts

Primary issues:

```text
#68
#72
#73
#69
#74
#75
#78
#79
#81
#82
#84
```

Skills:

```text
policy engines
contract design
verification
CLI/MCP UX
benchmark fixtures
```

## Team 4 — History, Runtime, Tests, and Risk

Primary issues:

```text
#70
#71
#76
#77
#80
#83
#109
#111
#112
#113
```

Skills:

```text
git history
coverage/JUnit
runtime trace/log artifacts
risk scoring
test selection
ranking integration
```

## Team 5 — Product Hardening and Release Trust

Primary issues:

```text
#115
#116
#117
#118
```

Skills:

```text
MCP protocol
agent install UX
doctor checks
CI/security
benchmarks
release engineering
documentation
```

---

# Suggested Batch Plan

## Batch 1 — Start now

```text
#95
#96
#97
#102
```

Why: these define the graph/schema/identity decisions that prevent bad downstream work.

## Batch 2 — After Batch 1 design is stable

```text
#98
#99
#100
#101
```

Why: storage, GraphBuffer, query, and search can now align with the decided schema.

## Batch 3 — Resolution and indexing

```text
#103
#104
#105
#106
#107
```

Why: these raise evidence precision and freshness.

## Batch 4 — Parallel domain streams

Architecture:

```text
#68
#72
#78
#82
```

Contracts:

```text
#73
#69
#74
#75
#81
#84
```

History:

```text
#70
#71
#76
#77
#80
#83
```

Evidence expansion:

```text
#108
#109
#110
#111
#112
#113
```

## Batch 5 — Final integration

```text
#115
#79
#114
#116
#117
#118
```

---

# Issues Not to Start Too Early

Do not start these until foundations are ready:

```text
#68
#69
#74
#79
#80
#114
```

Why:

```text
#68 needs accurate dependency edges.
#69 needs contract traceability and graph evidence stability.
#74 needs API/graph delta baselines.
#79 needs policy evaluation and contract integration.
#80 needs mature history signals and ranking surfaces.
#114 needs all evidence families to be available before enforcing gates.
```

---

# Agent Assignment Prompt

Use this prompt when assigning any issue to a coding agent:

```text
Implement issue #<number> in shivyadavus/open-kioku.

Use only the Open Kioku repository as the source of truth.

Before coding:
1. Read the issue body.
2. Identify existing crates, types, storage tables, commands, and tests that must be preserved.
3. Search the repo for existing implementations before adding new abstractions.
4. Prefer extending existing types/modules over creating parallel systems.
5. Write a concise implementation plan.
6. Implement in small commits.
7. Add focused unit/integration/fixture tests.
8. Run cargo fmt, clippy, and relevant tests.
9. Update docs only where user-facing behavior changes.

Hard constraints:
- Do not introduce a duplicate graph model.
- Do not introduce a duplicate SQLite storage model.
- Do not weaken local-first/read-only-by-default security.
- Do not let semantic or heuristic evidence outrank exact evidence.
- Do not hide stale, ambiguous, missing, or low-confidence evidence.
- Do not break existing CLI/MCP compatibility.
```

---

# PR Review Checklist

Every PR should answer:

```text
1. Which issue does this close?
2. Which existing Open Kioku types/crates were preserved?
3. Did this add or avoid any new abstraction?
4. Are migrations backward-compatible?
5. Are outputs deterministic?
6. Are results capped/paginated where needed?
7. Are confidence/caveats surfaced?
8. Does semantic/heuristic evidence remain lower priority than exact evidence?
9. Are secret paths and local-only constraints preserved?
10. What tests were added?
11. What commands were run?
```

Minimum command set:

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

For focused changes, also run relevant package tests:

```sh
cargo test -p open-kioku-core
cargo test -p open-kioku-graph
cargo test -p open-kioku-storage-sqlite
cargo test -p open-kioku-ingest
cargo test -p open-kioku-mcp
cargo test -p open-kioku-cli --test cli_smoke
```

---

# Final Target State

Open Kioku reaches the intended trust-layer shape when:

```text
local code + graph + history + runtime + tests + architecture policy
  -> evidence-backed plan_change
  -> durable ChangeContract
  -> constrained edit
  -> verify_change
  -> proof report
```

The final product should make agents safer because they can:

```text
understand before editing
plan with evidence
respect boundaries
select validations
verify after changes
surface uncertainty honestly
```

That is the core product differentiator.
