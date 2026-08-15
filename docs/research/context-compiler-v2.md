# Context Compiler V2 — Research Direction

Status: research-backed product direction

## Why this matters

Repository intelligence only helps an agent if the right evidence is selected for the task. Open Kioku already indexes lexical, graph, history, runtime, validation, memory, and semantic signals, but the current context builder begins from lexical candidates and uses a file-count limit. That leaves measurable headroom in retrieval quality and token efficiency.

Recent repository-context benchmarks reinforce three design principles:

1. No single retrieval family wins across coding workflows.
2. High recall without precision creates expensive, distracting context.
3. Retrieval should be evaluated on what an agent needs next, under an explicit context budget, including cases where the correct result is to abstain.

Primary research references:

- Agent Retrieval Bench (2026): https://agent-retrieval-bench.github.io/
- ContextBench (2026): https://contextbench.github.io/
- CORE-Bench (2026): https://arxiv.org/abs/2606.11864
- Aider repository map: https://github.com/Aider-AI/aider/blob/main/aider/website/docs/repomap.md
- SCIP: https://github.com/scip-code/scip
- Codanna: https://github.com/bartolli/codanna
- Serena: https://github.com/oraios/serena

## Product thesis

Open Kioku should not compete as another search server. It should compile the smallest evidence-backed context package an autonomous coding agent needs to act safely.

```text
Task / failure / review comment / changed symbol
                  ↓
          task-intent classifier
                  ↓
  ┌────────────── candidate streams ──────────────┐
  │ lexical/BM25                                  │
  │ exact symbols + SCIP/native semantic edges    │
  │ semantic embeddings                          │
  │ graph neighborhood / repo-map importance     │
  │ test proximity                               │
  │ git co-change / similar historical changes   │
  │ runtime traces / incidents                   │
  │ architecture / contract evidence             │
  └───────────────────────────────────────────────┘
                  ↓
        evidence-aware fusion + dedupe
                  ↓
       diversity / redundancy control
                  ↓
       token-budget context optimizer
                  ↓
 ContextPack + evidence + omissions + quality report
```

## Non-negotiable properties

- Exact evidence outranks heuristic or semantic similarity.
- Candidate generation and ranking are separate stages.
- No candidate stream may silently override ambiguity.
- Context is selected under a token budget, not only a file-count limit.
- Retrieval results expose why each item was selected.
- The system can abstain when evidence is insufficient.
- Every ranking change is evaluated against frozen benchmark cases.
- Local-first behavior and source privacy remain the default.

## Evaluation dimensions

At minimum track:

- Recall@1/5/10/20
- Precision@k
- MRR
- NDCG where graded relevance exists
- file F1
- token-budgeted context yield
- useful-context tokens / total-context tokens
- no-gold false-positive rate
- abstention precision/recall
- retrieval latency
- incremental index latency
- per-language results
- per-task-family results

Task families should include:

- issue/request → implementation context
- code/edit → regression tests
- failure trace → implementation
- review comment → missing context
- edit anchor → ripple/impact context
- changed symbol → validation and callers

## Retrieval architecture

Prefer reciprocal-rank or calibrated score fusion across independent candidate streams rather than relying exclusively on hand-tuned additive constants from a single lexical candidate pool. Keep each source's evidence and rank visible.

The token-budget optimizer should prefer coverage and diversity rather than repeatedly selecting near-duplicate chunks from the same file or subsystem.

## Semantic search direction

The existing local hash embedding provider is useful as a deterministic offline fallback, not as the long-term quality target. After the retrieval benchmark exists, evaluate a real opt-in local neural provider (for example an ONNX model through fastembed-rs) and an ANN backend for larger indexes. Keep model download explicit and source text local.

Do not choose an embedding model or ANN backend before measuring it against the Open Kioku retrieval corpus.

## Commercial/product differentiation

The defensible product is not "we can search code." The stronger claim is:

> Open Kioku measures, explains, and constrains the context an agent uses before it changes production code — then verifies the change against the same evidence graph afterward.

A public quality report should eventually show retrieval precision/recall, impact precision, test-selection quality, plan-boundary quality, and verification quality on frozen repositories and commits.
