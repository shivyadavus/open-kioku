# Graph Model

Node types include files, directories, modules, packages, classes, traits, interfaces, functions, methods, fields, endpoints, tables, queues, topics, configs, tests, build targets, runtime errors, tickets, pull requests, and architecture components.

Edge types include `CONTAINS`, `DEFINES`, `REFERENCES`, `CALLS`, `IMPLEMENTS`, `EXTENDS`, `IMPORTS`, `DEPENDS_ON`, endpoint edges, config reads/writes, table reads/writes, event publish/consume, `TESTS`, ownership, change, failure, and ticket relationships.

Every edge carries evidence:

- extractor source
- source type
- file path and line range when available
- symbol id when available
- confidence
- optional numeric confidence score and reason
- optional freshness label
- indexed timestamp

Nodes and edges also support additive metadata fields. `properties` stores
structured queryable facts that are specific to the node or edge family, such as
qualified names, route names, relation kinds, package names, or resolver output.
`schema_version`, `source_pass`, `index_mode`, and `extractor_version` record
where a fact came from. `ambiguity` and `quality_notes` preserve uncertainty and
quality caveats instead of flattening inferred facts into unsupported certainty.

All of these fields are backward-compatible serde defaults. Older graph JSON
without the fields still deserializes, and newer JSON keeps the original `id`,
type, label, endpoint, and evidence fields unchanged.

The graph builder creates file-to-symbol `DEFINES` edges from extracted symbols and `REFERENCES` edges from persisted exact symbol occurrences. Heuristic reference expansion is intentionally avoided for common repeated names; richer reference coverage should come from configured SCIP indexes or future language-specific resolvers. SQLite persists `graph_nodes` and `graph_edges`, and `open-kioku-storage::GraphStore` exposes neighborhood and shortest-path traversal to CLI and MCP callers.

SQLite keeps the full graph fact JSON as the source of truth. Query columns are
maintained only for common filters and traversal:

- `graph_nodes.node_type`
- `graph_nodes.file_id`
- `graph_nodes.symbol_id`
- `graph_edges.edge_type`
- `graph_edges.from_id`
- `graph_edges.to_id`
- `graph_edges.confidence`
- `graph_edges.source_type`

Graph schema migrations are additive and idempotent. Existing databases can be
opened without reindexing; `replace_graph` backfills query columns from the JSON
domain model when graph facts are written.
