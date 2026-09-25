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
- `exact_reference`: exact symbol references, SCIP evidence, or symbol-name hits. A result is an exact symbol reference only through its typed `exact_reference_provenance`, the SCIP, tree-sitter or LSP source of the indexed occurrence it came from. `match_reason` is not read: impact deduplication gives a merged result the prose of whichever duplicate scored higher, so a lexical hit on the reference's range used to erase the marker, and any other result could carry the words. The substring fallback on "exact symbol reference" has been removed.
- `graph_proximity`: dependency or impact-graph proximity when available.
- `boundary_fit`: source-like files that are better primary edit candidates. In context-pack ordering the quality tier (source above docs and tests unless the task is about them) is applied before anchor relevance, and anchor relevance distinguishes a file that *names* the anchor in its path or a symbol (edit target) from one that merely mentions it in a snippet (reference): on a Go repository every mention of an identifier had outranked the best full-task lexical hit. Docs never qualify; test files qualify only when the task asks about tests (`test`, `tests`, `spec`, `coverage`, ...). Test paths are recognised by directory segment and file name (`src/test`, `src/internalClusterTest`, `__tests__`, `FooTests.java`, `FooIT.java`, `foo_test.go`, `foo.spec.ts`), never by substring, so `latest` is not a test and `javaRestTest` is.
- `runtime_corroboration`: runtime traces or incidents when configured.
- Named targets are never demoted: a file whose path or symbol names a primary task anchor (`Guard pool shutdown in ConnectionPoolMetricsIT`) keeps source quality even when it is a test or a doc. Three streams had ranked that test first and the docs/tests tier still pushed it below twenty source files that merely shared vocabulary. Measured neutral-to-positive: on Java (10k files), holdout gold recall 0.514 → 0.540; on Go (~800 files) and TypeScript (~900 files), within one rank of one case.
- `scope:entry-point:<path>`: when a commit scope names a directory, that directory's public entry file (`mod.ts`, `index.ts`, `mod.rs`, `lib.rs`, `__init__.py`; empty package markers excluded) is a candidate even when it shares no word with the task, because `feat(queue): stabilize PoolLease` edits `queue/mod.ts`, which does not mention PoolLease until the commit lands. Where it sits depends on whether any file the scope matched knows the task's own words: if none does, the entry file goes just below the group's best (it is the best guess); if one does, it goes last in the group (the real module wins). On TypeScript (~900 files), holdout R@5 0.744 → 0.816, R@20 0.798 → 0.869, MRR 0.607 → 0.650; Python (~4k files) and Go (~800 files) were bit-identical. Placing it second unconditionally had cost the Python corpus 0.011 MRR; placing it last unconditionally gained nothing anywhere.
- `identifier_lattice_anchor_boost`: task vocabulary reaches the repository's own identifiers through an identifier lattice built per query from indexed symbol names and file stems. A task identifier whose CamelCase/snake_case parts share a light stem (plural, `-ing`, `-ed`, trailing `e`) with a repository identifier's parts reaches that identifier (`ChannelsUtils` → `ChannelUtils`); a part of six or more letters that the repository spells nowhere is corrected by one edit (`LaederUtils` → `LeaderUtils`). Reached identifiers enter the lexical stream as extra terms and, when one *names* a file's path or symbol, place it in the explicit-path relevance tier — one tier below a name the task spelled exactly, and without the named-target exemption from the docs/tests quality demotion. The tier is what governs: ordering compares quality, then relevance, then authority, and only then score, so a hop sharing the top tier would have let a guess outrank an exactly-resolved definition. A hop from a trailing reference mention ("… similar to X") ranks with the reference anchors instead. The accompanying score component is +0.45 (+0.25 from a reference mention) and orders results that already tie; every hop carries the evidence line `identifier lattice: task term X reached repository term Y (stem|one edit)`. They never enter the exact-symbol stream, so a near miss cannot claim exact authority. A multi-part code identifier that reaches nothing in any spelling is reported as a caveat rather than silently dropped, and so is a hop withheld for ambiguity. No model, no network, no index change, and no re-index: the map is built per query from the loaded symbol and file lists (built once per query; +51 ms median end-to-end CPU on a 10k-file index over queries that actually reach, 112 ms in isolation when the one-edit pass runs).
  Three restrictions are measured, not assumed. Only whole identifiers expand — re-inflecting the task's prose words (`batches` → `batch`, `loading` → `load`) floods the lexical stream with generically-named files and cost 0.021 MRR on a Python library (~4k files) for no gain anywhere. An identifier some repository name already contains is left alone (`AudioEncoder` inside `AudioEncoderModel`), because substring retrieval reaches it and expanding only re-tiered a whole module. Hyphenated prose (`strip-tailed`) is not a code name, and treating it as one reached an incidental helper. Two further restrictions came from the same measurement: the repository's spelling re-inflects the task's parts rather than adding new ones (`MinFreeBlocks` is not `_with_min_free_blocks`), and a name more than four files carry (`num_slots`) still widens retrieval but never confers the named-target tier, mirroring how an ambiguous exact symbol anchor is demoted to a corroborating possibility.
  **How to measure this signal.** Commit-derived cases cannot exercise it: their queries are commit subjects written by the author who just edited the file, so they already spell identifiers in the repository's casing. On those, the lattice is neutral by construction — Go (~800 files), TypeScript (~900 files), and Python (~4k files) are bit-identical and Java (10k files) moves one case (R@5 0.552 → 0.560, MRR 0.491 → 0.499). The signal is measured instead on perturbed queries (`scripts/perturb-identifiers.py`), which rewrite one identifier per case into a form a task description plausibly uses. Over 259 perturbed cases from all four corpora, paired: R@5 0.656 → 0.699, MRR 0.548 → 0.584 (+0.036, 95% CI [+0.015, +0.062]), 15 cases better and 6 worse. By class — verb form +0.143 MRR (n=10), one-character typo +0.047 (95% CI [-0.010, +0.108]), plural/singular +0.044 ([+0.001, +0.097]), snake_case→CamelCase +0.018 ([+0.000, +0.054]), CamelCase→snake_case +0.014 ([-0.003, +0.047]); the casing classes are small because the identifier-aware tokenizer already splits both forms. These numbers are an upper bound rather than an estimate: the perturbation classes are the inverse of the lattice's own hypothesis class, so the corpus contains the near misses it can resolve and few it cannot. The 367 unperturbed cases in the same runs are the control and move by exactly 0.000, so the lattice costs nothing when a query is already exact. Pull-request descriptions, tried as a more natural proxy, are bit-identical on both corpora measured (Java 112 cases, Python 197): they are written by the same author about the same change and spell identifiers the same way, so they are not a substitute for perturbation.
- Generated files are indexed, flagged, and ranked last. The ingester used to skip any file with a "do not edit" or "automatically generated" banner; on the Python corpus (~4k files) that removed 394 files and a tenth of the files real commits went on to change. They are now indexed with `is_generated`, pushed behind every other candidate in each stream before the per-stream cap (so a regenerated implementation module cannot push its specification twin out of the lexical pool), and ranked at the lowest quality tier (signal `generated_file_demotion`, evidence line "generated file: ranked below hand-written source") unless the task's anchors name the file's own path; a symbol match cannot exempt it, because a generated file defines the same symbols as its source. the Python holdout against the same index (`ok context` scored by `scripts/score-context-cases.py`): R@5 0.645 → 0.665, R@20 0.741 → 0.767, gold recall 0.662 → 0.680. That pair was measured in PR #390's own A/B, before the re-index and before #378 landed; the re-frozen baseline (holdout R@5 0.663, R@20 0.759, `CHANGELOG.md` 4.0.0) is measured after both, and the two changes' shares could not be separated.
- Source files are never dropped by the secret-path rule. A path component containing "secret" or "credential" or ending in `_key` used to exclude the file; that removed 25 Java source files (integration tests for credential providers among them) from one repository. Every file is now blocked only by key-material extensions and the `.env`/`.aws`/`.ssh` entries. Data, config, and prose files named for a secret (`credentials.json`, `secrets.yaml`, `SECRETS.md`) are indexed with their secret-like values replaced by `[REDACTED]` (`docs/security-model.md`, "Secret-value redaction", #379); a key hard-coded inside a source file is indexed as written.
- `derived:<edge>`: a candidate's derived siblings — the generated module and the modular source its banner names, a test and the module it is named after, a `.d.ts` and its implementation (`DERIVED_FROM` edges, `docs/graph-model.md`) — join the ordered pack with the candidate's score and the persisted edge as evidence. A declared-origin edge is corroborating and a naming-convention pairing heuristic; neither is ever authoritative, because a banner is prose rather than parsed structure. The sibling takes its own quality tier: a test admitted for a source task opens the test block below every source, a source admitted for a test candidate closes the source block, and a generated sibling keeps the generated-file demotion unless the task names it. A sibling already in the pack is left where it is. A sibling is admitted, not ranked: it carries its own retrieval source kind (`derived_sibling`), scores below every result already in its quality tier, is placed at the end of that tier, and never takes the first rank. The distinct source kind matters because budget selection ranks graph and validation evidence above score and exempts it from the redundancy cull; a sibling has no score of its own that earned either, so it must not arrive wearing them. Measured on the four locally derived case files (626 cases, the same files as the region-widening measurement, not the frozen splits) against a control built from the same commit of `main` (`be5d6ae`), on a local workstation: Java (10k files, 116 cases) R@5 0.5431 -> 0.5603, R@20 0.6810 -> 0.6897, MRR 0.4901 -> 0.4994, gold recall 0.5484 -> 0.5570 — one case that had found no gold at all now ranks it first; Go (~800 files, 145 cases) R@5 0.7724 -> 0.7793; Python (~4k files, 197 cases) R@5 0.6701 -> 0.6751; TypeScript (~900 files, 168 cases) R@5 unchanged, MRR 0.6473 -> 0.6443, one case slipping from rank 1 to 2. Gold recall at 20 slips by one case on Go and Python. Every delta sits inside its bootstrap interval; the edge is worth keeping for the evidence it persists and the impact it feeds, and these numbers say it does not cost retrieval. Earlier placements that let a sibling enter at the head of its tier, or keep its origin's score, each cost R@5 on every corpus (a gold file at rank 5 moved to 6) and are what these two rules prevent. The edge earns its place as evidence an agent and the impact graph can follow, not as a ranking win.
- `commit_scope_path_boost`: a commit-style scope prefix — `docs(cache): …`, `feat(codec/base): …`, `pkg/layout: …`, `[Planner] …` — names the package or directory the change lives in, which the subject body rarely repeats. Its tokens are matched against path segments and file stems (never file contents), and a match is the same relevance tier as an explicit path mention plus a +0.35 traceable component. Capitalised words before a colon ("Note:") and conventional type words alone ("docs:") are not scopes. On TypeScript (~900 files), whose subjects carry scopes throughout, holdout MRR moved 0.510 → 0.607; on Java (10k files), whose subjects carry none, nothing changes.
- Document sections cast one vote per file. A long release-notes or changelog file matches most task vocabularies in dozens of sections, and each section used to occupy its own primary slot: a documentation task on the TypeScript corpus returned twenty sections of its release-notes file and nothing else.
- `git_cochange`: legacy aggregate weight for bounded local history signals. Git history contributes primary-context candidates when the task names an exact symbol or path it can anchor co-change on, or when a past commit subject is near-identical to the task (`history:subject-twin:<commit>` evidence: subject tokens with numbers and PR references stripped, Jaccard ≥ 0.75, at least three tokens; a file is voted for when at least half of the twins touched it). The 24th "chore: sync plugin versions for the 5.3.0 train" is answered by the 23 before it, and a task with no twin gets no vote, so a corpus without the pattern is unchanged (TypeScript holdout: bit-identical). Loose commit-message similarity to task prose is still not a retrieval signal (measured on a 10k-file repository with history, those votes cost 0.075 MRR and ~5 s per query).
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
History names no path the security rules exclude: a touch, rename or co-change
pair naming a secret-like or `[paths] deny` path is not stored, so no history signal
can surface one (see `docs/security-model.md`). A large commit is judged by every
path it touched, so withholding one does not bring it under `max_files_per_commit`.
History components are advisory and bounded: exact references, exact symbol
evidence, direct test coverage, and explicit boundary evidence keep larger
weights than historical heuristics.
- `validation_proximity`: test and validation-path proximity. Test targets whose names overlap the task vocabulary enter candidate fusion with heuristic authority only; a test never outranks source merely because its name shares a word with the task. Candidate fusion is reciprocal-rank fusion with k=10 rather than the literature's 60: our streams are one full-text ranker plus name-overlap hints, and at k=60 a lexical rank 2 was indistinguishable from rank 13, so two weak votes always beat one strong one (neutral on the 490-case corpus, restores the workflow benchmark's `test-selector`). In candidate fusion (profile `rrf_measured_v1`) the validation stream votes at half weight: a name overlap is weaker evidence than a full-text match on the task, and at equal weight two such votes outranked a lexical #2 hit. Measured neutral on the 490-case commit-derived corpus; repository overrides scale this prior rather than replace it. The validation stream reports itself unavailable instead of running and finding nothing in two cases, both decided from the indexed targets alone: an index holding no test target (`no test targets are indexed for this repository`), and one whose every target is a test the runner skips such as `test.skip` or `it.todo` (`every indexed test target is a disabled test the runner skips`). Deciding from targets rather than from a census of files keeps the diagnostics and the pack consistent: validation is never called unavailable while the pack carries a validation target. Disabled targets are dropped from the stream's candidates, the pack's `validation_plan.tests`, the `validation_availability` and `test_coverage` confidence counts, the test selector that serves `find_tests_for_change` and `ok tests` (which count what they withheld in `excluded` and, when nothing is left, say so in a caveat rather than returning a bare empty list), the selection tier (a disabled target stays Optional and says the runner skips it), and the indexed test count `ok status` reports (`quality.test_count`; the withheld targets are counted beside it in `quality.excluded_test_targets`, so the setup audit advises enabling skipped tests rather than indexing test files). A file of skipped tests therefore supplies no validation evidence to retrieval, planning, or selection. The trust report's missing-test list and the risk and validation requirements derived from it judge files by whether a test file pairs with them, not by whether its tests run. `code_to_test` requires validation evidence, and a required source that is unavailable is a repository-level absence: the pack keeps its primary context and carries the caveat `task-family required evidence: validation is unavailable in this repository`, `validation` negative evidence, and `validation_availability` 0.2. The `missing_required_evidence:validation` blocker is kept for a stream that ran over indexed targets and matched none of them. A repository whose targets are all inline `#[cfg(test)]` tests keeps the blocker: it has usable targets, and the stream ran over them.
- `memory_signal`: repo memory evidence when available.
- `path_quality`: penalties for generated or vendor paths.

Region widening is not a ranking signal. Once selection has ordered the pack, the first
three primary files have their selected regions widened - enclosing symbol, the file's other
task-ranked units, adjacent chunks - up to a per-file token cap
(`docs/context-pack-spec.md`, "Selection and region widening"). It changes what the pack
shows of a file, never which files or in what order: on the 626 locally derived cases across four
large public repositories R@5, R@20, MRR and gold recall are identical before and after, and
no case changed its rank or its top five.

It is not free. Showing more of each top file raises the median pack from
836-1,152 to 2,753-3,612 estimated tokens, about three times as many, and the 95th
percentile from 1,549-5,600 to 4,322-7,629. A caller on a path that enforces
`ContextBudget::default()` has 6,000 spendable tokens, and `region_files` x
`region_tokens_per_file` is 3,600 of them; on such a path widening stops early and these
figures do not transfer. What it buys, over the primary units alone
(`gold_line_yield_primary@8k`, the share of the lines the real commit changed that the pack
shows within 8k tokens): Java 0.216 -> 0.248, Go 0.207 -> 0.299, TypeScript 0.155 ->
0.335, Python 0.130 -> 0.203. The design is paired, so the per-case delta carries the
verdict: +0.032 [+0.013, +0.058], +0.092 [+0.057, +0.133], +0.181 [+0.138, +0.229] and
+0.073 [+0.046, +0.102] respectively, all excluding zero, with no case worse in any corpus.
Java's is the smallest and rests on 12 of 116 cases; it is also the corpus where the
per-file cap binds, with widened files pinned at ~1,190 of their 1,200 tokens. Widening cannot
add a gold file - it grows regions inside files selection already chose - and at 8k and 16k
tokens file yield is unchanged everywhere. At 4k it costs a little: widened regions consume
the budget before later gold files are reached, so `gold_file_yield_primary@4k` reads 0.530
against 0.537 on Java, 0.693 against 0.696 on Go and 0.602 against 0.608 on Python;
TypeScript is 0.691 in both arms. Read the Go figure with its corpus in mind: 50 of its 145
cases are near-identical release-bump commits whose gold is a single three-line region, so
it reads higher there than the frozen split would give. Measured at `48e64c9` on a local
workstation with `--workers 2`, both arms built from that commit and their reports committed
as `benchmarks/commit-derived/region-widening-ab.json`; not re-scored after the branch was
rebased onto current main. CHANGELOG.md carries the full provenance. Each widening step is an
evidence ref (`region:enclosing-symbol`, `region:ranked-unit`, `region:adjacent-unit`) on
the unit.

## Text relevance scale (advisory)

On the search path (`ok search` and MCP `search_code`, which share
`open_kioku_context::search::ranked_search`, plus `ok eval`, `ok prove`, and the retrieval
benchmark's `fusion` strategy), `text_relevance` is the raw boosted BM25 score. That score is unbounded. The other
signals are bounded, except `boundary_fit`'s 18.0 tier and the `path_quality` penalty, which is
a share of the raw score. A bounded signal can therefore reorder only candidates whose lexical
scores are nearly tied. Context packs are unaffected, because they fuse candidate streams by
rank.

`ok search` and `search_code` rank a candidate pool of `4 × (offset + limit)` per source,
clamped to 100..500 (`open_kioku_context::search::candidate_depth`). The benchmarks rank
`ranking_candidate_limit`, clamped to 100..200. The two are equal for any page ending at 50
results or sooner, which covers every benchmark case, so the frozen baselines measure the pool
the product uses for those pages. A larger page — `ok search --limit 170`, or a `search_code`
page reaching past result 50 — ranks a deeper pool than any benchmark measures.

`ok retrieval-bench` measures two alternatives beside `fusion`. They are reported under
`stream_ablations` and excluded from `benchmarks/retrieval-baseline.json` and the release
thresholds:

- `fusion_pool_max`: the candidate's lexical score, including Tantivy's query-variant boost, divided by the highest such score in the pool. Only
  candidates that carry a lexical component and are not semantic-only set that maximum: the
  Tantivy index's `bm25_relevance` or the in-memory fallback's `lexical_relevance`. The top
  lexical hit reads 1.0. The divisor is a per-query constant, so with every other weight at
  zero the order is the raw order, and the value depends neither on how many candidates were
  fetched nor on corpus size.
- `fusion_rank10`: `(k + 1) / (k + rank)` with k = 10. Rank counts the strictly higher lexical
  scores in the pool, so equal scores share a rank.

Both arms keep every weight at its default and the exact-identity tier unchanged. That includes
`boundary_fit`'s unrescaled 18.0 tier. At the default weight it contributes 4.5, more than the
whole scaled lexical range of at most 1.0, so a candidate that hits the tier outranks every
candidate that does not, whatever its lexical score, and arm order is largely tier order. The
arms therefore measure scaled text relevance beside today's tiers, not the ranker a default
change would ship, which rescales the tiers in the same change. In both arms:

- the `path_quality` penalty is a share of the scaled value;
- only a candidate that carries a lexical component and is not semantic-only is scaled. Any
  other candidate, such as a git co-change candidate or a semantic hit, carries no text
  relevance, never sets the scale, and records why in a weight-0 `text_relevance_excluded`
  component;
- a pool with no positive lexical score is left unscaled, recorded in a weight-0
  `text_relevance_unscaled` component even when the score is 0;
- the producer's score parts (`bm25_relevance` or `lexical_relevance`, and Tantivy's
  `query_variant_boost`) stay in the breakdown at weight 0. A weight-0
  `text_relevance_pool_max` component carries the divisor as its raw value, or
  `text_relevance_rank` the rank, with the scaled value as its normalized value, so the scale
  survives JSON output.

The scale is an internal ranking option, not an `ok.toml` key, and every shipped surface ranks
with the raw score. Lexical baseline ranking is never scaled.

First measurement, on the frozen 30-case corpus (39-file fixture, 25 positive cases, 10 of the
30 in holdout). It ran on a local workstation (macOS 13, x86_64), comparing a debug build of this
change on `4a88274` with a baseline binary built from `4a88274`. The frozen `lexical` and
`fusion` blocks were bit-identical between the two builds.

| Strategy | R@1 | R@5 | R@10 | MRR | F1@10 | No-gold FP | Dev MRR | Holdout R@5 | Holdout MRR |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `fusion` | 0.3733 | 0.8600 | 1.0000 | 0.7800 | 0.3543 | 0.0 | 0.7719 | 0.7222 | 0.8056 |
| `fusion_pool_max` | 0.3533 | 0.8933 | 1.0000 | 0.7733 | 0.3543 | 0.0 | 0.7544 | 0.7778 | 0.8333 |
| `fusion_rank10` | 0.3533 | 0.8933 | 1.0000 | 0.7713 | 0.3543 | 0.0 | 0.7518 | 0.7778 | 0.8333 |

Under both arms the same six positive cases change their best gold rank. Four improve by one
or two ranks. Two worsen, one from rank 1 to 2 and one from rank 2 to 4 under both arms, which
is the R@1 drop. One case moves MRR by about 0.02 here, so every difference is two or three
cases wide. The fixture carries no persisted graph, runtime, memory or history components and
no vendor paths, so it cannot show what scaling does for those signals. These numbers are not
a basis for changing the default.

## Confidence breakdown

The `confidence_breakdown` on a context pack or plan is separate from result ranking: it
never reorders anything, and ranking never reads it. `ConfidenceBreakdown::from_signals`
in `open-kioku-core` computes it from typed inputs (weights in parentheses):

- `task_relevance` (0.20): share of the task's content terms present anywhere in the selected context.
- `exact_references` (0.20): 1.0 when at least one selection is backed by exact provenance - an exact-authority retrieval trace, a result carrying `exact_reference_provenance`, or evidence whose `source_type` is `scip`, `tree_sitter` or `lsp` - and 0.25 otherwise. The impact report's `impact:<path>` record takes the strongest source among its exact references (SCIP, then LSP, then tree-sitter) and its message names every source; it used to be stamped `scip` whenever any exact reference existed, including tree-sitter and LSP ones. Prose is never consulted. The lexical stream's `query variant `...` matched local index` evidence line used to satisfy a substring test for `scip`, so a target file defining `scip_setup_report` made the whole pack `Exact`; the ranking crate had already removed the same leak from its own `exact_reference` signal.
- `evidence_density` (0.10): distinct evidence records over twice the selected primary files, capped at 1.0. Counting evidence *lines* saturated it for any non-empty pack.
- `validation_availability` (0.15): 1.0 when at least one validation target was selected, else 0.2.
- `test_coverage` (0.10): 1.0 when a selected target carries a runnable command, 0.6 when targets need manual commands, 0.2 with none.
- `negative_evidence` (0.15): 1.0 with no counted negative evidence, 0.3 with one or two items, 0.1 beyond. Counted items are the pack's `negative_evidence` entries in the `primary_context` and `anchor` scopes; `exact_references`, `validation`, and `runtime` absence is priced by the components above; `history` and `boundary` items are reported but not priced; and the `coverage` item is priced by the `index_coverage` caps rather than counted.
- `boundary_tightness` (0.15) and `runtime_corroboration` (0.05): the allowed-file bound and the typed `runtime_corroboration` score component on selected results.
- `index_coverage` (weight 0; present only with a coverage gap): the lowest indexed share among the languages `IndexCoverage::gaps` returns for the manifest the pack or plan read, carrying one evidence id per gap, `coverage:<language>:<cause>` (`coverage:rust:git_ignore`). The id names the `coverage.by_language` and `coverage.policy_excluded_by_language` entries that `repo_status` and `ok --json status` report, so the signal traces to the persisted manifest. Caps price it, not weight; see "Index coverage gaps".
- `index_coverage_selected_language` (weight 0; present only when the 0.74 cap applies): a majority coverage gap whose language matches the selected context's, carrying the matching gaps' evidence ids. `ok preflight` and MCP `plan_change {detail: "preflight"}` read this signal to withhold `safe_to_start`; see "Index coverage gaps".

Caps apply after the weighted sum, in this order: 0.35 with no primary context; 0.55 when
exact references, validation targets, and runtime signals are all absent; 0.74 without exact
evidence; 0.30 when no task term appears in the selected context, or 0.50 when fewer than
`WEAK_TASK_RELEVANCE` (0.34) of them do; 0.60 with counted negative evidence; 0.50 when
every named task identifier is unmatched by the selected context; 0.50 or 0.74 beside a
majority coverage gap under the conditions in "Index coverage gaps"; and 0.94 with any
caveat except a coverage caveat, including a plan's evidence-quality caveats attached
after scoring. The `Exact` label
additionally requires `exact_reference_count > 0`; otherwise the label stops at `High`.
`docs/context-pack-spec.md` defines the label semantics.

### Index coverage gaps

A file the index never read cannot disprove anything. When the index set aside much of a
programming language, a pack or plan whose selected context lacks something must not read
as though the code does not exist. `IndexCoverage::gaps` in `open-kioku-core` applies the
`ok doctor` coverage predicates to each programming language; config and prose languages
never qualify:

| `cause` | Fires when | `language_files` |
| --- | --- | --- |
| `git_ignore` | git ignore rules (`.gitignore`, `.git/info/exclude`, `core.excludesFile`) excluded at least `INDEX_COVERAGE_MISSING_FILES_WARN` (20) files of the language, and more files than it has considered | git-ignored plus considered files |
| `excluded_by_policy` | discovery found programming-language source and policy left none of it to consider; every such language is a gap under its dominant source | discovered files |
| `omitted` | the language is under `INDEX_COVERAGE_WARN_PERCENT` (98%) with at least `INDEX_COVERAGE_LANGUAGE_FLOOR` (50) considered files, or is missing at least 20 considered files (`too-large`, `binary`, unreadable) | considered files |

The index's own settings (`hidden`, `vendor`, `fast_mode`, `denied`, `[index] exclude`,
`.okignore`) produce no gap while some programming-language source remains considered,
because they record an intended exclusion. Listing git-ignored paths under `[index] exclude`
marks them intended and removes the gap, and with it the 0.74 cap and the preflight caution:
ingest checks `[index] exclude` before the git ignore rules, so those files are recorded as
`config_exclude`, which is not a gap source. That is the remedy for a repository whose
vendored or generated tree would otherwise cap every task in its language. A manifest written before per-language sources were
recorded yields no `git_ignore` gap; a manifest without coverage yields none. The doctor's
repository-wide ratio and walk-error warnings are not per-language and do not reach
confidence.

How ignored directories are counted decides what can be a gap. Discovery descends into
git-ignored directories and records each file as `git_ignore`, before the vendor detector
runs, so a git-ignored `venv/`, `env/`, `out/` or `site-packages` tree of `.py` or `.js` files
is a gap. Directories pruned by name (`.git`, `.ok`, `target`, `node_modules`, `dist`,
`build`, `.venv`) count once each in `pruned_dirs`; their files are never discovered, so they
never produce a gap.

A gap is always reported, and by itself changes no score or label:

- a caveat naming the share and the reason category, `index coverage: 25 of 27 rust source
  files (92.6%) are not indexed (git-ignore); an absence among them is not evidence`. Coverage
  caveats are exempt from the 0.94 any-caveat cap, including when a plan attaches caveats
  after scoring;
- the `index_coverage` component;
- `index_coverage_selected_language`, a second zero-weight component emitted only when the
  0.74 cap applies, carrying the matching gaps' evidence ids. `ok preflight` and MCP
  `plan_change {detail: "preflight"}` read that signal rather than any caveat text, and
  withhold `safe_to_start` in favour of `start_with_caution` when it is present: an index that
  never read most of the language being edited is an evidence-completeness fact of the same
  class as an unresolved import or an ambiguous edge. The general `Medium` to `safe_to_start`
  mapping is unchanged;
- one `coverage` negative evidence item covering all gaps. Its `inspected_sources` are
  `index_manifest.quality.coverage` and the gap evidence ids, and its next probe names the
  governing setting. It is not counted in `negative_evidence_count`;
- `coverage_gaps` in `repo_status` and `ok --json status`, and a `coverage gap:` line beside
  the coverage summary of `ok index`, `ok status` and `ok status --markdown`. The summary
  ratio is computed over *considered* files, so it reads near-100% in exactly the git-ignore
  case a gap describes; the verdict is printed beside it rather than left to the ratio.

An index that published no coverage record at all is a different fact from one that recorded
full coverage, and `ConfidenceSignalInput::coverage` distinguishes them: `CoverageInput::
Recorded(gaps)` against `CoverageInput::Unavailable`. Unavailable coverage adds the caveat
`index coverage is unrecorded: this index does not report which source files it omitted, so
an absence in it is not evidence of absence`, which caps at 0.94 like any other caveat, and a
`coverage` negative evidence item saying the same. It reaches a current binary through
`ok index --mode cross-project`, which publishes no coverage. An imported snapshot carries
whatever the exporting index recorded, so it is unavailable only when the export was. A third
state, `CoverageInput::Unreadable`, covers a coverage record that could not be read at all and
carries its own caveat, `index coverage could not be read from the manifest, so what this index
omitted is unknown`: that is a claim about the read rather than about the index, and the two
must not be worded alike. A context pack reports it and still builds; `ok plan` takes the full
manifest and still fails on that state.

A majority gap is one whose missing files are at least `COVERAGE_GAP_MAJORITY_SHARE` (half)
of its `language_files`. Every `git_ignore` and `excluded_by_policy` gap is a majority gap;
an `omitted` gap usually is not. A majority gap lowers confidence only beside a symptom it
could explain, and at most one cap applies:

- **Absence symptom: cap 0.50 (`Low`).** A named task identifier is unmatched by the selected
  context, or no primary context matched. The blocker `the task may name code in source the
  index excluded: rust (25 of 27 files, git-ignore)` is added, the `anchor` item's next probe
  points at the `coverage` item instead of saying the name does not exist in this
  repository, and a plan adds the risk reason `low confidence: named task anchor(s) … may be
  defined in source the index excluded: …`. An unmatched hyphenated task word is not a
  symptom.
- **Selection in the excluded language: cap 0.74 (below `High`).** A primary selection's
  language is the gap's language, taken from its symbol or its indexed file record: the right
  file may be among those the index did not read. The blocker `the selected context is in a
  language the index mostly excluded: …` is added. Whether the task spelled an identifier, and
  whether the selected context matched it, does not change this: the label follows what the
  index holds, not how the task was phrased. `Exact` is therefore unreachable for a selection
  in a language the index read a minority of.

Otherwise the gap changes nothing: a gap in a language the selection does not use, or one
below the majority share, is reported and priced at zero. On a Python repository that
git-ignores a `venv/` larger than `src/`, a task answered from `src/` reports the gap and
stops below `High`, because the excluded files are Python too and the index cannot say what
they hold. A Rust task on that same repository is unaffected.

The cap is deliberately blunt about *what* was excluded. A git-ignored `venv/` is a
third-party dependency tree and a git-ignored `generated/` is first-party source, and only
the second plausibly holds callers of the code under edit — but `policy_excluded_dirs` is
recorded repository-wide rather than per language, so a `python` gap cannot be attributed to
one or the other today. Until that is recorded per language, the conservative rule applies to
both.

The languages are looked up only when a majority gap exists: a selection's symbol supplies
its language when it has one, and otherwise its file record is read by path, once per
distinct primary path. A pack over an index without a majority gap reads nothing extra.

Use `ok search --explain-ranking "query"` to inspect dominant signals for each
result. Use `ok eval` to compare baseline ranking, fused ranking, and signal
ablations with recall, MRR, and nDCG metrics.
