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

All of these fields are backward-compatible serde defaults on the wire: an MCP or
`--json` consumer that has not seen a field still deserializes, and the serialized shape of
a `GraphEdge` is unchanged by the 4.0 storage work.

The graph builder creates file-to-symbol `DEFINES` edges from extracted symbols and `REFERENCES` edges from persisted exact symbol occurrences. Heuristic reference expansion is intentionally avoided for common repeated names; richer reference coverage should come from configured SCIP indexes or future language-specific resolvers. SQLite persists `graph_nodes` and `graph_edges`, and `open-kioku-storage::GraphStore` exposes neighborhood and shortest-path traversal to CLI and MCP callers.

`graph_nodes` keeps the full node JSON as the source of truth, with query columns
maintained for common filters:

- `graph_nodes.node_type`
- `graph_nodes.file_id`
- `graph_nodes.symbol_id`

`graph_edges` does not. Since 4.0 an edge is stored as typed columns plus integer
references into a per-table string dictionary, because the JSON document duplicated the
columns beside it and re-stated a handful of distinct paths, pass names and messages once
per edge. Endpoints and evidence strings are dictionary references (`from_sid`, `to_sid`,
`source_sid`, `ev_path_sid`, `ev_symbol_sid`, `ev_message_sid`, `ev_indexed_at_sid`), the
filterable types stay inline (`edge_type`, `confidence`, `source_type`, `freshness`), and
only fields with no column of their own land in a residual document. See
[Storage Model](storage-model.md#compact-graph-tables) for the layout, the measurements
behind it, and how a pre-4.0 index is detected and rebuilt.

Node schema migrations remain additive and idempotent. The edge layout is not
forward-migratable: opening a pre-4.0 index discards its edge rows and reports that
`ok index` must rebuild them, rather than reading them as an empty graph.
