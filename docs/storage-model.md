# Storage Model

## SQLite

SQLite stores metadata:

- `manifests`
- `files`
- `symbols`
- `chunks`
- `tests`
- `imports`
- `occurrences`
- `graph_nodes`
- `graph_edges`

`replace_index` writes a complete metadata index inside one transaction for crash-safe replacement. Graph writes use a separate transactional `replace_graph` call. Each row stores query columns plus the full JSON domain object.

## Search

Lexical search is exposed behind `open-kioku-storage::SearchIndex`. `open-kioku-search-tantivy` builds a disk-backed Tantivy BM25 index under `.ok/search/tantivy` with stored chunk, file, and symbol payloads so search responses can return evidence without rereading source files. `open-kioku-search-regex` remains a deterministic fallback and regex utility.

## KV Graph

`open-kioku-storage-kv` owns the graph-adjacency extension point for a future redb/fjall optimized store. SQLite currently implements `GraphStore` directly so `ok path`, MCP `dependency_path`, and MCP `module_dependencies` work from persisted facts.

## Migrations

The schema version is recorded in `IndexManifest.schema_version`. Future migrations should be monotonic, idempotent, and run before index replacement.
