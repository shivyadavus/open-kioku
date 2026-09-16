# Changelog

All notable changes to Open Kioku are documented in this file.
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Breaking

- `repo_status` and `ok --json status` return `quality.quality_notes` as `{total, by_kind, sample}` and `quality.skipped_paths` as `{total, by_reason, sample}` (previously full lists of quality notes and skipped paths), each `sample` at most 20 entries drawn round-robin across kinds or reasons, instead of the full lists; `detail: "full"` (MCP) and `ok status --full` (JSON, Markdown, and text) return every entry, and the manifest still stores them all. Each quality note is now `{kind, message}` with the kind assigned by its producer (`discovery`, `scip`, `exact_references`, `index_mode`, `import_resolver_caveat`, `import_resolver_cap`, `symbol_registry_caveat`, `symbol_registry_unresolved`, `relationship_resolution`); notes stored by an earlier index deserialize as `unclassified`. Measured on this 380-file repository after a full index: `ok --json status` went from 1,771,248 bytes (9,767 notes, 3,286 skipped paths) to 24,392 bytes with the summaries (`--full`: 2,577,421 bytes, the typed notes being larger than the bare strings).
- `ok verify` without `--git`, `--diff`, `--changed` or `--since-plan` is a usage error with exit code 2, a diff that names no file exits 2 with `invalid input: …` (`OkError::InvalidInput`), and `verify_change` returns JSON-RPC `-32602` for the same input (previously exit 1 and `-32000`).
- `open-kioku-core`: `IndexCoverage::record_policy_exclusion` now takes the file's language as its first argument, so `policy_excluded_by_language` is recorded with `policy_excluded_by_source` and cannot drift from it. Migrate `coverage.record_policy_exclusion(source, top_dir)` to `coverage.record_policy_exclusion(&language, source, top_dir)`.
- `open-kioku-core`: `ConfidenceSignalInput` gains `named_anchor_count`, `unmatched_anchors` and `weak_anchors` and no longer derives `Copy`; `named_anchors`, `unmatched_named_anchors` and `weak_named_anchors` are new public functions (compared with 4.0.0).
- `open-kioku-ranking`: `RankingOptions` gains `text_relevance_scale` (default `Raw`, no behaviour change); breaking only for crates that build `RankingOptions` with a struct literal without `..Default::default()`. The new `TextRelevanceScale` enum is `#[non_exhaustive]`, so a downstream `match` on it needs a wildcard arm.
- `open_kioku_core::SearchResult` has a new public field, `exact_reference_provenance: Option<EvidenceSourceType>`, so struct-literal construction outside the workspace no longer compiles. Set `exact_reference_provenance: None` on results that are not produced from an indexed symbol occurrence (`SearchResult` does not implement `Default`). The field deserializes as `None` when absent and is omitted from JSON when `None` (#438).
- `open_kioku_impact::is_exact_reference_result` is removed; use `SearchResult::is_exact_reference()`, which reads the typed provenance instead of the `match_reason` prefix (#438).
- `open_kioku_core::SyntaxFacts` gains the public field `module_declarations: Vec<ModuleDeclarationSite>`, and `open_kioku_semantic_model::ImportBinding` gains `rule: ImportBindingRule` (a `#[non_exhaustive]` enum), so downstream code that builds either with a struct literal stops compiling; `SyntaxFacts::default()` and `..Default::default()` are unaffected, and serialized data without the fields still deserializes (#453).
- Caller-argument mistakes are `OkError::InvalidInput`: JSON-RPC `-32602` on MCP and exit code 2 on the CLI. A plan the tool generated itself that cannot become a contract, and any failure of the index, store or filesystem, stay `-32000` and exit 1. Each case, previous → now:
  - Blank `search_code` query, blank `ok search` query: `-32000`, exit 1 → `-32602`, exit 2. Both now say `search requires a non-empty query` (previously `` `search_code` requires a non-empty `query` `` and `` `ok search` requires a non-empty query ``).
  - Unknown `retrieve_context` or `ok retrieve-context` handle: `-32000`, exit 1 → `-32602`, exit 2.
  - Unknown `contract_id` on `verify_change`, and unknown id on `ok contract verify --id`, `ok contract explain --id`, `ok contract show` and `ok contract export`: `-32000`, exit 1 → `-32602`, exit 2, with `` invalid input: no contract `<id>` is stored for this repository; … `` (previously `Contract not found: <id>`). A `get_definition` or `get_references` name the index does not hold stays `-32000`: it reports index content, not a bad argument.
  - Unknown `detail` on `repo_status` or `plan_change`: `-32000` → `-32602`. `plan_change` checks it before the relationship-evidence check, and `repo_status` rejects it on a repository that is not indexed, where it was ignored.
  - Plan the contract builder rejects for lacking evidence references or files, when supplied (`plan_change` `plan` or `plan_json` with `persist: true`; `ok contract create --plan` or `--plan-json`): `-32000`, exit 1 → `-32602`, exit 2.
  - Unknown `mode` on `search_code`, unknown `kind` on `get_references`: `-32000` → `-32602`. The CLI equivalents are clap values and already exited 2.
  - Missing required argument, or argument of the wrong JSON type, on any tool: `-32000` → `-32602`.
  - `plan`, `plan_json`, `contract`, `contract_json`, `verification`, `verification_json` or `validation_attestations` that does not decode: `-32000` → `-32602`. A plan file that reads but does not decode (`ok verify --plan`, `ok verify-boundary --plan`, `ok contract create --plan`), `--plan-json`, and `--contract` or `--contract-json` that does not decode: exit 1 → exit 2. A file that cannot be read still exits 1.
  - Selector given none or several of its inputs (`task`/`plan`/`plan_json` with `persist: true`; `contract_id`/`contract`/`contract_json`; the CLI's TASK/`--plan`/`--plan-json` and `--id`/`--contract`/`--contract-json`), and `write_attestation` or `--write-attestation` without a stored contract: `-32000`, exit 1 → `-32602`, exit 2.
  - Unknown `format` on a contract output or verification explanation: `-32000` → `-32602`.
  - Blank `regex_search` pattern: `-32000` → `-32602`. `regex_search` or `ok search --regex` pattern that does not parse: `-32000`, exit 1 → `-32602`, exit 2, with `invalid input: regex parse error: …` (previously `search error: regex parse error: …`); the `regex_search_invalid_pattern.json` golden snapshot moves with it.

  Every message above gains the `invalid input: ` prefix. In `tools/list`, the descriptions of `repo_status` and `plan_change` `detail`, `search_code` `mode`, `regex_search` `pattern` and `get_references` `kind` say invalid params instead of tool error. (#465)

### Added

- Per-task-family metrics on the commit-derived benchmarks (`docs/retrieval-benchmark.md`, "Per-task-family breakdown"). `scripts/score-context-cases.py` adds a `by_task_family` section that reports every aggregate metric, its bootstrap interval, and case coverage for each routed family (`retrieval_diagnostics.routing.task_family`, named by `TaskFamily`), and each row records its `task_family`; existing report keys are unchanged. A family with fewer than 34 scored cases is marked `insufficient` and never gated. `scripts/compare-commit-derived-report.py` gates each sufficient family on R@5, R@20, MRR, and `gold_recall@20` with the aggregate's exit status once a baseline carries the section (membership and tolerance rules in the next entry). No frozen baseline carries it yet, so per-family numbers are informational until the next re-freeze. The scorer output, the compare output, and the nightly job summary print each family with its 95% intervals under a note that families are the router's labels, and the summary's gate column comes from the compare script. The scripts' unit tests (`scripts/tests/test_context_yield.py`, `scripts/tests/test_commit_derived_families.py`) run in the Benchmarks workflow (#388).
- Per-family membership, tolerance, and a validated freeze for the commit-derived baselines (`docs/retrieval-benchmark.md`, "Per-task-family breakdown"):
  - **Membership.** Each `by_task_family` family records `membership_fingerprint`, `sha256:` and a SHA-256 over the sorted positions of its cases in the split file and the split's case count (no commit hashes, paths, or subjects); `scripts/reduce-benchmark-report.py` allowlists exactly that format.
  - **Gate.** `scripts/compare-commit-derived-report.py` gates a family only when its fingerprint matches the baseline's and prints `membership changed` otherwise. A gated family's tolerance is `max(0.03, 2/n)` for a baseline family of `n` cases (0.0588 at the 34-case minimum, 0.03 from 67 cases), and each watched metric, `gold_recall@20` included, is printed with the baseline interval, delta, tolerance, and pass or REGRESSION, in the compare output and in a gated-families table in the job summary.
  - **Freeze.** `scripts/commit-derived-baselines.py` replaces the documented freeze snippet: `freeze` refuses unless the source run is a completed, successful `commit-derived-bench` run, validates all eight baselines before writing, and writes them through temporary files and renames with rollback, so either every baseline changes or none does; `check` validates the checked-in baselines. `freeze --accept-regression "<reason>" --jobs-json <jobs>` freezes a failed run only when the jobs API shows that every step except `Compare against the frozen baseline` succeeded and at least one comparison failed, refuses an empty reason, and records the reason and source run id in each baseline's `provenance.accepted_regression`. The validator additionally rejects a `by_task_family` section whose family case counts plus `unassigned_cases` differ from `scored_cases`, and a family without a well-formed fingerprint, and still requires `provenance.source_commit`. No baseline is frozen by this change (#388).
- `ok retrieval-bench` reports two advisory strategies under `stream_ablations`: `fusion_pool_max` ranks the `fusion` candidate pool with `text_relevance` divided by the pool's highest lexical score, and `fusion_rank10` with `text_relevance` replaced by `(k + 1) / (k + rank)`, k = 10. Both are excluded from `benchmarks/retrieval-baseline.json` and the release thresholds, and `ok search`, `ok eval`, `ok prove` and context packs still rank with the unscaled score.
- MCP tool calls refused because of the index's state carry `data: {state, next_step}` beside their unchanged `-32000` code and message. The state is `not_indexed`, `indexing_in_progress`, `index_unavailable`, or `index_newer_than_binary` (a database `user_version` or manifest `schema_version` newer than the binary). `SqliteStore::probe_repo_index` classifies it from the index, not from the message, and `IndexRefusalState` in `open-kioku-storage` names it. Retired and unknown tool names carry no `data`. When `ok watch` withdraws the manifest after an incremental update fails, it records the reason in a new `manifest_withdrawals` table. `repo_status` and `ok --json status` then return that reason as `reason` on the not-indexed status, only while no manifest is published. Publishing a manifest, by any binary (a trigger on `manifests`), or replacing the rows removes it; the field is absent otherwise. `ok status` and `ok doctor` print that reason beside the not-indexed sentence, as `repo_status` returns it. The probe opens the index without running the schema statements, so asking whether an index can be served adds no table, index or trigger to it and takes no write transaction; and an index that cannot be opened or read while `ok index` or `ok watch` holds the lock is `indexing_in_progress`, not `index_unavailable`, so MCP no longer tells an agent to rebuild while `ok doctor` says wait. (#452)
- New public items: `open_kioku_storage::require_search_query` and `BLANK_SEARCH_QUERY_MESSAGE`, `open_kioku_storage::generations::IndexRefusalState` and `NotIndexedStatus::reason`, `open_kioku_graph::resolve_graph_node`, `open_kioku_plan::PlanOrigin` and `ContractBuilder::from_plan_with_origin`, and `SqliteStore::probe_repo_index` (with `IndexOpenRefusal`), `repo_not_indexed_status`, `not_indexed_status` and `manifest_withdrawal`.

### Changed

- Import-bound call, receiver, declared-type and inheritance resolution in every language looks a name up through the scopes enclosing the use, nearest first: the nearest scope that imports the name decides, an explicit import shadows a glob in the same scope, and an import in a nearer scope shadows one further out. An import inside one function or `mod tests` block no longer binds a call or type use elsewhere in the file, and a module-level call in the same scope as its import can now bind through it. When the deciding import is unresolved, or a glob in a nearer scope could supply the name, the import rule produces no candidate and resolution falls back to the same-file and qualified-name rules. In Rust a `mod` block does not see the imports of the module around it, except through `use super::*;` (or `use super::super::*;` within the same file), which continues the lookup in the parent module when no other glob in the block could supply the name. In Python an `if`, `try`, `with` or loop body is not a scope for this lookup, so an import made in one (such as under `if TYPE_CHECKING:`) binds in the enclosing function, class or module, and the two imports of a `try:`/`except ImportError:` pair both stay candidates; a method body does not see imports made in its class body (#453).
- An `ok watch` event over an index whose analysis semantics are compatible but whose `IndexManifest.schema_version` is older than 3 re-indexes the repository in full: `partial_index_supported` requires the stored and current versions to match.
- A full `ok index` over an existing index refuses reads with `indexing in progress` from the row transaction until the manifest is published, covering the history, graph, and Tantivy stages; it previously served the new rows over the previous graph once the row transaction committed. Building the next index in a staging generation under `.ok/generations/` and publishing it with the atomic `active` pointer (`crates/open-kioku-storage/src/generations.rs`) is the follow-up that keeps the previous index readable during a rebuild.
- `ok doctor`'s coverage check warns when git ignore rules (`.gitignore`, `.git/info/exclude`, or `core.excludesFile`) exclude at least 20 files of a programming language and more than that language's considered files (`[ok] coverage 12 of 12 programming-language files indexed (100.0%) ... 640 excluded by policy` is now `[warn]`, naming the language and the git ignore rules); the next step says to remove the rule, or to list the paths under `[index] exclude` if the exclusion is intended, which is checked first and stops the warning. `hidden`, `vendor`, `fast_mode`, `denied`, `[index] exclude` and `.okignore` exclusions stay informational, and with the default `[security] allow_hidden_files = false` a git-ignored worktree under a hidden directory still counts as `hidden`. The rule reads the new `IndexCoverage.policy_excluded_by_language` (source counts per language, in `ok --json status` and `repo_status`); a manifest written before it existed never warns until the next `ok index`.
- Coverage's denominator is the files the index would consider under the current policy: discovered files minus those skipped as `hidden`, `ignored`, `denied`, `secret_policy`, `vendor`, `generated`, `fast_mode`, or `symlink_policy`. Those policy exclusions are reported beside the ratio — in `ok index`, `ok doctor`, `ok status`, and `repo_status` — with counts by reason, the top directories (`IndexCoverage.policy_excluded_dirs`), and the setting that governs the largest share (`policy_excluded_by_source`; `hidden` names `[security] allow_hidden_files`, `ignored` names `.gitignore`, `.okignore`, or `[index] exclude`). The under-98% warning, the per-language `!` marker, and the missing-file rule are judged on that basis; the doctor's coverage next step names `[index] max_file_size` when `too-large` dominates the remaining omissions and no longer points at `[index] exclude` or `.okignore` for a `hidden` skip. Existing manifests read the same way without re-indexing (`discovered` and `skipped` are unchanged; the two new maps are empty until the next `ok index`). On this repository `ok doctor` went from `[warn] coverage 241 of 1,690 programming-language files indexed (14.3%)` to `[ok] coverage 241 of 241 programming-language files indexed (100.0%); 440 of 440 recognised files indexed (100.0%) overall; 3,394 excluded by policy (3,386 hidden, 8 ignored; 3,344 under .claude/, 23 under .github/, 6 under .playwright-cli/; `[security] allow_hidden_files` governs the largest share)`, the hidden files being git-ignored agent worktrees.
- The corpus benchmark workflows, `commit-derived-bench.yml` and `semantic-experiment.yml`, now keep corpus identity out of public logs and artifacts:
  - **Secrets.** Each corpus's repository URL, base commit, and indexed subtree list is read from the `BENCH_<CODE>_URL`, `BENCH_<CODE>_BASE`, and (for a corpus indexed as subtrees) `BENCH_<CODE>_PATHS` repository secrets. The subtree list is no longer checked in: it leaves the workflow matrix, and `provenance.path_prefixes` in the baselines becomes `provenance.path_prefix_count`.
  - **Masking.** `scripts/mask-corpus-identity.sh` normalises these values and masks every form of them a log can still show.
  - **Clone and index output.** The clone and its remote are removed once cases are derived. `ok index` and `ok status` write to files, `scripts/extract-ok-json.py` reads the document past any log lines, and the log gets one counts-only line.
  - **Manual runs.** They no longer accept a repository URL or base commit. A `corpus` choice (`all` by default, which is what the schedule runs) limits a trial run to one corpus. Every input is validated, and none reach a `run:` script as expression text.
  - **Case derivation.** `scripts/commit-derived-cases.py --quiet` prints counts only and withholds failure details.
  - **Artifacts.** Uploads carry aggregate results only. `scripts/reduce-benchmark-report.py` keeps allowlisted fields of allowlisted shapes after the baseline comparison, and `cases.tsv` is not uploaded. The documented baseline freeze reads `cases_scored` from the reduced reports.
- The parser semantics version is `tier1-parser-semantics-v2`, so an index built by an earlier release reports `analysis_semantics_status: RebuildRequired`. Until it is rebuilt, MCP `plan_change`, `impact_analysis`, `get_references`, `dependency_path` and `query_evidence_graph`, `ok graph query` and `ok search --kind graph` answer "authoritative relationship evidence unavailable", and `ok watch` fails on every change event. Next step: `ok index <repo>`.
- Each evidence line of a context-pack primary result is its own record, `search:<path>:<range>:<index>`; a result ref that is not one of its lines' ids resolves to no record. Pack and plan evidence are deduplicated by id, `validation_plan.evidence` holds only the records its tests cite, and evidence density counts one fact per result path and range.
- `impact_analysis`, `ok impact`, context packs and plans list one direct impact per path and edge kind (exact reference, co-change, runtime, service boundary, lexical), up to 25 files; an entry carries the evidence lines of up to four further chunks of its path and one line naming the ranges of the rest. A plan's caution files are its structural impacts and at most eight lexical impact paths, direct impacts before indirect ones; the Markdown and prompt-text context renderings list each supporting path once. Impact risk counts these entries, so a file matched in several chunks raises it once per edge kind rather than once per chunk: on the demo repository `ok impact --file src/auth.rs` reports risk `low` (0.21) where it reported `medium` (0.31).

### Fixed

- Exact-reference provenance on impact results is typed. A result produced from an indexed symbol occurrence carries `exact_reference_provenance` (`scip`, `tree_sitter` or `lsp`), and deduplication keeps it when a higher-scoring lexical hit on the same path and range is merged in. The `exact symbol reference via` prefix of `match_reason` used to be the only marker, so that merge erased it and the reference silently left `exact_reference_count`. Context and plan exact-reference counts, impact's direct-impact classification, and ranking's `exact_reference` signal read the field; ranking's substring fallback on "exact symbol reference" is removed. The `impact:<path>` evidence record takes the source type of its exact references (SCIP, then LSP, then tree-sitter) and names every source in its message, instead of `scip` whenever any exact reference existed, and exact-reference counts accept `scip`, `tree_sitter` and `lsp` evidence explicitly. An occurrence from a lexical or heuristic source is no longer reported as an exact reference "via indexed". Missing-exact-reference negative evidence lists `impact.exact_reference_provenance` as the inspected source (#438).
- Impact deduplication keeps each evidence line with the ref it was published with. Refs that do not align one-to-one with a result's lines are replaced by per-line derived `search:` ids instead of being paired by position, which gave a runtime line from a second fact a lexical id and left that fact uncited; a line whose id is already cited under other text takes the next unused derived id. A grouped direct impact names every non-representative chunk in its listed lines or its "more matching ranges" summary, including a chunk whose lines are all already cited (#463).
- A bare call through a Rust item import (`use crate::auth::issue_token;`, one path of `use crate::auth::{issue_token, Token};`, or `use crate::auth::issue_token as mint;`) is an authoritative `CALLS` edge with the `exact_call_site`, `import_binding` and `qualified_name` proofs and resolver strategy `rust_item_import`, so `ok impact` and `impact_analysis` report it in `proven_impact`; it used to produce no edge and no impact. Rust imports are bound only by following the path through the importing file's own crate: `crate::` starts at the `src/` of the nearest `Cargo.toml` and `self::`/`super::` at the importing module, which must itself be declared where its path says; every module on the path must be declared as a file (`mod name;` with no body and no `#[path]`); and exactly one module-level Rust item of that name must be defined there. In a package with both `src/lib.rs` and `src/main.rs`, `crate::` follows only the root whose module tree declares the importing file. A path naming a declared module binds that module's file, and a path naming both a submodule and an item binds no call target. The module-key lookup other languages use no longer binds Rust imports, so `crate::auth::issue_token` in one workspace member cannot bind to another member's `auth/issue_token.rs`. For Rust only, a call through an import is a `CALLS` edge only when the bound item is a function; other languages keep their existing import-bound call targets. A declared type reached through an import also carries an `import_binding` proof. An associated-function call through an imported type (`use crate::auth::Token;` then `Token::parse(raw)`) resolves to that function as an authoritative `CALLS` edge. A receiver whose type comes from an initializer is proven only by a constructor form: `Foo { .. }`, a Rust path call `Foo::bar(..)` whose indexed signature returns `Self` or `Foo`, or `Foo::default()` on a struct, enum or alias with no indexed `default` (a derived `Default`). `let handle = Server::spawn()` returning a `ServerHandle` no longer types `handle` as `Server`. A `self.field` receiver matched through a local binding of the same name gives candidates without the receiver-type proof. A Rust parameter or `let` type that names a type parameter of the enclosing function, impl or trait (`fn f<Token: Parse>(t: Token)`) is no longer recorded as that type. Grouped Rust imports are recorded as one import per path, deduplicated per path and line, so `use a::{B, B as C};` stores one row and keeps both bindings. `PARSER_SEMANTICS_VERSION` is now `tier1-parser-semantics-v3` and `RELATIONSHIP_RESOLVER_SEMANTICS_VERSION` is now `ri3-relationship-resolver-v2`, so an index built before this change reports `RebuildRequired` until `ok index` rebuilds it (#453).
- `ok watch` replaces exactly the changed file's graph edges on an incremental re-index and reconciles the rest of the stored graph with the new snapshot by identity, so a renamed symbol loses its old callers, edges from unchanged files into nodes the change removed go with them, and the graph string dictionary does not grow across repeated runs. The per-file delete used to key on the producing pass name, which never matched a path, so no edge was ever removed (#413).
- `ok index` and `ok watch` publish the index manifest as the last step of a run. A full run removes the manifest when it stages the rows and puts it back after the graph and the Tantivy index are written, so a reader never opens a manifest over rows or a graph from a different run and a run that fails partway leaves the repository unindexed. An incremental `ok watch` run replaces the changed files' rows and graph in one transaction under the previous manifest, then rebuilds the Tantivy index in place, during which `ok search` and `search_code` (`mode=code`) fall back to SQLite chunk search or read an index that is empty until the rebuild commits, and `mode=graph` reports the index missing; if a step after that transaction fails, the run withdraws the previous manifest and returns the error. `.ok/index.lock` is an OS advisory lock (`flock`, `LockFileEx`) held for the whole run and released by the kernel however the writer exits; while a live writer holds it and no manifest is published, `ok status`, `ok doctor`, every read command, and the MCP server report `indexing in progress` instead of `repository is not indexed`, and a lock file with no holder is ignored by readers and taken over by the next writer.
- The MCP server keeps serving when its index probe fails (a live writer holds the lock, the file is unreadable, the manifest is newer than the binary): the request that hit the failure gets a `-32000` error carrying the reason, the handshake and `tools/list` still answer, and the failure is logged once per distinct message. The session used to end without a response. A session that already serves an index probes again when the manifest is withdrawn.
- `IndexManifest.schema_version` is 3. A manifest with a newer version than the binary supports is refused by every read surface and by `ok snapshot import` with `index was written by a newer Open Kioku (...): upgrade Open Kioku or run `ok index` to rebuild it`, instead of a serde error or `repository is not indexed`; older manifests still read.
- `ok snapshot import` holds `.ok/index.lock` for the whole import and publishes the manifest last, as `ok index` does: the imported database is moved into place with its manifest removed, the Tantivy index is rebuilt from it, and the manifest is put back afterwards, so a reader never opens the imported manifest before its search index exists and an import that fails at the search stage leaves the repository unindexed. The import took no lock before, so it could run alongside `ok index` or `ok watch` on the same repository. The manifest of the database being replaced is withdrawn before that database is moved aside, so a running MCP session switches to the imported index instead of answering from the replaced file. A replaced file that cannot be read as an index for any reason other than not being a SQLite database or lacking a `manifests` table fails the import with the index left in place. If the imported database cannot be moved into place or opened, the previous database is moved back and its manifest restored, only once it is back at the index path; if moving it back fails the error names the backup file that still holds it, and if restoring the manifest fails the error says so, the repository reading as not indexed until `ok index` runs in both cases. An import killed while the previous database is moved aside leaves it at `.ok/.index.sqlite.<pid>.<time>.backup`, which the `repository is not indexed` message then names. `ok snapshot export` opens the index as every read surface does and refuses with `indexing in progress` or `repository is not indexed` instead of `index database is missing` or `snapshot source database has no index manifest` (#446).
- `ok --help` and every subcommand `--help` describe the command and each option, with enum values listed; 24 of 38 top-level commands, the global `--json` and `--repo` flags, and the whole option lists of `context`, `plan`, `verify`, `index`, `mcp serve` and `mcp install` had no text. No command was renamed, reordered, added or removed. A unit test walks the clap tree and fails on any command or option without help.
- `ok mcp serve` and every CLI read command (`ok status`, `ok doctor`, `ok search`, `ok context`, `ok impact`, `ok plan`, `ok retrieve-context`, `ok memory search|recent`) no longer create `.ok/index.sqlite`, `.ok/context.sqlite` or `.ok/memory.sqlite` on a repository that has never been indexed; `SqliteStore::open_existing`/`open_repo_index`, `ContextHandleStore::open_repo_existing` and `RepoMemoryStore::open_repo_existing` are the non-creating opens, and `SqliteStore::open` remains the writers'. `repo_status` and `ok --json status` return `{indexed: false, index_path, message, next_step}` for such a repository; the indexed answer gains `indexed: true`; every other tool call and read command fails with the same message, which names `ok index <repo>`. `initialize` and `tools/list` still answer. A database without a manifest is reported as unindexed; the pre-4.0 "rebuild" message is unchanged for a real legacy index. An index built while the server runs is served from the next request.
- `ok setup agent --apply` inspects the client's `open-kioku` entry before indexing: an entry that launches `ok mcp serve` for this repository with at most `--repo`, `--read-only` and `--hide-experimental` is kept unchanged (`[kept] config … existing entry preserved`); any other entry, including one passing `--deny-network=false`, `--approval-required=false` or `--allow-command`, is refused before anything is indexed or written, naming `mcpServers.open-kioku` and the compatible value. `--check` reports `[mismatch]` for a conflicting entry or a foreign guidance file and no longer recommends `--apply` in that state.
- Markdown, text and TOON context packs render the file-limit budget as `no token ceiling; file limit N` instead of the `max_tokens` sentinel; `ContextBudget::has_token_ceiling` is the check, and the JSON field is unchanged.
- `impact_analysis` on a path the index does not hold reports `risk_report.level: "unknown"` and names the path in `risk_report.reasons` and its evidence message; `search_code` rejects a blank `query`; `dependency_path` rejects a `from` or `to` that resolves to no indexed file, symbol or graph node; `retrieve_context` rejects an unknown handle. `ok impact`, `ok search`, `ok path` and `ok retrieve-context` apply the same checks. The `{"value": …}` wrapper is unchanged.
- `ok init` and `ok setup agent --apply` write `ok.toml` with the `[ranking]` f32 weights as their shortest decimal (`graph_proximity = 0.35`; the weights now serialize that way in every output), `[runtime]` reduced to `enabled = false` with the provider fields as comments, and a comment documenting `resolution_mode`'s `legacy`, `shadow` and `v2`. The loader reads the file back as the defaults.
- `ok plan` and `plan_change` count a proven dependent in `impact.proven_impact` toward `exact_reference_count` when it is in a file other than the impact target, authoritative, unambiguous, and a `REFERENCES`, `USES_TYPE`, `CALLS`, `IMPLEMENTS` or `EXTENDS` edge; same-file edges, ambiguous edges, and `IMPORTS` edges still do not count, and `ok context` does not count proven dependents. A plan whose only exact evidence is such an edge no longer reports `exact symbol/reference evidence is unavailable`: `evidence_quality.exact_reference_available` becomes true, the `exact_references` negative evidence item, including the one the plan copies from its context pack, is dropped, `exact_references` scores 1.00 instead of 0.25, and the 0.74 cap for plans without exact evidence no longer applies, so such a plan can read High instead of Medium.
- An all-lowercase task word whose only separator is `-` (`re-index`, `drive-by`, `best-effort`) is a hyphenated task word, not a task identifier, in `ok context`, `ok plan`, `build_context_pack` and `plan_change`, unless the task quotes it in backticks, writes it as a flag (`--allow-network`), or joins it to `/`, `::`, `@`, `=` or a `.` followed by a word (`crates/open-kioku-cor/src/lib.rs`, `drive-by.rs`); a capital, a digit or `_` also makes it an identifier, and a task with an odd number of backticks has no hyphenated task words. A bare package name such as `serde-json` is a hyphenated task word. An unmatched hyphenated word no longer produces the `task identifier(s) name nothing` blocker or its 0.50 cap; it stays in the `anchor` negative evidence, whose reason names it under `hyphenated task word(s) spelled by no selected context`, in a plan's `low confidence:` risk reason as `top context did not spell hyphenated task word(s)`, and in the caveat `N hyphenated task word(s) appear in no selected context`. Labels move both ways: a task whose only unmatched anchors are hyphenated words can read Medium instead of Low (the 0.60 cap for negative evidence still applies), and an unmatched identifier beside a matched hyphenated word, which was counted as a second identifier, now gets the blocker and the 0.50 cap instead of a caveat.
- Context-pack and plan confidence is derived from evidence provenance, not from result prose. `exact_reference_count` and the runtime corroboration count were granted by substring tests over result evidence lines, so a target file defining `scip_setup_report` made `ok context` report `Exact (1.00)` beside `exact_evidence_count: 0`; they now count exact-authority retrieval traces, the impact engine's indexed symbol references, `source_type: scip` evidence, and the typed `runtime_corroboration` component, and `Exact` requires `exact_reference_count > 0`. `negative_evidence_count` is the `primary_context` and `anchor` items of the published `negative_evidence` list on both surfaces instead of a regex over `risk.reasons`; task identifiers that no selected context spells are `anchor` negative evidence, with a 0.50 cap and a blocker naming them when all are missing; a plan's evidence-quality caveats now cap the score like every other caveat, and `exact_reference_available` is reconciled with the typed count. `evidence_density` counts distinct evidence records, and the Markdown line `Exact-evidence selections` is now `Exact-authority selections`. In `ok plan` and `ok context`, tasks naming a uniquely-resolved symbol can read one band higher than before (Medium 0.60 to High 0.94) because the prose count over evidence-quality caveats no longer caps them; `docs/context-pack-spec.md` defines the label semantics.
- Test targets are function, method and test symbols with a test annotation (`#[test]`, `#[tokio::test]`, `#[async_std::test]`, `#[rstest]`, `#[test_case]`, `@Test`, `@ParameterizedTest`, `@RepeatedTest`, `it(`, `test(`, `def test_`) on their first line or in the attribute, annotation and comment stack directly above it, and every symbol in a test-path file, including a repository-root `tests/` or `test/`. Targets are stored at index time.
- `ok verify` and `verify_change` list the innermost symbols whose ranges overlap each diff hunk and report hunks no symbol covers under `changed_regions_without_symbol`; a hunk side with no lines contributes no range. Without a diff, symbols are listed per file with a `symbol_granularity` warning that does not change the verdict. Library callers can supply ranges through `VerifyChangeInput.changed_ranges`.
- `query_evidence_graph` with no `query` and `ok graph schema` return `syntax` (one sentence per clause), `examples` (queries that parse and run) and `unsupported` (rejected forms, each with an example and the accepted alternative), and `edge_types` lists `DerivedFrom`, which queries already accepted. Node and edge types in a query accept the schema's spelling (`DatabaseTable`, `DependsOn`) as well as the serialized one (`database_table`, `DEPENDS_ON`), case-insensitively, where the schema's spelling was rejected before. A parse error names the accepted form: an unknown node or edge type lists the schema's types, a MATCH without an edge pattern shows one, `RETURN f.file_path` says to filter properties in WHERE, a function, DISTINCT or AS alias in RETURN says RETURN accepts only variables bound in MATCH, and trailing input names the token and its column. `ok graph schema --format markdown` escapes Markdown characters in the schema prose so grammar placeholders such as `<path>` render. Every query accepted before is still accepted.
- `ok verify`, `ok contract verify` and `verify_change` check both sides of a rename: the previous path is listed in `changed_files` and held to the forbidden and boundary rules, a copy's source is held to the forbidden rules, the finding names both paths, and a new `previous_paths` field pairs them; `--git` and `--since-plan` diffs are taken with `--find-renames`. Only the new path was checked before. Because the previous path is a changed file, it also feeds changed symbols, recommended tests, changed impact, the API-surface check and the dependency check, matching git's delete-plus-add view of a rename. A rename's hunks are attributed by side (pre-edit lines to the previous path, post-edit lines to the new path), and a side of a text rename with no hunk lists no symbols and no `symbol_granularity` warning. `--check-api-surface` reports a public item whose kind, name and signature are unchanged across a rename as `api_surface_moved`, a warning naming both paths, unless the contract's `api_surface_constraints` forbid removals in the previous path's scope or additions in the new path's scope, in which case the move fails; an item missing from the new path still fails as removed, and a changed signature still fails. Each hunk's content ends where the counts in its `@@ -a,b +c,d @@` header say, so removing a `-- x` line or adding a `++ y` line no longer produces a path or a rename, and a plain `---`/`+++` entry appended after a `diff --git` entry is still a changed file held to the boundary. Rename and copy paths from a CRLF diff carry no trailing `\r`. `verify_change` explanations list the rename and copy pairs. `--git`, `--since-plan`, `ok impact --since` and `ok plan --since` run git with `--no-color --find-renames --src-prefix=a/ --dst-prefix=b/`, so neither `diff.renames`, a local path-prefix setting nor `color.diff` changes what is parsed; copies are detected only in a supplied diff. `ok impact --since` analyses a renamed file's previous path as well as its new one, `ok plan --since` shows a rename as `<new> (from <old>)`, and neither labels a file that follows an added, deleted, renamed or copied file with that file's status.
- `ok setup agent <client> --check` reports `[failed] index` with the storage error, and not ready, when the index exists but cannot be served (unreadable, being built, or written by a newer Open Kioku), instead of exiting with that error before printing any check. (#443)
- `ok path` and MCP `dependency_path` resolve nodes through one function, `open_kioku_graph::resolve_graph_node`, instead of two copies. `ok search` and `search_code` refuse a blank query through one check, `open_kioku_storage::require_search_query`, with one message. (#443)

## [4.0.0] — 2026-09-11

Two things need doing on upgrade, both detailed first below: **run `ok index` to rebuild every existing index**, and **reconfigure any agent that names an MCP tool by one of the retired names** (`ok setup agent --apply` rewrites its own guidance for you).

### Breaking

- Four public enums gain a variant, so a downstream `match` over any of them without a wildcard arm stops compiling: `GraphEdgeType::DerivedFrom`, `RelationshipProofKind::DeclaredOrigin` and `RetrievalSourceKind::DerivedSibling` (#381), and `SkipSource::Parser` (#350). None is `#[non_exhaustive]`, so this is a semver break rather than an additive change; serialized data is unaffected in both directions, since the new names only ever appear in newly written indexes and older readers never emitted them.
- **Every existing index must be rebuilt: run `ok index` after upgrading.** The index storage format changed: SQLite `user_version` 3 -> 4 and `IndexManifest.schema_version` 1 -> 2. `graph_edges` and `call_sites` no longer carry a JSON document per row beside the query columns holding the same values; every field now has a typed column and every repeated string is written once into a per-table dictionary (`graph_strings`, `call_site_strings`) and referenced by integer id. Opening a pre-4.0 index detects the old layout by the `json` column (a `PRAGMA table_info` check, not a scan), drops those two tables, and makes every relationship read report ``run `ok index` to rebuild them`` rather than answering from an empty table — an empty answer would read as "no such relationship exists". The detection cannot fire twice, because the column it keys on is gone afterwards. `ok snapshot export` refuses a store awaiting the rebuild, `ok snapshot import` refuses an artifact exported from a pre-4.0 index, and the cross-project workspace linker refuses a member index awaiting it — each names the fix instead of producing something that only looks complete. Search, symbol lookup and the file inventory keep working on the old index; it is relationship evidence specifically that is withheld until `ok index` runs, and the surfaces built on it — `ok impact`, `ok plan`, `ok preflight`, `ok context` and MCP `impact_analysis`, `plan_change`, `build_context_pack` — refuse with that instruction rather than answer from the empty graph. The same index fails `ok doctor`'s `graph` check, is named by `ok setup audit`, and reports `graph_rebuild_required: true` in `ok --json status` and MCP `repo_status`; the analysis-semantics fingerprint those surfaces also report is unchanged since 3.1.0 and does not see it. (#363)
- The advertised MCP tool surface is 16 tools, down from 58 (#406). Every advertised tool now answers one question no other tool answers. Removal is outright: retired names left `tools/list` and the dispatch table in the same change, so an agent holding a stale name from a cached config or an old prompt gets an error rather than a response whose shape no longer matches the description that name was chosen from. A `RETIRED_TOOLS` table asserts that none of them can reappear on either surface, and carries the replacement each one points at (below).
  - **Folded into the sixteen, no capability lost (23 names).** `repo_status` absorbs `list_languages` (as a `languages` field, taken from index coverage where it exists) and `semantic_status` (as `semantic_lifecycle`, which now carries the whole `SemanticStatus` — provider, backend, model, model artifact hash, dimensions and counts included — rather than the readiness summary it held before, so an agent can still ask which model produced the vectors it is about to trust; `ok semantic status` is the CLI equivalent). `list_files` absorbs `explain_file` as a per-path detail mode. `search_code` absorbs `search_files`, `semantic_search`, `hybrid_search` and `explain_search_result` behind `mode` (`code`, `graph`, `semantic`, `hybrid`); no `explain` parameter was added, because `score_breakdown` and `evidence_refs` already ship on every result and a flag that toggled nothing would be a new false claim. `search_symbols` absorbs `list_symbols` by making `query` optional. `get_definition` absorbs `get_symbol_context` behind `include_body` and `explain_symbol`, which was already the same call. `get_references` absorbs `get_callers`, `get_callees` and `get_implementations` behind `kind`. `dependency_path` absorbs `module_dependencies` by making `to` optional. `build_context_pack` absorbs `build_compressed_context` behind `compress`. `plan_change` absorbs `preflight_change` and `propose_patch` behind `detail`, and `create_change_contract` behind `persist` (with `store` for a transient contract, and `plan`/`plan_json` so a saved plan becomes a contract without re-planning). `verify_change` absorbs `verify_change_contract` (`contract_id`, `contract`, `contract_json`) and `explain_verification` (`verification`, `verification_json`, or `explain: true` on a contract verification). `find_tests_for_change` absorbs `recommend_validation_plan` and `explain_test_coverage` by making `path` optional. `query_evidence_graph` absorbs `get_evidence_schema`: called with no `query` it returns the schema.
  - **`get_references` response shape.** The merged tool returns an object, not a bare occurrence array. Each evidence kind is its own section naming its own `evidence_source` — `symbol_occurrences`, `sqlite_graph_store`, `persisted_implements_facts` — with its own caveats and its own payload key (`occurrences`, `nodes`/`edges`, `implementations`). Occurrence evidence and persisted IMPLEMENTS facts have different provenance, different confidence, and different meanings of empty, so folding them behind one name moved that distinction into the response rather than erasing it; a test asserts the sections stay separable. `kind: "implementations"` does not require the target symbol to be indexed, because IMPLEMENTS facts are keyed by target name and requiring resolution would have dropped evidence the retired tool returned.
  - **Removed with nothing lost (1).** `structural_search`. No structural or AST matching exists anywhere in the workspace; only the name did.
  - **Config-gated, advertised when the feature is configured (5).** `remember_fact` and `search_memory` when `[memory] enabled = true`; `map_stacktrace_to_code`, `find_errors_for_symbol` and `find_recent_failures` when `[runtime]` names an enabled provider. All five stay dispatchable and the runtime three still return their structured disabled response; what the gate removes is a name an agent would otherwise be taught to reach for and get nothing from.
  - **Moved to the CLI, still shipped (13).** The seven architecture tools (`ok architecture detect|boundaries|violations|summary|policy validate|policy check|policy explain`), the five history and ownership tools (`ok history provenance|churn|similar|ownership|reviewers`), and `get_change_contract` (`ok contract show <id>`). **State the trade plainly: a coding agent with a shell can still reach every one of these through `ok`, but a pure MCP client with no shell cannot.** That is a deliberate reduction of the agent-callable surface, not a free move.
- A retired tool name answers with where its capability went, not just a refusal. `` `get_callers` was retired from the MCP tool surface in 4.0.0: use `get_references` with `kind: "callers"` ``; `structural_search` points at `regex_search` or `search_code`; `churn_analysis` points at `ok history churn --path`, `--module`, or `--symbol`. All 40 retired names — the 37 retired here and the three patch tools (`apply_patch`, `review_patch`, `validate_patch`) removed before 4.0.0 — carry one, a test asserts the guidance text is in the error, and `scripts/validate-agent-guidance.py` holds every shipped agent-guidance file — including the rule `ok setup agent --apply` writes into the user's own repository — to naming only tools the server still answers.
- The guidance `ok setup agent --apply` installs no longer tells the agent to call `preflight_change`. It is the only copy of the routine written into a user's own repository, so a retired name there failed on first use; it now says `plan_change` with `detail: "preflight"`. The Cursor plugin's second skill (`.cursor-plugin/skills/open-kioku.mdc`) and its `search-before-edit` and `validate-before-apply` rules named eight retired tools plus `resolve_symbol` and `validate_patch`, which have never existed in this workspace; all now describe the sixteen.
- `[runtime]` gates on the provider's own validation rather than on a flag. `enabled = true` with an incomplete provider block advertises nothing and answers nothing, because `open_kioku_sentry::ensure_configured` decides — a switch that only advertised would have been worse than the always-inert names it replaced. A provider that does validate is answered with an explicit `configured: true` response naming that this build ships no runtime query implementation, so an empty result is never mistaken for "no runtime errors". `[runtime]` therefore takes `organization`, `project` and `auth_token_env` alongside `enabled` and `provider`.
- `ok architecture violations` reports `configured`, `uncertainty` and `caveats` alongside the violations. It printed a bare list, so `[]` read as "no violations" when it meant "no policy was configured and nothing was evaluated".
- Three tools that made a parameter optional now reject a present-but-non-string value instead of answering the broader question. `find_tests_for_change` with `path: 123` returned repository-wide evidence, `list_files` returned the inventory, and `query_evidence_graph` returned the schema; two of those parameters had been required before the fold, so a type error had been an error.
- `get_references` with `kind: "all"` degrades the way `kind: "implementations"` does when the name does not resolve to an indexed symbol: it returns the IMPLEMENTS evidence, which is keyed by target name, with a caveat naming what could not be gathered, rather than failing the whole call.
- `mcp.hide_experimental` can no longer hide semantic search, hybrid search, or the call and implementation lookups. All sixteen advertised tools are `stable`; those capabilities are now modes and sections inside them (`search_code`'s `semantic`/`hybrid`, `get_references`'s `callers`/`callees`/`implementations`) rather than separately-named experimental tools, so a tool-level flag cannot reach them. Their uncertainty is reported per response instead — `semantic_status` on a search, and each `get_references` section's `evidence_source` and `caveats` — which is where it belongs, but a repository relying on the flag to keep heuristic evidence out of an agent's reach will see behaviour change. `map_stacktrace_to_code`, `find_errors_for_symbol` and `find_recent_failures` remain `experimental`, so the flag still applies to them.
- A plan's `tool_calls` no longer recommends `search_memory` unless the repository has enabled memory. It was emitted on every plan, and with the memory tools now advertised only when `[memory] enabled` is set, an agent reading a plan that named a tool absent from its own inventory had no way to tell a gated name from a stale one. `PlanEngine::with_memory_enabled` carries the answer from config; memory facts themselves are unaffected and still appear in the plan's memory section either way.
- `repo_status` reports languages by their canonical key (`rust`, `type_script`) rather than the Rust `Debug` spelling (`Rust`, `TypeScript`) the retired `list_languages` used, matching `coverage.by_language` and every other serialized language field.
- `build_context_pack` and `plan_change` now declare `readOnlyHint: false` and `idempotentHint: false`, because `compress: true` and `persist: true` write under `.ok`. With those flags absent — the default call — both are read-only, as `verify_change` already documented for its own flags.
- `build_context_pack` and `plan_change` default `format` is `markdown`, not `json`. A 3.1.0 client that parsed the default `structuredContent` as a `ContextPack` or `PlanReport` now receives `{rendered_in, bytes, truncated}` with the rendering in `content`; pass `format: "json"` to keep the object (`docs/mcp-tools.md`).
- New public fields on structs that downstream code builds with a struct literal, so those literals stop compiling: `ConfidenceSignalInput.task_relevance`, `ContextBudget.region_files` and `ContextBudget.region_tokens_per_file`, `ContextSelectedUnit.kind`, and `IndexQuality.coverage` (reached as `manifest.quality.coverage`). Each serializes additively; `coverage` is `null` on a manifest that predates it.
- `ConfidenceBreakdown::from_signals` is re-weighted, so every confidence score moves between 3.1.0 and 4.0.0: a new `task_relevance` component at weight 0.20, `evidence_density` 0.20 -> 0.10, and a 0.74 cap on the overall score when no exact reference evidence supports the selection (the strict-below-Medium cap for weak task relevance, under Fixed below, is the other new cap). A pack or plan scored under 3.1.0 is not comparable to one scored under 4.0.0; `confidence_summary` names the current signal set.
- Two public constants are re-valued: `open_kioku_context::candidates::DEFAULT_RRF_K` 60 -> 10, and `open_kioku_embeddings::QWEN3_MAX_LENGTH` 8,192 -> 2,048 (the Qwen3 provider version string embeds the value).
- `ok architecture violations` reports the violations of the evaluated architecture policy. It previously ran heuristic detection with no policy resolver, so a repository with a configured policy got detection output rather than its own rule violations — the MCP tool it replaces did not have that defect.
- A plan's `boundary.signal_hooks.architecture_components` names the CLI reads (`ok architecture summary`, `ok architecture violations`, `ok architecture policy check`) rather than the retired MCP tool names, so a plan no longer points an agent at names the server does not answer.
- `open-kioku-core` evidence types share their strings and paths instead of owning a copy per graph edge. `Evidence.message`, `Evidence.source`, `AnalysisFact.message` and `AnalysisFact.source` change from `String` to `SharedStr` (`Arc<str>`), and `FileRange.path` changes from `PathBuf` to `SharedPath` (`Arc<Path>`); `EvidenceMessage` is renamed `SharedStr`, `MessageInterner` is renamed `StringInterner`, and `PathInterner` is new. This is a Rust API break only — each type serializes with `serialize_str` and deserializes through `String`, so JSON output is byte-identical, rows written when the field was a `String` still load, `JsonSchema` still reports `{"type": "string"}`, and every golden MCP and tools-list snapshot passes untouched. Each type derefs to its borrowed form (`str`, `Path`) and converts from the owned one, so downstream call sites migrate with `.into()`: `message: text.into()`, `path: path_buf.into()`. Struct literals and explicit `String` / `PathBuf` annotations are what need editing. Why: 95.3% of `Evidence.message` bytes were clones of `AnalysisFact.message`, held twice at the indexing peak while the facts were still resident, and on the measured corpus 153,856 edges cited 2,388 distinct evidence paths while 156,515 edges carried 66,663 distinct messages (`docs/storage-model.md`). This is the change that required a major version. (#343, #329)
- Ranking signals `runtime_corroboration`, `graph_proximity`, `git_cochange`, `memory_signal` and `exact_reference` read their persisted `ScoreComponent` and nothing else. All five were substring probes over the joined evidence text, and the lexical stream writes the user's own query into that text, so the probes were reading the query back out and scoring it as corroboration: a search whose wording contains "trace" awarded itself `runtime_corroboration = 0.18` — with the BM25 evidence id borrowed as its provenance — on repositories holding zero runtime facts (139 candidates across the 6 frozen-corpus cases whose queries contain that word). "where is the dependency graph built" likewise scored `graph_proximity` on a result with no graph evidence. That is absence rendered as presence in a ranker whose stated contract is that signals are persisted or traceable. The prose fallbacks are deleted rather than demoted, because a fallback that fabricates is worse than a zero; `exact_reference` keeps one non-prose fallback on `match_reason`, a structured field the search layer sets deliberately. Scores and `top_score_signals` change for any caller that was benefiting from a fabricated signal, and `ok search --explain-ranking` output changes with them. The retrieval benchmark matches `benchmarks/retrieval-baseline.json` to the digit either way — the fabricated signal reordered candidates but not across a gold cutoff on that corpus, which is a finding about the corpus, not evidence the signal was harmless.
- `open_kioku_parse::evidence_timestamp()` is removed. It had no callers anywhere in the workspace and was vestigial (#335); the next release is a major, so the removal rides it.

### Added

- `ok architecture summary` returns what the retired `summarize_architecture` MCP tool returned: detected components, the configured policy, the evaluated `policy_check`, and its violations, with an explicit `configured: false` and a caveat when no policy exists rather than inferred violations.
- `[memory]` and `[runtime]` configuration sections. Both default to off and both gate MCP advertising only: `[memory] enabled` controls whether the memory tools are listed, `[runtime] enabled` plus a non-empty `provider` controls whether the runtime tools are. Repository memory stays readable and every gated tool stays dispatchable either way.
- Derived-file edges (`DERIVED_FROM`): a file and the file it is produced from, exercises, or describes are siblings of one edit, which no import edge records. Built at index time from a generation banner that names its origin (`automatically generated from <path>`, high confidence with a `declared_origin` proof, corroborating and never authoritative — a banner is prose, and every authoritative proof in the workspace comes from parsed structure), or from a test or declaration naming convention (`foo_test.go`, `foo.test.ts`, `test_foo.py`, `FooTests.java`, `foo.d.ts`; medium confidence, no proof, heuristic by construction). A convention that could mean two files emits nothing. `ok impact` and MCP `impact_analysis` walk the edge in the derived -> origin direction, so editing a source names the files generated from it and the tests that exercise it — both as labeled possibilities, the declared origin carrying its proof kind so a reader can tell it from a naming guess. No untyped graph read returns the edge, so `dependency_path` and `module_dependencies` are unaffected by it. Retrieval admits a candidate's siblings into the context pack with the edge as evidence (`derived:<edge>`). Edges found: Java (10k files) 2,232 test pairings, Python (~4k files) 397 declared-origin and 816 test pairings, TypeScript (~900 files) 313, Go (~800 files) 228. Retrieval on the four locally derived line-range-annotated case files (116/145/168/197 = 626 cases, the same files as the region-widening measurement below, not the frozen 562-case splits), against a control built from the same commit of `main` (`be5d6ae`), on a local workstation: Java (10k files) R@5 0.5431 -> 0.5603, R@20 0.6810 -> 0.6897, MRR 0.4901 -> 0.4994 and gold recall 0.5484 -> 0.5570, including one case that had found no gold at all and now ranks it first; Go (~800 files) R@5 0.7724 -> 0.7793; Python (~4k files) R@5 0.6701 -> 0.6751; TypeScript (~900 files) R@5 unchanged with MRR 0.6473 -> 0.6443, one case slipping from rank 1 to 2. Gold recall slips by one case on Go and Python. Every delta is inside its bootstrap interval, so this ships for the evidence it persists and the impact edges it feeds rather than for a ranking gain. Index time and peak RSS are within run-to-run variance on the Go corpus (three alternating runs per arm: 66.8 s and 692 MB median before, 63.9 s and 685 MB after); the 10k-file Java corpus, which adds the most edges, was a single pair, 5.36 GB peak RSS before and 5.48 GB after (+2.2%), which one pair cannot separate from noise (#381).
- `ok search <pattern> --regex` runs exact regular-expression line matching over the indexed corpus. It makes the same call the MCP `regex_search` tool now makes and reports the same `results`, `truncated`, `warnings` and `caveats` fields, so the index-only corpus caveat survives `--json` on both surfaces.
- `SymbolEngine::context` and `ok symbol context <name>`: a symbol joined back to the indexed chunk text covering it — the definition body with the line range it spans, and up to ten indexed lines above and below it verbatim. `MetadataStore::file_by_id` resolves a `FileId` to its file, with an indexed SQLite lookup replacing the file-list scan callers were writing by hand.
- Golden MCP snapshots for a successful `tools/call` envelope: `tools_call_json_tool.json` (a JSON tool — `structuredContent` is the object, `content[0].text` the pretty-printed JSON) and `tools_call_rendered_tool.json` (a Markdown rendering — `structuredContent` is `{rendered_in, bytes, truncated}`). Only the error envelope was pinned before, so the single-payload change altered the wire shape of every rendered response without a snapshot moving (#392). A successful tool result now also states `"isError": false` rather than leaving the optional field absent.
- `open_kioku_core::process::process_peak_rss()` measures this process's peak RSS and names the instrument that produced it: `VmHWM` from `/proc/self/status` on Linux, `getrusage(RUSAGE_SELF).ru_maxrss` elsewhere on Unix (bytes on Apple platforms, kilobytes on Linux and the BSDs — the conversion lives in one place with a per-platform test). It replaces the four private `/proc`-only copies in the vector and embeddings profiling examples, which returned `None` on macOS, where those artifacts are produced. Each report now carries `process_peak_rss_instrument` beside `process_peak_rss_bytes`, so a platform with no instrument says `unsupported: …` instead of leaving a silent `null` that reads like an absent field rather than a broken instrument (#338).
- Gold yield at a token budget on the commit-derived benchmarks (`docs/retrieval-benchmark.md`): `scripts/score-context-cases.py` walks the pack's selected units in order under 4,000 / 8,000 / 16,000-token budgets and reports `gold_file_yield@B`, `gold_line_yield@B` and the median `tokens_to_first_gold` next to R@k and MRR, each with a bootstrap interval. `scripts/commit-derived-cases.py` records the modified line ranges per gold file as a fifth TSV column (base side of `git diff -U0`, numbered on the commit's parent as a proxy for the indexed base) and can re-derive it for an existing file with `--annotate`; the scorer tolerates its absence. `scripts/compare-commit-derived-report.py` prints the yields informationally; nothing gates on them yet.
- The index reports what it did not index. Discovery records, per recognised language, the source files it saw on disk versus the files the index holds, with each discovered file's omission attributed to a skip reason and directories pruned by name or unreadable counted beside the ratio (`quality.coverage` in the manifest; `coverage` in `ok --json status` and the MCP `repo_status` result; `null` for indexes written before this). `ok index` ends with one line (`coverage: 9,982 of 10,012 programming-language files indexed (99.7%); 12,004 of 12,140 recognised files indexed (98.9%) overall; skipped: 25 secret-policy, 5 too-large`), `ok doctor` prints the per-language table for every language and warns with the top three skip reasons when the programming-language ratio (rust, java, typescript, javascript, python, go, sql — the files whose omission costs evidence) or any single programming language falls under 98%, or a programming language is missing 20 or more files; config and prose files are reported in the all-languages ratio beside it but never trigger a warning, because hidden `.github/*.yml` files would otherwise flag almost every repository. The commit-derived benchmark records the line beside every accuracy number. An ingest rule had silently dropped 25 Java files from a 10k-file Java repository (#379) and nothing surfaced it.
- `gte-modernbert-base` (Apache-2.0, 149M parameters, int8 ONNX) as a local neural embedding profile and the default when `[semantic] provider = "fastembed"` names no model. It is pinned to a fixed upstream revision with a SHA-256 check on every file, so upstream cannot change or remove it silently. Chosen on commit-derived corpora against jina-v2-code and Qwen3-0.6B (`docs/embedding-providers.md`). ONNX and Qwen3 embedding now run one length-sorted batch at a time with bounded sequence lengths; the previous parallel, 8k-token batches were killed for memory on a 16 GB machine.
- `scripts/commit-derived-cases.py` and `scripts/score-context-cases.py`: derive leakage-safe retrieval cases from a repository's own history (index at a base commit, gold = files a later commit modified that already existed at base) and score the production `ok context` path against them with bootstrap confidence intervals.
- `scripts/commit-derived-cases.py` keeps one instance of a subject that repeats up to numbers (`--keep-repeated-subjects` restores the old behaviour); the Go corpus shrinks from 481 to 280 cases because 201 were release-bump commits with one gold file.

### Fixed

- The four benchmark corpora's base commits are no longer checked in. A full commit hash of a public repository is searchable and therefore identifies it, so a hash names the corpus as surely as a URL does. The hashes move to `BENCH_<CODE>_A_BASE` repository variables beside the existing `BENCH_<CODE>_A_URL`, both benchmark workflows resolve them from there and fail closed when unset, and the `provenance.base` field in every frozen baseline says where the value lives instead of holding it. The workflow comment claiming base commits "name nothing" was wrong and is corrected.
- `scripts/validate-docs.sh` guards the MCP tool count. It derives the count from the tool table in `crates/open-kioku-mcp/src/lib.rs`, rejects duplicate tool names, checks the in-crate `tools_ro` inventory assertion against that same table, and holds `README.md` and `docs/mcp-tools.md` to the result. The count had been unguarded prose in both documents (#405).
- `regex_search` disclosed the walk's file budget but not the shared `MAX_MCP_FETCH` candidate cap. `search_fetch_limit` asks for `offset + limit + 1` and clamps at 500 while `offset` accepts 10,000, so a deep page lost the sentinel `has_more` is derived from: `{pattern: "fn ", limit: 20, offset: 500}` on a repository with tens of thousands of matches returned `has_more: false, truncated: false` and no warning, and an agent paging a broad pattern would conclude it had seen every occurrence. The cap disclosure now lives in one place that both the ranked and regex paths run their metadata through, so the two cannot drift again.
- `get_symbol_context` reported a body it could not recover at all, but not one it recovered only in part. Lines that fall outside every indexed chunk are skipped and the returned range was rebuilt from what came back, so a short bundle read as a whole definition; it now names how many of the sought lines were missing. The chunk-boundary caveat also named the last line recovered instead of the boundary itself, so it could contradict the evidence string beside it.
- Six MCP tool descriptions said what their names suggested rather than what their implementations do, and now say the latter (#405). `structural_search` and `explain_search_result` are named as aliases of `search_code` and `hybrid_search` — no AST matching and no separate explanation step exist in the workspace. `recommend_validation_plan` is named as an alias of `find_tests_for_change` and no longer claims static checks or coverage actions. `architecture_violations` is named as an alias of `architecture_boundaries` returning the same summary. `search_symbols` no longer claims fuzzy ranked matching: it is `list_symbols` with a case-insensitive substring filter ordered by qualified name. `search_files` shares the `search_code` dispatch arm and no longer advertises the size and language metadata a `SearchResult` has never carried. Input-schema parameter descriptions were corrected alongside the tool descriptions, so the two halves of a tool no longer disagree. `get_definition` and `explain_symbol` no longer claim a body or relationship edges they never returned. No tool was added, removed, or merged by that change; the consolidation to sixteen advertised tools (see Breaking) came afterwards in the same release.
- `get_symbol_context` returns a symbol's definition body. (It was folded into `get_definition` behind `include_body` later in this same release; the behaviour below is what `get_definition` now delivers.) It promised "full definition body … documentation comments, and surrounding code context" and was an alias of `get_definition`, returning one `Symbol` record with no text at all (#405). It now returns the body recovered from indexed chunk text with the line range it spans, plus up to ten indexed lines on each side. Documentation comments are reported only as the verbatim leading lines the indexer actually chunked, never parsed out or reconstructed, and their absence — the first symbol in a file has no indexed preamble — is stated as a caveat. A body that cannot be recovered returns the symbol with an explicit caveat rather than a shorter bundle that reads like a complete one.
- The MCP `regex_search` tool performs regular-expression matching. It advertised "exact regular-expression pattern matching against indexed source code lines" and dispatched to the ranked BM25 path, so an agent that asked for a pattern got lexical guesses with nothing in the response saying so; a correct line-by-line matcher had shipped in `open-kioku-search-regex` with no caller anywhere in the workspace (#405). The pattern is now compiled once and evaluated over indexed chunk text, file by file in path order, returning exact single-line hits at confidence 1.0. The walk is paged so only one file's chunks are resident, is capped at 20,000 files, and reports both the number of files scanned and a `truncated` warning when it stopped early. An unparseable pattern is a tool error rather than an empty result.
- Gold yield is averaged over every scored case instead of only the cases whose pack selected units. A pack that selects nothing delivered no gold lines, so it now scores 0 rather than leaving the denominator; the previous mean excluded those cases, which inflated it and made yield incomparable with `gold_recall@20`, which always averaged over every case. Line yield is averaged over the cases that carry modified line ranges, with abstaining cases among them scoring 0. Published yield figures move down slightly as a result.
- One file can no longer abort an index. A file that vanished, lost read permission between discovery and parsing, or crashed a grammar is now dropped on its own: it is recorded as a `SkipReason::Error` entry in `skip_counts` / `skipped_paths` with source `filesystem` or `parser`, moved from `indexed` to `skipped` in `quality.coverage` so the ratio never claims a file the index does not hold, surfaced as a phase warning, and the rest of the repository still indexes. Previously the parse phase propagated the first `fs::read` error and the user was left with an empty `.ok/` and the identical failure on retry (#350). The panic payload is not recorded — it can quote the source text around the failing byte, and parser messages stay redacted.
- The indexer no longer drops source files whose path contains "secret" or "credential" (25 Java files vanished from one repository), and no longer skips generated files: they are indexed, flagged `is_generated`, ordered behind other candidates before each stream's cap, and ranked at the lowest quality tier (`generated_file_demotion`) unless the task names the file's own path. Data, config and prose files keep the secret-name rule (#379). The Python holdout against the same index, measured on the production `ok context` path with `scripts/score-context-cases.py`: R@5 0.645 → 0.665, R@20 0.741 → 0.767. That pair was measured in PR #390's own A/B, before the re-index and before #378 landed; the re-frozen baseline under Changed (0.663 / 0.759) is measured after both, and the two changes' shares could not be separated.
- Task vocabulary now reaches the repository's own identifiers through an identifier lattice built per query from indexed symbol names and file stems: `ChannelsUtils Tests` reaches `ChannelUtilsTests`, and a misspelt part of six or more letters that the repository spells nowhere is corrected by one edit. Reached identifiers are extra lexical terms and, when one names a file, place it one relevance tier below a name the task spelled exactly — never at the named-target tier and never exempt from the docs/tests demotion, so a guess cannot outrank an exactly-resolved definition. They never become exact-symbol anchors. An identifier that reaches nothing, and a hop withheld because too many files carry the name it reached, are both reported as caveats. Only whole identifiers expand, only when substring retrieval cannot already reach them, and only for code-shaped tokens; re-inflecting prose words was measured and dropped because it cost 0.021 MRR on a Python library (~4k files) for no gain. No model, no network, no re-index (+51 ms median end-to-end CPU per reaching query on a 10k-file index). Commit-subject benchmarks cannot exercise this path — their queries are written by the author who just edited the file, so identifiers are already spelled the repository's way, and on them the lattice is neutral (Go (~800 files), TypeScript (~900 files), and Python (~4k files) bit-identical; Java (10k files) moves one case). Measured instead on perturbed queries that rewrite one identifier per case into a plausible task-description form: over 259 such cases from all four corpora, R@5 0.656 → 0.699 and MRR 0.548 → 0.584 (+0.036, 95% CI [+0.015, +0.062]); the 367 unperturbed control cases move by exactly 0.000. Those figures are an upper bound: the perturbations are the inverse of the lattice's own hypothesis class.
- Context packs on test-heavy repositories no longer fill their primary files with tests: the validation candidate stream declared corroborating authority for any test whose name shared a word with the task, and fusion orders by authority before score. Test paths are now recognised by directory segment and file name (`src/internalClusterTest`, `*IT.java`, `foo.spec.ts`), never by substring, and a task that asks for tests keeps them in the source tier. On an 11k-file Java checkout, "quota enforcer" went from 0 of 5 source files in the top five to 5 of 5.
- The first capitalised word of a commit-style task ("Fix", "Enable", "Assert") was treated as the primary edit anchor and boosted every file containing that substring above the real lexical hits; identifiers now need an inner case change, a separator, or digits beside capitals. The lexical candidate stream also merged per-term results by minimum rank, letting the top hit for a single expansion word tie with the top hit for the whole task. Production-path R@20 on the same 60 commit-derived cases: 0.42 → 0.73, MRR 0.22 → 0.44.
- Git history no longer votes for primary context from commit-message similarity to task prose: on a 10k-file repository with history those votes cost 0.075 MRR (monotonically across weights) and ~5 s per query. The history stream now anchors on exact symbols or paths only, and per-file history annotation queries churn and co-change for the file rather than the task text. Similar-change statics (co-change edges, hotspots) are cached per store instead of re-read per result.
- Function words and commit verbs ("for", "fix", "add") no longer count as task vocabulary for test/runtime name overlap, and the fusion profile `rrf_measured_v1` votes the validation stream at half weight (neutral on the 490-case corpus; restores a strong lexical hit that two weak test-name votes had outranked).
- Reciprocal-rank fusion uses k=10 instead of 60: with one full-text ranker and several name-overlap hint streams, k=60 made lexical rank 2 indistinguishable from rank 13, so any file with two weak votes outranked a single strong one. Neutral on the 490-case corpus (dev MRR 0.386 → 0.384, holdout 0.345 → 0.348); restores the workflow benchmark's `test-selector` case.
- Measured on the Go and TypeScript commit-derived corpora, six further ranking defects: routing keywords matched substrings ("docstool" routed a code task to the documentation family); a task that merely says "panic" was routed to trace-to-code and *blocked* outright on repositories that have never ingested a runtime trace (a required source that cannot run is now a caveat, not a blocker); benchmarks were not test intent although in Go they live in `_test.go`; an anchor *mentioned* in a snippet or a docs page shared the top relevance tier with the file that names it and outranked the best full-task hit (tiers are now definition > explicit path/ticket > mention, and the docs/tests quality tier is applied first); documentation tasks ran no lexical stream although doc comments live in source files; and the corpus extractor now drops commits whose subject names a path.
- A directory merely named after tests (`crates/open-kioku-tests/`, `packages/e2e-tests-runner/`) is not a test path; only exact test directory names, CamelCase test source sets, and test file names are.
- Git history votes for the files that near-identical past commit subjects touched (numbers and PR references stripped, Jaccard ≥ 0.75, half the twins must agree on a file). On a Go repository whose holdout contains 50 release-bump commits with one gold file, holdout R@5 0.428 → 0.772 and MRR 0.338 → 0.511 with history present; a repository without repeated subjects is unchanged.
- A commit scope's directory entry file (`mod.ts`, `index.ts`, `lib.rs`, `__init__.py`) is a candidate even without shared vocabulary, placed just below the scope's best hit when nothing in the directory knows the task's words and last otherwise. The TypeScript holdout R@5 0.744 → 0.816, MRR 0.607 → 0.650; other corpora unchanged.
- Commit-style scope prefixes (`docs(cache): …`, `pkg/layout: …`, `[Planner] …`) now anchor retrieval on the package or directory they name, and document sections cast one vote per file instead of one per section (a documentation task on the TypeScript corpus had returned twenty sections of its release-notes file and nothing else). Holdout MRR on the commit-derived corpora: TypeScript (~900 files) 0.510 → 0.607, Go (~800 files) 0.308 → 0.338, Python (~4k files) 0.539 → 0.565.
- Context packs showed the right file but the wrong region: selection units are chunk-sized, and on the 626 locally derived line-range-annotated cases from four large public repositories the selected units covered 3-22% of the lines the real commit changed even when the file was right, identical at 4k, 8k and 16k tokens (packs were 0.8-1.2k tokens; the budget was not the binding constraint). After selection, the first three primary files (`ContextBudget::region_files`) now have their selected regions widened - enclosing symbol, the file's other task-ranked units, adjacent chunks - up to 1,200 estimated tokens per file (`region_tokens_per_file`). Widening runs after selection and only grows or appends units, so nothing selection chose is removed or reordered; each step is a `region:` evidence ref on the unit.

  Measured over the pack's primary units, which is what widening can affect. `gold_line_yield@8k` is the share of the lines the real commit changed that the pack shows within 8k tokens:

  | Corpus | `gold_line_yield_primary@8k` | per-case delta (95% CI) | improved / unchanged / worse | `gold_file_yield_primary@8k` | primary pack tokens p50 / p95 | pack JSON KB p50 / p95 |
  |---|---|---|---|---|---|---|
  | Java (10k files) | 0.216 → 0.248 | +0.032 [+0.013, +0.058] | 12 / 104 / 0 | 0.537 → 0.537 | 1,152 → 3,317 / 2,178 → 4,540 | 647 → 700 / 825 → 876 |
  | Go (~800 files) | 0.207 → 0.299 | +0.092 [+0.057, +0.133] | 27 / 118 / 0 | 0.696 → 0.696 | 883 → 2,882 / 1,549 → 4,322 | 790 → 849 / 1,014 → 1,084 |
  | TypeScript (~900 files) | 0.155 → 0.335 | +0.181 [+0.138, +0.229] | 60 / 108 / 0 | 0.691 → 0.691 | 836 → 2,753 / 2,752 → 4,460 | 728 → 773 / 892 → 951 |
  | Python (~4k files) | 0.130 → 0.203 | +0.073 [+0.046, +0.102] | 37 / 160 / 0 | 0.608 → 0.608 | 935 → 3,612 / 5,600 → 7,629 | 732 → 765 / 907 → 948 |

  The design is paired - the same cases, the same corpus, one change, and no case changed its rank - so the statistic that answers whether widening moved anything is the interval around the per-case delta, not the overlap of two independently bootstrapped absolute intervals. All four deltas exclude zero, and in 626 cases not one got worse; Java's is the smallest and rests on 12 cases, so it is real but slight. Widening cannot add a gold file - it grows regions inside files selection already chose - and at 8k and 16k tokens `gold_file_yield_primary` is unchanged in every corpus. At 4k it is not: widened regions consume the budget before later gold files are reached, so the region arm's `gold_file_yield_primary@4k` reads 0.530 against 0.537 on Java, 0.693 against 0.696 on Go and 0.602 against 0.608 on Python (TypeScript 0.691 in both arms; the base arm is identical at every budget). Java is where the per-file cap binds, with widened files pinned at ~1,190 of their 1,200 tokens. Showing more of each top file costs roughly three times the tokens. R@5, R@20, MRR and `gold_recall@20` are identical in every corpus and no case of 626 changed its rank, its top five, or its gold recall.

  Measured at `48e64c9` on a local workstation with `--workers 2`, both arms built from that commit; the branch was afterwards rebased onto `0684e62` and not re-scored. Both arms' scorer reports are committed as `benchmarks/commit-derived/region-widening-ab.json`. The case files are locally derived line-range-annotated cases (116/145/168/197 = 626) on the same base checkouts as the frozen splits, not those splits themselves (113/84/166/199 = 562); the Go file keeps 50 near-identical release-bump commits, whose gold is a single three-line region, so its line yield reads higher than the frozen corpus would give. This is a scale record, not a replayable corpus.

  Two cautions. On the file-limit budget the CLI and MCP default path uses, `max_tokens` is a sentinel and no token ceiling is enforced, so `region_tokens_per_file` is the only bound that binds; a caller that passes `ContextBudget::default()` has 6,000 spendable tokens (8,000 less the two 1,000-token reserves), and the Python p95 of 7,629 is above that, so on a budget-enforcing path widening stops early and these figures do not transfer. `region_files` x `region_tokens_per_file` is 3,600 tokens, over half of that 6,000 ceiling, so revisit the defaults if a budget-enforcing path becomes the default.
- Supporting files are costed in the context pack's selection ledger at their listing size (path and reason, not the impact snippet), so the ledger accounts for everything the pack presents rather than the primary units alone. Measured separately from region widening, because it changes what the yield metric counts rather than what retrieval returns: over all ledger units `gold_file_yield@8k` reads 0.537 → 0.548 (Java), 0.696 → 0.739 (Go), 0.691 → 0.771 (TypeScript) and 0.608 → 0.679 (Python), tracking `gold_recall@20`, and `gold_line_yield@8k` reads higher than the primary-only figures above by +0.0001 (Java, one case), +0.009 (Go, six cases), +0.024 (TypeScript, nineteen) and +0.013 (Python, nine) - near-zero on Java and concentrated on TypeScript. `scripts/score-context-cases.py` therefore reports both families, and the `_primary` one is what compares across versions. Each ledger unit carries a `kind` (`primary` or `supporting`) so the split is a field rather than a phrase a reword could silently flip; both sides pin it (`ledger_units_carry_the_kind_that_tells_selection_from_impact_expansion`, `scripts/tests/test_context_yield.py`).
- A pack whose selected context contains fewer than a third of the task's terms is capped strictly below Medium confidence; previously the cap sat exactly on the Medium threshold and a nonsense query with one incidental word match reported Medium.

### Performance

- The index is 39% smaller and indexing peaks 15% lower in resident memory. Measured on a fixed subtree of a public 10k-file Java service at a fixed commit (1,751 Java files -> 2,528 indexed files, 161,562 edges, **identical totals on all 16 runs in both arms**), release builds differing only by this change (base `a8daa49`, branch `be5d6ae`, identical tree to the merged commit), on a local workstation, fresh `.ok` per run, arms alternated in ABBA pairs. `.ok/index.sqlite` 1,202,847,744 -> 732,633,088 bytes (**-39.1%**, largest within-arm spread 4,096 bytes over 8 runs per arm); the whole `.ok` directory -36.4%. `graph_edges` with its indexes and dictionary went 400 -> 137 MiB, `call_sites` 243 -> 57 MiB; `analysis_facts` is untouched at 170 MiB and is now the largest table. Peak RSS 1,958,623,232 -> 1,659,136,000 bytes (**-285.6 MiB, -15.3%**, medians of 8 runs per arm, with every after-run below every before-run), reproduced independently at -15.4% by the three `--features mem-profile` pairs. The allocator counter puts only 18.03 MiB (-1.70%, within-arm spread 1,376 bytes) of that in bytes *requested* from the Rust allocator; the remaining ~268 MiB is not requested bytes and is attributed to the bulk-load page cache touching a 39%-smaller database by inference from elimination, not by measurement: the counter cannot see the page cache (`docs/memory-profiling.md`), and no instrument in the run observed it. Allocation count is up 2.26% — interning hashes every string — so the win is in bytes retained, not churn. **Wall-clock indexing time is unresolvable on this host**: the within-arm spread was 108 s on runs of 55-163 s as the host load ranged from 9 to 56, and the paired median was +2 s with a range of -84 to +27 s. No indexing-time claim is made either way. (#363, #329)
- `ok context`, `ok plan`, and MCP `build_context_pack` on a 10k-file Java index: 78 s → ~5 s per query (a single `ok context` on the Java service's base index, local workstation, measured in `ec8a937` (78 s → 16.6 s) and `f73c22a` (16.6 s → 4.8 s) against their parent commits; no committed artifact). Impact expansion now uses the Tantivy index instead of regex-scanning every chunk once per term; per-file fact lookups use the existing `file_id` index instead of scanning and sorting every fact of a source type; the relationship-semantics verdict is cached per store (keyed by SQLite `data_version`) instead of re-parsing a multi-megabyte manifest on every relationship query.

### Changed

- Commit-derived baselines re-frozen from the 2026-09-08 hosted matrix on main (after generated-file indexing and the scope entry-file candidate). Holdout R@5 / R@20 / MRR now read Java 0.566 / 0.699 / 0.504 (was 0.549 / 0.681 / 0.482), Go 0.679 / 0.809 / 0.535 (was 0.690 / 0.810 / 0.551), TypeScript 0.825 / 0.874 / 0.658 (was 0.753 / 0.801 / 0.621), Python 0.663 / 0.759 / 0.545 (was 0.658 / 0.749 / 0.556); the Go and Python MRR dips are inside the nightly slack and are recorded rather than hidden.
- The Tantivy index tokenizes code text and symbols identifier-aware: `SlotPlanner` is indexed as `slotplanner`, `slot`, and `planner`. Lexical MRR on a 490-case commit-derived benchmark on the Java corpus: 0.235 → 0.393 (dev), 0.202 → 0.337 (holdout). Existing indexes keep working; run `ok index` to rebuild with the new tokenizer.
- MCP tool responses no longer send rendered Markdown/TOON text twice: `content` carries the text once and `structuredContent` carries a small pointer (`rendered_in`, `bytes`, `truncated`) instead of a copy. A `build_context_pack` response that measured 122 KB on the wire is now about 61 KB; JSON results are unchanged.
- Retrieval benchmark no-gold false positives count only results presented with confidence above Low (the product's abstention signal), for pack-less strategies via the same shared weak-relevance rule. Baseline and thresholds re-frozen; rationale in `docs/retrieval-benchmark.md`.

### Artifacts

- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [3.1.0] — 2026-08-31

### Fixed
- Stopped the graph query-column migration backfill from full-scanning (and partially rewriting) the graph tables on every store open. On a 16.5k-file Java corpus this removed ~14 seconds of fixed latency from every CLI command; existing stores migrate once and record completion.
- Fixed exact symbol lookups returning `symbol not found` for symbols that exist: the substring scan ordered by qualified name could truncate the true exact match out of its candidate window. `ok symbol definition` now consults an indexed exact-name path first (13.9s → 0.02s on a 247k-symbol corpus, with the correct result).

### Performance
- Bulk graph replacement drops and rebuilds secondary indexes around a prepared-statement, primary-key-ordered insert under a scoped page cache. Cold structural indexing of a 16.5k-file Java repository improved from 40m40s to 19m28s (graph write 28m37s → 8m17s).
- Ingest releases the parsed corpus in one consuming pass instead of cloning every extracted field, graph nodes are moved rather than cloned twice, and search git-history annotation groups facts by file instead of rescanning the full fact list per result.

### Added
- Index components now live in atomically published generations under `.ok/generations/<id>/` with an `active` pointer that is only ever replaced atomically. Legacy layouts keep working and are adopted in place (a directory move, not a copy) on the next `ok index` under the write lock; every read path resolves through the active generation. `ok status`/`ok doctor` report the generation identity and classify on-disk generations; MCP `repo_status` exposes `generation_id`. Design: `docs/index-generations-design.md`.
- Calibrated abstention can now be activated at runtime. `ok retrieval-bench --write-abstention-activation` emits a fail-closed activation artifact only when the calibrated policy passes the holdout readiness gate; with the artifact present, context packs that fail the calibrated evidence gates carry an explicit `calibrated_cc6_abstention` reason and caveat instead of presenting weak context confidently. The benchmark and the runtime share one decision code path so measured and deployed behavior cannot drift.
- Semantic search routing now carries an explicit caveat when the persistent ANN backend serves a candidate population above the measured recall-degradation ceiling (~300K vectors, per `benchmarks/cc5-ann-scale-evidence`).
- Test selection carries an explicit `selection_tier` (required/recommended/optional) with evidence justification; heuristic name or path similarity alone can never mark a test required. Plans distinguish structurally proven dependents from possible (heuristic) ones in summaries and per-file caution rules.
- Churn policy gates and per-dimension retrieval floors are version-controlled contracts (`benchmarks/cc5-ann-churn-thresholds.json`, `benchmarks/retrieval-dimension-thresholds.json`, advisory-first) wired into CI.
- Semantic lifecycle health is now explained by `ok status`, `ok doctor` (semantic-lifecycle check with concrete rebuild reasons), structured `rebuild_required`/`rebuild_reasons`/`last_rebuilt_at`/`stale_ratio` fields on semantic status, and a `semantic_lifecycle` block in MCP `repo_status`.
- Impact analysis classifies relationship-edge dependents into `proven_impact` and `possible_impact` through the shared fail-closed `RelationshipUsePolicy` — a heuristic same-name edge can never be presented as structural truth. Wired through the CLI, MCP `impact_analysis`, context compilation, planning, and patch verification.
- `ok --version --json` machine-readable version output and copy-paste examples in `ok status/search/impact --help`.
- Measured 50K→1M ANN scale evidence recorded under `benchmarks/cc5-ann-scale-evidence` (recall collapses beyond ~300K vectors on the current HNSW profile; profile decision tracked in #328).

### Compatibility
- Index layouts migrate one-way into atomic generations on the next `ok index`. 3.0.x binaries pointed at a migrated repository will report a missing index (and would rebuild at the legacy path); no data is lost, but downgrading means re-indexing. All CLI flags, MCP tools, and response shapes remain backward compatible; new fields are additive.
- Existing stores gain the new symbol-lookup indexes and the one-time graph migration marker automatically on first open (a final full scan on large stores, then never again).

### Validated
- Large-Java validation on a private 16.5k-file corpus, same host and protocol as the 3.0.4 record: cold structural index 19m28s, exact class lookup 0.02–0.05s in a fresh process (previously 13.9s with an incorrect symbol-not-found), lexical search 0.24s, repeat totals identical, four parallel readers with zero lock failures, 495,606 semantic vectors with zero failures across both backends. Record: `docs/large-java-validation-2026-08-31.md`.
- Full 50K→1M ANN scale evidence recorded with the measured recall ceiling documented and caveated at runtime.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [3.0.4] — 2026-08-23

### Fixed
- Made concurrent readers wait for short-lived SQLite schema writers instead of failing with `database is locked` on large indexes.
- Anchored one-hop graph equality queries through indexed node labels, symbol names, and directional edge endpoints, eliminating full edge scans for exact Java definition, call, and implementation lookups.
- Ranked exact symbol and file-stem identities ahead of broader camel-case prefix matches while preserving natural-language workflow relevance and explicit score provenance.

### Validated
- Indexed a large Java repository: 9,312 indexed files, 136,212 symbols, 136,646 chunks, 211,057 graph nodes, and 707,271 graph edges.
- Reproduced the full structural index deterministically, validated concurrent lexical and graph reads, and built 272,858 semantic vectors with both exact-flat and persistent HNSW backends.

### Changed
- Added machine-readable MCP routing categories alongside titles, detailed usage guidance, schemas, maturity, and safety annotations for all 58 tools, with a regression test that rejects incomplete tool metadata.

### Compatibility
- Existing indexes are upgraded in place with the new graph-label index; no functionality or semantic backend is removed.
- Compiler-grade Java SCIP remains optional and externally provided. Structural Java indexing remains fully operational when an upstream `scip-java` build integration is unavailable.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [3.0.3] — 2026-08-22

### Changed
- Upgraded the persistent lexical search backend from Tantivy 0.22 to Tantivy 0.26.1 while preserving score-ordered BM25 results and compatibility with existing indexes.
- Raised the Rust MSRV to 1.86, the minimum supported by Tantivy 0.26.1.

### Fixed
- Removed large-repository indexing hot spots by pre-indexing legacy call facts and deterministic same-file and lexical-scope symbol candidates instead of repeatedly scanning global fact and symbol collections.
- Bounded human-readable status quality diagnostics to 100 deterministic notes with an explicit omitted count and a pointer to the complete JSON report, preventing multi-megabyte terminal output without hiding totals.
- Preserved complete semantic functionality across deterministic local embeddings, persistent HNSW ANN, and opt-in Jina neural embeddings.

### Compatibility
- Existing Tantivy 0.22 indexes remain searchable, and a normal full index rebuild recreates the lexical index using Tantivy 0.26.1.
- The supported release artifacts remain Linux x86_64/ARM64 GNU, macOS ARM64, and Windows x86_64.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [3.0.2] — 2026-08-20

### Fixed
- Fixed repository discovery so nested `.gitignore` and `.okignore` rules remain correctly scoped, tracked Git files remain indexable, large ignore batches cannot deadlock, and zero-result filtering is reported instead of silently succeeding.
- Kept Context Compiler ambiguity telemetry consistent across top-level and selection-scoped diagnostics without borrowing authority from ambiguous legacy traces.
- Reported exact, graph, and validation candidates as high-value omissions when the context budget has zero remaining capacity.
- Bound downstream Context Compiler authority to the caller-visible primary selection so hidden retrieval candidates cannot widen symbols, dependency seeds, impact anchoring, or allowed edit boundaries.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [3.0.0] — 2026-08-17

### Added
- Added evidence-first routed context retrieval with provenance-aware bounded context, explicit blockers, and measured retrieval quality gates.
- Added task/query-shape routing measurement with frozen labels, adversarial probes, classifier accuracy, task-family × query-shape quality, and latency reporting while preserving the generic blocking retrieval baseline.
- Added quality-tiered local neural embedding profiles alongside the deterministic local-hash baseline, with opt-in local model acquisition and provenance.
- Added persistent local HNSW semantic indexing with exact-flat correctness fallback, persistence, filter parity, and scale calibration.
- Added proof-carrying relationship authority and deterministic proof-gated structural resolution for calls, references, type use, inheritance, and imports.
- Added the `ok relationship-bench` conformance scoring foundation with strict proof/range/outcome/metamorphic threshold policy and reproducibility metadata.

### Changed
- Made `ResolutionMode::Shadow` the default so proof-gated structural relationships are operational while legacy evidence remains available for compatibility; explicit `Legacy` and `V2` modes remain available.
- Made authoritative architecture/context consumers fail closed on unproven structural relationships instead of promoting heuristic confidence into graph truth.
- Refreshed the homepage and README around the evidence-first workflow and real pinned-main dogfood proof.
- Bumped the 43-crate workspace and all release/install channels to 3.0.0, including explicit 3.0.0 requirements for publishable internal Cargo path dependencies.
- Hardened release publishing so built binary SHA-256 values must match checked-in release metadata before GitHub/npm publication.

### Fixed
- Preserved typed authority for uniquely resolved import targets and exact Rust `crate::module::member()` calls without enabling fuzzy structural fallbacks.
- Updated public quickstart validation to treat `ok setup agent ...` as the primary onboarding flow while retaining lower-level `init`, `index`, and manual MCP commands as supported primitives.

### Compatibility
- Reindexing is recommended for 3.0. Existing heuristic structural edges from older indexes are not trusted as authoritative unless reconstructed with proof; relationship counts may decrease when ambiguous evidence correctly fails closed.
- 3.0 Linux release binaries target GNU/glibc on x86_64 and ARM64 because the local neural runtime does not provide supported MUSL prebuilts; npm Linux platform packages declare `libc: glibc` accordingly.
- The checked-in relationship scorer is a conformance-scoring foundation; the full frozen >=300-case #240 corpus remains follow-up work and is not claimed complete by this release.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [2.4.0] — 2026-08-14

### Added
- Implemented 1-command agent onboarding command (`ok setup agent <agent>`) for Cursor, Claude Code, and Codex.

### Changed
- Scaled symbol-edge resolution for large codebases by pre-indexing file imports, leveraging Rayon chunk parallelization, zero-allocation matching keys, pre-indexing symbol suffixes, and capping fuzzy name scans.
- Improved `publish-crates.sh` with a 15-second crates.io index propagation pause between workspace crate publications.

### Fixed
- Fixed non-UTF8 binary patch parsing in `open-kioku-git`.
- Removed experimental nonfunctional `apply_patch` MCP tool.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [2.3.0] — 2026-07-04

### Added
- Added dedicated setup guides for Claude Code, Cursor, Codex, and Gemini CLI in the `demo/` directory.

### Changed
- Optimized the documentation layout and repository quickstart to focus on local-first onboarding diagnostics.
- Updated the release manifest synchronization script to automate version propagation to demo site resources.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [2.2.3] — 2026-07-04

### Fixed
- Fixed JSON-RPC notification handling in the MCP stdio server so `notifications/initialized` produces no response, matching MCP client expectations and unblocking Glama/mcp-proxy container inspection.
- Hardened release publishing so reruns skip npm package versions that are already published instead of failing with immutable registry conflicts.
- Hardened GitHub Pages demo deployment by canceling stale queued deployments and extending the deployment timeout.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [2.2.2] — 2026-07-02

### Changed
- Rewrote the descriptions and guidance text of 27 MCP tools to state each tool's purpose, inputs, and outputs more clearly.
- Added explicit "Do NOT use when..." instructions, detailed sibling tool alternatives, and clarified data source and side-effect transparency.
- Enriched all tool parameter schemas with default values, value constraints, and explicit semantic descriptions.
- Updated integration test tools list snapshot to reflect the updated tool specifications.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [2.2.1] — 2026-07-02

### Added
- Added root `glama.json` metadata so Glama can associate the MCP listing with the repository maintainer.
- Added MCP `title`, `annotations`, and `outputSchema` metadata to every tool definition.

### Changed
- Expanded MCP tool descriptions with explicit when-to-use guidance, sibling alternatives, and side-effect transparency.
- Marked write-like MCP tools with accurate read/write/destructive/open-world annotations.
- Decomposed the CLI crate into command, benchmark, report, and shared type modules while keeping the binary behavior intact.
- Added GitHub star and npm download badges to the README.
- Reconciled the release line after `v2.2.0` so package registries receive the current `main` MCP tool surface.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [2.1.1] — 2026-06-21

### Fixed
- **SQLite Ingestion & Backfill Performance**:
  - Wrapped SQLite node and edge backfill updates (`backfill_graph_query_columns`) inside transactions. This critical performance fix reduces backfill time on large codebases (such as a 10k-file Java repository with 269k stale edges) from several hours to under 25 seconds.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [2.1.0] — 2026-06-21

### Added
- **Architecture Boundaries & Enforcement**:
  - Implemented architecture policy validation with the new `ok architecture policy check` CLI command and `architecture_policy_check` MCP tool, evaluating dependency rules against graph imports, calls, and references.
  - Implemented configuration-based policy component resolution (`ok.toml` components).
- **Evidence Graph v2 (E1-E19 Series)**:
  - Added complexity analysis and relationship evidence passes (E19).
  - Strengthened validation evidence selection and test-to-code target selection (E18).
  - Strengthened runtime evidence aggregation and ingestion (E17).
  - Promoted service boundary graph facts (E15).
  - Introduced versioned evidence graph schema manifest and mapped SQLite metadata directly to it.
- **Cross-Project Workspace Linking**:
  - Added cross-project workspace linking (E16) to allow multi-repository planning and context packs.
- **Git History & Provenance Tracking**:
  - Implemented incremental git commit history parsing and ingestion to extract co-change metrics.
  - Added file and symbol level historical provenance lookup CLI command and MCP tool.
- **Change Contracts**:
  - Introduced versioned change contracts (`ContractBuilder` and schemas) and contract store persistence to ensure pre-edit plans are verified post-edit.
- **High-Performance Ingestion & Graph Buffer**:
  - Implemented incremental index updates, parsing only modified files for rapid re-indexing.
  - Implemented a high-throughput deduplicating `GraphBuffer` for buffered database writes.
  - Added symbol registry resolution, discovery skip reporting, and import manifest resolution.
  - Supported index snapshot export/import for transferability.
- **Client & Integration Ecosystem**:
  - Added auto-installation and support for **Windsurf** and **Trae** MCP configurations.
  - Scaffolded the repository-scoped **Codex** marketplace and browser plugin.
  - Added Glama verification metadata.

### Changed
- Bumped workspace packages, plugins, manifests, and homebrew formulas to version 2.1.0.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [2.0.1] — 2026-06-05

### Added
- Added styled GitHub star call-to-action cards and buttons on the landing page and npm README to bridge package discovery and GitHub conversions.
- Added subtle, action-oriented post-install success prints to `ok init`, `ok demo`, and `ok prove` commands.
- Synced metadata repositories, homepages, and bugs fields for all sub-packages in the workspace.

### Changed
- Bumped workspace crates and manifests to version 2.0.1 to publish patch updates.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [2.0.0] — 2026-06-05

### Added
- Added a README motion demo and copy-paste 60-second quickstart that runs `ok demo`, generates an evidence-backed plan, and verifies a bounded edit.
- Added reproducible demo scripts: `scripts/quickstart-demo.sh` runs the flow and `scripts/render-quickstart-demo.py` regenerates the GIF asset.
- Added local vector index and hybrid semantic search.
- Added visual crate map showing codebase architecture and dependency layers.
- Added Elastic License 2.0 FAQ and STABILITY.md documentation.
- Added workflow benchmark regression suite.
- Added git co-change history signals and runtime evidence integration.
- Added integration test coverage for Java fixtures and CLI smoke tests.

### Changed
- Bumped all crates and workspace packages to version 2.0.0.
- Evolved homepage to highlight plan-before-edit paradigm and show real large-Java proof numbers.
- Upgraded domain routing for openkioku.com.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [1.0.4] — 2026-06-04

### Fixed
- Re-published the 1.0.3 release candidate as 1.0.4 so crates.io can resolve all internal Open Kioku packages against the corrected static/runtime analysis APIs.
- Kept the GitHub release, npm packages, Cursor manifest, Claude manifest, and crates.io package versions aligned.

### Artifacts
- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`

---

## [1.0.3] — 2026-06-04

### Added
- Added repo-scoped memory facts with local append-only storage and MCP/CLI recall.
- Added reversible compressed context handles with local original retrieval.
- Added optional TOON output for context packs, compressed context packs, and pre-edit plans.
- Added language-specific static analysis facts for imports, inheritance, implementations, routes, config reads, and table mappings.
- Added optional local runtime evidence ingestion from repository-owned JSONL artifacts under `.ok/runtime/` or `.ok/analysis/runtime/`.
- Added release-readiness smoke coverage for status, setup audit, TOON planning, proof reports, and MCP installer output.
- Added large-repo proof documentation for a local large-Java validation run.

### Changed
- Improved task-anchor planning, impact evidence, test selection, and low-confidence risk reporting.
- Updated MCP tool schemas and docs for memory, compressed context, and TOON prompt handoff.
- Strengthened Gradle Java validation command selection and setup/status quality reporting.

---

## [1.0.1] — 2026-06-04

### Changed
- Added crates.io publishing metadata and versioned internal workspace dependencies.
- Reduced README duplication and focused the getting-started path on install, index, verify, and MCP setup.
- Updated npm, Cursor, and demo package metadata for the 1.0.1 release.

## [1.0.0] — 2026-06-04

### Added
- Added phase-level indexing progress for CLI indexing, benchmark, and proof flows.
- Added an index writer lock to prevent concurrent SQLite/Tantivy writers from corrupting or racing index updates.
- Added bounded context and planning paths that reuse persisted Tantivy search results for large repositories.
- Added fast validation-target selection for large repositories.

### Changed
- Replaced heuristic reference expansion with exact definition occurrences plus SCIP-imported occurrences when available.
- Optimized graph construction, Tantivy rebuilds, symbol definition lookup, context building, planning, and test selection for large repositories.
- Expanded default excludes for dependency, build, generated, and internal index paths.

### Fixed
- Fixed indexing blowups caused by highly repeated method and property names in large repositories.
- Fixed JSON and YAML files emitting every key as a symbol.
- Fixed duplicate chunk and symbol records around same-line symbol boundaries.
- Fixed `patch review --json` to return structured JSON.
- Fixed `symbol definition` ranking so exact class/interface definitions beat lower-quality prefix matches.
- Documented the recommended MCP pre-edit routine for Claude Code, Cursor, and other MCP clients.

## [0.1.4] — 2026-05-26

### Fixed
- Added npm package READMEs for the main wrapper package and platform-specific binary packages.

## [0.1.3] — 2026-05-26

### Fixed
- Fixed release packaging for cross-compiled Linux arm64 binaries by skipping host `strip` on incompatible targets.
- Synced Cursor and npm package manifests with the canonical workspace version.
- Extended version validation so CI catches npm wrapper and platform package drift before release.

## [0.1.0] — 2026-05-25

### Added
- **Enhanced health checks** via `ok doctor` with Rust toolchain, Tree-sitter parsers, and MCP initialize checks
- **Signed release binaries** via GitHub Actions with SHA256 checksums and cross-compilation for musl/darwin
- **Fixture repositories** (Rust, TypeScript, Python, Go) and integration tests under `open-kioku-tests`
- **Search evidence wiring** — search results now provide explanatory evidence strings and normalized confidence scores
- **Experimental tool labeling** — `tools/list` differentiates stable vs experimental tools with `--hide-experimental` flag
- **Write safety** — `apply_patch` handler gated behind `OPEN_KIOKU_ALLOW_WRITE=1` environment variable
- **Context export formats** — `build_context_pack` supports JSON, Markdown, and PromptText formats
- **Performance benchmarks** — `ok bench` CLI command and criterion benchmarks under `benches/`
- **MCP server** (`ok mcp serve`) — full Model Context Protocol implementation over stdio with 35+ tools covering search, symbol navigation, impact analysis, architecture detection, and patch planning
- **BM25 / Tantivy search index** — disk-backed full-text search across all indexed code chunks (`search_code`, `regex_search`, `semantic_search`)
- **Tree-sitter parser** — precise symbol extraction for Rust, Java, Python, TypeScript, and Go (`get_definition`, `get_references`, `get_callers`, `get_callees`, `get_implementations`)
- **SQLite metadata graph** — file manifest, symbol table, and dependency graph stored under `.ok/` (`impact_analysis`, `dependency_path`, `module_dependencies`)
- **Architecture detector** — infers high-level component boundaries from file paths (`detect_architecture`, `architecture_violations`)
- **Context pack builder** — assembles AI-ready bundles of primary files, symbols, and tests for a task (`build_context_pack`)
- **Patch planner** — plans code changes without writing files (`propose_patch`, `review_patch`, `validate_patch`)
- **Security posture** — read-only by default; secret paths (`.env`, `.aws`, `.ssh`) blocked from indexing; `apply_patch` gated behind `allow_write: true`
- **Claude Code marketplace manifest** (`.claude-plugin/plugin.json` and `skills/open-kioku/SKILL.md`)
- **Cursor marketplace manifest** (`.cursor-plugin/plugin.json` and `.cursor-plugin/skills/open-kioku.mdc`)
- **CLI** (`ok init`, `ok index`, `ok search`, `ok symbol`, `ok context`, `ok impact`, `ok tests`, `ok status`)

### Fixed
- `serverInfo.name` in MCP `initialize` response corrected to `open-kioku`
- `repository` URL in `Cargo.toml` corrected to `https://github.com/shivyadavus/open-kioku`
- `claude_plugin.json` updated to use `${workspaceFolder}` instead of hardcoded `.`
- LICENSE copyright holder updated to Shiv Yadav
- Added `NOTICE` file as required by Apache License 2.0

[2.4.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.4.0
[2.3.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.3.0
[2.2.3]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.2.3
[2.2.2]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.2.2
[2.2.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.2.1
[2.1.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.1.1
[2.1.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.1.0
[2.0.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.0.1
[2.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v2.0.0
[1.0.4]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.4
[1.0.3]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.3
[1.0.1]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.1
[1.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v1.0.0
[0.1.4]: https://github.com/shivyadavus/open-kioku/releases/tag/v0.1.4
[0.1.3]: https://github.com/shivyadavus/open-kioku/releases/tag/v0.1.3
[0.1.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v0.1.0

[3.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.0
[3.0.2]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.2
[3.0.3]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.3
[3.0.4]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.0.4
[3.1.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v3.1.0
[4.0.0]: https://github.com/shivyadavus/open-kioku/releases/tag/v4.0.0
