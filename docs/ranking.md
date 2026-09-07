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
- `git_cochange`: legacy aggregate weight for bounded local history signals. Git history contributes primary-context candidates only when the task names an exact symbol or path it can anchor co-change on; commit-message similarity to task prose is not a retrieval signal (measured on a 10k-file repository with history, those votes cost 0.075 MRR and ~5 s per query).
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
