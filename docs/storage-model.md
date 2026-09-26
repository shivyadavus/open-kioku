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
re-index cannot accumulate entries nothing points at. The incremental writer adds to
`graph_strings` rather than clearing it, and removes whichever entries the edges it deleted
referenced once no surviving edge references them, so repeated incremental runs do not grow
the dictionary either.

### Incremental graph updates

`ok watch` re-indexes through `SqliteStore::stage_files_index_with_graph`, which takes the
changed files' rows and the complete graph of the new snapshot, in one transaction:

1. A file's edges are removed: every edge anchored at a node the file owns (its file node and
   its symbols' nodes, in either direction) and every edge whose evidence range lies in the
   file. The pre-4.0 writer keyed this delete on `evidence.source`, the producing pass name,
   which never equals a path, so it deleted nothing (#413).
2. The stored graph is reconciled with the new graph by identity: every stored node and edge
   the new graph does not hold is removed, and every node and edge of the new graph the store
   does not hold is added. This is what handles edges whose target moved — a resolved call
   from an unchanged file into a symbol the changed file renamed ends at a node no file owns,
   so step 1 cannot see it; the new graph no longer holds it, so step 2 removes it. The pass
   reads every stored edge id once; it is proportional to the graph, not to the change, and
   is what makes the incremental graph equal a clean rebuild rather than approximate it.
3. Unchanged edges keep their stored evidence, including `indexed_at`.

### Publication order

The manifest is the publication marker, written as the last step of an index run.

- **Full runs** (`ok index`, and `ok watch` when it cannot update incrementally) stage the
  rows in one transaction that also removes the manifest, write the git history and the
  graph in transactions of their own, rebuild the Tantivy index, and then put the manifest.
  A reader never opens a manifest over rows or a graph from a different run, and a run that
  fails partway leaves no manifest, so the repository reads as unindexed until a run
  completes. From the row transaction until the manifest is put, reads are refused with
  `indexing in progress` rather than served: the previous index's rows are already gone.
  A request already dispatched when a full run begins can read rows from both states; the
  next request is refused until the manifest is published.
  Building the next index in a staging generation and publishing it with the atomic
  `active` pointer (`crates/open-kioku-storage/src/generations.rs`) is what would keep the
  previous index readable during a rebuild; it is not done yet.
- **Incremental `ok watch` runs** replace the changed files' rows and reconcile the graph in
  one transaction while the previous manifest stays published, so readers see the new rows
  and graph as soon as it commits, under the previous manifest's counts and timestamps. The
  Tantivy index is then rebuilt in place: its directory is removed and recreated. For that
  stage `ok search` and `search_code` with `mode=code` find either no index, and fall back to
  lexical search over the SQLite chunks, or an index that is empty until the rebuild commits;
  `mode=graph` reports the graph search index missing. The new manifest is put last. If any
  step after the transaction fails, the previous manifest is withdrawn and the error is
  returned, so the repository reads as unindexed instead of serving this run's rows under
  the previous manifest with no search index; the next `ok watch` event finds no manifest and
  rebuilds in full.
- **`ok snapshot import`** removes the manifest from the imported database before moving it
  into place, rebuilds the Tantivy index from the imported rows, and then puts the imported
  manifest, so a failure at the search stage or at the manifest write leaves the repository
  unindexed rather than a manifest over a missing search index. The manifest of the database
  being replaced is withdrawn before that database is moved aside, so a session that holds it
  (an MCP server) probes again and switches to the imported index instead of answering from
  the replaced file. A replaced file is imported over without a withdrawal only when it is not
  a readable index — SQLite reports it is not a database, or it has no `manifests` table; any
  other failure to read or withdraw its manifest fails the import with the index left in
  place. If the imported database cannot be moved into place or opened, the previous database
  is moved back over it and its manifest restored; the manifest is written only into a
  database that is back at the index path. If moving it back fails, the error names the
  backup file, `.ok/.index.sqlite.<pid>.<time>.backup`, that still holds the previous
  database without its manifest; if restoring the manifest fails, the error says so. Either
  way the repository reads as unindexed until `ok index` runs. An import killed while the
  previous database is moved aside leaves it at that backup path too, and the
  `repository is not indexed` message on every read surface (`ok status`, `ok doctor`, MCP
  `repo_status`) then names the file.

SQLite components are therefore consistent per transaction; the search index is not
versioned with them.

`.ok/index.lock` is an OS advisory lock (`flock` on unix, `LockFileEx` on Windows) that every
writer — `ok index`, `ok watch`, `ok snapshot import` — holds for its whole run, and the
kernel releases it however the writer exits, Ctrl-C and OOM kills included. On unix the
writer removes the file as it finishes, while still holding the lock; on Windows the file
stays. A lock file nobody holds is ignored by readers and taken over by the next writer at
once. On a filesystem where advisory locks do not work, `ok index`, `ok watch` and
`ok snapshot import` fail with `could not lock` and readers treat the lock as absent; on a
network mount without lock support, the lock is local to each machine and does not exclude
a writer on another. While a live writer holds the lock and no
manifest is published, every read surface — `ok status`, `ok doctor`, every read command,
and the MCP server — reports `indexing in progress` rather than `repository is not indexed`.
The MCP session survives the failed probe, and a session that already holds a store checks
for the manifest before each request and probes again when it is gone, so it gives the same
answer.

### Snapshot import: revision and local policy

Before the current index is touched, `ok snapshot import` checks two things on the staged
copy of the artifact:

- **Revision.** The artifact records the commit its index was built from (the embedded
  manifest's, else the metadata's `repo_commit`; the two must agree). That commit is related
  to the local `HEAD` with Git: the same commit is fresh, and a commit that shares history
  with `HEAD` (an ancestor, a descendant, or a diverged branch) is imported with the number
  of commits behind and ahead and the number of files whose working-tree content differs
  from it (tracked files changed since it, committed or not, and untracked files Git does
  not ignore, leaving out the directories discovery prunes: `.ok`, `.git`, `target`,
  `node_modules`, `dist`, `build`, `.venv`). An artifact whose relation cannot be established — its commit is not in
  this repository, shares no history with `HEAD`, or was never recorded, or the directory has
  no `HEAD` — is refused, and the current index stays published; `--allow-foreign` imports it
  marked `foreign`. `ok index --from-snapshot auto` applies the same refusal and indexes from
  source instead. `source_root_hash` in the metadata hashes the exporter's absolute path and
  is not compared.
- **Consistency.** The policy below is decided on the path columns, while readers serve the
  path inside each row's JSON, resolve content through `file_id`, and find graph strings by
  their hash. Before it runs, the staged database must keep every invariant the writers
  keep: each path column (files, document sections, every history table) equals the path in
  the row's JSON; every symbol, chunk, occurrence, test, import, fact, scope, binding, call
  site, vector target and file-owned graph node belongs to an indexed file; file nodes, and
  every `file:` reference in the graph dictionary, name an indexed file; edge evidence and
  history facts name indexed files; and every graph dictionary entry is keyed by its value's
  hash. SCIP symbols and occurrences are the one exception to belonging to an indexed file:
  `ok index` stores them for a document discovery skipped for a reason other than security
  (generated or `.gitignore`d code), so a reference into it still resolves, and the check
  accepts them; the policy step below then removes them. An artifact
  that breaks any of the others is refused, `--allow-foreign` or not, because no writer
  produces one: `ok watch` removes the facts other files hold about a file it deletes, as a
  full index never records them.
- **Local index policy.** Every indexed file and document the importing repository's policy
  excludes — secret-like and `[paths] deny` paths, hidden files, `[index] exclude`,
  `.gitignore`, `.okignore`, judged by `open-kioku-ingest`'s `IndexPathPolicy`, the checks
  `ok index` applies — is removed with every row derived from it (symbols, chunks,
  occurrences, graph nodes and the edges anchored at them or evidenced in the file, vector
  targets, document sections, facts other files hold about it, and its symbols' history), and
  is recorded in the manifest's coverage and skipped paths as discovery records a skip. Git
  history rows that name a secret-like or denied path are removed too, as `ok index`
  withholds them when it reads history, and so are graph nodes no file owns whose label is
  one. Such labels are often not repository paths at all (an import specifier like
  `../utils/foo`, a route like `/api/users`), so they are judged by the security rules alone
  (`SecurityPathPolicy`, the rules history ingestion uses) and never sent to Git. File-level
  history of a path excluded for any other reason is kept, as `ok index` keeps it. Secret-like paths the exporter
  listed as skipped are withheld under the importing repository's `redact_secrets`. Rules
  that do not depend on local configuration — vendor detection, pruning of build and
  dependency directories, the size limit, symlinks — are not applied again. Every SCIP symbol
  and occurrence no indexed file owns is removed as well, with the graph edges at those
  symbols (an indexed file's `references` edge names the symbol): such a row records only a
  hash of its document's path, so the local policy cannot be checked against it, and a path
  this repository denies but the exporter only ignored would otherwise be served through the
  SCIP symbol string, which spells the module path. The import reports the count as a caveat
  and a `scip` quality note and lowers `quality.scip_symbols`, `scip_occurrences` and
  `scip_exact_references` to match; `ok index` imports SCIP for generated or ignored code
  again. The search index is rebuilt from what remains.
- **What the remaining rows said about removed ones.** A removed file's symbols are named
  elsewhere by their qualified names, which no path rule matches: the symbol registry's
  resolution of a bare `KeyAnchored` in an admitted file is a fact targeting
  `internal::vault::keys::KeyAnchored`, drawn as a graph node with that label and an edge
  whose message quotes it. The names are taken from the removed symbols themselves, as the
  language's parser spelled them (for Rust, the module path), not derived from the file's
  path. Only facts that record a symbol the code resolved to are withdrawn by name (sources
  `open-kioku-symbol-registry/*` and the similarity passes, `open-kioku-relationships:*`),
  with the edges those facts drew, unless an indexed file still defines a symbol of that
  name. An admitted file's own statements that spell the same string stay: its
  `use internal::vault::keys::KeyAnchored` is an import fact, with its edge to a module node,
  which `ok index` also keeps (as an unresolved import) when the target file is not indexed.
  The withdrawn resolutions are counted, never named, in a `symbol_registry_caveat` quality
  note and an import caveat: those uses now read as unresolved. A graph node no file owns
  that the removal leaves with no edge (the resolution's target, a removed symbol's
  `complexity:` resource) is removed too: such nodes exist only as the target of an edge.
  Call-site dictionary entries no remaining call site uses are removed, since a call site's
  id spells its file's path. Every history hotspot is recomputed from the file and symbol
  touches that remain, so a directory hotspot for a directory only removed files were in is
  gone and a parent directory's counts no longer include the removed touches. The artifact's
  quality notes about removed content are dropped from the imported manifest: an
  import-resolver caveat in a removed file, an unresolved name in a removed chunk, and a
  symbol-registry caveat for a name no remaining chunk uses (the registry records each name
  once, without its chunk, so a name a remaining chunk also uses keeps its caveat). The
  manifest's skipped paths still name a denied file, as `ok index` records it; a secret-like
  one is withheld under `redact_secrets`.

  This is removal, not re-analysis, so the imported index is not what `ok index` would
  write for the same files. On a small Rust fixture (an admitted file that imports and calls a
  struct in the removed one) the hotspots, facts, graph nodes and edge count agreed with a
  fresh `ok index` under the same deny; in general they can
  differ where the import cannot recompute: a use the registry resolved to a removed symbol
  is withdrawn, not re-resolved to another candidate; an import whose target became
  ambiguous or unresolved keeps the edge and resolution status the exporter recorded; the
  counts of unresolved names, and nodes' `source_pass`, are the exporter's.
- **Free pages.** The staged database is compacted with `VACUUM`, once the artifact's
  manifest has been withheld and before it is moved into place, when the policy step removed
  a row, when the manifest lost notes or skipped paths the artifact's copy still holds, or
  when the artifact arrived with free pages (a `--quality fast` artifact is a page copy of the
  exporter's database, and an exporter that denied a path and re-indexed keeps that path's
  rows in free pages). SQLite keeps the bytes of deleted rows in free pages until they are
  reused, so without it the imported file would still hold the removed names, and the
  artifact's manifest with its notes about them, although no query serves either. The
  compaction runs in rollback-journal mode, so no write-ahead log holds the old pages
  afterwards; the manifest published after the search index is rebuilt is written into new
  pages. The cost is one rewrite of the staged database, with free disk space of about twice
  its size while it runs (the rewritten copy and the rollback journal). A `--quality best`
  artifact from which nothing is removed skips it. As with the pre-redaction compaction,
  `VACUUM` cannot scrub blocks the filesystem has already freed, such as the deleted rollback
  journal's. `ok index` itself does not compact after a full rebuild, so a path denied after
  it was indexed stays readable in the free pages of the index `ok index` writes until they
  are reused.

The published manifest carries the result as `snapshot`: `imported_from_commit`,
`local_commit`, `relation` (`same_commit`, `related` or `foreign`), `commits_behind`,
`commits_ahead`, `changed_files`, and `policy_filtered`, the number of paths removed (indexed
files and documents, plus secret-like or denied paths named only by history or an unowned
graph node). The
counts describe the checkout at import time. `ok status`, `ok doctor` (the `snapshot` check)
and MCP `repo_status` report it; unless the import was of the checked-out commit with no
changed files, `build_context_pack`/`ok context` carry a caveat in
`retrieval_diagnostics.caveats` and `confidence_breakdown.caveats`, and `plan_change`/`ok plan`
a risk reason, until `ok index` publishes a manifest without it. An exporter's uncommitted
changes are not recorded, so an artifact exported from a dirty tree is indistinguishable here
from one exported from its commit.

### Snapshot export

`ok snapshot export` is a reader. It takes no writer lock and does not checkpoint the index,
so it runs alongside `ok index` and `ok watch`. Both qualities copy the database from inside
one read transaction on a read-only connection, so the artifact holds one committed state,
including commits still in the WAL, whatever a writer commits or checkpoints during the copy.
The manifest is read from that copy, not from the live file: a state without one is refused
with `indexing in progress` while a live writer holds the lock, and with `repository is not
indexed` otherwise. The Tantivy index is not part of the artifact; import rebuilds it from the
copied rows.

| | `--quality best` (default) | `--quality fast` |
|---|---|---|
| Copy | `VACUUM INTO`: every table and index is rebuilt into a new file | SQLite online backup API, all pages in one step: pages are copied as they are |
| Database size | Free pages dropped and b-trees packed | Same page count as the live file, free pages included |
| Compression | zstd level 9 | zstd level 1 |
| Result | Smaller artifact, longer export | Larger artifact, shorter export |

The size gap is widest on an index that has had rows replaced since it was built, as
`ok watch` does, because that is where free pages accumulate. The table is qualitative: no
measured size or time ratio between the two qualities is published.

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
and `ok history churn` do not scan raw commit history on every request.

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

`ok history churn` accepts exactly one of:

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

`ok history provenance` accepts exactly one of:

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

The MCP tool that accepted the same signals as a JSON object was retired in
4.0.0; `ok --json history similar` with `--task`, `--path`, `--symbol`, and
`--limit` returns the structured result.

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

`ok history ownership` accepts:

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

`ok history reviewers` accepts:

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

`open-kioku-storage-kv` owns the graph-adjacency extension point for a future redb/fjall optimized store. SQLite currently implements `GraphStore` directly so `ok path` and MCP `dependency_path` (with or without a destination) work from persisted facts.

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
2. Both tables are dropped and the `schema_meta` key `graph_rebuild_required_v4` is set, **in
   one transaction**. Writing the marker separately would leave a window in which the edges
   are gone and nothing records it: the discriminating column would be gone too, so the next
   open would create an empty compact table and answer relationship questions with a
   confident zero indefinitely. As a second layer, an index that has a manifest but no
   `graph_edges` table is treated the same way, whatever removed it.
3. While that marker is set, every relationship read fails with an instruction to run
   `ok index`, rather than answering from an empty table. An empty answer would read as "no
   such relationship exists", which is the failure this design exists to prevent. That covers
   the `GraphStore` reads, the per-edge-type statistics, and the read-only readers used by the
   cross-project workspace linker, which open member indexes directly and so never run this
   gate through a store — they carry it themselves, and `ok workspace link` refuses a member
   project awaiting a rebuild rather than reporting that it has no cross-project edges.
4. `replace_graph` clears the marker, so a full rebuild restores normal reads.

`ok watch` does **not** clear the marker: it persists snapshots through
`replace_index_with_documents`, which does not rewrite the graph. That is the safe direction —
watch never fabricates a graph — but it means watching a repository alone never recovers it.
Run `ok index` once.

`IndexManifest.schema_version` is bumped to 2 in the same release, and to 3 for the typed
quality notes. A stored manifest whose version differs from the current one is not partially
indexable, so the incremental path (`ok watch`) falls back to a full index rather than
updating rows the current reader cannot interpret; `ok index` is already a full rebuild. A
stored manifest whose version is *newer* than the reader's is refused before its body is
deserialized, on every surface and on `ok snapshot import`, with one message: the index was
written by a newer Open Kioku; upgrade Open Kioku or run `ok index` to rebuild it. Older
manifests still read through serde defaults.

`ok snapshot import` refuses an artifact whose `sqlite_user_version` is below the supported
version and names the fix, instead of importing a store whose graph would be discarded on
first open. `ok snapshot export` likewise refuses a store awaiting a rebuild, rather than
writing `graph_edge_count: 0` into the artifact metadata as though it were a measurement. It
opens the index the way every read surface does, so it refuses with `indexing in progress`
while a live writer holds the lock and no manifest is published, and with `repository is not
indexed` when there is no published index to export.
