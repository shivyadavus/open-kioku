# Changelog

All notable changes to Open Kioku are documented in this file.
This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Breaking
- The advertised MCP tool surface is 16 tools, down from 58 (#406). Every advertised tool now answers one question no other tool answers. Removal is outright: retired names left `tools/list` and the dispatch table in the same change, so an agent holding a stale name from a cached config or an old prompt gets `unknown MCP method or tool` rather than a response whose shape no longer matches the description that name was chosen from. A `RETIRED_TOOLS` list asserts that none of them can reappear.
  - **Folded into the sixteen, no capability lost (23 names).** `repo_status` absorbs `list_languages` (as a `languages` field, taken from index coverage where it exists) and `semantic_status` (as `semantic_lifecycle`, already present). `list_files` absorbs `explain_file` as a per-path detail mode. `search_code` absorbs `search_files`, `semantic_search`, `hybrid_search` and `explain_search_result` behind `mode` (`code`, `graph`, `semantic`, `hybrid`); no `explain` parameter was added, because `score_breakdown` and `evidence_refs` already ship on every result and a flag that toggled nothing would be a new false claim. `search_symbols` absorbs `list_symbols` by making `query` optional. `get_definition` absorbs `get_symbol_context` behind `include_body` and `explain_symbol`, which was already the same call. `get_references` absorbs `get_callers`, `get_callees` and `get_implementations` behind `kind`. `dependency_path` absorbs `module_dependencies` by making `to` optional. `build_context_pack` absorbs `build_compressed_context` behind `compress`. `plan_change` absorbs `preflight_change` and `propose_patch` behind `detail`, and `create_change_contract` behind `persist` (with `store` for a transient contract, and `plan`/`plan_json` so a saved plan becomes a contract without re-planning). `verify_change` absorbs `verify_change_contract` (`contract_id`, `contract`, `contract_json`) and `explain_verification` (`verification`, `verification_json`, or `explain: true` on a contract verification). `find_tests_for_change` absorbs `recommend_validation_plan` and `explain_test_coverage` by making `path` optional. `query_evidence_graph` absorbs `get_evidence_schema`: called with no `query` it returns the schema.
  - **`get_references` response shape.** The merged tool returns an object, not a bare occurrence array. Each evidence kind is its own section naming its own `evidence_source` — `symbol_occurrences`, `sqlite_graph_store`, `persisted_implements_facts` — with its own caveats and its own payload key (`occurrences`, `nodes`/`edges`, `implementations`). Occurrence evidence and persisted IMPLEMENTS facts have different provenance, different confidence, and different meanings of empty, so folding them behind one name moved that distinction into the response rather than erasing it; a test asserts the sections stay separable. `kind: "implementations"` does not require the target symbol to be indexed, because IMPLEMENTS facts are keyed by target name and requiring resolution would have dropped evidence the retired tool returned.
  - **Removed with nothing lost (1).** `structural_search`. No structural or AST matching exists anywhere in the workspace; only the name did.
  - **Config-gated, advertised when the feature is configured (5).** `remember_fact` and `search_memory` when `[memory] enabled = true`; `map_stacktrace_to_code`, `find_errors_for_symbol` and `find_recent_failures` when `[runtime]` names an enabled provider. All five stay dispatchable and the runtime three still return their structured disabled response; what the gate removes is a name an agent would otherwise be taught to reach for and get nothing from.
  - **Moved to the CLI, still shipped (13).** The seven architecture tools (`ok architecture detect|boundaries|violations|summary|policy validate|policy check|policy explain`), the five history and ownership tools (`ok history provenance|churn|similar|ownership|reviewers`), and `get_change_contract` (`ok contract show <id>`). **State the trade plainly: a coding agent with a shell can still reach every one of these through `ok`, but a pure MCP client with no shell cannot.** That is a deliberate reduction of the agent-callable surface, not a free move.
- `mcp.hide_experimental` can no longer hide semantic search, hybrid search, or the call and implementation lookups. All sixteen advertised tools are `stable`; those capabilities are now modes and sections inside them (`search_code`'s `semantic`/`hybrid`, `get_references`'s `callers`/`callees`/`implementations`) rather than separately-named experimental tools, so a tool-level flag cannot reach them. Their uncertainty is reported per response instead — `semantic_status` on a search, and each `get_references` section's `evidence_source` and `caveats` — which is where it belongs, but a repository relying on the flag to keep heuristic evidence out of an agent's reach will see behaviour change. `map_stacktrace_to_code`, `find_errors_for_symbol` and `find_recent_failures` remain `experimental`, so the flag still applies to them.
- A plan's `tool_calls` no longer recommends `search_memory` unless the repository has enabled memory. It was emitted on every plan, and with the memory tools now advertised only when `[memory] enabled` is set, an agent reading a plan that named a tool absent from its own inventory had no way to tell a gated name from a stale one. `PlanEngine::with_memory_enabled` carries the answer from config; memory facts themselves are unaffected and still appear in the plan's memory section either way.
- `repo_status` reports languages by their canonical key (`rust`, `type_script`) rather than the Rust `Debug` spelling (`Rust`, `TypeScript`) the retired `list_languages` used, matching `coverage.by_language` and every other serialized language field.
- `build_context_pack` and `plan_change` now declare `readOnlyHint: false` and `idempotentHint: false`, because `compress: true` and `persist: true` write under `.ok`. With those flags absent — the default call — both are read-only, as `verify_change` already documented for its own flags.
- `ok architecture violations` reports the violations of the evaluated architecture policy. It previously ran heuristic detection with no policy resolver, so a repository with a configured policy got detection output rather than its own rule violations — the MCP tool it replaces did not have that defect.
- A plan's `boundary.signal_hooks.architecture_components` names the CLI reads (`ok architecture summary`, `ok architecture violations`, `ok architecture policy check`) rather than the retired MCP tool names, so a plan no longer points an agent at names the server does not answer.
- The index storage format changed: SQLite `user_version` 3 -> 4 and `IndexManifest.schema_version` 1 -> 2. `graph_edges` and `call_sites` no longer carry a JSON document per row beside the query columns holding the same values; every field now has a typed column and every repeated string is written once into a per-table dictionary (`graph_strings`, `call_site_strings`) and referenced by integer id. Opening a pre-4.0 index detects the old layout by the `json` column (a `PRAGMA table_info` check, not a scan), drops those two tables, and makes every relationship read report `run \`ok index\` to rebuild them` rather than answering from an empty table — an empty answer would read as "no such relationship exists". The detection cannot fire twice, because the column it keys on is gone afterwards. `ok snapshot import` now also refuses an artifact exported from a pre-4.0 index and names the fix instead of importing a store whose graph would be discarded on first open. (#363)
- `open_kioku_parse::evidence_timestamp()` is removed. It had no callers anywhere in the workspace and was vestigial (#335); the next release is a major, so the removal rides it.

### Added
- `ok architecture summary` returns what the retired `summarize_architecture` MCP tool returned: detected components, the configured policy, the evaluated `policy_check`, and its violations, with an explicit `configured: false` and a caveat when no policy exists rather than inferred violations.
- `[memory]` and `[runtime]` configuration sections. Both default to off and both gate MCP advertising only: `[memory] enabled` controls whether the memory tools are listed, `[runtime] enabled` plus a non-empty `provider` controls whether the runtime tools are. Repository memory stays readable and every gated tool stays dispatchable either way.
- `ok search <pattern> --regex` runs exact regular-expression line matching over the indexed corpus. It makes the same call the MCP `regex_search` tool now makes and reports the same `results`, `truncated`, `warnings` and `caveats` fields, so the index-only corpus caveat survives `--json` on both surfaces.
- `SymbolEngine::context` and `ok symbol context <name>`: a symbol joined back to the indexed chunk text covering it — the definition body with the line range it spans, and up to ten indexed lines above and below it verbatim. `MetadataStore::file_by_id` resolves a `FileId` to its file, with an indexed SQLite lookup replacing the file-list scan callers were writing by hand.
- Golden MCP snapshots for a successful `tools/call` envelope: `tools_call_json_tool.json` (a JSON tool — `structuredContent` is the object, `content[0].text` the pretty-printed JSON) and `tools_call_rendered_tool.json` (a Markdown rendering — `structuredContent` is `{rendered_in, bytes, truncated}`). Only the error envelope was pinned before, so the single-payload change altered the wire shape of every rendered response without a snapshot moving (#392). A successful tool result now also states `"isError": false` rather than leaving the optional field absent.
- `open_kioku_core::process::process_peak_rss()` measures this process's peak RSS and names the instrument that produced it: `VmHWM` from `/proc/self/status` on Linux, `getrusage(RUSAGE_SELF).ru_maxrss` elsewhere on Unix (bytes on Apple platforms, kilobytes on Linux and the BSDs — the conversion lives in one place with a per-platform test). It replaces the four private `/proc`-only copies in the vector and embeddings profiling examples, which returned `None` on macOS, where those artifacts are produced. Each report now carries `process_peak_rss_instrument` beside `process_peak_rss_bytes`, so a platform with no instrument says `unsupported: …` instead of leaving a silent `null` that reads like an absent field rather than a broken instrument (#338).
- Gold yield at a token budget on the commit-derived benchmarks (`docs/retrieval-benchmark.md`): `scripts/score-context-cases.py` walks the pack's selected units in order under 4,000 / 8,000 / 16,000-token budgets and reports `gold_file_yield@B`, `gold_line_yield@B` and the median `tokens_to_first_gold` next to R@k and MRR, each with a bootstrap interval. `scripts/commit-derived-cases.py` records the modified line ranges per gold file as a fifth TSV column (base side of `git diff -U0`, numbered on the commit's parent as a proxy for the indexed base) and can re-derive it for an existing file with `--annotate`; the scorer tolerates its absence. `scripts/compare-commit-derived-report.py` prints the yields informationally; nothing gates on them yet.
- The index reports what it did not index. Discovery records, per recognised language, the source files it saw on disk versus the files the index holds, with each discovered file's omission attributed to a skip reason and directories pruned by name or unreadable counted beside the ratio (`quality.coverage` in the manifest; `coverage` in `ok --json status` and the MCP `repo_status` result; `null` for indexes written before this). `ok index` ends with one line (`coverage: 9,982 of 10,012 programming-language files indexed (99.7%); 12,004 of 12,140 recognised files indexed (98.9%) overall; skipped: 25 secret-policy, 5 too-large`), `ok doctor` prints the per-language table for every language and warns with the top three skip reasons when the programming-language ratio (rust, java, typescript, javascript, python, go, sql — the files whose omission costs evidence) or any single programming language falls under 98%, or a programming language is missing 20 or more files; config and prose files are reported in the all-languages ratio beside it but never trigger a warning, because hidden `.github/*.yml` files would otherwise flag almost every repository. The commit-derived benchmark records the line beside every accuracy number. An ingest rule had silently dropped 25 Java files from a 10k-file Java service (#379) and nothing surfaced it.
- `gte-modernbert-base` (Apache-2.0, 149M parameters, int8 ONNX) as a local neural embedding profile and the default when `[semantic] provider = "fastembed"` names no model. It is pinned to a fixed upstream revision with a SHA-256 check on every file, so upstream cannot change or remove it silently. Chosen on commit-derived corpora against jina-v2-code and Qwen3-0.6B (`docs/embedding-providers.md`). ONNX and Qwen3 embedding now run one length-sorted batch at a time with bounded sequence lengths; the previous parallel, 8k-token batches were killed for memory on a 16 GB machine.

### Fixed
- The four benchmark corpora's base commits are no longer checked in. A full commit hash of a public repository is searchable and therefore identifies it, so a hash names the corpus as surely as a URL does. The hashes move to `BENCH_<CODE>_A_BASE` repository variables beside the existing `BENCH_<CODE>_A_URL`, both benchmark workflows resolve them from there and fail closed when unset, and the `provenance.base` field in every frozen baseline says where the value lives instead of holding it. The workflow comment claiming base commits "name nothing" was wrong and is corrected.
- `scripts/validate-docs.sh` guards the MCP tool count. It derives the count from the tool table in `crates/open-kioku-mcp/src/lib.rs`, rejects duplicate tool names, checks the in-crate `tools_ro` inventory assertion against that same table, and holds `README.md` and `docs/mcp-tools.md` to the result. The count had been unguarded prose in both documents (#405).
- `regex_search` disclosed the walk's file budget but not the shared `MAX_MCP_FETCH` candidate cap. `search_fetch_limit` asks for `offset + limit + 1` and clamps at 500 while `offset` accepts 10,000, so a deep page lost the sentinel `has_more` is derived from: `{pattern: "fn ", limit: 20, offset: 500}` on a repository with tens of thousands of matches returned `has_more: false, truncated: false` and no warning, and an agent paging a broad pattern would conclude it had seen every occurrence. The cap disclosure now lives in one place that both the ranked and regex paths run their metadata through, so the two cannot drift again.
- `get_symbol_context` reported a body it could not recover at all, but not one it recovered only in part. Lines that fall outside every indexed chunk are skipped and the returned range was rebuilt from what came back, so a short bundle read as a whole definition; it now names how many of the sought lines were missing. The chunk-boundary caveat also named the last line recovered instead of the boundary itself, so it could contradict the evidence string beside it.
- Six MCP tool descriptions said what their names suggested rather than what their implementations do, and now say the latter (#405). `structural_search` and `explain_search_result` are named as aliases of `search_code` and `hybrid_search` — no AST matching and no separate explanation step exist in the workspace. `recommend_validation_plan` is named as an alias of `find_tests_for_change` and no longer claims static checks or coverage actions. `architecture_violations` is named as an alias of `architecture_boundaries` returning the same summary. `search_symbols` no longer claims fuzzy ranked matching: it is `list_symbols` with a case-insensitive substring filter ordered by qualified name. `search_files` shares the `search_code` dispatch arm and no longer advertises the size and language metadata a `SearchResult` has never carried. Input-schema parameter descriptions were corrected alongside the tool descriptions, so the two halves of a tool no longer disagree. `get_definition` and `explain_symbol` no longer claim a body or relationship edges they never returned. No tool was added, removed, or merged; the inventory is unchanged at 58.
- The MCP `get_symbol_context` tool returns a symbol's definition body. It promised "full definition body … documentation comments, and surrounding code context" and was an alias of `get_definition`, returning one `Symbol` record with no text at all (#405). It now returns the body recovered from indexed chunk text with the line range it spans, plus up to ten indexed lines on each side. Documentation comments are reported only as the verbatim leading lines the indexer actually chunked, never parsed out or reconstructed, and their absence — the first symbol in a file has no indexed preamble — is stated as a caveat. A body that cannot be recovered returns the symbol with an explicit caveat rather than a shorter bundle that reads like a complete one.
- The MCP `regex_search` tool performs regular-expression matching. It advertised "exact regular-expression pattern matching against indexed source code lines" and dispatched to the ranked BM25 path, so an agent that asked for a pattern got lexical guesses with nothing in the response saying so; a correct line-by-line matcher had shipped in `open-kioku-search-regex` with no caller anywhere in the workspace (#405). The pattern is now compiled once and evaluated over indexed chunk text, file by file in path order, returning exact single-line hits at confidence 1.0. The walk is paged so only one file's chunks are resident, is capped at 20,000 files, and reports both the number of files scanned and a `truncated` warning when it stopped early. An unparseable pattern is a tool error rather than an empty result.
- Gold yield is averaged over every scored case instead of only the cases whose pack selected units. A pack that selects nothing delivered no gold lines, so it now scores 0 rather than leaving the denominator; the previous mean excluded those cases, which inflated it and made yield incomparable with `gold_recall@20`, which always averaged over every case. Line yield is averaged over the cases that carry modified line ranges, with abstaining cases among them scoring 0. Published yield figures move down slightly as a result.
- One file can no longer abort an index. A file that vanished, lost read permission between discovery and parsing, or crashed a grammar is now dropped on its own: it is recorded as a `SkipReason::Error` entry in `skip_counts` / `skipped_paths` with source `filesystem` or `parser`, moved from `indexed` to `skipped` in `quality.coverage` so the ratio never claims a file the index does not hold, surfaced as a phase warning, and the rest of the repository still indexes. Previously the parse phase propagated the first `fs::read` error and the user was left with an empty `.ok/` and the identical failure on retry (#350). The panic payload is not recorded — it can quote the source text around the failing byte, and parser messages stay redacted.
- The indexer no longer drops source files whose path contains "secret" or "credential" (25 Java files vanished from one repository), and no longer skips generated files: they are indexed, flagged `is_generated`, ordered behind other candidates before each stream's cap, and ranked at the lowest quality tier (`generated_file_demotion`) unless the task names the file's own path. Data, config and prose files keep the secret-name rule (#379). The Python ML library holdout against the same index, measured on the production `ok context` path with `scripts/score-context-cases.py`: R@5 0.645 → 0.665, R@20 0.741 → 0.767.
- Task vocabulary now reaches the repository's own identifiers through an identifier lattice built per query from indexed symbol names and file stems: `CollectionsUtils Tests` reaches `CollectionUtilsTests`, and a misspelt part of six or more letters that the repository spells nowhere is corrected by one edit. Reached identifiers are extra lexical terms and, when one names a file, place it one relevance tier below a name the task spelled exactly — never at the named-target tier and never exempt from the docs/tests demotion, so a guess cannot outrank an exactly-resolved definition. They never become exact-symbol anchors. An identifier that reaches nothing, and a hop withheld because too many files carry the name it reached, are both reported as caveats. Only whole identifiers expand, only when substring retrieval cannot already reach them, and only for code-shaped tokens; re-inflecting prose words was measured and dropped because it cost 0.021 MRR on a Python library (~4k files) for no gain. No model, no network, no re-index (+51 ms median end-to-end CPU per reaching query on a 10k-file index). Commit-subject benchmarks cannot exercise this path — their queries are written by the author who just edited the file, so identifiers are already spelled the repository's way, and on them the lattice is neutral (Go (~800 files), TypeScript (~900 files), and Python (~4k files) bit-identical; Java (10k files) moves one case). Measured instead on perturbed queries that rewrite one identifier per case into a plausible task-description form: over 259 such cases from all four corpora, R@5 0.656 → 0.699 and MRR 0.548 → 0.584 (+0.036, 95% CI [+0.015, +0.062]); the 367 unperturbed control cases move by exactly 0.000. Those figures are an upper bound: the perturbations are the inverse of the lattice's own hypothesis class.
- Context packs on test-heavy repositories no longer fill their primary files with tests: the validation candidate stream declared corroborating authority for any test whose name shared a word with the task, and fusion orders by authority before score. Test paths are now recognised by directory segment and file name (`src/internalClusterTest`, `*IT.java`, `foo.spec.ts`), never by substring, and a task that asks for tests keeps them in the source tier. On an 11k-file Java checkout, "geoip processor" went from 0 of 5 source files in the top five to 5 of 5.
- The first capitalised word of a commit-style task ("Fix", "Enable", "Assert") was treated as the primary edit anchor and boosted every file containing that substring above the real lexical hits; identifiers now need an inner case change, a separator, or digits beside capitals. The lexical candidate stream also merged per-term results by minimum rank, letting the top hit for a single expansion word tie with the top hit for the whole task. Production-path R@20 on the same 60 commit-derived cases: 0.42 → 0.73, MRR 0.22 → 0.44.
- Git history no longer votes for primary context from commit-message similarity to task prose: on a 10k-file repository with history those votes cost 0.075 MRR (monotonically across weights) and ~5 s per query. The history stream now anchors on exact symbols or paths only, and per-file history annotation queries churn and co-change for the file rather than the task text. Similar-change statics (co-change edges, hotspots) are cached per store instead of re-read per result.
- Function words and commit verbs ("for", "fix", "add") no longer count as task vocabulary for test/runtime name overlap, and the fusion profile `rrf_measured_v1` votes the validation stream at half weight (neutral on the 490-case corpus; restores a strong lexical hit that two weak test-name votes had outranked).
- Reciprocal-rank fusion uses k=10 instead of 60: with one full-text ranker and several name-overlap hint streams, k=60 made lexical rank 2 indistinguishable from rank 13, so any file with two weak votes outranked a single strong one. Neutral on the 490-case corpus (dev MRR 0.386 → 0.384, holdout 0.345 → 0.348); restores the workflow benchmark's `test-selector` case.
- Measured on the Go application and the TypeScript standard library commit-derived corpora, six further ranking defects: routing keywords matched substrings ("docstool" routed a code task to the documentation family); a task that merely says "panic" was routed to trace-to-code and *blocked* outright on repositories that have never ingested a runtime trace (a required source that cannot run is now a caveat, not a blocker); benchmarks were not test intent although in Go they live in `_test.go`; an anchor *mentioned* in a snippet or a docs page shared the top relevance tier with the file that names it and outranked the best full-task hit (tiers are now definition > explicit path/ticket > mention, and the docs/tests quality tier is applied first); documentation tasks ran no lexical stream although doc comments live in source files; and the corpus extractor now drops commits whose subject names a path.
- A directory merely named after tests (`crates/open-kioku-tests/`, `packages/e2e-tests-runner/`) is not a test path; only exact test directory names, CamelCase test source sets, and test file names are.
- Git history votes for the files that near-identical past commit subjects touched (numbers and PR references stripped, Jaccard ≥ 0.75, half the twins must agree on a file). On a Go repository whose holdout contains 50 release-bump commits with one gold file, holdout R@5 0.428 → 0.772 and MRR 0.338 → 0.511 with history present; a repository without repeated subjects is unchanged.
- A commit scope's directory entry file (`mod.ts`, `index.ts`, `lib.rs`, `__init__.py`) is a candidate even without shared vocabulary, placed just below the scope's best hit when nothing in the directory knows the task's words and last otherwise. The TypeScript standard library holdout R@5 0.744 → 0.816, MRR 0.607 → 0.650; other corpora unchanged.
- Commit-style scope prefixes (`docs(fs): …`, `pkg/render: …`, `[Scheduler] …`) now anchor retrieval on the package or directory they name, and document sections cast one vote per file instead of one per section (a documentation task on the TypeScript standard library had returned twenty sections of `Releases.md` and nothing else). Holdout MRR on the commit-derived corpora: the TypeScript standard library 0.510 → 0.607, the Go application 0.308 → 0.338, the Python ML library 0.539 → 0.565.
- A pack whose selected context contains fewer than a third of the task's terms is capped strictly below Medium confidence; previously the cap sat exactly on the Medium threshold and a nonsense query with one incidental word match reported Medium.

### Performance
- The index is 39% smaller and indexing peaks 15% lower in resident memory. Measured on `modules/` from a public 10k-file Java service at a fixed commit (1,751 Java files -> 2,528 indexed files, 161,562 edges, **identical totals on all 16 runs in both arms**), release builds differing only by this change, fresh `.ok` per run, arms alternated in ABBA pairs. `.ok/index.sqlite` 1,202,847,744 -> 732,633,088 bytes (**-39.1%**, largest within-arm spread 4,096 bytes over 8 runs per arm); the whole `.ok` directory -36.4%. `graph_edges` with its indexes and dictionary went 400 -> 137 MiB, `call_sites` 243 -> 57 MiB; `analysis_facts` is untouched at 170 MiB and is now the largest table. Peak RSS 1,958,623,232 -> 1,659,136,000 bytes (**-285.6 MiB, -15.3%**, medians of 8 runs per arm, with every after-run below every before-run), reproduced independently at -15.4% by the three `--features mem-profile` pairs. The allocator counter puts only 18.03 MiB (-1.70%, within-arm spread 1,376 bytes) of that in bytes *requested* from the Rust allocator; the rest is not requested bytes and is consistent with the bulk-load page cache touching a 39%-smaller database, which `docs/memory-profiling.md` notes the counter cannot see. Allocation count is up 2.26% — interning hashes every string — so the win is in bytes retained, not churn. **Wall-clock indexing time is unresolvable on this host**: the within-arm spread was 108 s on runs of 55-163 s as the host load ranged from 9 to 56, and the paired median was +2 s with a range of -84 to +27 s. No indexing-time claim is made either way. (#363, #329)
- `ok context`, `ok plan`, and MCP `build_context_pack` on a 10k-file Java index: 78 s → ~5 s per query. Impact expansion now uses the Tantivy index instead of regex-scanning every chunk once per term; per-file fact lookups use the existing `file_id` index instead of scanning and sorting every fact of a source type; the relationship-semantics verdict is cached per store (keyed by SQLite `data_version`) instead of re-parsing a multi-megabyte manifest on every relationship query.

### Changed
- Commit-derived baselines re-frozen from the 2026-09-08 hosted matrix on main (after generated-file indexing and the scope entry-file candidate). Holdout R@5 / R@20 / MRR now read Java 0.566 / 0.699 / 0.504 (was 0.549 / 0.681 / 0.482), Go 0.679 / 0.809 / 0.535 (was 0.690 / 0.810 / 0.551), TypeScript 0.825 / 0.874 / 0.658 (was 0.753 / 0.801 / 0.621), Python 0.663 / 0.759 / 0.545 (was 0.658 / 0.749 / 0.556); the Go and Python MRR dips are inside the nightly slack and are recorded rather than hidden.
- The Tantivy index tokenizes code text and symbols identifier-aware: `FieldMapper` is indexed as `fieldmapper`, `field`, and `mapper`. Lexical MRR on a 490-case commit-derived benchmark on the Java service: 0.235 → 0.393 (dev), 0.202 → 0.337 (holdout). Existing indexes keep working; run `ok index` to rebuild with the new tokenizer.
- MCP tool responses no longer send rendered Markdown/TOON text twice: `content` carries the text once and `structuredContent` carries a small pointer (`rendered_in`, `bytes`, `truncated`) instead of a copy. A `build_context_pack` response that measured 122 KB on the wire is now about 61 KB; JSON results are unchanged.
- Retrieval benchmark no-gold false positives count only results presented with confidence above Low (the product's abstention signal), for pack-less strategies via the same shared weak-relevance rule. Baseline and thresholds re-frozen; rationale in `docs/retrieval-benchmark.md`.

### Added
- `scripts/commit-derived-cases.py` keeps one instance of a subject that repeats up to numbers (`--keep-repeated-subjects` restores the old behaviour); the Go application's corpus shrinks from 481 to 280 cases because 201 were release-bump commits with one gold file.
- `scripts/commit-derived-cases.py` and `scripts/score-context-cases.py`: derive leakage-safe retrieval cases from a repository's own history (index at a base commit, gold = files a later commit modified that already existed at base) and score the production `ok context` path against them with bootstrap confidence intervals.

---

## [3.1.0] — 2026-08-31

### Fixed
- Stopped the graph query-column migration backfill from full-scanning (and partially rewriting) the graph tables on every store open. On a 16.5k-file Java corpus this removed ~14 seconds of fixed latency from every CLI command; existing stores migrate once and record completion.
- Fixed exact symbol lookups returning `symbol not found` for symbols that exist: the substring scan ordered by qualified name could truncate the true exact match out of its candidate window. `ok symbol definition` now consults an indexed exact-name path first (13.9s → 0.02s on a 247k-symbol corpus, with the correct result).

### Performance
- Bulk graph replacement drops and rebuilds secondary indexes around a prepared-statement, primary-key-ordered insert under a scoped page cache. Cold structural indexing of a 16.5k-file Java repository improved from 40m40s to 19m28s (graph write 28m37s → 8m17s).
- Ingest releases the parsed corpus in one consuming pass instead of cloning every extracted field, graph nodes are moved rather than cloned twice, and search git-history annotation groups facts by file instead of rescanning the full fact list per result.

### Added
- RI3.6 (phase 1): index components now live in atomically published generations under `.ok/generations/<id>/` with an `active` pointer that is only ever replaced atomically. Legacy layouts keep working and are adopted in place (a directory move, not a copy) on the next `ok index` under the write lock; every read path resolves through the active generation. `ok status`/`ok doctor` report the generation identity and classify on-disk generations; MCP `repo_status` exposes `generation_id`. Design: `docs/ri3-index-generations-design.md`.
- CC6: calibrated abstention can now be activated at runtime. `ok retrieval-bench --write-abstention-activation` emits a fail-closed activation artifact only when the calibrated policy passes the holdout readiness gate; with the artifact present, context packs that fail the calibrated evidence gates carry an explicit `calibrated_cc6_abstention` reason and caveat instead of presenting weak context confidently. The benchmark and the runtime share one decision code path so measured and deployed behavior cannot drift.
- Semantic search routing now carries an explicit caveat when the persistent ANN backend serves a candidate population above the measured recall-degradation ceiling (~300K vectors, per `benchmarks/cc5-ann-scale-evidence`).
- RI3.7: test selection carries an explicit `selection_tier` (required/recommended/optional) with evidence justification; heuristic name or path similarity alone can never mark a test required. Plans distinguish structurally proven dependents from possible (heuristic) ones in summaries and per-file caution rules.
- CC5.3/CC7: churn policy gates and per-dimension retrieval floors are version-controlled contracts (`benchmarks/cc5-ann-churn-thresholds.json`, `benchmarks/retrieval-dimension-thresholds.json`, advisory-first) wired into CI.
- CC5.3: semantic lifecycle health is now explained by `ok status`, `ok doctor` (semantic-lifecycle check with concrete rebuild reasons), structured `rebuild_required`/`rebuild_reasons`/`last_rebuilt_at`/`stale_ratio` fields on semantic status, and a `semantic_lifecycle` block in MCP `repo_status`.
- RI3.7: impact analysis classifies relationship-edge dependents into `proven_impact` and `possible_impact` through the shared fail-closed `RelationshipUsePolicy` — a heuristic same-name edge can never be presented as structural truth. Wired through the CLI, MCP `impact_analysis`, context compilation, planning, and patch verification.
- `ok --version --json` machine-readable version output and copy-paste examples in `ok status/search/impact --help`.
- CC5.2: measured 50K→1M ANN scale evidence recorded under `benchmarks/cc5-ann-scale-evidence` (recall collapses beyond ~300K vectors on the current HNSW profile; profile decision tracked in #328).

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
- V3 Linux release binaries target GNU/glibc on x86_64 and ARM64 because the local neural runtime does not provide supported MUSL prebuilts; npm Linux platform packages declare `libc: glibc` accordingly.
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
- Comprehensively refactored and enriched MCP tool descriptions and guidance text for all 27 low-scoring tools to achieve A-level ratings on the Glama TDQS rubric.
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
- Expanded MCP tool descriptions with explicit when-to-use guidance, sibling alternatives, and side-effect transparency for better Glama TDQS scoring.
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
  - Wrapped SQLite node and edge backfill updates (`backfill_graph_query_columns`) inside transactions. This critical performance fix reduces backfill time on large codebases (such as a 10k-file Java service with 269k stale edges) from several hours to under 25 seconds.

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
