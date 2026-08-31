# RI3.6 design: atomic multi-store index generations

Status: proposed (design for issue #242). Informed by the 2026-08-31 large-corpus
profiling recorded in #329 and `docs/large-java-validation-2026-08-31.md`.

## Measured motivation

On a 16.5k-file Java corpus (247k symbols, 1.5M graph edges, 495k semantic vectors):

1. **Peak indexing memory (~7.0–8.0 GB RSS) occurs in the resolution/analysis phase**,
   where the extracted corpus, the resolution indexes (symbols/scopes/bindings), the
   semantic repository model, and 1.1M analysis facts coexist before anything is written.
   The whole-corpus `IndexSnapshot` then stays alive through store, graph, and Tantivy
   phases because each phase re-reads slices of it.
2. **Rebuilding into an existing store is slower than a cold build and grows the file**
   (repeat: 27m49s vs cold 19m28s; `index.sqlite` grew to ~10.5 GB from ~5-6 GB) because
   delete-and-reinsert churns pages that are never vacuumed.
3. **A killed index leaves debris**: the SQLite rollback is safe, but the file keeps its
   doubled size, a stale `index.lock` remains, and the first subsequent open pays journal
   recovery. Readers during the write window contend with the writer.
4. The semantic store already solves this correctly at small scale:
   `build_and_promote` builds into a fresh directory and atomically renames
   `current` → `previous`, with fail-closed manifest compatibility checks
   (`source_index_fingerprint`). RI3.6 generalizes that working pattern to every store.

## Design

### Generation layout

```
.ok/
  generations/
    active                      # small JSON pointer file, atomically replaced
    <generation-id>/
      generation.json           # generation manifest (see below)
      index.sqlite              # structural + graph + history store
      search/                   # Tantivy
      semantic/                 # semantic generation (existing layout, nested)
  models/                       # shared caches that survive generations
  embeddings.cache
```

`<generation-id>` is `g<unix-seconds>-<short-random>`; ids never repeat and sort by
creation time. Legacy layouts (`.ok/index.sqlite` at the top level) are detected at open
and either adopted in place as generation `g0-legacy` (no data copy: files are moved into
a generation directory in one directory-rename pass) or, when a move is impossible,
served read-only with a migration note.

### Generation manifest (`generation.json`)

```json
{
  "schema_version": 1,
  "generation_id": "g1756612345-4f2a",
  "created_at": "...",
  "source": { "commit": "...", "branch": "...", "dirty": false },
  "analysis_semantics_fingerprint": "...",
  "components": {
    "structural": { "state": "complete", "files": 13607, "symbols": 247499 },
    "graph":      { "state": "complete", "nodes": 402844, "edges": 1522135 },
    "search":     { "state": "complete", "chunks": 248107 },
    "semantic":   { "state": "complete", "vectors": 495606, "backend": "usearch-hnsw-f32" }
  }
}
```

A generation is publishable only when every required component reports `complete` and
all components carry the same `analysis_semantics_fingerprint` — this replaces today's
pairwise semantic↔structural check with one global compatibility rule.

### Atomic publication

1. The indexer creates `generations/<new-id>/` and builds **into it** while the previous
   active generation keeps serving reads untouched. Every build is a cold build into
   fresh files — the measured fast path — which structurally eliminates the
   delete-and-reinsert slowdown, the file growth, and writer/reader lock contention.
2. `generation.json` is written last within the staging directory (write temp + rename).
3. Publication replaces `generations/active` atomically (write temp + rename). The
   pointer contains the generation id plus the manifest digest, so a reader can detect a
   corrupted pointer.
4. Rollback is trivial: the previous generation directory is still present and
   consistent; restoring the pointer restores service. This is `build_and_promote`
   generalized.

### Reader pinning

Readers (CLI queries, MCP server, watch queries) resolve `active` once per logical
operation and open component stores by absolute generation path, so a publication during
a long read never mixes generations. The MCP server re-resolves between requests;
long-lived handles hold the generation open (POSIX keeps the files alive even if GC
unlinks the directory).

### Garbage collection

Bounded and conservative: keep the active generation plus the most recent complete
predecessor (instant rollback), delete older ones. Incomplete generations older than a
grace period (default 24h) are abandoned debris from killed builds and are removed at
open with a startup classification note (`ready`, `ready-unpublished`, `abandoned`,
`corrupt`). GC never runs while a build is staging.

### Memory strategy (the #329 structural fix)

Generations make streaming safe: because nothing under `generations/<new-id>/` is
visible until publication, the indexer no longer needs the all-or-nothing in-memory
`IndexSnapshot` for atomicity. Phases stream into the staging store as they complete:

- parse/extract writes files, chunks, tests, and per-file facts in batches as produced,
  releasing per-file memory immediately (the extraction pass already moves rather than
  clones);
- resolution reads back what it needs through the staging store's indexes instead of
  holding every extracted vector alive, and writes occurrences/relationships in batches;
- graph emission consumes resolved relationships from the staging store in bounded
  batches and bulk-loads edges with the already-landed drop/rebuild index strategy;
- Tantivy builds from staged chunks.

The `IndexSnapshot` type remains for API compatibility (tests, snapshot import/export)
but the large-corpus path stops materializing the full corpus. Target: peak RSS bounded
by the largest single phase working set (measured target ≤ 2 GB on the reference
corpus), not by corpus size times coexisting phases.

### Compatibility

- `SemanticManifest.source_index_fingerprint` keeps working; the semantic component nests
  inside the generation so its own current/previous mechanism collapses into the
  generation's (one promotion concept, per #235's "no parallel lifecycle architecture").
- `ok status` / `ok doctor` report generation id, component states, startup
  classification, and GC counts; MCP `repo_status` gains `generation_id` (this is the
  field #244's exploration envelope requires).
- Existing repositories migrate by directory adoption on first open with the new binary;
  old binaries opening a migrated repo fail with a clear versioned error rather than
  corrupting state (`user_version` bump).

## Phasing

1. **P1 — layout + adoption + pointer**: generation directories, legacy adoption,
   atomic pointer, reader resolution, status/doctor fields. No behavior change to the
   build itself. (Tests: adoption, pointer atomicity under kill, startup classification.)
2. **P2 — build-into-staging**: the indexer builds into a staging generation and
   publishes; kill-at-any-point leaves the active generation serving. Repeat index cost
   becomes cold-build cost; file growth disappears. (Tests: kill-injection matrix,
   rollback, concurrent readers during publication.)
3. **P3 — streaming phases**: convert extract → resolve → graph to staged batch writes;
   measure peak RSS on the reference corpus against the ≤2 GB target.
4. **P4 — GC + cross-component gates**: bounded GC, global fingerprint validation,
   `generation_id` in MCP, migration notes.

Each phase lands independently green with the issue's required tests mapped to it.
