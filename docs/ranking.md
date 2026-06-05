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
- `boundary_fit`: source-like files that are better primary edit candidates.
- `runtime_corroboration`: runtime traces or incidents when configured.
- `git_cochange`: local `git log --name-only` co-change and path-to-test co-run evidence from `.ok/index.sqlite`.

Git-history indexing is local and enabled by default. Configure it in `ok.toml`:

```toml
[history]
enabled = true
max_commits = 500
max_files_per_commit = 40
```

Set `enabled = false` to skip history indexing entirely. Large commits above
`max_files_per_commit` are ignored so mass-formatting or generated-file commits
do not dominate ranking.
- `validation_proximity`: test and validation-path proximity.
- `memory_signal`: repo memory evidence when available.
- `path_quality`: penalties for generated or vendor paths.

Use `ok search --explain-ranking "query"` to inspect dominant signals for each
result. Use `ok eval` to compare baseline ranking, fused ranking, and signal
ablations with recall, MRR, and nDCG metrics.
