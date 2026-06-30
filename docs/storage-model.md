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
- `history_hotspots`

`open-kioku-storage::HistoryStore` exposes:

- `put_history_snapshot`
- `history_for_file`
- `churn_for_file`
- `churn_for_module`
- `churn_for_symbol`
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
provenance confidence and uncertainty fields.

## Historical Churn And Hotspots

`put_history_snapshot` also materializes file, module, and symbol churn
summaries into `history_hotspots`. The table is keyed by entity kind and entity
key, stores query columns for hotspot ordering, and keeps the full typed
`ChurnSummary` JSON payload. Churn lookups read that cached table, so
`churn_for_file`, `churn_for_module`, `churn_for_symbol`, CLI `history churn`,
and MCP `churn_analysis` do not scan raw commit history on every request.

Each `ChurnSummary` includes:

- `all_time`, `last_30d`, and `last_90d` touch counts;
- `recency_weighted` touch count;
- `touch_count` and `hotspot_score`;
- `confidence` and explicit `uncertainty`.

Refreshes are deterministic for the same ingested history snapshot. Window
calculations use the newest persisted file or symbol touch as the reference
time, not wall-clock time. Module churn is aggregated from persisted file
touches in the directory tree. Symbol churn is keyed by stable symbol ID when
line-level history can be mapped; missing or low-confidence symbol history is
reported explicitly instead of silently fabricating a score.

Query a repository-relative file path:

```sh
ok --repo /path/to/repo history churn \
  --path crates/open-kioku-core/src/lib.rs
```

Query a module directory:

```sh
ok --repo /path/to/repo history churn --module crates/open-kioku-core/src
```

Query an indexed symbol by exact name, qualified name, or stable symbol ID:

```sh
ok --repo /path/to/repo history churn --symbol PolicyGate
```

The experimental MCP tool `churn_analysis` accepts exactly one of:

```json
{"path":"crates/open-kioku-core/src/lib.rs"}
```

```json
{"module":"crates/open-kioku-core/src"}
```

```json
{"symbol":"PolicyGate"}
```

Impact and planning reports can surface materialized file hotspot signals as
risk evidence when a history store is available. These signals are labeled as
local-history risk evidence and do not replace exact references, ranking, or
contract verification.

## Provenance Lookup

History provenance is an experimental local trust-layer surface. Run `ok index`
after enabling history so `.ok/index.sqlite` contains typed commit, file-touch,
and symbol-touch records.

Query a repository-relative path:

```sh
ok --repo /path/to/repo history provenance \
  --path crates/open-kioku-core/src/lib.rs
```

Query an indexed symbol by exact name, qualified name, or stable symbol ID:

```sh
ok --repo /path/to/repo history provenance --symbol PolicyGate
```

Use `--json` for the typed `FileProvenance` or `SymbolProvenance` payload and
`--limit <n>` to bound recent touches. Ambiguous symbol names fail with
candidate qualified names and IDs instead of selecting one silently. Overloaded
symbols can share a qualified name, so use the reported symbol ID to select one
exactly.

The experimental MCP tool `history_provenance_lookup` accepts exactly one of:

```json
{"path":"crates/open-kioku-core/src/lib.rs","limit":20}
```

```json
{"symbol":"PolicyGate","limit":20}
```

The result includes `first_seen`, `last_touched`, `recent_touches`,
`confidence`, `truncated`, and `uncertainty`.

File provenance is derived from exact structured Git file touches. Rename
aliases are followed in both directions so a current or historical path can
retrieve the same chain.

Symbol provenance maps zero-context Git patch hunks onto current indexed symbol
ranges. The mapper prefers the narrowest overlapping range so a method can be
selected instead of its enclosing class. It lowers confidence when:

- historical line coordinates may have shifted after later edits;
- a hunk overlaps multiple equally specific symbols;
- a historical path must be mapped through a rename;
- the indexed symbol has no usable line range;
- the configured history window may omit an earlier touch.

These signals never outrank exact indexed code evidence. `first_seen` means the
earliest persisted or line-mapped touch inside the configured local history
window unless the result explicitly proves an added file.

## Ownership Lookup

Ownership lookup is an experimental local trust-layer surface computed from
three sources:

- CODEOWNERS or equivalent owner config files in `.open-kioku/CODEOWNERS`,
  `.github/CODEOWNERS`, repository root `CODEOWNERS`, `docs/CODEOWNERS`, or
  `OWNERS`;
- persisted local git provenance from `provenance_for_path`;
- repo memory search results that contain owner handles or email tokens.

Query a repository-relative path:

```sh
ok --repo /path/to/repo history ownership \
  --path crates/open-kioku-core/src/lib.rs
```

The experimental MCP tool `ownership_lookup` accepts:

```json
{"path":"crates/open-kioku-core/src/lib.rs"}
```

The typed `OwnershipReport` returns ranked `OwnerSuggestion` values with:

- `OwnershipEvidence` entries for each source;
- `OwnershipConfidenceBreakdown` contributions for CODEOWNERS, git history,
  repo memory, freshness, and ambiguity;
- explicit `stale` flags on evidence and suggestions;
- component matches when architecture policy or inferred architecture mapping is
  available;
- uncertainty notes for missing, stale, ambiguous, truncated, or invalid source
  evidence.

CODEOWNERS evidence is intentionally weighted above weak memory-only evidence.
Repo memory is secondary context: memory-only owner suggestions are capped at
low confidence and include uncertainty explaining that they are uncorroborated
by CODEOWNERS or git history.

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
