# Large Repo Proof

This proof records a local validation run against `/Users/shivyadav/dev/elasticsearch`.
It is not a marketing benchmark; it is evidence that Open Kioku can index and use a
large Java/Gradle repository with local-only code intelligence.

## Environment

- Date: 2026-06-04
- Command under test: `target/release/ok`
- Repository: Elasticsearch local checkout
- SCIP mode: `auto`
- SCIP Java availability: not installed on PATH during this run

## Index Command

```sh
target/release/ok index /Users/shivyadav/dev/elasticsearch --with-scip auto
```

Result:

```text
index[complete] index ready, elapsed=467.0s
Indexed 36640 files, 495919 symbols, 509665 chunks
SCIP: mode Auto, imported 0 index(es), 0 exact references
SCIP java: Skipped - scip-java is not installed or not on PATH
```

The enriched graph write reported:

```text
writing 565677 graph nodes and 1015502 graph edges
```

## Status Snapshot

```sh
target/release/ok --repo /Users/shivyadav/dev/elasticsearch status --markdown
```

Key metrics:

| Metric | Value |
| --- | ---: |
| Files | 36640 |
| Symbols | 495919 |
| Chunks | 509665 |
| Tests | 159483 |
| Imports | 483296 |
| SCIP indexes imported | 0 |
| SCIP exact references | 0 |
| Static analysis facts | 36363 |

Local signal notes:

- build systems detected: gradle
- language static analysis facts detected: 36363

Quality notes:

- SCIP was enabled but no SCIP index was imported
- exact reference coverage is unavailable; impact and test selection are heuristic

## Setup Audit

```sh
target/release/ok setup audit /Users/shivyadav/dev/elasticsearch --markdown
```

Default quality signals:

| Status | Signal | Evidence |
| --- | --- | --- |
| pass | build | detected gradle |
| pass | tests | 159483 indexed test target(s) |
| pass | imports | 483296 indexed import edge(s) |
| pass | static | 36363 language-specific static analysis fact(s) |
| pass | validation | Gradle-scoped validation commands enabled for indexed Java test paths |

Advanced providers were not required for default readiness. No CodeQL, BSP, LSP,
coverage, or JUnit artifacts were treated as mandatory.

## Graph Evidence

SQLite graph edge counts after indexing:

```text
Defines      495919
Imports      483220
Extends       25821
Implements    10492
ReadsConfig      50
```

Evidence source distribution:

```text
static_analysis  519583
tree_sitter      495221
heuristic           698
```

This means the graph is not only symbol definitions. It also carries local static
analysis facts such as imports, inheritance, implemented interfaces, and config
reads.

## Planning Smoke

```sh
target/release/ok --repo /Users/shivyadav/dev/elasticsearch \
  plan "AssignmentPlanner allocation planning" --format toon --limit 8
```

Relevant validation output:

```text
AssignmentPlannerTests |
  ./gradlew :x-pack:plugin:ml:test --tests org.elasticsearch.xpack.ml.inference.assignment.planning.AssignmentPlannerTests |
  High |
  test-like path, annotation, or naming convention; Gradle-scoped test command; test metadata matches changed file stem; test metadata shares path token

MlAssignmentPlannerUpgradeIT |
  ./gradlew :x-pack:qa:rolling-upgrade:internalClusterTest --tests org.elasticsearch.upgrades.MlAssignmentPlannerUpgradeIT |
  High |
  test-like path, annotation, or naming convention; Gradle-scoped test command; test metadata matches changed file stem; test metadata shares path token

ZoneAwareAssignmentPlannerTests |
  ./gradlew :x-pack:plugin:ml:test --tests org.elasticsearch.xpack.ml.inference.assignment.planning.ZoneAwareAssignmentPlannerTests |
  High |
  test-like path, annotation, or naming convention; Gradle-scoped test command; test metadata matches changed file stem; test metadata shares path token
```

## Runtime Evidence Smoke

Runtime analysis is opt-in and local. A fixture with `.ok/runtime/spans.jsonl`
containing source file paths, `http.route`, `http.request.method`, and
`db.statement` produced:

```text
Static analysis facts | 4
Runtime analysis facts | 2
Graph edges: ExposesEndpoint, ReadsTable, ReadsConfig, Extends, Implements, Imports
```

Open Kioku did not install or run a runtime agent. It only consumed local runtime
artifacts supplied by the repository owner.

## Interpretation

This run shows that Open Kioku can:

- index a multi-GB Java repository locally
- persist large symbol, test, import, and graph indexes
- add language-specific static analysis facts without external providers
- keep optional providers optional
- produce scoped validation commands for large Gradle projects

Known gap from this run: SCIP Java was not installed, so exact Java references
were unavailable. Installing `scip-java` and re-indexing should improve direct
impact precision beyond the current heuristic/static-analysis layer.
