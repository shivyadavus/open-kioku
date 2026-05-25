# Indexing Pipeline

1. Discover and canonicalize the repository root.
2. Load `ok.toml`, falling back to secure defaults.
3. Apply ignore, exclude, hidden-file, max-size, and deny-path policy.
4. Detect Git branch and commit from `.git/HEAD` when available.
5. Walk files using the `ignore` crate.
6. Skip binary, vendor, generated, unsupported, ignored, denied, and over-limit files.
7. Fingerprint indexed files with SHA-256.
8. Detect language from extension.
9. Extract imports, symbols, chunks, test candidates, and symbol occurrences. Supported code languages use tree-sitter grammars first and regex heuristics only as fallback.
10. Import configured SCIP indexes when present, merging SCIP symbols and occurrences with extracted facts.
11. Store files, symbols, chunks, tests, imports, occurrences, and the index manifest in SQLite.
12. Build and persist graph nodes and edges in SQLite.
13. Rebuild the Tantivy BM25 index from indexed chunks and symbols.
14. Build search results from Tantivy, falling back to SQLite-backed in-memory lexical search if a legacy index is missing.
15. Produce context, impact, test, and architecture answers from indexed facts.

The pipeline is designed for incremental indexing by content hash. The current implementation replaces the SQLite index atomically in one transaction; changed-file scheduling is the next step behind the same storage traits.
