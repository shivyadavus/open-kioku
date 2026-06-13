# Evidence Graph v2 Alignment

## Current domain model
The graph domain currently lives in `open-kioku-core` and includes:
- `GraphNode`: `id`, `node_type`, `label`, `file_id`, `symbol_id`.
- `GraphEdge`: `id`, `from`, `to`, `edge_type`, `evidence`.
- Enums: `GraphNodeType`, `GraphEdgeType`.
- Associated Types: `Evidence`, `Confidence`, `IndexManifest`, `IndexQuality`.

## Current storage model
The storage layer lives in `open-kioku-storage-sqlite`:
- `graph_nodes` table (`id`, `label`, `json`)
- `graph_edges` table (`id`, `from_id`, `to_id`, `edge_type`, `json`) with basic indexes on `from_id` and `to_id`.
- Legacy `replace_index` and `replace_graph` transactional replacement paths.
- Typed git history tables (`git_commits`, `git_file_touches`, `git_symbol_touches`, `git_cochange_edges`, `git_review_events`).

## Current ingest pipeline
Graph ingestion is driven by `InMemoryGraph::from_index_with_analysis` in `open-kioku-graph`:
- Iterates over `files`, `symbols`, `chunks`, `occurrences`, `imports`, and `analysis_facts`.
- Maps these structurally into `GraphNode` and `GraphEdge` entries.
- Persisted using `store.replace_graph` via `open-kioku-cli`'s indexing pipeline (`ok index`).

## Current CLI/MCP surfaces
- **MCP**: `module_dependencies` tool uses `resolve_graph_node` and storage graph APIs.
- **CLI**: `resolve_graph_node` supports multiple commands. Graph data influences `RankingSignal::GraphProximity`.

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

## Backward compatibility
- Existing `open-kioku-core` types MUST be extended. **Do not introduce a parallel graph model or second SQLite storage model.**
- The `replace_graph` APIs must remain backward-compatible to avoid breaking existing indexes.
- CLI ranking and MCP tool output formats must not break.

## Feature flags
- New query-optimized columns can be rolled out iteratively under an `IndexManifest` schema version bump, allowing legacy indexes to seamlessly rebuild.
- The `GraphBuffer` determinism guarantees can replace `InMemoryGraph` iteratively without breaking user-facing flags.

## Test plan
- No major code tests added in this issue.
- Future issues (#97, #98, #99) must include focused fixture tests for deterministic graph buffering and optimized SQLite queries.
- CLI/MCP smoke tests must continuously pass throughout the migration.
