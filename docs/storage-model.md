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

`replace_index` writes a complete metadata index inside one transaction for crash-safe replacement. Graph writes use a separate transactional `replace_graph` call. Most tables store query columns plus the full JSON domain object; `graph_edges` and `call_sites` do not — see [Compact graph tables](#compact-graph-tables).

## Compact graph tables

`graph_edges` and `call_sites` are the two largest tables in a real index, and until
4.0 each of their rows carried a self-contained JSON document *beside* the query columns
holding the same values. Measured on a 1,751-file Java corpus, the edge document averaged
1,288 bytes: 22% of it re-stated `id`, `from`, `to` and `edge_type`, another 30% was a
`properties` map whose four commonest keys have three to eight distinct values across the
whole corpus, and 48% was the edge's own `Evidence`.

4.0 replaces that with two mechanisms.

**Typed columns.** Every scalar field of a `GraphEdge` and a `CallSite` has its own
column. `graph_edges` keeps `id`, `edge_type`, `confidence`, `source_type` and `freshness`
as before, and adds `ev_id`, `ev_line_start`, `ev_line_end`. Only fields with no column of
their own — `properties`, `ambiguity`, `quality_notes`, `schema_version`, `source_pass`,
`index_mode`, `extractor_version` and the three optional `Evidence` tail fields — are
serialized, into a residual document that is `NULL` for an edge that has none of them.

**A string dictionary per table.** `graph_strings` and `call_site_strings` hold each
distinct string once as `(sid, vhash, value)`; the row columns reference it by integer id.
That is where the redundancy actually lives: on the measured corpus 153,856 edges cite
2,388 distinct evidence paths, 156,515 edges carry 66,663 distinct messages, and a single
index run stamps one distinct `indexed_at`. The `vhash` column is an FNV-1a of the value,
so value lookups are an integer index probe plus an exact comparison rather than an index
over full strings.

Each dictionary is owned by the table that references it and is emptied with it —
`replace_graph` clears `graph_strings`, `replace_index` clears `call_site_strings` — so a
re-index cannot accumulate entries nothing points at. The incremental writers add to a
dictionary rather than clearing it, which can leave unreferenced entries between full
re-indexes; the next full index run removes them.

Object-level deduplication of evidence was measured and rejected. Evidence objects are 1.0x
distinct per edge — their `id` is a content hash and `indexed_at` is stamped per run — so a
normalized evidence table keyed by evidence id would have added a join and a second
64-character index for no saving. The redundancy is at field level, which is what the
dictionary captures.

`call_sites` rows are written by indexing and are not read back by any product path today;
resolution consumes call sites from the in-memory snapshot. The columns are still the
complete record, so nothing was dropped in the move.

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
- `similar_changes`
- `history_score_components`
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

Impact, ranking, test selection, planning, and contract generation can request
`history_score_components` when a history store is available. The API returns
bounded `ScoreComponent` entries named `history_churn`, `ownership_risk`,
`similar_change_overlap`, and `reviewer_affinity`, plus evidence refs and
explicit uncertainty. These local-history heuristics are advisory: they do not
replace exact references, exact symbol/file evidence, direct test coverage,
architecture policy, or contract verification.

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

## Similar Historical Changes

Similar-change retrieval is an experimental local trust-layer surface over the
persisted history tables. It does not call Git during lookup. The query can
combine task text, repository-relative paths, and symbol names or IDs:

```sh
ok --repo /path/to/repo history similar \
  --task "fix token expiration" \
  --path src/auth.rs \
  --symbol validate_token
```

The experimental MCP tool `history_similar_changes` accepts the same signals:

```json
{"task":"fix token expiration","path":"src/auth.rs","symbol":"validate_token","limit":5}
```

Results are ranked deterministically by combined evidence rather than path-only
matching. Each `SimilarChangeHit` includes:

- the historical commit summary and touched paths/symbols;
- a bounded score and confidence;
- `SimilarityEvidence` entries for task text, path, symbol, churn, co-change,
  and commit metadata matches;
- explicit uncertainty when the result is low-confidence or only weakly
  grounded.

Weak historical similarity is advisory. It does not outrank exact indexed code,
symbol, test, architecture, or verification evidence. Low-confidence hits are
kept visible so agents can inspect them, but they are marked with explicit
uncertainty before any downstream H8 ranking integration.

The deterministic benchmark corpus for this retrieval path lives at
`benchmarks/similar-history-cases.json` and can be checked with:

```sh
ok --repo /path/to/repo history similar-bench --min-recall-at-5 0.75
```

The public history API regression corpus lives at `benchmarks/history-cases.json`
and exercises similar changes, ownership lookup, reviewer suggestions, churn
analysis, and provenance lookup against the same persisted history snapshot:

```sh
ok --repo /path/to/repo history bench \
  --min-similar-recall-at-5 0.75 \
  --min-reviewer-accuracy 0.80 \
  --max-similar-p95-ms 700 \
  --max-lookup-p95-ms 200
```

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

## Reviewer Suggestions

Reviewer suggestions are an experimental local trust-layer surface built on top
of stored `ReviewerEvidence`, ownership lookup, and persisted git author/touch
history.

Query a repository-relative path:

```sh
ok --repo /path/to/repo history reviewers \
  --path crates/open-kioku-core/src/lib.rs
```

The experimental MCP tool `reviewer_suggestions` accepts:

```json
{"path":"crates/open-kioku-core/src/lib.rs"}
```

The typed `ReviewerSuggestionReport` returns ranked `ReviewerSuggestion`
values with:

- `ReviewerSignal` entries for stored review evidence, ownership inference, and
  git-author inference;
- `source_types`, rationale, confidence, staleness, and confidence-breakdown
  fields for each suggestion;
- top-level and per-suggestion `availability` values such as
  `actual_review_evidence`, `inferred_from_ownership_and_authors`,
  `inferred_from_ownership`, `inferred_from_authors`, or `unavailable`;
- explicit booleans for `actual_review_evidence` and `inferred_from_authors`;
- uncertainty notes when true PR-review evidence is unavailable in the local
  index.

Actual review certainty is used only for stored `ReviewerEvidence` with
reviewer or approver roles. Standard local clones do not contain remote GitHub
PR review state, so reviewer suggestions commonly fall back to ownership and
author-history inference. Those fallback suggestions are intentionally capped
below actual review evidence and do not imply PR-review certainty.

The deterministic benchmark corpus for this ranking path lives at
`benchmarks/reviewer-cases.json` and can be checked with:

```sh
ok --repo /path/to/repo history reviewers-bench --min-accuracy 0.80
```

The unified `history bench` corpus also reports reviewer suggestion accuracy and
per-family p95 latency so the public CLI/MCP history APIs stay regression-proof
as a group.

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

### Opening a pre-4.0 index

`user_version` 4 introduces the compact graph tables. Their rows cannot be read by the
pre-4.0 statements or vice versa, so opening an older index does not attempt to reinterpret
them:

1. The presence of a `json` column on `graph_edges` or `call_sites` is an exact
   discriminator for the old layout. It is checked with `PRAGMA table_info`, never a table
   scan, so store open stays constant-time.
2. Both tables are dropped and recreated in the compact shape, and the `schema_meta` key
   `graph_rebuild_required_v4` is set. Because the discriminating column is then gone, the
   detection cannot match again — the reset runs exactly once per store.
3. While that marker is set, every relationship read fails with an instruction to run
   `ok index`, rather than answering from an empty table. An empty answer would read as "no
   such relationship exists", which is the failure this design exists to prevent.
4. `replace_graph` clears the marker, so a rebuild restores normal reads.

`IndexManifest.schema_version` is bumped to 2 in the same release, which marks every file
stale and routes the next `ok index` to a full rebuild rather than a partial update.

`ok snapshot import` refuses an artifact whose `sqlite_user_version` is below the supported
version and names the fix, instead of importing a store whose graph would be discarded on
first open.
