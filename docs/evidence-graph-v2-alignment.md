# Evidence Graph v2 Alignment

## Current domain model
The graph domain currently lives in `open-kioku-core` and includes:
- `GraphNode`: `id`, `node_type`, `label`, `file_id`, `symbol_id`.
- `GraphEdge`: `id`, `from`, `to`, `edge_type`, `evidence`.
- Enums: `GraphNodeType`, `GraphEdgeType`.
- Associated Types: `Evidence`, `Confidence`, `IndexManifest`, `IndexQuality`.

## Current storage model
The storage layer lives in `open-kioku-storage-sqlite`:
- `manifests`
- `files`
- `symbols`
- `chunks`
- `tests`
- `imports`
- `occurrences`
- `analysis_facts`
- `graph_nodes` table (`id`, `label`, `json`)
- `graph_edges` table (`id`, `from_id`, `to_id`, `edge_type`, `json`) with basic indexes on `from_id` and `to_id`.
- Typed git history tables (`git_commits`, `git_file_touches`, `git_symbol_touches`, `git_cochange_edges`, `git_review_events`).
- Semantic/vector tables where relevant.
- Legacy `replace_index` and `replace_graph` transactional replacement paths.

## Current ingest pipeline
Graph ingestion is driven by `InMemoryGraph::from_index_with_analysis` in `open-kioku-graph`:
- Iterates over `files`, `symbols`, `chunks`, `occurrences`, `imports`, and `analysis_facts`.
- Maps these structurally into `GraphNode` and `GraphEdge` entries.
- Persisted using `store.replace_graph` via `open-kioku-cli`'s indexing pipeline (`ok index`).

## Current CLI/MCP surfaces
- **CLI**: `ok path`, `ok impact`, `ok tests`, `ok plan`, `ok architecture`, `ok prove`
- **MCP**: `dependency_path`, `module_dependencies`, `impact_analysis`, `find_tests_for_change`, `plan_change`, `architecture_*`
- **Ranking**: `RankingSignal::GraphProximity`

## Gaps
1. **Schema Definition**: No schema manifest to explicitly version the structural evidence graph.
2. **Metadata**: `GraphNode` and `GraphEdge` lack directly queryable properties/provenance, currently buried in `Evidence` JSON blobs.
3. **Buffering**: `InMemoryGraph` may hold too much in memory or lack determinism guarantees for huge graphs.
4. **Storage Performance**: Queries need optimized columns for fast traversals without repeatedly parsing JSON.
5. **Traversal**: Read-only graph query DSL is missing for agent tools.

## Migration plan
We will *extend* the existing types, not replace them.
1. **Issue #96 (E2)**: Add a versioned evidence graph schema manifest without removing existing properties.
2. **Issue #97 (E3)**: Extend `GraphNode`, `GraphEdge`, and `Evidence` with queryable attributes (e.g. `provenance`).
3. **Issue #102 (E8)**: Standardize stable identity and qualified-name rules for nodes.
4. **Issue #98 (E4)**: Introduce `GraphBuffer` in `open-kioku-graph` to build deterministically before flush.
5. **Issue #99 (E5)**: Add optimized SQLite columns and indices (e.g. `provenance_metadata`) to `graph_nodes` and `graph_edges`.
6. **Issue #100 (E6)**: Build the safe, read-only graph query DSL over the optimized SQLite tables.
7. **Issue #101 (E7)**: Extend Tantivy/search/ranking with graph-node identifier search.

## Consistency checklist

| Issue | Existing crates to modify | Existing tables/types to preserve | Migration note |
|---|---|---|---|
| #96 | open-kioku-core, open-kioku-graph, open-kioku-cli, open-kioku-mcp | GraphNodeType, GraphEdgeType, Evidence, IndexManifest | Add schema manifest, no model replacement |
| #97 | open-kioku-core, open-kioku-storage-sqlite | GraphNode, GraphEdge, Evidence, graph_nodes, graph_edges | Add optional metadata fields additively |
| #98 | open-kioku-graph, open-kioku-ingest | InMemoryGraph, IndexSnapshot, replace_graph | Introduce GraphBuffer behind existing graph flow |
| #99 | open-kioku-storage, open-kioku-storage-sqlite | graph_nodes, graph_edges | Add columns/indexes with migration fallback |
| #100 | open-kioku-graph, open-kioku-storage, open-kioku-mcp, open-kioku-cli | GraphStore APIs | Read-only DSL only |
| #101 | open-kioku-search-tantivy, open-kioku-ranking | SearchResult, score_breakdown, evidence_refs | Extend ranking/search, do not create separate search engine |
| #102 | open-kioku-core, open-kioku-graph | GraphNode.id, NodeId | Standardize identity rules, keep string IDs |

## Backward compatibility
- Existing `open-kioku-core` types MUST be extended. **Do not introduce a parallel graph model or second SQLite storage model.**
- The `replace_graph` APIs must remain backward-compatible to avoid breaking existing indexes.
- CLI ranking and MCP tool output formats must not break.

## Feature flags
- New query-optimized columns can be rolled out iteratively under an `IndexManifest` schema version bump, allowing legacy indexes to seamlessly rebuild.
- The `GraphBuffer` determinism guarantees can replace `InMemoryGraph` iteratively without breaking user-facing flags.

## Risks and rollback plan

### Risks
- Schema migration could make existing `.ok/index.sqlite` unreadable.
- Query-optimized columns could drift from JSON payloads.
- GraphBuffer could change node/edge ordering and affect ranking or MCP outputs.
- Identity changes could invalidate stored graph edges or history provenance.
- Graph query DSL could expose overly broad/costly traversals.

### Rollback
- Keep existing JSON payloads authoritative during migration.
- Gate new columns behind `IndexManifest.schema_version`.
- Rebuild graph/search indexes when schema mismatch is detected.
- Keep `replace_graph` backward-compatible.
- Preserve old MCP output shapes until new fields are additive and tested.

## Test plan
- No major code tests added in this issue.
- Future implementation PRs must include migration/backward-compatibility tests before closing #96–#102.
- Future issues (#97, #98, #99) must include focused fixture tests for deterministic graph buffering and optimized SQLite queries.
- CLI/MCP smoke tests must continuously pass throughout the migration.
