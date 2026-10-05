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
10. Extract imports, symbols, chunks, test candidates, and symbol occurrences. Supported code languages use tree-sitter grammars first and regex heuristics only as fallback. A file with syntax errors falls back whole, except a C# file: its declarations that error recovery left whole and in place are kept at medium confidence, nothing inside an error node is read, and patterns name its types only when recovery kept none. A file that cannot be read (removed or permission-denied between discovery and parsing) or that crashes a grammar is dropped from the index, recorded as a `SkipReason::Error` entry in `skip_counts` / `skipped_paths` with source `filesystem` or `parser`, and surfaced as a phase warning. No single file aborts the index.
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
  (`build_output`, `undeclared_build_dir`, `dependencies`, `virtual_env`) and, in a Git
  work tree, `tracked_source_files`, the git-tracked programming-language files under it
  (absent outside Git, where it is unknown). Undeclared build directories holding tracked
  source come first, then those holding any tracked source, then the rest by path. The
  manifest stores every one, since plans forbid edits under each; a status summary
  (`ok --json status`, MCP `repo_status`) shows the first 50 and counts the rest in
  `pruned_unlisted`, and `--full` / `detail: "full"` shows all. A secret-like directory is
  counted in `pruned_unlisted` and never named. Both are absent on a manifest
  written before paths were recorded, which reads as `N directories pruned by name`;
- `policy_excluded_by_source` and `policy_excluded_dirs`: the files a policy excluded,
  by the rule that excluded them (`hidden_policy`, `git_ignore`, `ok_ignore`,
  `config_exclude`, `security_policy`, `detector`, `fast_mode`, `symlink_policy`) and by
  top-level directory (`.claude`, `.github`; `.` for files at the root). Both are empty on
  a manifest written before they were recorded; every other number reads the same way.
- `policy_excluded_by_language`: the same source counts per language key
  (`{"rust": {"git_ignore": 640, "hidden_policy": 30}}`). Empty on a manifest written
  before it was recorded, which reads as no per-language data and never warns.
- `policy_excluded_dirs_by_language`: the same files per language key and directory, each
  directory with its counts by source and, when evidence shows it holds installed
  third-party packages, its `dependency` evidence
  (`{"python": {"env/lib/python3.12/site-packages": {"by_source": {"git_ignore": 340},
  "dependency": "python_environment"}, "generated": {"by_source": {"git_ignore": 40}}}}`).
  A file under such a directory counts under it; any other file under its top-level
  directory. Only the directory packages are installed into is classed: an environment
  marker (`pyvenv.cfg`, `conda-meta/`) classes its `lib/python*/site-packages` or
  `Lib/site-packages`, never the rest of the tree around it. Ingest probes only the ancestors
  of excluded programming-language files, each directory once per scan, and only those named
  `site-packages`, `dist-packages` or `vendor`: `*.dist-info`/`*.egg-info` inside, or an
  environment marker at the root of the layout above it; `modules.txt` or
  `composer/installed.json` in a `vendor`. Redacted paths are not recorded, and secret-like
  directories are withheld like `policy_excluded_dirs`. Coverage gaps name their directories
  and price installed dependencies apart from it (`docs/ranking.md`, "Index coverage gaps").
  Empty on a manifest written before it was recorded, which prices every missing file as
  first-party source.

What is counted:

- A file is *discovered* when the walker visits it and its extension maps to a
  recognised language. Files under a pruned directory (see "Pruned directories") are
  not discovered; each directory is counted in `pruned_dirs` and named in `pruned`
  (`.git` and `.ok` are pruned but not counted; they are never source). The exception
  is committed source under a guess: a git-tracked programming-language file under an
  `undeclared_build_dir` is discovered and skipped as `pruned`, an omission the ratio is
  judged on, because someone committed it and the rule that pruned it may have
  misclassified a source directory. Untracked build output never counts against the
  ratio, and neither do committed files under a directory pruned on strong evidence (a
  `CACHEDIR.TAG`, a build manifest beside it, `node_modules`, a marked environment): a
  committed `dist/` bundle beside its `package.json` is listed with its count only.
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
ratio** counts files in rust, java, typescript, javascript, python, go, c_sharp, and sql — the
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
can. Pruned directories and walk errors are appended to the summary line whenever
nonzero. The pruned directories are named three at a time with how many more, those
holding git-tracked source first, each with its count
(`2 directories pruned as build output or dependencies: dist/ (30 tracked source files), target/`),
so a source directory pruned by mistake shows at a glance; pruned directories alone do not
force a warning, since `target/` and `node_modules/` are pruned on nearly every
repository. Git-tracked source under an undeclared build directory warns at any
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
MCP `repo_status` list it as `coverage_gaps`, each gap naming the directories behind its missing
files and counting those under installed dependencies. The repository-wide ratio and walk errors stay
in the doctor's check. Committed source under an undeclared build directory is an
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
| any name, holding a `.git` entry, in a Git repository | it is a nested work tree: a checked-out submodule, a clone or a linked worktree | `submodule` |
| `node_modules` | always | `dependencies` |
| `.venv`, `venv` | it holds `pyvenv.cfg` or `conda-meta` | `virtual_env` |
| `target` | it holds `CACHEDIR.TAG`, or sits beside `Cargo.toml`, `pom.xml`, `build.sbt`, `build.properties` (sbt's `project/`) or `project.clj` | `build_output` |
| `build`, `dist` | no module or package declares it, and it holds `CACHEDIR.TAG` or sits beside a build manifest | `build_output` |
| `build`, `dist` | no module or package declares it, and nothing else accounts for it | `undeclared_build_dir` |

A `build` or `dist` directory is declared, and walked, when it directly holds a `mod.rs`,
an `__init__.py` or a `.go` file; when a Rust `build.rs`/`dist.rs` module file sits beside
it in a directory without a `Cargo.toml` (beside one, `build.rs` is the build script); or
when it lies under a `src/` directory and no build manifest (`Cargo.toml`, `pom.xml`,
`build.sbt`, `build.gradle(.kts)`, `package.json`, `setup.py`, `pyproject.toml`,
`CMakeLists.txt`, `go.mod`, `meson.build`) sits beside it (a `CACHEDIR.TAG` inside prunes
it regardless). Undeclared `build` and `dist` default to pruned because they are
overwhelmingly Gradle, setuptools, CMake and bundler output, often larger than the
source. The two `build_output` rows are strong evidence; `undeclared_build_dir` is a
guess, and what it gets wrong stays visible as committed source counted `pruned` (see
"Coverage"). Committed files under strong evidence are only listed: JavaScript actions and
published libraries commit their `dist/` bundle beside `package.json`, and counting it
would read a fully indexed repository as 25% covered and cap every plan's confidence. An unmarked `target` or `venv` is
walked; a `.venv` without a marker is walked and its files skipped by the hidden-file rule.
Only directories are pruned: a file named `build` is discovered like any other.

Every walk applies the same rule: discovery, the `.gitignore`/`.okignore` file walk, the
Git ignore candidate walk, the project model's and import resolver's manifest walks, the
snapshot import's changed-file count, `ok watch`'s event filter and `ok doctor`'s language
sampling. So an edit to a declared `src/build/` module triggers a re-index and a
`cargo build` writing `target/` does not, and a workspace member in a directory named
`target` is a crate while Cargo's packaged copies under the real `target/package/` are not
(`PROJECT_RESOLVER_SEMANTICS_VERSION` v2 for `target`, v3 for nested work trees; earlier
indexes report `RebuildRequired`). Each
pruned directory is recorded in
`skipped_paths` with reason `pruned` and source `detector`, and in coverage as above. When
it pruned anything in a Git work tree, discovery runs `git ls-files` once to count the
tracked files beneath; nothing else is read under a pruned directory.

`[index] exclude` applies to files in a walked directory as it does anywhere else. The
`ok.toml` that `ok init` wrote before this rule listed `**/target/**`, `**/dist/**` and
`**/build/**`; those patterns are the user's once written and still apply, so such a
repository keeps excluding a `src/build/` module, now as a visible `config_exclude` skip,
until they are removed. They are no longer written by `ok init` or added on load, and
`ok doctor` names any of them an `ok.toml` still lists, with a next step to remove them
unless the exclusion is intended.

### Submodules and nested repositories

In a Git repository, a directory holding a `.git` entry (a `gitdir:` file or a directory) is
a nested work tree: a checked-out submodule, a repository cloned inside this one, or a linked
worktree added inside it. Discovery prunes it as `submodule` whatever its name, before the
name rules above, and `[index] keep_dirs` does not walk it. Git tracks none of its files in
this repository, at most a gitlink naming a commit, so its content is another repository's
source: this repository's diffs do not show an edit inside it, and `ok verify` could not
hold one to a plan. Until #677, `ok index` failed outright on a checked-out submodule,
because `git check-ignore` rejects the whole batch when one path lies inside a submodule
("Pathspec ... is in submodule").

- The submodule is named in coverage (`pruned`, reason `submodule`) and in `skipped_paths`
  as `pruned`, with its tracked source count, which is 0: this repository tracks only the
  gitlink, so it never lowers coverage. A secret-like submodule path is counted in
  `pruned_unlisted`, never named.
- A submodule inside it, or a submodule under another pruned directory (`node_modules`), is
  cut with its outermost pruned directory and not listed separately.
- An uninitialised submodule is a directory with no `.git` beside its gitlink: it is walked.
  It is normally empty; a file left in it is indexed like any other, though Git shows no
  edit to it, and `git check-ignore` is asked about the rest of the batch without it.
- A directory with its own `.git` that holds files this repository tracks is not a real
  submodule (Git tracks no file under one): the `.git` is stray. It is still pruned, but its
  tracked source counts as missing, as under an undeclared build directory, and `ok doctor`
  names the directory and says to remove the `.git`. A plan sets no rule for it.
- The root is in a Git repository when it or a directory above it holds a `.git`. When
  neither does (a folder of clones), there is no repository for a clone to be nested in,
  and every clone is walked. The check looks above the root, so a project that is not a
  repository itself but sits under one higher up (a home directory kept in Git, a workspace
  folder inside a monorepo checkout) counts as in that repository: each clone inside the
  project is pruned as `submodule` and named in coverage. Index each clone on its own.
- `ok snapshot import` prunes a served path under a submodule here as it prunes other
  directories, and asks Git about the rest: when `git check-ignore` fails on a path under a
  gitlink (an artifact built before the directory became a submodule, or a submodule this
  checkout has not initialised), paths under the gitlinks the index records are dropped
  from that question and the others are asked again. Under an uninitialised submodule such
  a path is served like any other file the checkout does not hold.
- `ok watch` ignores events inside a submodule. `ok impact --since`, `ok plan --since` and
  `ok verify` read a moved submodule as one changed path, its gitlink, which the index does
  not hold: each pins `git diff --submodule=short` whatever `diff.submodule` says (#671).
  A plan forbids `<submodule>/**`, the files under it, but not the gitlink itself: a bump
  task edits only that path, so `ok verify` reports it as `out_of_boundary`, which
  `--evidence-ref` admits (the rule's own `coverage:pruned:<submodule>` ref will do), never
  as a `forbidden_boundary` nothing can admit.

### Keeping a build directory

When the rule is wrong for a repository, `[index] keep_dirs` lists `build` and `dist`
directories to walk whatever the evidence says, a cache tag or a manifest beside them
included:

```toml
[index]
keep_dirs = ["tools/dist", "build"]
```

Each entry is one repository-relative directory written with `/` (a trailing `/` is
accepted), and `OkConfig::load_from_repo` rejects anything else: an absolute path, a glob,
an empty, `.` or `..` segment, or a directory not named `build` or `dist`. `target`, Python
environments and `node_modules` cannot be listed: discovery prunes the first two only on
markers their tools write, and installed packages are never source. Keeping a directory
only lets discovery reach its files; each one is still judged by the security policy (a
secret-like path is skipped as `secret_policy` with its path withheld, `[paths] deny`
applies), the hidden-file rule, `[index] exclude` and the ignore files, like any other.
Only the listed directory is kept, not a sibling of the same name, and an entry under a
pruned directory (`target/pkg/dist`) keeps nothing; `ok doctor` names such an entry, and
one with no directory behind it, in the config check. `ok init` does not write the key.
The doctor's next step for committed source under an undeclared build directory names it,
spelled for the first such directory.

### What pruning means downstream

Two consumers once repeated the pruning by name and now follow the record instead:

- **Plans.** A plan forbids edits under each directory the coverage record names as pruned
  (`<path>/**`, at any depth, citing `coverage:pruned:<path>`): build output is changed
  through its source or generator, and installed packages and environments through their
  manifests. The default forbidden rules no longer list a root `target/**`, `build/**` or
  `dist/**`, which forbade a declared `build/` package and missed a nested `web/dist/`
  bundle. An undeclared build directory holding committed source gets no rule, since the
  directory is only a guess and the coverage record already counts its files as missing;
  neither does a directory the record does not name (secret-like, or an index written before
  paths were recorded; one written by 4.0 releases after #477 named at most 50). Nor does a
  root `target/`, `build/` or `dist/` that did not exist when the repository was indexed, as
  in a fresh clone before its first build: an edit there was a `forbidden_boundary`
  violation and is now `out_of_boundary`. Any such edit is still outside `allowed_files` and
  needs expansion evidence, so the loop stays closed, but evidence can now admit it; no
  rule is forbidden by name alone.
- **The semantic corpus** embeds the files the index holds, less vendored, generated, lock and
  secret-like files. It no longer drops every path containing `/target/`: discovery already
  pruned Cargo's and Maven's output on evidence, and the name rule dropped a Java
  `com.acme.target` package and a Rust `src/target/` module that the lexical index holds.

