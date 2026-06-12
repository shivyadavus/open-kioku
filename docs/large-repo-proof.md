# Large Repo Proof

This proof records a local validation run against a checkout of the public
[java-a repository](<withheld>/tree/<withheld>).
It is not a marketing benchmark; it is evidence that Open Kioku can index and use
a large Java/Gradle repository with local-only code intelligence.

## Environment

- Date: 2026-06-04
- Open Kioku version: 1.0.3
- Open Kioku source revision: `793d241f0c7e280a609020ac805876379e8a7a11`
- Command under test: `target/release/ok`
- Repository: `java-a`
- Repository revision: [`<withheld>`](<withheld>/tree/<withheld>)
- SCIP mode: `auto`
- SCIP Java availability: not installed on PATH during this run

## Index Command

```sh
target/release/ok index /Users/shivyadav/dev/java-a --with-scip auto
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
target/release/ok --repo /Users/shivyadav/dev/java-a status --markdown
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
target/release/ok setup audit /Users/shivyadav/dev/java-a --markdown
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
target/release/ok --repo /Users/shivyadav/dev/java-a \
  plan "CapacityAllocator allocation planning" --format toon --limit 8
```

Relevant validation output:

```text
CapacityAllocatorTests |
  ./gradlew :plugins:mesh:test --tests com.acme.mesh.routing.capacity.allocation.CapacityAllocatorTests |
  High |
  test-like path, annotation, or naming convention; Gradle-scoped test command; test metadata matches changed file stem; test metadata shares path token

MlCapacityAllocatorUpgradeIT |
  ./gradlew :plugins:qa:rolling-upgrade:internalClusterTest --tests com.acme.upgrades.MlCapacityAllocatorUpgradeIT |
  High |
  test-like path, annotation, or naming convention; Gradle-scoped test command; test metadata matches changed file stem; test metadata shares path token

ZoneAwareCapacityAllocatorTests |
  ./gradlew :plugins:mesh:test --tests com.acme.mesh.routing.capacity.allocation.ZoneAwareCapacityAllocatorTests |
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

java-a is a trademark of the corpus owner. Open Kioku is not affiliated
with or endorsed by the corpus owner.
