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
- `identifier_lattice_anchor_boost`: task vocabulary reaches the repository's own identifiers through an identifier lattice built per query from indexed symbol names and file stems. A task identifier whose CamelCase/snake_case parts share a light stem (plural, `-ing`, `-ed`, trailing `e`) with a repository identifier's parts reaches that identifier (`CollectionsUtils` → `CollectionUtils`); a part of six or more letters that the repository spells nowhere is corrected by one edit (`HaederUtils` → `HeaderUtils`). Reached identifiers enter the lexical stream as extra terms and, when one *names* a file's path or symbol, place it in the explicit-path relevance tier — one tier below a name the task spelled exactly, and without the named-target exemption from the docs/tests quality demotion. The tier is what governs: ordering compares quality, then relevance, then authority, and only then score, so a hop sharing the top tier would have let a guess outrank an exactly-resolved definition. A hop from a trailing reference mention ("… similar to X") ranks with the reference anchors instead. The accompanying score component is +0.45 (+0.25 from a reference mention) and orders results that already tie; every hop carries the evidence line `identifier lattice: task term X reached repository term Y (stem|one edit)`. They never enter the exact-symbol stream, so a near miss cannot claim exact authority. A multi-part code identifier that reaches nothing in any spelling is reported as a caveat rather than silently dropped, and so is a hop withheld for ambiguity. No model, no network, no index change, and no re-index: the map is built per query from the loaded symbol and file lists (built once per query; +51 ms median end-to-end CPU on a 10k-file index over queries that actually reach, 112 ms in isolation when the one-edit pass runs).
  Three restrictions are measured, not assumed. Only whole identifiers expand — re-inflecting the task's prose words (`batches` → `batch`, `loading` → `load`) floods the lexical stream with generically-named files and cost 0.021 MRR on a Python library (~4k files) for no gain anywhere. An identifier some repository name already contains is left alone (`ImageBackbone` inside `ImageBackboneModel`), because substring retrieval reaches it and expanding only re-tiered a whole module. Hyphenated prose (`right-trimmed`) is not a code name, and treating it as one reached an incidental helper. Two further restrictions came from the same measurement: the repository's spelling re-inflects the task's parts rather than adding new ones (`MaxNewTokens` is not `_with_max_new_tokens`), and a name more than four files carry (`num_frames`) still widens retrieval but never confers the named-target tier, mirroring how an ambiguous exact symbol anchor is demoted to a corroborating possibility.
  **How to measure this signal.** Commit-derived cases cannot exercise it: their queries are commit subjects written by the author who just edited the file, so they already spell identifiers in the repository's casing. On those, the lattice is neutral by construction — Go (~800 files), TypeScript (~900 files), and Python (~4k files) are bit-identical and Java (10k files) moves one case (R@5 0.552 → 0.560, MRR 0.491 → 0.499). The signal is measured instead on perturbed queries (`scripts/perturb-identifiers.py`), which rewrite one identifier per case into a form a task description plausibly uses. Over 259 perturbed cases from all four corpora, paired: R@5 0.656 → 0.699, MRR 0.548 → 0.584 (+0.036, 95% CI [+0.015, +0.062]), 15 cases better and 6 worse. By class — verb form +0.143 MRR (n=10), one-character typo +0.047 (95% CI [-0.010, +0.108]), plural/singular +0.044 ([+0.001, +0.097]), snake_case→CamelCase +0.018 ([+0.000, +0.054]), CamelCase→snake_case +0.014 ([-0.003, +0.047]); the casing classes are small because the identifier-aware tokenizer already splits both forms. These numbers are an upper bound rather than an estimate: the perturbation classes are the inverse of the lattice's own hypothesis class, so the corpus contains the near misses it can resolve and few it cannot. The 367 unperturbed cases in the same runs are the control and move by exactly 0.000, so the lattice costs nothing when a query is already exact. Pull-request descriptions, tried as a more natural proxy, are bit-identical on both corpora measured (Java 112 cases, Python 197): they are written by the same author about the same change and spell identifiers the same way, so they are not a substitute for perturbation.
- Generated files are indexed, flagged, and ranked last. The ingester used to skip any file with a "do not edit" or "automatically generated" banner; on a Python ML library that removed 394 files and a tenth of the files real commits went on to change. They are now indexed with `is_generated`, pushed behind every other candidate in each stream before the per-stream cap (so a regenerated implementation module cannot push its specification twin out of the lexical pool), and ranked at the lowest quality tier (signal `generated_file_demotion`, evidence line "generated file: ranked below hand-written source") unless the task's anchors name the file's own path; a symbol match cannot exempt it, because a generated file defines the same symbols as its source. the Python ML library holdout against the same index (`ok context` scored by `scripts/score-context-cases.py`): R@5 0.645 → 0.665, R@20 0.741 → 0.767, gold recall 0.662 → 0.680.
- Source files are never dropped by the secret-path rule. A path component containing "secret" or "credential" or ending in `_key` used to exclude the file; that removed 25 Java source files (`BasicCredentialsProviderIT` and its kin) from one repository. Programming-language source is blocked only by key-material extensions and the `.env`/`.aws`/`.ssh` entries. Data, config, and prose files (`credentials.json`, `secrets.yaml`, `SECRETS.md`) keep the name rule, because chunk contents are not redacted today (#379); a key hard-coded inside a source file is indexed exactly as before, when only the file's name decided.
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

Region widening is not a ranking signal. Once selection has ordered the pack, the first
three primary files have their selected regions widened - enclosing symbol, the file's other
task-ranked units, adjacent chunks - up to a per-file token cap
(`docs/context-pack-spec.md`, "Selection and region widening"). It changes what the pack
shows of a file, never which files or in what order: on the commit-derived holdouts of four
large public repositories R@5, R@20, MRR and gold recall are identical before and after, and
no case of 626 changed its rank or its top five.

It is not free. Showing more of each top file raises the median pack from
836-1,152 to 2,753-3,612 estimated tokens, about three times as many, and the 95th
percentile from 1,549-5,600 to 4,322-7,629. A caller on a path that enforces
`ContextBudget::default()` has 6,000 spendable tokens, and `region_files` x
`region_tokens_per_file` is 3,600 of them; on such a path widening stops early and these
figures do not transfer. What it buys, over the primary units alone
(`gold_line_yield_primary@8k`, the share of the lines the real commit changed that the pack
shows within 8k tokens): Java 0.216 -> 0.248, Go 0.207 -> 0.299, TypeScript 0.155 ->
0.335, Python 0.130 -> 0.203. The design is paired, so the per-case delta carries the
verdict: +0.032 [+0.013, +0.058], +0.093 [+0.057, +0.133], +0.181 [+0.138, +0.229] and
+0.073 [+0.046, +0.102] respectively, all excluding zero, with no case worse in any corpus.
Java's is the smallest and rests on 12 of 116 cases; it is also the corpus where the
per-file cap binds, with widened files pinned at ~1,190 of their 1,200 tokens. File yield is
unchanged everywhere, as it must be - widening grows regions inside files selection already
chose and cannot add a gold file. Read the Go figure with its corpus in mind: 50 of its 145
cases are near-identical release-bump commits whose gold is a single three-line region, so
it reads higher there than the frozen split would give. Measured at `48e64c9` on a local
workstation with `--workers 2`, both arms built from that commit and their reports committed
as `benchmarks/commit-derived/region-widening-ab.json`; not re-scored after the branch was
rebased onto current main. CHANGELOG.md carries the full provenance. Each widening step is an
evidence ref (`region:enclosing-symbol`, `region:ranked-unit`, `region:adjacent-unit`) on
the unit.

Use `ok search --explain-ranking "query"` to inspect dominant signals for each
result. Use `ok eval` to compare baseline ranking, fused ranking, and signal
ablations with recall, MRR, and nDCG metrics.
