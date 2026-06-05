# Embedding Providers

Semantic indexing defaults to the built-in local hash embedding provider. It is deterministic, normalized, and network-free. It is designed as a private baseline provider, not a hosted model integration.

Provider state is visible through:

```sh
ok semantic status
ok semantic status --json
```

External providers are blocked unless `semantic.external_provider_allowed = true` is configured. This keeps repository source local by default and makes any provider that could send code off-machine an explicit opt-in.

The current persisted manifest records provider, model, dimensions, distance metric, chunker version, index version, source commit, target counts, and vector count.
