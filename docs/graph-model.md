# Graph Model

Node types include files, directories, modules, packages, classes, traits, interfaces, functions, methods, fields, endpoints, tables, queues, topics, configs, tests, build targets, runtime errors, tickets, pull requests, and architecture components.

Edge types include `CONTAINS`, `DEFINES`, `REFERENCES`, `CALLS`, `IMPLEMENTS`, `EXTENDS`, `IMPORTS`, `DEPENDS_ON`, endpoint edges, config reads/writes, table reads/writes, event publish/consume, `TESTS`, ownership, change, failure, and ticket relationships.

Every edge carries evidence:

- extractor source
- source type
- file path and line range when available
- symbol id when available
- confidence
- indexed timestamp

The graph builder creates file-to-symbol `DEFINES` edges from extracted symbols and `REFERENCES` edges from persisted exact symbol occurrences. Heuristic reference expansion is intentionally avoided for common repeated names; richer reference coverage should come from configured SCIP indexes or future language-specific resolvers. SQLite persists `graph_nodes` and `graph_edges`, and `open-kioku-storage::GraphStore` exposes neighborhood and shortest-path traversal to CLI and MCP callers.
