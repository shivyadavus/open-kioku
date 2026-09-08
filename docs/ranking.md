# Ranking Fusion

Open Kioku ranks search and planning candidates with a local, deterministic
multi-signal fusion model. The model starts from lexical/BM25 relevance and adds
or subtracts weighted signals when evidence is available.

Default `ok.toml` weights:

```toml
[ranking]
text_relevance = 1.0
exact_reference = 1.0
graph_proximity = 0.35
boundary_fit = 0.25
runtime_corroboration = 0.30
git_cochange = 0.25
validation_proximity = 1.0
memory_signal = 0.20
path_quality = 1.0
```

Signals:

- `text_relevance`: BM25 or lexical score from indexed code text.
- `exact_reference`: exact symbol references, SCIP evidence, or symbol-name hits.
- `graph_proximity`: dependency or impact-graph proximity when available.
- `boundary_fit`: source-like files that are better primary edit candidates. In context-pack ordering the quality tier (source above docs and tests unless the task is about them) is applied before anchor relevance, and anchor relevance distinguishes a file that *names* the anchor in its path or a symbol (edit target) from one that merely mentions it in a snippet (reference): on a Go repository every mention of an identifier had outranked the best full-task lexical hit. Docs never qualify; test files qualify only when the task asks about tests (`test`, `tests`, `spec`, `coverage`, ...). Test paths are recognised by directory segment and file name (`src/test`, `src/internalClusterTest`, `__tests__`, `FooTests.java`, `FooIT.java`, `foo_test.go`, `foo.spec.ts`), never by substring, so `latest` is not a test and `javaRestTest` is.
- `runtime_corroboration`: runtime traces or incidents when configured.
- Named targets are never demoted: a file whose path or symbol names a primary task anchor (`Guard pool shutdown in ConnectionPoolMetricsIT`) keeps source quality even when it is a test or a doc. Three streams had ranked that test first and the docs/tests tier still pushed it below twenty source files that merely shared vocabulary. Measured neutral-to-positive: on a 10k-file Java service, holdout gold recall 0.514 → 0.540; on a Go application (~800 files) and a TypeScript standard library (~900 files), within one rank of one case.
- `scope:entry-point:<path>`: when a commit scope names a directory, that directory's public entry file (`mod.ts`, `index.ts`, `mod.rs`, `lib.rs`, `__init__.py`; empty package markers excluded) is a candidate even when it shares no word with the task, because `feat(async): stabilize Channel` edits `async/mod.ts`, which does not mention Channel until the commit lands. Where it sits depends on whether any file the scope matched knows the task's own words: if none does, the entry file goes just below the group's best (it is the best guess); if one does, it goes last in the group (the real module wins). On a TypeScript standard library (~900 files), holdout R@5 0.744 → 0.816, R@20 0.798 → 0.869, MRR 0.607 → 0.650; a Python ML library (~4k files) and a Go application (~800 files) were bit-identical. Placing it second unconditionally had cost the Python ML library 0.011 MRR; placing it last unconditionally gained nothing anywhere.
- Generated files are indexed, flagged, and ranked last. The ingester used to skip any file with a "do not edit" or "automatically generated" banner; on a Python monorepo that removed 394 files and a tenth of the files real commits went on to change. They are now indexed with `is_generated`, pushed behind every other candidate in each stream before the per-stream cap (so a regenerated `modeling_*.py` cannot push its `modular_*.py` twin out of the lexical pool), and ranked at the lowest quality tier (signal `generated_file_demotion`, evidence line "generated file: ranked below hand-written source") unless the task's anchors name the file's own path; a symbol match cannot exempt it, because a generated file defines the same symbols as its source. py-a holdout against the same index (`ok context` scored by `scripts/score-context-cases.py`): R@5 0.645 → 0.665, R@20 0.741 → 0.767, gold recall 0.662 → 0.680.
- Source files are never dropped by the secret-path rule. A path component containing "secret" or "credential" or ending in `_key` used to exclude the file; that removed 25 Java source files (`RepositoryS3BasicCredentialsRestIT` and its kin) from one repository. Programming-language source is blocked only by key-material extensions and the `.env`/`.aws`/`.ssh` entries. Data, config, and prose files (`credentials.json`, `secrets.yaml`, `SECRETS.md`) keep the name rule, because chunk contents are not redacted today (#379); a key hard-coded inside a source file is indexed exactly as before, when only the file's name decided.
- `commit_scope_path_boost`: a commit-style scope prefix — `docs(fs): …`, `feat(path/posix): …`, `pkg/render: …`, `[Scheduler] …` — names the package or directory the change lives in, which the subject body rarely repeats. Its tokens are matched against path segments and file stems (never file contents), and a match is the same relevance tier as an explicit path mention plus a +0.35 traceable component. Capitalised words before a colon ("Note:") and conventional type words alone ("docs:") are not scopes. On a TypeScript standard library (~900 files), whose subjects carry scopes throughout, holdout MRR moved 0.510 → 0.607; on a 10k-file Java service, whose subjects carry none, nothing changes.
- Document sections cast one vote per file. A long release-notes or changelog file matches most task vocabularies in dozens of sections, and each section used to occupy its own primary slot: a documentation task on the TypeScript standard library returned twenty sections of its release-notes file and nothing else.
- `git_cochange`: legacy aggregate weight for bounded local history signals. Git history contributes primary-context candidates when the task names an exact symbol or path it can anchor co-change on, or when a past commit subject is near-identical to the task (`history:subject-twin:<commit>` evidence: subject tokens with numbers and PR references stripped, Jaccard ≥ 0.75, at least three tokens; a file is voted for when at least half of the twins touched it). The 24th "publisher: Bump versions for release of X" is answered by the 23 before it, and a task with no twin gets no vote, so a corpus without the pattern is unchanged (TypeScript standard library holdout: bit-identical). Loose commit-message similarity to task prose is still not a retrieval signal (measured on a 10k-file repository with history, those votes cost 0.075 MRR and ~5 s per query).
  Current explanations use the finer-grained component names:
  `history_churn`, `ownership_risk`, `similar_change_overlap`, and
  `reviewer_affinity`.

Git-history indexing is local and enabled by default. Configure it in `ok.toml`:

```toml
[history]
enabled = true
max_commits = 500
max_files_per_commit = 40
```

Set `enabled = false` to skip history indexing entirely. Large commits above
`max_files_per_commit` are ignored so mass-formatting or generated-file commits
do not dominate co-change ranking. Their commit and file-touch records still
remain available inside the configured `max_commits` window.
History components are advisory and bounded: exact references, exact symbol
evidence, direct test coverage, and explicit boundary evidence keep larger
weights than historical heuristics.
- `validation_proximity`: test and validation-path proximity. Test targets whose names overlap the task vocabulary enter candidate fusion with heuristic authority only; a test never outranks source merely because its name shares a word with the task. Candidate fusion is reciprocal-rank fusion with k=10 rather than the literature's 60: our streams are one full-text ranker plus name-overlap hints, and at k=60 a lexical rank 2 was indistinguishable from rank 13, so two weak votes always beat one strong one (neutral on the 490-case corpus, restores the workflow benchmark's `test-selector`). In candidate fusion (profile `rrf_measured_v1`) the validation stream votes at half weight: a name overlap is weaker evidence than a full-text match on the task, and at equal weight two such votes outranked a lexical #2 hit. Measured neutral on the 490-case commit-derived corpus; repository overrides scale this prior rather than replace it.
- `memory_signal`: repo memory evidence when available.
- `path_quality`: penalties for generated or vendor paths.

Use `ok search --explain-ranking "query"` to inspect dominant signals for each
result. Use `ok eval` to compare baseline ranking, fused ranking, and signal
ablations with recall, MRR, and nDCG metrics.
