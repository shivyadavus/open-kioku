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
- `analysis_facts`
- `graph_nodes`
- `graph_edges`

`replace_index` writes a complete metadata index inside one transaction for crash-safe replacement. Graph writes use a separate transactional `replace_graph` call. Each row stores query columns plus the full JSON domain object.

## Historical Evidence

`open-kioku-core` defines versioned typed records for commits, file touches,
symbol touches, co-change edges, and reviewer or owner evidence. SQLite stores
them in:

- `git_commits`
- `git_file_touches`
- `git_symbol_touches`
- `git_cochange_edges`
- `git_review_events`

`open-kioku-storage::HistoryStore` exposes:

- `put_history_snapshot`
- `history_for_file`
- `cochange_neighbors`
- `recent_commits`

`put_history_snapshot` validates and replaces the complete typed history
snapshot in one transaction. Invalid snapshots leave the previous history
untouched. Normal `replace_index` calls do not delete these tables, so file and
symbol re-indexing cannot accidentally erase historical evidence.

Existing `analysis_facts` rows with `source_type = "git_history"` remain
supported during migration. Typed history queries read the dedicated tables and
do not reconstruct commit, touch, co-change, or reviewer records from message
strings.

History summaries report truncation and missing symbol/reviewer evidence as
explicit uncertainty. Local git ingestion is not part of this storage layer; it
is supplied by the historical ingest pipeline.

## Search

Lexical search is exposed behind `open-kioku-storage::SearchIndex`. `open-kioku-search-tantivy` builds a disk-backed Tantivy BM25 index under `.ok/search/tantivy` with stored chunk, file, and symbol payloads so search responses can return evidence without rereading source files. `open-kioku-search-regex` remains a deterministic fallback and regex utility.

## KV Graph

`open-kioku-storage-kv` owns the graph-adjacency extension point for a future redb/fjall optimized store. SQLite currently implements `GraphStore` directly so `ok path`, MCP `dependency_path`, and MCP `module_dependencies` work from persisted facts.

## Migrations

SQLite migration versioning uses `PRAGMA user_version`. History schema migration
version 1 is monotonic, transactional, and idempotent; opening an existing
database creates missing history tables and indexes without deleting metadata,
graph rows, or legacy analysis facts. Databases with a newer unsupported schema
version fail explicitly.

`IndexManifest.schema_version` remains the logical index payload version and is
separate from SQLite migration state.
