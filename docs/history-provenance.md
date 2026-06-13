# History Provenance

History provenance is an experimental local trust-layer surface. Run `ok index`
after enabling history so `.ok/index.sqlite` contains typed commit, file-touch,
and symbol-touch records.

## CLI

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
`--limit <n>` to bound recent touches. Ambiguous symbol names fail with candidate
qualified names and IDs instead of selecting one silently. Overloaded symbols
can share a qualified name, so use the reported symbol ID to select one exactly.

## MCP

`history_provenance_lookup` is experimental and accepts exactly one of:

```json
{"path":"crates/open-kioku-core/src/lib.rs","limit":20}
```

```json
{"symbol":"PolicyGate","limit":20}
```

The result includes `first_seen`, `last_touched`, `recent_touches`,
`confidence`, `truncated`, and `uncertainty`.

## Evidence Rules

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
