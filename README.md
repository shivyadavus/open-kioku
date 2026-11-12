<div align="center">

<img src="assets/logo.svg" alt="Open Kioku" width="88" height="88">

# Open Kioku

**Your coding agent shows its evidence before it edits, and its diff is verified against the plan it declared.**

A local index of your repository feeds a bounded plan; after the edit, `ok verify` checks the actual changed files against that plan. Nothing leaves your machine.

[![CI](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml/badge.svg)](https://github.com/shivyadavus/open-kioku/actions/workflows/ci.yml)
[![npm](https://img.shields.io/npm/v/open-kioku)](https://www.npmjs.com/package/open-kioku)
[![npm downloads](https://img.shields.io/npm/dm/open-kioku)](https://www.npmjs.com/package/open-kioku)
[![crates.io](https://img.shields.io/crates/v/open-kioku-cli)](https://crates.io/crates/open-kioku-cli)
[![crates.io downloads](https://img.shields.io/crates/d/open-kioku-cli)](https://crates.io/crates/open-kioku-cli)
[![License](https://img.shields.io/badge/license-Elastic--2.0-blue)](LICENSE)

[Website](https://www.openkioku.com) · [First win](#first-win-2-commands) · [What to expect](#what-to-expect) · [Install](#install) · [MCP tools](docs/mcp-tools.md) · [Architecture](docs/architecture.md)

</div>

---

<p align="center">
  <img src="assets/demo.gif" alt="Terminal recording: ok setup agent indexes a repository, ok context shows evidence and confidence, ok plan writes a boundary, an edit outside the boundary makes ok verify fail." width="920">
</p>

## First Win: 2 Commands

```sh
npm install -g open-kioku
ok setup agent cursor --repo . --apply
```

Use `claude` instead of `cursor` for Claude Code. One command indexes the repository, writes repository-scoped MCP configuration and agent guidance, and checks that the local server answers (run without `--apply` to preview; nothing is written). Then ask for evidence on a real task:

```sh
ok context "reap the doctor's MCP probe child process" --format markdown
```

This is the actual output on this repository, trimmed: `…` marks cut lines, and each primary unit is collapsed to its line range and first line. The commit that made this change touched exactly one file, and it is the first result:

```markdown
# Task: reap the doctor's MCP probe child process

## Confidence
- Overall: `Medium` (`0.74`)
- Caveats:
  - exact symbol/reference evidence is absent
  - runtime corroboration is absent
- Components:
  …
  - `exact_references` score `0.25`, weight `0.20`, contribution `0.05`
  …
  - `task_relevance` score `1.00`, weight `0.20`, contribution `0.20`
  …
## Retrieval
…
- Attempted: `lexical, document, exact_semantic, graph, validation, git_history, runtime`
- Succeeded: `lexical, document, exact_semantic, graph, validation, git_history`
…
- Exact-authority selections: `0`; ambiguity/unresolved signals: `0`
- Retrieval confidence: `Medium` (qualitative ContextPack confidence, not a calibrated probability)
- Caveats:
  - no runtime traces, logs, or incidents are ingested for this repository
  …
## Primary Context
### crates/open-kioku-cli/src/reports/status_setup_doctor.rs
Lines 1-107  `fn file_path_for_symbol(store: &dyn MetadataStore, symbol: &Symbol) -> anyhow::Result<PathBuf> {`
### crates/open-kioku-core/src/process.rs
Lines 66-180  `fn proc_status_peak_rss_bytes() -> Option<u64> {`
…
```

The label is `Medium`, not higher, for one reason the pack states: the task names no identifier the index resolves exactly and this repository has no SCIP index, so `exact_reference_count` is 0 (`exact_references` scores `0.25`; `Exact-authority selections` is `0`), which caps the score at 0.74 and the label at Medium. Runtime artifacts are absent too; that is the second caveat, and it lowers the score, not the band. `Exact` is reserved for packs with at least one exact-authority selection; a lexical match, however good, does not earn it. The task is phrased as the fixing commit's subject, so git history corroborates the first file alongside the lexical match. Every pack says which evidence streams ran, which succeeded, and what is missing. Missing evidence lowers the stated confidence; it is never papered over.

## What You Get

```sh
ok plan "change token expiration" --format json > plan.json   # context, impact, tests, edit boundary, caveats
# ...edit with your normal agent or editor...
ok verify --plan plan.json --git                               # the real diff against the declared boundary
```

`ok plan` (or the `plan_change` MCP tool) returns primary context with provenance, impact candidates split into structurally proven and heuristic, validation targets tiered by evidence, an edit boundary (allowed, caution, forbidden paths), and explicit caveats. `ok verify` reads the actual changed files and reports, for example, `[out_of_boundary] go/shipping/carrier.go: path is outside the saved plan boundary`. A green exit code from a test runner is not proof the right files changed; this is.

<p align="center">
  <img src="assets/demo-verify.gif" alt="Terminal recording: the allowed files from plan.json, one edit inside that boundary, one edit outside it, then ok verify prints Verification: Fail, reports [out_of_boundary] go/shipping/carrier.go, and exits 1." width="920">
</p>

## The Gate

Every verification step a coding agent is given today is an instruction to the model. This one runs.

```sh
ok gate install claude --apply     # once per repository; the Claude Code plugin ships the same hooks
```

That is the whole setup. The first prompt of a conversation plans an edit boundary in the background; later prompts widen it; every Edit, Write and shell write is judged against it; every stop that follows a change verifies the real change (files created with no hook included) and writes a **Change Dossier**: what was declared, what the harness reported, what was denied, what actually changed, what verification found, and what is still uncertain.

A session started from a prompt is **advisory**: it tells the agent which edits fall outside the boundary and records them, denies secret-like and generated paths, and its boundary holds nothing, because a plan built from a prompt cannot tell a task from a remark (measured: `"fix it"` plans with the same confidence as a real commit subject). The boundary is binding where someone declared it: `ok gate start "<task>"`, `mode = "enforce"` in `ok.toml`, or `--mode enforce` in CI. There a `FAIL` holds the agent, or fails the check, with the violations and the one remedy, `ok gate expand <path> --reason "<why>"`.

```sh
ok gate ci --base origin/main --task "$PR_TITLE" --write dossier.md   # a dossier for any pull request, no hooks; fails on forbidden paths
ok gate install git --apply        # any agent, any person: a pre-commit hook that refuses a staged secret
```

Measured on commit-derived sessions and three live Claude Code sessions; method, numbers and caveats in [`benchmarks/gate/`](benchmarks/gate/README.md). [`docs/gate.md`](docs/gate.md)

### Ripple: what the diff does not show

```console
$ ok ripple
Ripple: 2 site(s) no longer fit 1 changed declaration(s).

src/click/utils.py:62  function `_make_default_short_help`: signature changed
  - def _make_default_short_help(help: str, max_length: int = 45)
  + def _make_default_short_help(help: str, max_length: int, strict: bool)
  broken: 2 site(s)
    src/click/core.py:1275 (in Command.get_short_help_str)  text = _make_default_short_help(self.help, limit)
        does not pass `strict`, which is required now [by name]
    tests/test_utils/test_make_default_short_help.py:62 (in test_make_default_short_help)  out = click.utils._make_default_short_help(value, max_length)
        does not pass `strict`, which is required now [by name]
```

A change that alters a function's parameters, or removes or renames a declaration, breaks code the diff never shows. `ok ripple` finds the calls that no longer fit and the code that still names what is gone, in Rust, Python, TypeScript, JavaScript, Go, Java and C#, **with no index and nothing to set up**. The same hooks tell a coding agent right after the edit that caused it, and tell you at the stop what it left; `ok gate ci` puts the list in the pull request's dossier. It reads syntax, not types: a call is claimed only when it fit the old signature and does not fit the new one, and only when an import or a qualifier leads from it to the declaration.

Replayed with no index over the last 300 commits of 46 open-source repositories in seven languages (7,711 commits that change code): given only the file that declares each change, it named 913 broken sites; the real commit went on to change 896 of them, 12 of the other 17 were code the commit really left broken, and 5 were tests it commented out instead. On the complete merged commits it named 15 sites in 4 commits, all four real: commits on the main branch of a widely used library that still named something they had removed, one of them still in place. Those repositories are the ones its rules were written and corrected against, an independent review found 18 faults in its first form, and on five small tasks a frontier model, asked to update the callers, left nothing broken with or without the hook. So the hooks tell and do not hold unless you ask them to (`[gate] ripple = "hold"`).

[`docs/ripple.md`](docs/ripple.md) · [`benchmarks/ripple/`](benchmarks/ripple/README.md)

Underneath: exact definitions, references, and dependency paths from source (and optional SCIP) are authoritative. Lexical, semantic, history, test, and runtime signals can reorder retrieval; they cannot overwrite repository truth.

## What Changed in 5.0.0

Released 2026-11-12. Run `ok index` after upgrading: every analysis-semantics version moved, and a 4.0 index withholds relationship evidence and says so until it is rebuilt. Then `ok semantic index` if you use semantic search. Source builds need Rust 1.89. The upgrade notes at the top of the [changelog](CHANGELOG.md) list the JSON shapes and exit codes a script should check.

- **The gate (experimental).** `ok gate` holds a coding agent's edits to the boundary its plan declared, through Claude Code hooks, and writes a verified Change Dossier when it stops; `ok gate ci` writes one for any pull request, and `ok gate install git` puts the forbidden-path check in a pre-commit hook for every other agent and for people. A boundary nobody declared is reported, not enforced: replayed over this repository's last 20 merged commits, a binding boundary failed all 20, so by default only a forbidden path (a secret, key material, a dependency directory, generated output) fails. On a repository that is not this one, with a real agent, the gate did not change what the agent produced on four real commits, took about six seconds longer per task, and refused the one edit to a secret file ([`docs/gate.md`](docs/gate.md), [`benchmarks/gate/`](benchmarks/gate/README.md), [`benchmarks/agent-ab/outside-repository.md`](benchmarks/agent-ab/outside-repository.md)).
- **Answers an agent can afford to read.** `plan_change` with `detail: "brief"` and `ok preflight --format brief` give the start decision in a kilobyte or two: on this repository one plan is 155 KB as Markdown and the brief of the same task is 1.9 KB. `ok --json status` went from 1,771,248 bytes to 24,392 on this repository by summarising its notes.
- **Rust relationships proven across crates, re-exports and module layouts.** Calls and imports resolve across workspace crates, through `pub use`, struct fields, and `cfg`- and `#[path]`-selected modules: 1,765 authoritative edges added on this repository and thirteen published crates by one change alone, each where the previous build had no edge (#476). A glob-heavy crate of about 110 Rust files that ran for more than 15 minutes indexes in about 12 seconds (#659).
- **Impact analysis reads by importance and counts what it left out.** `ok impact` takes `--symbol` and `--since`, reports deleted files and downstream crates, and lists proven dependents first; the "list was cut" caveat fires on 5 of 55 files of this repository instead of 48, and workflow-benchmark impact recall went from 0.925 to 0.975 (#584).
- **Fewer name-match edges, and authority on every graph answer.** The symbol registry no longer links across languages, from comments or strings, or from locals: estimated precision of its unique-name edges rose from about 50% to 70% on a Go service, 51% to 77% on a Java module, 27% to 87% on a Python library and 13% to 97% on this repository, at a stated cost in correct edges (#582). `ok path`, `dependency_path` and graph queries label each hop's authority.
- **Secrets stay out of the index files.** Secret-like values in config, data and prose are stored as `[REDACTED]`, as is a private key pasted into source; content of a path later excluded is removed from the database, its write-ahead log, the vector store and context handles (#379, #676). `cargo audit` is clean, including the SQLite fix for RUSTSEC-2026-0459.
- **Coverage says what is missing.** Build and dependency directories are pruned on evidence and named, not on their name alone, and a coverage gap names its language, cause and directories. On this repository `ok doctor` went from `coverage 241 of 1,690 programming-language files indexed (14.3%)` to `241 of 241 (100.0%)` (#477, #661).
- **One ranking, repeatable.** `ok search` and MCP `search_code` share one ranked search (#448), and two indexes of one tree score identically: byte-identical `ok --json search` output for all 30 retrieval-benchmark queries, against 237 differing values on the previous build (#491).
- **C# symbols.** `.cs` files are parsed into namespaces, types and members (#680). Relationships, `using` directives and test discovery are not there yet (#681–#683).

## What to Expect

Retrieval is measured on the production path (`ok context`, the same builder behind `ok plan` and the MCP `build_context_pack` tool) on four real repositories, each indexed at a fixed base commit. Every case is a later commit: the query is its subject line, the answer is the source files it changed. Cases are split chronologically; both splits are gated nightly, and the table shows holdout.

| Corpus | Holdout cases | R@5 | R@20 | MRR |
|---|---:|---:|---:|---:|
| Java, about 10k files | 113 | 0.566 | 0.708 | 0.516 |
| Go application, ~800 files | 84 | 0.690 | 0.821 | 0.539 |
| TypeScript, ~900 files | 167 | 0.826 | 0.880 | 0.683 |
| Python library, ~4k files | 199 | 0.663 | 0.754 | 0.543 |

- **R@5** — the share of tasks for which at least one file the commit changed is in the first five results.
- **R@20** — the same within the first twenty results, roughly the whole context pack.
- **MRR** — the average of 1 / rank of the first correct file; 1.0 means it was always first, 0.5 is what you get if the first correct file were always second, or first half the time and never found the rest.

Read it plainly. On a Java repository of about ten thousand files, the right file is in the top five about half the time and in the pack about seven times in ten; on a TypeScript repository of about nine hundred files, in the pack nearly nine in ten and in the top five about four in five. That is the floor the agent starts from before it has looked at anything, and it is the number to watch. Exact lookups (definitions, references, dependency paths) and the plan → edit → verify loop sit on top of it.

These baselines were frozen on 2026-09-25 from one hosted Linux runner matrix run of commit `bd5d06ab`, and a second run of the same commit produced identical scores (timings aside). `.github/workflows/commit-derived-bench.yml` re-derives them nightly; the job fails when a watched metric falls more than 0.03 below its frozen baseline, or when a routed task family of 34 or more cases whose case membership is unchanged falls more than `max(0.03, 2/n)` below its own. Queries are commit subjects, not issue text, so the numbers are not comparable with published benchmarks that use issue text. Corpus descriptions, both splits, the scripts, and the regression policy: [`docs/retrieval-benchmark.md`](docs/retrieval-benchmark.md); frozen baselines: [`benchmarks/commit-derived/`](benchmarks/commit-derived/).

Two more measured facts:

- **When the task has no answer.** On the 30-case frozen fixture, none of the five no-gold tasks is presented as an answer: the no-gold false-positive rate is 0.0 (the CI ceiling is 0.25), and the advisory routed context-pack strategy in the same run (`cc4:routed_contextpack`) returns none of them above `Low`. A low-confidence pack still lists candidates; it tells the caller not to trust them rather than returning nothing. [`benchmarks/retrieval-baseline.json`](benchmarks/retrieval-baseline.json)
- **Optional local neural embeddings.** The default local neural profile (a 149M-parameter int8 model, pinned by digest) improved every metric on the Go and TypeScript corpora against a same-day control, by about +0.025 MRR, on 4-vCPU / 16 GB hosted runners. Real but modest; the lexical ranking fixes landed the same day were worth about four times as much. [`docs/embedding-providers.md`](docs/embedding-providers.md)

## Measured at Scale

Performance claims are observations tied to an identifiable build, published with method and caveats. The most recent end-to-end scale record validates the `3.1.0` release lineage at source commit `3959fdfb6ca27d0c279b635fca7fc1b7935d4889` on a large Java repository, on the same host and protocol as the previous public record. 4.0.0 changed the index storage format and has not been re-run on this corpus; the table describes 3.1.0. 4.0.0's own measured changes are listed with their commits and methods in [`CHANGELOG.md`](CHANGELOG.md).

| Measurement (v3.1.0 lineage, end to end) | Result |
|---|---:|
| Tracked source files / Java files | 16,537 / 12,580 |
| Indexed files / symbols / chunks | 13,607 / 247,499 / 248,107 |
| Graph nodes / edges | 402,844 / 1,522,135 |
| Cold structural index | 19m 28s |
| Exact class lookup, fresh process | 0.02–0.05s |
| Exact references / lexical search, fresh process | 0.74s / 0.24s |
| Exact-flat semantic build | 495,606 vectors in 58.8s; 0 failures |
| Persistent HNSW build | 495,606 vectors in 10m 19s; 0 failures |

Against `main` at `c96f61a` on the identical corpus and host ([methodology](docs/large-java-validation-2026-08-31.md)): per-command startup ~14s → sub-second, exact class lookup 13.9s (returning an incorrect `symbol not found`) → 0.02s with the correct class, cold structural index 40m 40s → 19m 28s. The repeat index reproduced identical totals and four parallel graph readers completed with zero lock failures. The repository identity is withheld, so this is a scale record rather than a replayable corpus: [machine-readable evidence](demo/proof/large-java-2026-08-31-main.json) · [methodology](docs/large-java-validation-2026-08-31.md) · previous record: [v3.0.4 evidence](demo/proof/large-java-3.0.4.json).

More artifacts: local semantic scale, 51,349 vectors, persistent HNSW auto-selected, 21.70s fresh build, 0 stale / 0 failed vectors ([`demo/proof/ann-50k-dogfood.json`](demo/proof/ann-50k-dogfood.json)); plan → edit → validate → verify through the policy-gated runner, 2 passed, 0 boundary violations, final verdict `warn` because stronger evidence was absent ([`demo/proof/verification-dogfood.json`](demo/proof/verification-dogfood.json)); a public repository audit, 4,600+ files, 46,000+ symbols, 8,900+ tests indexed in 33.1s ([`docs/large-repo-proof.md`](docs/large-repo-proof.md)).

These are local workstation timings, not universal guarantees.

## Install

| Channel | How |
|---|---|
| **npm** (recommended) | `npm install -g open-kioku` — the wrapper pulls `@open-kioku/{darwin-arm64,linux-x64,linux-arm64,win32-x64}` (sources under [`packages/`](packages/)) |
| crates.io | `cargo install open-kioku-cli` or `cargo binstall open-kioku-cli` |
| GitHub releases | Binaries with `SHA256SUMS`, `SBOM.cargo-metadata.json`, `PROVENANCE.json`, and GitHub build-provenance attestations ([`docs/release-trust.md`](docs/release-trust.md)) |
| Claude Code plugin | [`claude_plugin.json`](claude_plugin.json) and [`.claude-plugin/`](.claude-plugin/) |
| Cursor / Codex plugins | [`.cursor-plugin/`](.cursor-plugin/) · [`.codex-plugin/`](.codex-plugin/) |
| MCP directories | Glama ([`glama.json`](glama.json)) · Smithery ([`smithery.yaml`](smithery.yaml)) |
| From source | `git clone https://github.com/shivyadavus/open-kioku.git && cargo install --path open-kioku/crates/open-kioku-cli` |

## Connect an Agent

```sh
ok setup agent claude --repo . --apply    # Claude Code: index + .mcp.json + managed skill, then a live MCP check
ok setup agent cursor --repo . --apply    # Cursor: index + .cursor/mcp.json + managed rule
ok mcp install codex  --repo .            # Codex: prints the TOML server entry
ok mcp install gemini --repo .            # Gemini CLI: prints the JSON server entry
```

`ok setup agent --apply` is wired for `claude` and `cursor`; every other client listed by `ok mcp install --help` gets a read-only configuration snippet from `ok mcp install <client>`. The MCP server is local, read-only, and speaks stdio. It advertises 16 tools — one per question nothing else answers — each carrying usage guidance, input/output schemas, safety annotations, and routing categories, and a metadata regression test rejects new tools that omit any of it. Memory and runtime-error tools appear only once those features are configured; the architecture, history and ownership capabilities ship on the CLI (`ok architecture …`, `ok history …`, `ok contract show`).

Step-by-step guides: [Claude Code](https://www.openkioku.com/claude-code-setup.html) · [Cursor](https://www.openkioku.com/cursor-setup.html) · [Codex](https://www.openkioku.com/codex-setup.html) · [Gemini CLI](https://www.openkioku.com/gemini-cli-setup.html) · CI: [`open-kioku-action`](https://github.com/shivyadavus/open-kioku-action) ([`docs/github-action.md`](docs/github-action.md))

Every `ok mcp install` client, with its generated config shape and how to confirm the connection: [`docs/guides/cross-harness-setup.md`](docs/guides/cross-harness-setup.md)

## Why Local

- No hosted index and no source upload: everything lives under the repository's `.ok/` directory, and `ok prove` shares counts and scores without source snippets.
- MCP is read-only by default; source edits stay in your normal editor or agent harness.
- Command execution and model downloads are policy-gated, secret-like paths are blocked, and network denial fails closed rather than degrading silently.

[`docs/security-model.md`](docs/security-model.md) · [`SECURITY.md`](SECURITY.md) · [`docs/release-trust.md`](docs/release-trust.md)

## How It Is Measured

- Commit-derived corpora on real repositories, re-run nightly: [`docs/retrieval-benchmark.md`](docs/retrieval-benchmark.md) (derive with `scripts/commit-derived-cases.py`, score with `scripts/score-context-cases.py`, compare with `scripts/compare-commit-derived-report.py`).
- The 30-case frozen fixture with a held-out split and CI thresholds: `ok retrieval-bench . --cases-file benchmarks/retrieval-cases.json --min-cases 30`.
- Workflow, relationship, and contract suites: [`docs/workflow-benchmarks.md`](docs/workflow-benchmarks.md) · [`docs/relationship-benchmark.md`](docs/relationship-benchmark.md) · [`docs/contract-benchmarks.md`](docs/contract-benchmarks.md).
- Scale and dogfood records: [`docs/proof.md`](docs/proof.md) and [`demo/proof/`](demo/proof/).

Threshold changes are product changes and are reviewed as such; a threshold is never lowered to make CI green.

## More Than One Repository

Semantic retrieval is optional and local (`ok --repo . semantic index`, then `ok search "authorization expiry" --hybrid`); model acquisition needs explicit consent and is refused under network denial ([`docs/semantic-search.md`](docs/semantic-search.md), [`docs/vector-index.md`](docs/vector-index.md)). Index projects individually and link them into a workspace (`ok index --mode cross-project --workspace <dir>`, `ok architecture fleet`). Export and import known-good indexes for team and CI reuse (`ok --repo . snapshot export --quality best`, `ok --repo . index --from-snapshot auto`); personal memory is excluded from shared snapshots by default. Detect architecture, check policies, and create bounded change contracts (`ok --repo . architecture detect`, `ok --repo . contract create "update API boundary"`). Git history is on by default with a bounded window; runtime traces and coverage reports are opt-in local inputs that never outrank exact source truth.

## Language Support

Tree-sitter parsing and symbol extraction cover **Rust, Python, TypeScript/TSX, JavaScript/JSX, Go, Java, and C#**. C# records symbols and chunks only: its `using` directives, calls and type relationships are not resolved yet. YAML and JSON are parsed structurally; file/chunk indexing also covers TOML, SQL, Markdown, Terraform, and other repository text. Language-aware resolution adds scope, import, receiver/type, containment, and inheritance semantics where supported.

Exact Java definitions and references from a `scip-java` index: [`docs/guides/java-scip.md`](docs/guides/java-scip.md)

## Useful Commands

```sh
ok --repo . search "token expiration handler"
ok --repo . symbol definition PolicyGate
ok --repo . symbol refs PolicyGate
ok --repo . impact --file src/auth.rs
ok --repo . ripple --base origin/main
ok --repo . tests --changed src/auth.rs
ok --repo . context "change token expiration" --format markdown
ok --repo . plan "change token expiration" --format markdown
ok --repo . verify --plan /tmp/plan.json --git
ok --repo . history similar --task "change token expiration" --path src/auth.rs
ok prove . --task "change token expiration"
```

<details>
<summary>All 40 top-level commands</summary>

Current top-level commands (40): `init`, `index`, `snapshot`, `watch`, `status`, `doctor`, `setup`, `demo`, `search`, `semantic`, `symbol`, `explain`, `impact`, `ripple`, `path`, `tests`, `context`, `retrieve-context`, `plan`, `preflight`, `verify-boundary`, `verify`, `gate`, `contract`, `bench`, `workflow-bench`, `retrieval-bench`, `relationship-bench`, `contract-bench`, `eval`, `prove`, `adr`, `ui`, `architecture`, `history`, `patch`, `memory`, `mcp`, `scip`, and `graph`.

</details>

Full MCP tool reference: [`docs/mcp-tools.md`](docs/mcp-tools.md)

## Repository Layout

This is a 44-crate Cargo workspace with a strict downward dependency direction: CLI / MCP → agent intelligence (`context`, `impact`, `tests`, `plan`, `patch`, `gate`, `actions`) → code-intelligence kernel (`ingest`, `parse`, `tree-sitter`, `resolution`, `graph`, `architecture`) → storage and search (`storage-sqlite`, `search-tantivy`). `open-kioku-core` holds the evidence, graph, and report contracts; optional integrations (`scip`, `lsp`, `semantic`, `vector`, `qdrant`, `sentry`) return explicit disabled/unsupported diagnostics rather than degrading silently.

Architecture: [`docs/architecture.md`](docs/architecture.md) · Crate map: [`docs/crate-map.md`](docs/crate-map.md) · Storage: [`docs/storage-model.md`](docs/storage-model.md)

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
scripts/validate-docs.sh
ok retrieval-bench . --cases-file benchmarks/retrieval-cases.json --min-cases 30
ok workflow-bench . --cases-file benchmarks/workflow-cases.json --limit 10
```

Maintainer-led and source-available under Elastic-2.0; see [`CONTRIBUTING.md`](CONTRIBUTING.md) before opening a pull request.

---

<div align="center">

If Open Kioku improves your agent workflow, consider [starring the repository](https://github.com/shivyadavus/open-kioku).

</div>
