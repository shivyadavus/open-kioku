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
- `provenance_for_path`
- `provenance_for_symbol`
- `cochange_neighbors`
- `recent_commits`

`put_history_snapshot` validates and replaces the complete typed history
snapshot in one transaction. Invalid snapshots leave the previous history
untouched. Normal `replace_index` calls do not delete these tables, so file and
symbol re-indexing cannot accidentally erase historical evidence.

When history is enabled, index and watch flows read at most
`history.max_commits` local commits using NUL-delimited Git output. They persist
commit SHA, parents, author and committer identity, authored and committed
timestamps, summary/message, and every touched path. Rename and copy statuses
preserve the previous path. Empty repositories and shallow clones produce the
history that is locally available without requiring network access.

Existing `analysis_facts` rows with `source_type = "git_history"` remain
supported during migration. Typed history queries read the dedicated tables and
do not reconstruct commit, touch, co-change, or reviewer records from message
strings. Existing co-change analysis facts are derived from the same parsed
commit window and remain capped for ranking compatibility; large commits above
`history.max_files_per_commit` are excluded only from pairwise co-change
generation, not from commit or file-touch persistence.

History summaries report truncation and missing symbol/reviewer evidence as
explicit uncertainty. Zero-context Git patch hunks are mapped to the most
specific overlapping current symbol ranges and stored in `git_symbol_touches`.
Historical coordinate drift, equally specific overlaps, missing symbol ranges,
rename mapping, and bounded-window first-seen results remain explicit in typed
provenance confidence and uncertainty fields. Reviewer evidence is supplied by
later ownership work.

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
