# Indexing Pipeline

1. Discover and canonicalize the repository root.
2. Load `ok.toml`, falling back to secure defaults.
3. Apply ignore, exclude, hidden-file, max-size, and deny-path policy.
4. Detect Git branch and commit from `.git/HEAD` when available.
5. Walk files using the `ignore` crate.
6. Skip binary, vendor, unsupported, ignored, denied, and over-limit files; index generated source files and flag them `is_generated` (they rank last unless the task names them).
7. Fingerprint indexed files with SHA-256.
8. Detect language from extension.
9. Replace secret-like values in data, config, and prose files (YAML, JSON, TOML, Markdown, plain text, and document-corpus files) with `[REDACTED]`, within their lines, so nothing below ever sees the value; programming-language source is not changed. Rules and limits: `docs/security-model.md`, "Secret-value redaction". The count of files with a replaced value is `IndexQuality.redacted_files`.
10. Extract imports, symbols, chunks, test candidates, and symbol occurrences. Supported code languages use tree-sitter grammars first and regex heuristics only as fallback. A file that cannot be read (removed or permission-denied between discovery and parsing) or that crashes a grammar is dropped from the index, recorded as a `SkipReason::Error` entry in `skip_counts` / `skipped_paths` with source `filesystem` or `parser`, and surfaced as a phase warning. No single file aborts the index.
11. Import configured SCIP indexes when present, merging SCIP symbols and occurrences with extracted facts.
12. Store files, symbols, chunks, tests, imports, and occurrences in SQLite, in one transaction that also removes the previous index manifest.
13. Build and persist graph nodes and edges in SQLite.
14. Rebuild the Tantivy BM25 index from indexed chunks and symbols. Identifiers are indexed whole and as their CamelCase/snake_case parts (`SlotPlanner` -> `slotplanner`, `slot`, `planner`); parts live in a separate field queried at half weight so a whole-word match always outranks a part match. Indexes built before this keep working until the next `ok index`.
15. Publish the index manifest. Until then no manifest is published, and readers report `indexing in progress` while the writer holds `.ok/index.lock`; see `docs/storage-model.md`, "Publication order".
16. Build search results from Tantivy, falling back to SQLite-backed in-memory lexical search if the Tantivy index is missing.
17. Produce context, impact, test, and architecture answers from indexed facts.

`ok index` runs every step above. `ok watch` re-indexes changed files incrementally through the same storage traits: it replaces their rows and reconciles the graph in one transaction under the previous manifest, rebuilds the Tantivy index, and publishes the new manifest last; see `docs/storage-model.md`, "Incremental graph updates" and "Publication order".

## Coverage

The manifest records what discovery saw versus what the index holds, so an ingest rule
can never drop files silently. `IndexQuality.coverage` (JSON: `quality.coverage`, and
`coverage` at the top of `ok --json status` and the MCP `repo_status` result) carries:

- `discovered`, `indexed`, `generated`, and `skipped` (a count per skip reason) over
  every file whose language is recognised;
- `by_language`: the same four numbers per language key (`java`, `python`,
  `type_script`, ...);
- `pruned_dirs` and `walk_errors`: what the ratio cannot see, counted beside it;
- `policy_excluded_by_source` and `policy_excluded_dirs`: the files a policy excluded,
  by the rule that excluded them (`hidden_policy`, `git_ignore`, `ok_ignore`,
  `config_exclude`, `security_policy`, `detector`, `fast_mode`, `symlink_policy`) and by
  top-level directory (`.claude`, `.github`; `.` for files at the root). Both are empty on
  a manifest written before they were recorded; every other number reads the same way.
- `policy_excluded_by_language`: the same source counts per language key
  (`{"rust": {"git_ignore": 640, "hidden_policy": 30}}`). Empty on a manifest written
  before it was recorded, which reads as no per-language data and never warns.

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
- A skip is either a *policy exclusion* — `hidden`, `ignored`, `denied`,
  `secret_policy`, `vendor`, `generated`, `fast_mode`, `symlink_policy`: a rule chose it
  (`SkipReason::is_policy`) — or an *omission* the index did not intend: `too_large`,
  `binary`, `error`, `unsupported_language`. The ratio is `indexed` over *considered*,
  which is `discovered` minus the policy exclusions, per language and overall. A
  git-ignored agent worktree under `.claude/` is therefore reported, not counted as
  missing: on this repository 1,485 hidden `.rs` files had read as 24.9% coverage of a
  fully indexed tree. Policy exclusions are still every bit as visible — the summary
  line, the doctor check, and `ok doctor`'s table carry the count by reason, the top
  directories, and the setting that governs the largest share (`hidden` names
  `[security] allow_hidden_files`; `ignored` names `.gitignore`, `.okignore`, or
  `[index] exclude`; `denied` names `[paths] deny`; `fast_mode` names
  `ok index --mode full`). Only `too_large` among the judged omissions has a setting,
  `[index] max_file_size`, and the doctor's next step names it when that reason
  dominates.
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
(`coverage: 9,982 of 9,987 programming-language files indexed (99.9%); 12,004 of 12,115 recognised files indexed (99.1%) overall; 25 excluded by policy (25 secret-policy; 25 under config/; `[paths] deny` or the built-in secret-path rule governs the largest share); skipped: 5 too-large`);
`ok doctor` prints the per-language table for every language (with an `excluded`
column for policy exclusions, and the ratio over the considered files), then an
`Excluded by policy` block with the top directories and governing setting, and warns,
with the top three judged skip reasons, when the programming-language ratio falls under
98%, when a programming language with at least 50 considered files falls under 98%,
when a programming language is missing 20 or more considered files regardless of
percentage, or when any walk error occurred. It also warns when git ignore rules exclude at
least 20 files of a programming language and more files than that language has
considered (`mostly git-ignored: rust (640 ignored, 12 considered)`). In a git work tree
those rules are everything `git check-ignore` applies (`.gitignore` at any depth,
`.git/info/exclude`, and `core.excludesFile`), so the verdict can differ between machines;
outside one, only `.gitignore` files. They are written for git, so they can remove most of
a language's source behind a 100% ratio. The next step names them: remove the rule if the
files are source an agent should see, or list the paths under `[index] exclude` if the
exclusion is intended. `[index] exclude` is checked before the git ignore rules, so those
files are recorded as `config_exclude` and the warning stops; `.okignore` is checked after
them and does not silence it. `hidden`, `vendor`, `fast_mode`, `denied`, `[index] exclude`
and `.okignore` exclusions never warn; they are this tool's own settings. With the default
`[security] allow_hidden_files = false`, the hidden rule is checked before the git ignore
rules, so a git-ignored worktree under `.claude/` counts as `hidden` and does not trigger
the warning; with `allow_hidden_files = true` the same worktree counts as git-ignored and
can. Pruned directories and walk errors are appended to the summary
line whenever nonzero; pruned directories alone do not force a warning, since `target/`
and `node_modules/` are pruned on nearly every repository. A repository with no
recognised programming source reports `no programming-language files discovered` rather
than implying a verdict. `ok status --markdown` carries the summary line. A manifest written before coverage was recorded reports `null`, and
`ok doctor` says so rather than assuming full coverage. The commit-derived benchmark
records the same line beside every accuracy number (`docs/retrieval-benchmark.md`);
lines recorded before policy exclusions left the denominator read lower on repositories
with hidden or ignored source, and are not comparable to lines recorded after.

