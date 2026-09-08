# Indexing Pipeline

1. Discover and canonicalize the repository root.
2. Load `ok.toml`, falling back to secure defaults.
3. Apply ignore, exclude, hidden-file, max-size, and deny-path policy.
4. Detect Git branch and commit from `.git/HEAD` when available.
5. Walk files using the `ignore` crate.
6. Skip binary, vendor, unsupported, ignored, denied, and over-limit files; index generated source files and flag them `is_generated` (they rank last unless the task names them).
7. Fingerprint indexed files with SHA-256.
8. Detect language from extension.
9. Extract imports, symbols, chunks, test candidates, and symbol occurrences. Supported code languages use tree-sitter grammars first and regex heuristics only as fallback.
10. Import configured SCIP indexes when present, merging SCIP symbols and occurrences with extracted facts.
11. Store files, symbols, chunks, tests, imports, occurrences, and the index manifest in SQLite.
12. Build and persist graph nodes and edges in SQLite.
13. Rebuild the Tantivy BM25 index from indexed chunks and symbols. Identifiers are indexed whole and as their CamelCase/snake_case parts (`FieldMapper` -> `fieldmapper`, `field`, `mapper`); parts live in a separate field queried at half weight so a whole-word match always outranks a part match. Indexes built before this keep working until the next `ok index`.
14. Build search results from Tantivy, falling back to SQLite-backed in-memory lexical search if a legacy index is missing.
15. Produce context, impact, test, and architecture answers from indexed facts.

The pipeline is designed for incremental indexing by content hash. The current implementation replaces the SQLite index atomically in one transaction; changed-file scheduling is the next step behind the same storage traits.

## Coverage

The manifest records what discovery saw versus what the index holds, so an ingest rule
can never drop files silently. `IndexQuality.coverage` (JSON: `quality.coverage`, and
`coverage` at the top of `ok --json status` and the MCP `repo_status` result) carries:

- `discovered`, `indexed`, `generated`, and `skipped` (a count per skip reason) over
  every file whose language is recognised;
- `by_language`: the same four numbers per language key (`java`, `python`,
  `type_script`, ...);
- `pruned_dirs` and `walk_errors`: what the ratio cannot see, counted beside it.

What is counted:

- A file is *discovered* when the walker visits it and its extension maps to a
  recognised language. Directories pruned by name before the walk (`target`,
  `node_modules`, `dist`, `build`, `.venv`) are never discovered; each one is counted
  in `pruned_dirs` because a real package named `build` would otherwise vanish behind
  a 100% figure (`.git` and `.ok` are pruned but not counted; they are never source).
  A directory the walker could not read is counted in `walk_errors` (also
  `skip_counts.error`); its files were never discovered. Files of unknown language are
  not source files; their skips stay in `skip_counts`, outside coverage.
- A discovered file is either *indexed* (parsed as code, or admitted to the document
  corpus) or attributed to exactly one skip reason: `ignored`, `denied`, `hidden`,
  `binary`, `too_large`, `generated`, `vendor`, `fast_mode`, `secret_policy`,
  `symlink_policy`, `error`. Per language, `discovered == indexed + sum(skipped)`.
- `generated` counts indexed files flagged `is_generated`: source files that look
  generated are indexed and flagged, so they stay in coverage. Only document-corpus files
  the generated-content detector rejects are skipped as `generated`.

Two ratios are reported, and only one of them is judged. The **programming-language
ratio** counts files in rust, java, typescript, javascript, python, go, and sql — the
languages whose omission costs an agent evidence — and is what a warning is measured
against. The **all-languages ratio** counts every recognised language and is always
reported beside it. Config and prose files (yaml, json, toml, markdown, text) are
therefore visible but never a verdict: hidden `.github/*.yml` and `.vscode/*.json`
files sink the all-languages ratio on almost every repository, and a warning that
always fires stops being read.

Where it surfaces: `ok index` ends with one line
(`coverage: 9,982 of 10,012 programming-language files indexed (99.7%); 12,004 of 12,140 recognised files indexed (98.9%) overall; skipped: 25 secret-policy, 5 too-large`);
`ok doctor` prints the per-language table for every language and warns, with the top
three skip reasons, when the programming-language ratio falls under 98%, when a
programming language with at least 50 discovered files falls under 98%, when a
programming language is missing 20 or more files regardless of percentage, or when any
walk error occurred. Pruned directories and walk errors are appended to the summary
line whenever nonzero; pruned directories alone do not force a warning, since `target/`
and `node_modules/` are pruned on nearly every repository. A repository with no
recognised programming source reports `no programming-language files discovered` rather
than implying a verdict. `ok status --markdown` carries the summary line. A manifest written before coverage was recorded reports `null`, and
`ok doctor` says so rather than assuming full coverage. The commit-derived benchmark
records the same line beside every accuracy number (`docs/retrieval-benchmark.md`).

