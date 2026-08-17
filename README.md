# Open Kioku (`ok`)

[![CI](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml/badge.svg)](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/open-kioku)](https://www.npmjs.com/package/open-kioku)
[![npm downloads](https://img.shields.io/npm/dm/open-kioku)](https://www.npmjs.com/package/open-kioku)
[![License](https://img.shields.io/badge/license-Elastic--2.0-blue)](LICENSE)

## Plan before edit. Verify after edit.

**Open Kioku is a local repository evidence and change-safety layer for coding agents.**

It builds a local evidence model of a repository, compiles the smallest useful context for a task, produces bounded change plans before edits begin, and verifies the resulting change against what the agent intended to modify.

```text
CODE + SYMBOLS + RELATIONSHIPS + TESTS + HISTORY + RUNTIME + DOCS + ARCHITECTURE + LOCAL SEMANTICS
                                      │
                                      ▼
                               EVIDENCE MODEL
                                      │
                                      ▼
                              CONTEXT COMPILER
                                      │
                                      ▼
                         PLAN → EDIT → VERIFY → PROVE
```

No hosted code index. No source upload. Read-only MCP tools by default. Optional semantic retrieval runs locally.

![Open Kioku quickstart](assets/open-kioku-quickstart.gif)

## First Win: 2 Commands

```sh
npm install -g open-kioku
ok setup agent cursor --repo . --apply
```

For Claude Code:

```sh
ok setup agent claude --repo . --apply
```

The setup command indexes the repository, installs repository-scoped guidance and MCP configuration, and checks that the local MCP server responds. Run it without `--apply` first to inspect the exact changes.

Then ask the agent for the change you need. Open Kioku gives it a pre-edit evidence routine instead of making it rediscover the repository from scratch for every task.

## What Open Kioku Understands

Open Kioku combines multiple evidence streams instead of treating repository understanding as a single search problem:

| Evidence | What it contributes |
|---|---|
| **Code & symbols** | Definitions, chunks, imports, occurrences, scopes, source ranges |
| **Relationships & impact** | Dependency paths, calls, references, types, inheritance, affected files/symbols |
| **Tests & coverage** | Validation candidates, test-to-code evidence, local coverage reports |
| **Git history** | Churn, co-change, ownership, reviewers, provenance, similar changes |
| **Runtime evidence** | Local traces, spans, logs, incidents, errors, failures |
| **Documentation** | Heading-aware repository documentation retrieval |
| **Architecture & contracts** | Boundaries, policies, public API/dependency constraints, change contracts |
| **Local semantics** | Optional local embeddings, hybrid retrieval, exact-flat and persistent ANN backends |

Exact evidence remains authoritative over heuristic evidence. Ambiguity is represented as ambiguity rather than silently promoted to a fact.

## Context Compiler

A coding task is routed through independent candidate streams and compiled into a bounded `ContextPack`:

```text
TASK
  │
  ├─ lexical / BM25
  ├─ exact symbol + reference evidence
  ├─ graph + impact evidence
  ├─ tests + coverage
  ├─ history
  ├─ runtime
  ├─ docs
  ├─ architecture + contracts
  └─ optional local semantic retrieval
          │
          ▼
 authority-aware fusion
 diversity / redundancy control
 token-budget optimization
          │
          ▼
 CONTEXTPACK + omissions + provenance + quality
```

Task-family routing, evidence provenance, blocker handling, token budgets, and retrieval quality are benchmarked through the real routed path rather than only through isolated search functions.

## What Your Agent Gets

```text
Explore → plan_change → edit → verify_change
```

Start with:

```sh
ok plan "change token expiration"
```

or MCP `plan_change`.

A plan can include:

- primary context with source identity and evidence provenance
- impact candidates
- likely validation targets
- edit boundaries
- explicit missing-evidence caveats
- confidence and quality signals

After the edit, verification checks the actual change against the plan rather than treating a successful command exit as proof that the right files changed.

## Dogfooded Proof

The homepage includes proof artifacts produced from a pinned `main` build at commit `acbc5bcb387551501b3bc350247d25c133116d75`.

### Local semantic scale

A synthetic offline repository produced:

- **51,349 semantic vectors**
- **25,673 symbols**
- **25,676 chunks**
- persistent local HNSW selected automatically above the crossover
- **21.70s** fresh semantic build
- about **554 MB** peak RSS during that build
- **0 stale / 0 failed** vectors
- successful fresh-process reopen of the persisted ANN index

See [`demo/proof/ann-50k-dogfood.json`](demo/proof/ann-50k-dogfood.json).

### Plan → edit → validate → verify

The same pinned build was exercised through a real sandbox workflow. Open Kioku ran `cargo test` through its policy-gated validation runner, recorded the validation attestation, observed **2 tests passed / 0 failed**, and found **0 boundary violations**. The final verdict still remained `warn` because stronger supporting evidence was absent.

That behavior is intentional: passing tests do not manufacture certainty that the available evidence does not support.

See [`demo/proof/verification-dogfood.json`](demo/proof/verification-dogfood.json).

A separate public-repository audit indexed 4,600+ files, 46,000+ symbols, and 8,900+ tests locally in 33.1s. Methodology, revisions, caveats, and language limitations are recorded in [`docs/large-repo-proof.md`](docs/large-repo-proof.md).

## Local Semantic Retrieval

Semantic search is optional. The default repository-intelligence workflow does not require a hosted embedding service.

When enabled, Open Kioku can build embeddings locally, combine semantic and lexical retrieval, persist the semantic manifest and model provenance, and select between an exact-flat correctness oracle and a persistent ANN backend based on the configured/indexed scale.

```sh
ok --repo . semantic status
ok --repo . semantic index
ok --repo . search "authorization expiry" --hybrid
```

Model acquisition is explicit and policy-controlled. Network-denied execution fails closed rather than silently reaching a hosted service.

See [`docs/semantic-search.md`](docs/semantic-search.md), [`docs/vector-index.md`](docs/vector-index.md), and [`docs/embedding-providers.md`](docs/embedding-providers.md).

## See the Proof on Your Repo

```sh
ok prove . --task "the feature you're working on"
ok prove . --task "the feature you're working on" --html
```

`ok prove` creates a shareable report with indexed counts, task scores, validation signals, and caveats while intentionally omitting source snippets.

For pull requests, the opt-in [`open-kioku-action`](https://github.com/shivyadavus/open-kioku-action) can attach a privacy-safe preflight artifact:

```yaml
permissions:
  contents: read

steps:
  - uses: actions/checkout@v4
  - uses: shivyadavus/open-kioku-action@v1
    with:
      task: "change token expiration"
      verify: true
```

See [`docs/github-action.md`](docs/github-action.md).

## Install

### npm (recommended)

```sh
npm install -g open-kioku
ok --version
```

### cargo-binstall

```sh
cargo binstall open-kioku-cli
```

### crates.io

```sh
cargo install open-kioku-cli
```

### From source

```sh
git clone https://github.com/shivyadavus/open-kioku.git
cd open-kioku
cargo install --path crates/open-kioku-cli
```

## Set Up a Repository

```sh
ok init /absolute/path/to/repo
ok index /absolute/path/to/repo
ok doctor /absolute/path/to/repo
ok status /absolute/path/to/repo --markdown --write ok-status.md
```

Open Kioku writes repository intelligence under `.ok/`, including SQLite metadata/graph state and Tantivy lexical search data. Source files are not rewritten by indexing.

Keep the index current while editing:

```sh
ok watch /absolute/path/to/repo
```

## Connect an Agent

Repository-scoped onboarding:

```sh
ok setup agent claude --repo /absolute/path/to/repo --apply
ok setup agent cursor --repo /absolute/path/to/repo --apply
```

Manual MCP configuration is available for the supported client matrix:

```sh
ok mcp install cursor --repo /absolute/path/to/repo
ok mcp install claude --repo /absolute/path/to/repo
ok mcp install codex --repo /absolute/path/to/repo
ok mcp install gemini --repo /absolute/path/to/repo
ok mcp install windsurf --repo /absolute/path/to/repo
ok mcp install trae --repo /absolute/path/to/repo
ok mcp install opencode --repo /absolute/path/to/repo
ok mcp install zed --repo /absolute/path/to/repo
```

The default MCP server is local, read-only, and communicates over stdio.

Agent setup guides: [Claude](https://www.openkioku.com/claude-code-setup.html) · [Cursor](https://www.openkioku.com/cursor-setup.html) · [Codex](https://www.openkioku.com/codex-setup.html) · [Gemini CLI](https://www.openkioku.com/gemini-cli-setup.html).

## Multi-Project Intelligence

Index projects individually, then link them into a workspace without reparsing source:

```toml
[workspace]
projects = [
  { name = "service-a", repo = "../service-a" },
  { name = "service-b", repo = "../service-b" },
]
```

```sh
ok index --mode cross-project --workspace /absolute/path/to/workspace
ok architecture fleet --workspace /absolute/path/to/workspace
```

## Index Snapshots

Known-good indexes can be exported/imported for local team and CI reuse:

```sh
ok --repo . snapshot export --quality best
ok --repo . snapshot doctor
ok --repo . snapshot import
ok --repo . index --from-snapshot auto
```

Personal memory and compressed-context state are excluded from the shared index snapshot by default.

## History, Runtime, and Validation Evidence

Git history is local and enabled by default with a bounded window. It contributes typed commit metadata, file touches and renames, co-change, churn, provenance, ownership, reviewer, and similar-change signals.

Runtime evidence is opt-in: local JSONL traces/logs/incidents/errors can be placed under `.ok/runtime/` or `.ok/analysis/runtime/` and re-indexed.

Validation evidence is opt-in: Open Kioku can ingest JUnit XML, lcov, Cobertura XML, JaCoCo XML, and coverage.py XML/JSON from common local report directories and map covered lines back to indexed files, symbols, and plausible tests.

These sources contribute evidence; they do not outrank exact source/reference truth.

## Architecture, Contracts, and Verification

Open Kioku can detect architecture, evaluate policies, create bounded change contracts, and verify dependency/API/boundary constraints around a change.

```sh
ok --repo . architecture detect
ok --repo . architecture overview
ok --repo . architecture policy check --json
ok --repo . --json contract create "update API boundary"
ok --repo . contract verify --id <contract-id> --changed src/api.rs
ok --repo . verify --plan /tmp/plan.json --git
```

## Benchmarks

Quality is treated as a measurable product surface:

```sh
ok retrieval-bench . --cases-file benchmarks/retrieval-cases.json --min-cases 30
ok workflow-bench . --cases-file benchmarks/workflow-cases.json --limit 10
ok eval . --case "auth flow=src/auth.rs,tests/auth_flow.rs"
```

The frozen retrieval corpus and regression policy are documented in [`docs/retrieval-benchmark.md`](docs/retrieval-benchmark.md).

## Security Model

- read-only MCP by default
- no hosted repository index
- no source upload
- no hosted embeddings required for the core workflow
- optional semantic inference stays local
- secret-like paths are blocked by policy
- command execution is policy-gated
- source edits remain in the normal editor/agent harness
- network denial is supported and failures are explicit

See [`docs/security-model.md`](docs/security-model.md) and [`SECURITY.md`](SECURITY.md).

## Language Support

Tree-sitter parsing and symbol extraction covers Rust, Python, TypeScript/TSX, JavaScript/JSX, Go, and Java. YAML and JSON are parsed structurally. File/chunk indexing also covers repository text and configuration formats including TOML, SQL, Markdown, and Terraform.

Language-aware resolution adds scope, import, receiver/type, containment, and inheritance semantics where supported. Exact-reference indexes can further raise authority and precision.

## Useful Commands

```sh
ok --repo . search "token expiration handler"
ok --repo . symbol definition PolicyGate
ok --repo . symbol refs PolicyGate
ok --repo . impact --file src/auth.rs
ok --repo . tests --changed src/auth.rs
ok --repo . context "change token expiration" --format markdown
ok --repo . plan "change token expiration" --format markdown
ok --repo . preflight "change token expiration"
ok --repo . verify --plan /tmp/plan.json --git
ok --repo . history similar --task "change token expiration" --path src/auth.rs
ok --repo . semantic status
ok --repo . graph schema
ok prove . --task "change token expiration"
```

Current top-level commands: `init`, `index`, `snapshot`, `watch`, `status`, `doctor`, `setup`, `demo`, `search`, `semantic`, `symbol`, `explain`, `impact`, `path`, `tests`, `context`, `retrieve-context`, `plan`, `preflight`, `verify-boundary`, `verify`, `contract`, `bench`, `workflow-bench`, `retrieval-bench`, `contract-bench`, `eval`, `prove`, `adr`, `ui`, `architecture`, `history`, `patch`, `memory`, `mcp`, `scip`, and `graph`.

Full MCP tool reference: [`docs/mcp-tools.md`](docs/mcp-tools.md).

## Repository Layout

Open Kioku is a Rust workspace. Important crates include:

- `open-kioku-cli` — `ok` CLI and top-level product surface
- `open-kioku-mcp` — local JSON-RPC MCP server
- `open-kioku-core` — evidence, graph, report, and relationship authority contracts
- `open-kioku-ingest` — indexing pipeline and evidence ingestion
- `open-kioku-resolution` — scope/receiver/type-aware semantic resolution
- `open-kioku-context` — routed candidate streams and ContextPack compilation
- `open-kioku-graph` — evidence graph and query layer
- `open-kioku-semantic` — local semantic indexing and hybrid retrieval
- `open-kioku-vector` — exact-flat oracle and persistent local ANN backend
- `open-kioku-plan` — evidence-backed pre-edit planning
- `open-kioku-impact` — impact analysis
- `open-kioku-tests` — validation target selection
- `open-kioku-architecture` — architecture detection and policy evaluation
- `open-kioku-contract` — change-contract schema and validation
- `open-kioku-patch` — post-edit verification
- `open-kioku-storage-sqlite` — local persistence

Architecture: [`docs/architecture.md`](docs/architecture.md) · Crate map: [`docs/crate-map.md`](docs/crate-map.md) · Storage: [`docs/storage-model.md`](docs/storage-model.md)

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo test -p open-kioku-cli --test cli_smoke
ok retrieval-bench . --cases-file benchmarks/retrieval-cases.json --min-cases 30
ok workflow-bench . --cases-file benchmarks/workflow-cases.json --limit 10
```

## Contributing

Issues and pull requests are welcome. See [`CONTRIBUTING.md`](CONTRIBUTING.md).

---

If Open Kioku improves your agent workflow, consider [starring the repository](https://github.com/shivyadavus/open-kioku).
