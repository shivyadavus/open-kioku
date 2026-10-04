# Indexing Pipeline

1. Discover and canonicalize the repository root.
2. Load `ok.toml`, falling back to secure defaults.
3. Apply ignore, exclude, hidden-file, max-size, and deny-path policy.
4. Detect Git branch and commit from `.git/HEAD` when available.
5. Walk files using the `ignore` crate, pruning build output and installed packages (see "Pruned directories" below); each pruned directory is recorded by path.
6. Skip binary, vendor, unsupported, ignored, denied, and over-limit files; index generated source files and flag them `is_generated` (they rank last unless the task names them).
7. Fingerprint indexed files with SHA-256.
8. Detect language from extension.
9. Replace secret-like values in data, config, and prose files (YAML, JSON, TOML, Markdown, plain text, and document-corpus files) with `[REDACTED]`, within their lines, so nothing below ever sees the value; unlabelled high-entropy tokens are replaced only in config and data formats, not in prose, and programming-language source is not changed. Rules and limits: `docs/security-model.md`, "Secret-value redaction". The count of files with a replaced value is `IndexQuality.redacted_files`.
10. Extract imports, symbols, chunks, test candidates, and symbol occurrences. Supported code languages use tree-sitter grammars first and regex heuristics only as fallback. A file that cannot be read (removed or permission-denied between discovery and parsing) or that crashes a grammar is dropped from the index, recorded as a `SkipReason::Error` entry in `skip_counts` / `skipped_paths` with source `filesystem` or `parser`, and surfaced as a phase warning. No single file aborts the index.
11. Import configured SCIP indexes when present, merging SCIP symbols and occurrences with extracted facts.
12. Store files, symbols, chunks, tests, imports, and occurrences in SQLite, in one transaction that also removes the previous index manifest.
13. Build and persist graph nodes and edges in SQLite.
14. Rebuild the Tantivy BM25 index from indexed chunks and symbols. A document's searchable text is its path, its chunk text, and its symbol's name, qualified name, signature and declaring file path; the rest of the symbol record is stored only. Identifiers are indexed whole and as their CamelCase/snake_case parts (`SlotPlanner` -> `slotplanner`, `slot`, `planner`), the parts at the whole word's position. Indexes built before either change keep working, as they were written, until the next `ok index`.
15. Publish the index manifest. When the manifest it replaces predates secret-value redaction, `VACUUM` the database once afterwards so free pages holding values stored as read are dropped (`docs/security-model.md`, "Secret-value redaction"). Until then no manifest is published, and readers report `indexing in progress` while the writer holds `.ok/index.lock`; see `docs/storage-model.md`, "Publication order".
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
- `pruned_dirs` and `walk_errors`: what the walk cannot see, counted beside the ratio;
- `pruned`: the pruned directories by repository-relative path, each with its `reason`
  (`build_output`, `dependencies`, `virtual_env`) and, in a Git work tree,
  `tracked_source_files`, the git-tracked programming-language files under it (absent
  outside Git, where it is unknown). Build-output directories holding tracked source come
  first, then the rest by path, at most 50; `pruned_unlisted` counts the others, and a
  secret-like directory is counted there and never named. Both are absent on a manifest
  written before paths were recorded, which reads as `N directories pruned by name`;
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
  recognised language. Files under a pruned directory (see "Pruned directories") are
  not discovered; each directory is counted in `pruned_dirs` and named in `pruned`
  (`.git` and `.ok` are pruned but not counted; they are never source). The exception
  is committed source: a git-tracked programming-language file under a directory
  pruned as build output is discovered and skipped as `pruned`, an omission the ratio
  is judged on, because someone committed it and the index does not hold it. Untracked
  build output and installed packages never count against the ratio.
  A directory the walker could not read is counted in `walk_errors` (also
  `skip_counts.error`); its files were never discovered. Files of unknown language are
  not source files; their skips stay in `skip_counts`, outside coverage.
- A discovered file is either *indexed* (parsed as code, or admitted to the document
  corpus) or attributed to exactly one skip reason: `ignored`, `denied`, `hidden`,
  `binary`, `too_large`, `generated`, `vendor`, `fast_mode`, `secret_policy`,
  `symlink_policy`, `error`, `pruned`. Per language, `discovered == indexed + sum(skipped)`.
- A skip is either a *policy exclusion* — `hidden`, `ignored`, `denied`,
  `secret_policy`, `vendor`, `generated`, `fast_mode`, `symlink_policy`: a rule chose it
  (`SkipReason::is_policy`) — or an *omission* the index did not intend: `too_large`,
  `binary`, `error`, `unsupported_language`, `pruned`. The ratio is `indexed` over *considered*,
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
(`coverage: 9,982 of 9,987 programming-language files indexed (99.9%); 12,004 of 12,115 recognised files indexed (99.1%) overall; 25 excluded by policy (25 secret-policy; `[paths] deny` or the built-in secret-path rule governs the largest share); skipped: 5 too-large`);
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
can. Pruned directories (the first three by path, and how many more) and walk errors
are appended to the summary line whenever nonzero; pruned directories alone do not
force a warning, since `target/` and `node_modules/` are pruned on nearly every
repository. Git-tracked source under a directory pruned as build output warns at any
count, like a walk error, and the summary line and the doctor's next step name the
directories: no ratio threshold can say a committed `tools/build/` package was meant
to vanish. A repository with no
recognised programming source reports `no programming-language files discovered` rather
than implying a verdict. `ok status --markdown` carries the summary line. A manifest written before coverage was recorded reports `null`, and
`ok doctor` says so rather than assuming full coverage. The commit-derived benchmark
records the same line beside every accuracy number (`docs/retrieval-benchmark.md`);
lines recorded before policy exclusions left the denominator read lower on repositories
with hidden or ignored source, and are not comparable to lines recorded after.

`IndexCoverage::gaps` applies the doctor's per-language predicates as a verdict that reaches
confidence. It lists a language git ignore rules mostly excluded, a language under the 98%
or 20-file rule, and, when policy left no programming-language source to consider, every
language it emptied. Context packs and plans read it from the manifest and price it as the
`index_coverage` signal (`docs/ranking.md`, "Index coverage gaps"); `ok --json status` and
MCP `repo_status` list it as `coverage_gaps`. The repository-wide ratio and walk errors stay
in the doctor's check. Committed source under a pruned build-output directory is an
`omitted` gap with reason `pruned` once it crosses those thresholds.

## Pruned directories

Discovery prunes a directory only on evidence that it is build output or installed
packages, never by its name alone (`open-kioku-ingest`, `prune.rs`). Until #477 every
directory named `target`, `build`, `dist`, `node_modules` or `.venv` was pruned at any
depth, so a Rust `src/build/` module or a Java `com.acme.dist` package vanished and coverage
still read 100%. The rule, by directory name:

| Directory | Pruned when | Reason |
|---|---|---|
| `.git`, `.ok` | always (a worktree's `.git` file too); never recorded | tooling |
| `node_modules` | always | `dependencies` |
| `.venv`, `venv` | it holds `pyvenv.cfg` or `conda-meta` | `virtual_env` |
| `target` | it holds `CACHEDIR.TAG`, or sits beside `Cargo.toml`, `pom.xml` or `build.sbt` | `build_output` |
| `build`, `dist` | it holds `CACHEDIR.TAG`, or no module or package declares it | `build_output` |

A `build` or `dist` directory is declared, and walked, when it directly holds a `mod.rs`,
an `__init__.py` or a `.go` file; when a Rust `build.rs`/`dist.rs` module file sits beside
it in a directory without a `Cargo.toml` (beside one, `build.rs` is the build script); or
when it lies under a `src/` directory and no build manifest (`Cargo.toml`, `pom.xml`,
`build.sbt`, `build.gradle(.kts)`, `package.json`, `setup.py`, `pyproject.toml`,
`CMakeLists.txt`, `go.mod`, `meson.build`) sits beside it. Undeclared `build` and `dist`
default to pruned because they are overwhelmingly Gradle, setuptools, CMake and bundler
output, often larger than the source; what that default gets wrong stays visible as
committed source counted `pruned` (see "Coverage"). An unmarked `target` or `venv` is
walked; a `.venv` without a marker is walked and its files skipped by the hidden-file rule.
Only directories are pruned: a file named `build` is discovered like any other.

Every walk applies the same rule: discovery, the `.gitignore`/`.okignore` file walk, the
Git ignore candidate walk, the snapshot import's changed-file count, and `ok watch`'s
event filter, so an edit to a declared `src/build/` module triggers a re-index and a
`cargo build` writing `target/` does not. Each pruned directory is recorded in
`skipped_paths` with reason `pruned` and source `detector`, and in coverage as above. When
it pruned anything in a Git work tree, discovery runs `git ls-files` once to count the
tracked files beneath; nothing else is read under a pruned directory.

`[index] exclude` applies to files in a walked directory as it does anywhere else. The
`ok.toml` that `ok init` wrote before this rule listed `**/target/**`, `**/dist/**` and
`**/build/**`; those patterns are the user's once written and still apply, so such a
repository keeps excluding a `src/build/` module, now as a visible `config_exclude` skip,
until they are removed. They are no longer written by `ok init` or added on load.

