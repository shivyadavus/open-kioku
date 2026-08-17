# Embedding Providers

Semantic indexing defaults to the built-in local hash embedding provider. It is deterministic, normalized, network-free, and remains the regression baseline. Neural profiles are optional local quality tiers rather than a required hosted dependency.

Provider state is visible through:

```sh
ok semantic status
ok semantic status --json
```

Status reports the selected provider/model and model-artifact provenance. External providers are blocked unless `semantic.external_provider_allowed = true` is configured. This keeps repository source local by default and makes any provider that could send code off-machine an explicit opt-in.

## Local profiles

Open Kioku provides these explicit local neural profiles:

- Qwen3 Embedding 0.6B
- Qwen3 Embedding 4B
- Qwen3 Embedding 8B
- Jina Embeddings v2 Base Code

Qwen profiles support bounded Matryoshka dimensions and are normalized after truncation. The Jina code profile uses its fixed 768-dimensional representation. Query and document preparation stay distinct; coding-retrieval instructions are applied to queries rather than prefixed to stored documents.

Neural model artifacts are stored under the Open Kioku model cache for the selected profile. A first download requires explicit model-download consent, and model download is refused when the repository security configuration denies network access. Reuse is guarded by provider/model/implementation identity, model-artifact digest, dimensions, and content hashes so changing a model artifact cannot silently reuse embeddings from a different artifact.

## Measured guidance

The checked-in retrieval benchmark compares the deterministic hash baseline with local neural profiles over the repository retrieval fixture and reports Recall@5, Recall@10, MRR, build time, mean/p95 query latency, vector bytes, model-cache size, peak process RSS where the platform exposes it, and host profile. For memory and cache comparisons, run one provider per process with `OK_CC5_BENCH_ONLY` so peak RSS is not inherited from a previously loaded model.

On the isolated 25-case Linux benchmark used for CC5 (4 vCPU AMD EPYC runner):

- `local-hash-384`: Recall@5 ≈ 0.787, Recall@10 = 0.94, MRR ≈ 0.891, p95 query latency ≈ 0.012 ms, peak RSS ≈ 68.6 MB.
- Qwen3 0.6B at 768 dimensions: Recall@5 = 0.90, Recall@10 = 1.0, MRR ≈ 0.913, p95 query latency ≈ 796 ms, local model cache ≈ 1.20 GB, peak RSS ≈ 3.61 GB.
- Jina code at 768 dimensions: Recall@5 = 0.94, Recall@10 = 1.0, MRR = 0.91, p95 query latency ≈ 36.3 ms, local model cache ≈ 1.29 GB, peak RSS ≈ 518 MB.

For that fixture and machine profile, **Jina code is the current measured quality/practicality recommendation**: it improved top-5 and top-10 retrieval over the deterministic baseline while using far less peak memory and substantially lower query latency than the measured Qwen3 0.6B profile. Qwen3 0.6B achieved the highest measured MRR by a small margin and remains useful when users deliberately choose that model family or want its retrieval tradeoff. The 4B and 8B Qwen profiles are available as higher-cost opt-in tiers, but Open Kioku does not claim laptop practicality for them without a corresponding benchmark result.

The isolated rerun also verifies the corrected Qwen cache ownership: its model artifacts are now present under the explicitly supplied Open Kioku cache root. Open Kioku resolves those files through an explicit local cache before constructing the FastEmbed/Candle model, rather than relying on the dependency's default home-cache location.

A secondary Apple-Silicon CI run showed the same retrieval ordering and the same broad latency tradeoff (Jina materially faster than Qwen). Treat all measurements as fixture- and machine-specific guidance rather than universal model rankings, and re-run the benchmark on the target repository and hardware when model choice materially affects latency or memory budgets.

## Reproducing the benchmark

The neural benchmark will not download a model unless consent is explicit:

```sh
OK_CC5_ALLOW_MODEL_DOWNLOAD=1 \
  cargo run --release -p open-kioku-embeddings --example retrieval_quality
```

To obtain isolated memory/cache measurements, select one profile per process, for example:

```sh
OK_CC5_ALLOW_MODEL_DOWNLOAD=1 \
OK_CC5_BENCH_ONLY=jina-embeddings-v2-base-code-768 \
  cargo run --release -p open-kioku-embeddings --example retrieval_quality
```

Set `OK_CC5_MODEL_CACHE` to choose a benchmark-only cache location and `OK_CC5_HOST_PROFILE` to label the hardware profile in the report.

The persisted semantic manifest records provider, model, embedding implementation, model-artifact digest, dimensions, distance metric, chunker version, index version, source commit, target counts, and vector count.
