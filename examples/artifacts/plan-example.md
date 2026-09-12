# Plan: token

Found 5 primary context item(s), 5 direct impact candidate(s), 5 validation candidate(s), 0 history signal(s), 0 repo memory fact(s); risk is medium.

## Risk

- Level: `medium`
- Score: `0.36`
- 3 complexity/hot-path risk signal(s) touch this file
- Evidence quality caveat: exact symbol/reference evidence is unavailable
- Evidence quality caveat: runtime evidence is unavailable
- Evidence quality caveat: history evidence is unavailable
- Evidence quality caveat: coverage or JUnit evidence is unavailable

### Score Signals

- `plan_risk_score` contribution `0.360`: plan risk is `medium` from merged context and impact risk

## Confidence

- Overall: `Medium` (`0.74`)
- Caveats:
  - exact symbol/reference evidence is absent
  - runtime corroboration is absent
  - exact symbol/reference evidence is unavailable
  - runtime evidence is unavailable
  - history evidence is unavailable
  - coverage or JUnit evidence is unavailable
- Components:
  - `boundary_tightness` score `0.85`, weight `0.15`, contribution `0.13`: how narrowly allowed edit files bound the proposed change
  - `evidence_density` score `0.90`, weight `0.10`, contribution `0.09`: distinct evidence records over twice the selected primary files, capped at 1.0
  - `exact_references` score `0.25`, weight `0.20`, contribution `0.05`: selections backed by exact-authority retrieval, indexed symbol references, or SCIP evidence
  - `negative_evidence` score `1.00`, weight `0.15`, contribution `0.15`: absence of low-confidence, missing-anchor, or no-match evidence
  - `runtime_corroboration` score `0.25`, weight `0.05`, contribution `0.01`: runtime traces, incidents, or error signals that support the context
  - `task_relevance` score `1.00`, weight `0.20`, contribution `0.20`: share of the task's terms that appear in the selected context
  - `test_coverage` score `1.00`, weight `0.10`, contribution `0.10`: at least one selected validation target carries a runnable command
  - `validation_availability` score `1.00`, weight `0.15`, contribution `0.15`: at least one validation target was selected near the primary context

## Evidence Quality

- Index mode: `full`
- Freshness: `fresh`
- Exact references: `false`
- Runtime evidence: `false`
- History evidence: `false`
- Coverage or JUnit evidence: `false`
- Counts: skipped paths `0`, unresolved imports `0`, ambiguous edges `0`
- Caveats:
  - exact symbol/reference evidence is unavailable
  - runtime evidence is unavailable
  - history evidence is unavailable
  - coverage or JUnit evidence is unavailable

## Negative Evidence

- `exact_references`: no explicit exact symbol reference or SCIP evidence was found (`0.85`)
  - query: `token`; inspected: `retrieval_trace.authority, impact.match_reason, evidence.source_type`
  - next probe: `Run `ok scip setup .` and re-index with `ok index . --with-scip auto`.`
- `history`: no churn, ownership, similar-change, reviewer, or historical validation evidence was available (`0.70`)
  - query: `token`; inspected: `plan.evidence, search_result.evidence`
  - next probe: `Run `ok history similar --path src/auth.rs` or `git log --name-only -- src/auth.rs` to inspect history manually.`
- `runtime`: no runtime trace, incident, or error artifact corroborated the selected context (`0.75`)
  - query: `token`; inspected: `runtime_signals, search_result.score_breakdown`
  - next probe: `Import or configure runtime artifacts, then rerun `ok plan`.`

## Evidence Provenance

### Section References

- `boundary`: `document:README.md:1-3, region:adjacent-unit:1-2, region:adjacent-unit:1-80, region:adjacent-unit:12-16, region:adjacent-unit:17-23, region:adjacent-unit:3-6, search:ok.toml:81-147:0, search:ok.toml:81-147:1, search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1, search:src/lib.rs:1-2:0, search:src/lib.rs:1-2:1, search:src/lib.rs:3-6:0, search:src/lib.rs:3-6:1, search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1, search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1, test:314d20ce2e07fafa14267b0cf2fc46952eefae3c809afb228c50c863f45a6e76, test:9f8fd72a3efef98669b10543eb7c7fa7a75584a4c3d16a705c7616ea05306fd1, test:aeb43ec5523f20785b5dde21a67defb7d9c9416205bcf3a941778e0b2c88cb58, test:cdd809b815a1af2f7081c8aad676fc1c8ee04497063e945415dec9b4d1dd0a10`
- `history`: `none`
- `impact`: `230cfac07e26bff70c633f088134b95b1ae4eed87fb3d89dd62136178d180f71, 797c391f41bcb9d28cdff235a68958cc47fa0630d1f2a071dec90a7a9588bf3c, abf2fb2ec505b44f17b260d57342d783859ef3db9db48c9db7b699665c7d14a2, impact:src/auth.rs, search:ok.toml:81-147:0, search:ok.toml:81-147:1, search:src/lib.rs:1-2:0, search:src/lib.rs:1-2:1, search:src/lib.rs:3-6:0, search:src/lib.rs:3-6:1, search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1, search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1`
- `negative_evidence`: `negative:exact_references:token-no-explicit-exact-symbol-reference-or-scip, negative:history:token-no-churn-ownership-similar-change-reviewer-or, negative:runtime:token-no-runtime-trace-incident-or-error-artifact`
- `primary_context`: `document:README.md:1-3, region:adjacent-unit:1-2, region:adjacent-unit:1-80, region:adjacent-unit:12-16, region:adjacent-unit:17-23, region:adjacent-unit:3-6, search:ok.toml:81-147:0, search:ok.toml:81-147:1, search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1, search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1, search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1, test:314d20ce2e07fafa14267b0cf2fc46952eefae3c809afb228c50c863f45a6e76, test:9f8fd72a3efef98669b10543eb7c7fa7a75584a4c3d16a705c7616ea05306fd1, test:aeb43ec5523f20785b5dde21a67defb7d9c9416205bcf3a941778e0b2c88cb58, test:cdd809b815a1af2f7081c8aad676fc1c8ee04497063e945415dec9b4d1dd0a10`
- `validation`: `314d20ce2e07fafa14267b0cf2fc46952eefae3c809afb228c50c863f45a6e76, 9f8fd72a3efef98669b10543eb7c7fa7a75584a4c3d16a705c7616ea05306fd1, ae205155363199d68c9b593ef5680df439ba9213c0536a84d77f326fd6fdbcfd, aeb43ec5523f20785b5dde21a67defb7d9c9416205bcf3a941778e0b2c88cb58, cdd809b815a1af2f7081c8aad676fc1c8ee04497063e945415dec9b4d1dd0a10`

### Evidence Items

- `context:src/auth.rs` `open-kioku-search` (Lexical): BM25 lexical match from local Tantivy index
- `context:src/auth.rs` `open-kioku-search` (Lexical): query variant `token` matched local index
- `context:src/auth.rs` `open-kioku-search` (Lexical): region extended to adjacent chunk (lines 12-16)
- `context:src/auth.rs` `open-kioku-search` (Lexical): region extended to adjacent chunk (lines 3-6)
- `context:src/auth.rs` `open-kioku-search` (Lexical): region extended to adjacent chunk (lines 17-23)
- `context:src/lib.rs` `open-kioku-search` (Lexical): BM25 lexical match from local Tantivy index
- `context:src/lib.rs` `open-kioku-search` (Lexical): query variant `token` matched local index
- `context:src/lib.rs` `open-kioku-search` (Lexical): region extended to adjacent chunk (lines 3-6)
- `context:src/lib.rs` `open-kioku-search` (Lexical): region extended to adjacent chunk (lines 1-2)
- `context:ok.toml` `open-kioku-search` (Lexical): BM25 lexical match from local Tantivy index
- `context:ok.toml` `open-kioku-search` (Lexical): query variant `token` matched local index
- `context:ok.toml` `open-kioku-search` (Lexical): region extended to adjacent chunk (lines 1-80)
- `context:tests/auth_flow.rs` `open-kioku-search` (Lexical): BM25 lexical match from local Tantivy index
- `context:tests/auth_flow.rs` `open-kioku-search` (Lexical): query variant `token` matched local index
- `context:README.md` `open-kioku-search` (Lexical): document section `Open Kioku Demo: expired sessions` matched task vocabulary
- `context:README.md` `open-kioku-search` (Lexical): document heading path: Open Kioku Demo: expired sessions
- `context:README.md` `open-kioku-search` (Lexical): document content hash: 7768b68a12afc15f06ae744a8777f5f5de0a0f7c4de39c1a9e9e3a0ddd8cdedf
- `impact:src/auth.rs` `open-kioku-impact` (Lexical): impact report derived from indexed symbols and lexical references
- `797c391f41bcb9d28cdff235a68958cc47fa0630d1f2a071dec90a7a9588bf3c` `open-kioku-relationships:complexity` (StaticAnalysis): complexity_risk=low; cyclomatic=1; cognitive=1; loop_count=0; max_loop_depth=0; transitive_loop_depth=0; recursive=false; linear_scan_in_loop=false; allocation_in_loop=false; recursion_in_loop=false; unguarded_recursion=false; parameter_count=2; max_access_depth=1; blocking_network_db_call_count=0; caveat=risk signal, not proof of complexity
- `abf2fb2ec505b44f17b260d57342d783859ef3db9db48c9db7b699665c7d14a2` `open-kioku-relationships:complexity` (StaticAnalysis): complexity_risk=low; cyclomatic=1; cognitive=1; loop_count=0; max_loop_depth=0; transitive_loop_depth=0; recursive=false; linear_scan_in_loop=false; allocation_in_loop=false; recursion_in_loop=false; unguarded_recursion=false; parameter_count=0; max_access_depth=1; blocking_network_db_call_count=0; caveat=risk signal, not proof of complexity
- `230cfac07e26bff70c633f088134b95b1ae4eed87fb3d89dd62136178d180f71` `open-kioku-relationships:complexity` (StaticAnalysis): complexity_risk=low; cyclomatic=1; cognitive=1; loop_count=0; max_loop_depth=0; transitive_loop_depth=0; recursive=false; linear_scan_in_loop=false; allocation_in_loop=false; recursion_in_loop=false; unguarded_recursion=false; parameter_count=1; max_access_depth=1; blocking_network_db_call_count=0; caveat=risk signal, not proof of complexity
- `impact:src/auth.rs` `open-kioku-impact` (Lexical): impact report derived from indexed symbols and lexical references
- `797c391f41bcb9d28cdff235a68958cc47fa0630d1f2a071dec90a7a9588bf3c` `open-kioku-relationships:complexity` (StaticAnalysis): complexity_risk=low; cyclomatic=1; cognitive=1; loop_count=0; max_loop_depth=0; transitive_loop_depth=0; recursive=false; linear_scan_in_loop=false; allocation_in_loop=false; recursion_in_loop=false; unguarded_recursion=false; parameter_count=2; max_access_depth=1; blocking_network_db_call_count=0; caveat=risk signal, not proof of complexity
- `abf2fb2ec505b44f17b260d57342d783859ef3db9db48c9db7b699665c7d14a2` `open-kioku-relationships:complexity` (StaticAnalysis): complexity_risk=low; cyclomatic=1; cognitive=1; loop_count=0; max_loop_depth=0; transitive_loop_depth=0; recursive=false; linear_scan_in_loop=false; allocation_in_loop=false; recursion_in_loop=false; unguarded_recursion=false; parameter_count=0; max_access_depth=1; blocking_network_db_call_count=0; caveat=risk signal, not proof of complexity
- `230cfac07e26bff70c633f088134b95b1ae4eed87fb3d89dd62136178d180f71` `open-kioku-relationships:complexity` (StaticAnalysis): complexity_risk=low; cyclomatic=1; cognitive=1; loop_count=0; max_loop_depth=0; transitive_loop_depth=0; recursive=false; linear_scan_in_loop=false; allocation_in_loop=false; recursion_in_loop=false; unguarded_recursion=false; parameter_count=1; max_access_depth=1; blocking_network_db_call_count=0; caveat=risk signal, not proof of complexity

## Primary Context

- `src/auth.rs`:3-23: pub fn issue_token(context: &RequestContext, ttl_seconds: u64) -> String { format!("token:{}:{}", context.user_id, ttl_seconds) } pub fn validate_token(token: &str) -> bool { token.starts_with("token:") } #[cfg(test)] mod tests { use super::*; use crate::RequestContext; #[test] fn issues_token_with_user_id() { let context = RequestContext { user_id: "demo-user".into(), }; assert!(issue_token(&context, 60).contains("demo-user")); } }
  - score: `0.136`; signals: `bm25_relevance` +3.173, `query_variant_boost` +1.000, `retrieval_rrf:lexical` +0.091
  - evidence: `search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1, test:9f8fd72a3efef98669b10543eb7c7fa7a75584a4c3d16a705c7616ea05306fd1, test:aeb43ec5523f20785b5dde21a67defb7d9c9416205bcf3a941778e0b2c88cb58, test:cdd809b815a1af2f7081c8aad676fc1c8ee04497063e945415dec9b4d1dd0a10, region:adjacent-unit:12-16, region:adjacent-unit:3-6, region:adjacent-unit:17-23`
- `src/lib.rs`:1-12: pub mod auth; pub struct RequestContext { pub user_id: String, } pub fn handle_login(user_id: &str) -> String { let context = RequestContext { user_id: user_id.to_string(), }; auth::issue_token(&context, 3600) }
  - score: `0.077`; signals: `bm25_relevance` +0.988, `retrieval_rrf:lexical` +0.077, `score_reconciliation` -0.988
  - evidence: `search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1, region:adjacent-unit:3-6, region:adjacent-unit:1-2`
- `ok.toml`:1-147: [repo] name = "open-kioku-repo" root = "." [index] incremental = true max_file_size = "1mb" exclude = [ ".git/**", "**/.git/**", "node_modules/**", "**/node_modules/**", "target/**", "**/target/**", "dist/**", "**/dist/**", "build/**", "**/build/**", ".venv/**", "**/.venv/**", ".ok/**", "**/.ok/**", "package-lock.json", "**/package-lock.json", "pnpm-lock.yaml", "**/pnpm-lock.yaml", "yarn.lock", "**/yarn.lock", "bun.lockb", "**/bun.lockb", ] # How call, inheritance and type-use edges are resolved while indexing: # "legacy": symbol-registry resolution only; the resolution quality report is skipped. # "shadow": runs the proof-gated resolver beside the registry, records its quality report # and its proven edges, and keeps the registry's CALLS facts in the graph. # "v2": the proof-gated resolver's proven CALLS edges replace the registry's. resolution_mode = "shadow" [documents] enabled = true plain_text = [] [languages] enabled = [ "rust", "java", "typescript", "javascript", "python", "go", "yaml", "json", "toml", "sql", ] [scip] enabled = false mode = "off" auto_generate = false allow_install = false timeout_seconds = 300 paths = [ "index.scip", ".ok/indexes/go.scip", ".ok/indexes/java.scip", ".ok/indexes/typescript.scip", ".ok/indexes/python.scip", ] [history] enabled = true max_commits = 500 max_files_per_commit = 40 [search] lexical = "tantivy" semantic = "disabled" structural = true [ranking] text_relevance = 1.0 exact_reference = 1.0 graph_proximity = 0.35 boundary_fit = 0.25 runtime_corroboration = 0.3 git_cochange = 0.25 validation_proximity = 1.0 memory_signal = 0.2 path_quality = 1.0 semantic_similarity = 0.3 [semantic] enabled = false backend = "exact-flat" provider = "local" model = "local-hash" dimensions = 384 distance = "cosine" batch_size = 64 ann_min_rows = 10000 index_symbols = true index_chunks = true index_docs = true index_memory = true external_provider_allowed = false [memory] enabled = false [runtime] # Runtime error provider; inert while enabled = false. Enabling it requires # provider = "sentry", organization, project and auth_token_env = "SENTRY_AUTH_TOKEN". enabled = false [mcp] mode = "read-only" transport = "stdio" allow_write = false hide_experimental = false [security] redact_secrets = true deny_network = true allow_hidden_files = false allow_write = false approval_required = true [commands] allow = [ "cargo test", "cargo check", "mvn test", "npm test", "pytest", ] [paths] deny = [ ".env", ".aws/**", ".ssh/**", "**/secrets/**", ] [architecture] rules = ".ok/architecture-rules.yml"
  - score: `0.071`; signals: `bm25_relevance` +0.652, `retrieval_rrf:lexical` +0.071, `score_reconciliation` -0.652
  - evidence: `search:ok.toml:81-147:0, search:ok.toml:81-147:1, region:adjacent-unit:1-80`
- `tests/auth_flow.rs`:4-7: fn login_returns_valid_token() {
  - score: `0.125`; signals: `bm25_relevance` +2.864, `query_variant_boost` +1.000, `retrieval_rrf:lexical` +0.083
  - evidence: `search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1, test:314d20ce2e07fafa14267b0cf2fc46952eefae3c809afb228c50c863f45a6e76`
- `README.md`:1-3: # Open Kioku Demo: expired sessions Try `ok preflight "change token expiration"`. The safe path begins in `src/auth.rs` and must preserve the login flow in `src/lib.rs` plus `tests/auth_flow.rs`; changing only the handler is the tempting but incomplete edit.
  - score: `0.091`; signals: `retrieval_rrf:document` +0.091
  - evidence: `document:README.md:1-3`

## Relevant Symbols

- `src::auth::validate_token` (Function)
- `src::lib::handle_login` (Function)
- `tests::auth_flow::login_returns_valid_token` (Function)

## Impact Candidates

- `src/lib.rs`:7-12: pub fn handle_login(user_id: &str) -> String {
  - score: `7.886`; signals: `bm25_relevance` +7.836, `query_variant_boost` +0.050
  - evidence: `search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1`
- `tests/auth_flow.rs`:4-7: assert!(auth::validate_token(&token));
  - score: `5.362`; signals: `bm25_relevance` +5.312, `query_variant_boost` +0.050
  - evidence: `search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1`
- `src/lib.rs`:3-6: pub struct RequestContext {
  - score: `4.634`; signals: `bm25_relevance` +4.584, `query_variant_boost` +0.050
  - evidence: `search:src/lib.rs:3-6:0, search:src/lib.rs:3-6:1`
- `src/lib.rs`:1-2: pub mod auth;
  - score: `2.525`; signals: `bm25_relevance` +1.525, `query_variant_boost` +1.000
  - evidence: `search:src/lib.rs:1-2:0, search:src/lib.rs:1-2:1`
- `ok.toml`:81-147: [ranking]
  - score: `0.702`; signals: `bm25_relevance` +0.652, `query_variant_boost` +0.050
  - evidence: `search:ok.toml:81-147:0, search:ok.toml:81-147:1`

## Runtime Signals

- None found

## Validation Candidates

- `issue_token` via `cargo test`; signals: `test_selection_score` +0.600, `indexed_test_confidence` +0.600, `command_availability` +0.050; evidence: `9f8fd72a3efef98669b10543eb7c7fa7a75584a4c3d16a705c7616ea05306fd1`
- `issues_token_with_user_id` via `cargo test`; signals: `test_selection_score` +0.600, `indexed_test_confidence` +0.600, `command_availability` +0.050; evidence: `cdd809b815a1af2f7081c8aad676fc1c8ee04497063e945415dec9b4d1dd0a10`
- `login_returns_valid_token` via `cargo test`; signals: `test_selection_score` +0.600, `indexed_test_confidence` +0.600, `command_availability` +0.050; evidence: `314d20ce2e07fafa14267b0cf2fc46952eefae3c809afb228c50c863f45a6e76`
- `tests` via `cargo test`; signals: `test_selection_score` +0.600, `indexed_test_confidence` +0.600, `command_availability` +0.050; evidence: `ae205155363199d68c9b593ef5680df439ba9213c0536a84d77f326fd6fdbcfd`
- `validate_token` via `cargo test`; signals: `test_selection_score` +0.600, `indexed_test_confidence` +0.600, `command_availability` +0.050; evidence: `aeb43ec5523f20785b5dde21a67defb7d9c9416205bcf3a941778e0b2c88cb58`

## Repo Memory

- None matched

## Edit Boundary

Allowed files:
- `README.md`
  - reason: primary context matched the requested edit intent
  - evidence: `document:README.md:1-3`
- `ok.toml`
  - reason: primary context matched the requested edit intent
  - evidence: `region:adjacent-unit:1-80, search:ok.toml:81-147:0, search:ok.toml:81-147:1`
- `src/auth.rs`
  - reason: primary context matched the requested edit intent
  - evidence: `region:adjacent-unit:12-16, region:adjacent-unit:17-23, region:adjacent-unit:3-6, search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1, test:9f8fd72a3efef98669b10543eb7c7fa7a75584a4c3d16a705c7616ea05306fd1, test:aeb43ec5523f20785b5dde21a67defb7d9c9416205bcf3a941778e0b2c88cb58, test:cdd809b815a1af2f7081c8aad676fc1c8ee04497063e945415dec9b4d1dd0a10`
  - symbols: `src::auth::validate_token`
- `src/lib.rs`
  - reason: primary context matched the requested edit intent
  - evidence: `region:adjacent-unit:1-2, region:adjacent-unit:3-6, search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1`
  - symbols: `src::lib::handle_login`
- `tests/auth_flow.rs`
  - reason: primary context matched the requested edit intent
  - evidence: `search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1, test:314d20ce2e07fafa14267b0cf2fc46952eefae3c809afb228c50c863f45a6e76`
  - symbols: `tests::auth_flow::login_returns_valid_token`

Caution files:
- None

Forbidden patterns:
- `**/*Generated*`
  - reason: generated sources should be changed through their source generator
  - evidence: `boundary:default-forbidden`
- `**/generated/**`
  - reason: generated sources should be changed through their source generator
  - evidence: `boundary:default-forbidden`
- `**/secrets/**`
  - reason: security-sensitive secret paths are outside normal edit boundaries
  - evidence: `boundary:default-forbidden`
- `.git/**`
  - reason: git internals are never part of product edits
  - evidence: `boundary:default-forbidden`
- `.ok/**`
  - reason: Open Kioku local index artifacts are generated state
  - evidence: `boundary:default-forbidden`
- `build/**`
  - reason: build output is generated state
  - evidence: `boundary:default-forbidden`
- `dist/**`
  - reason: distribution output is generated state
  - evidence: `boundary:default-forbidden`
- `generated/**`
  - reason: generated sources should be changed through their source generator
  - evidence: `boundary:default-forbidden`
- `node_modules/**`
  - reason: vendored package dependencies are out of scope
  - evidence: `boundary:default-forbidden`
- `target/**`
  - reason: Rust build output is generated state
  - evidence: `boundary:default-forbidden`
- `third_party/**`
  - reason: third-party dependencies require a separate explicit change
  - evidence: `boundary:default-forbidden`
- `vendor/**`
  - reason: vendored dependencies require a separate explicit change
  - evidence: `boundary:default-forbidden`

Boundary expansion:
- Any edit outside allowed_files must cite concrete evidence from search, impact, references, tests, architecture, ownership, or history analysis.
  - required evidence refs: `document:README.md:1-3, region:adjacent-unit:1-2, region:adjacent-unit:1-80, region:adjacent-unit:12-16, region:adjacent-unit:17-23, region:adjacent-unit:3-6, search:ok.toml:81-147:0, search:ok.toml:81-147:1, search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1, search:src/lib.rs:1-2:0, search:src/lib.rs:1-2:1, search:src/lib.rs:3-6:0, search:src/lib.rs:3-6:1, search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1, search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1, test:314d20ce2e07fafa14267b0cf2fc46952eefae3c809afb228c50c863f45a6e76, test:9f8fd72a3efef98669b10543eb7c7fa7a75584a4c3d16a705c7616ea05306fd1, test:aeb43ec5523f20785b5dde21a67defb7d9c9416205bcf3a941778e0b2c88cb58, test:cdd809b815a1af2f7081c8aad676fc1c8ee04497063e945415dec9b4d1dd0a10`

Signal hooks:
- architecture: `ok architecture summary, ok architecture violations, ok architecture policy check`
- ownership: `CODEOWNERS, git_history`
- co-change: `similar_change_overlap, historical_prs`

Boundary evidence: `document:README.md:1-3, region:adjacent-unit:1-2, region:adjacent-unit:1-80, region:adjacent-unit:12-16, region:adjacent-unit:17-23, region:adjacent-unit:3-6, search:ok.toml:81-147:0, search:ok.toml:81-147:1, search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1, search:src/lib.rs:1-2:0, search:src/lib.rs:1-2:1, search:src/lib.rs:3-6:0, search:src/lib.rs:3-6:1, search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1, search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1, test:314d20ce2e07fafa14267b0cf2fc46952eefae3c809afb228c50c863f45a6e76, test:9f8fd72a3efef98669b10543eb7c7fa7a75584a4c3d16a705c7616ea05306fd1, test:aeb43ec5523f20785b5dde21a67defb7d9c9416205bcf3a941778e0b2c88cb58, test:cdd809b815a1af2f7081c8aad676fc1c8ee04497063e945415dec9b4d1dd0a10`

## Recommended Next Steps

- Inspect the primary context files and symbol ranges before editing.
- Review direct impact candidates before deciding the edit boundary.
- Run the recommended validation commands after the change.
- Keep edits within allowed files unless new evidence justifies expanding scope.

## Agent Tool Calls

- `search_code`: Find indexed evidence for the task. `{"limit":12,"query":"token"}`
- `impact_analysis`: Estimate likely downstream files for the primary source file. `{"path":"src/auth.rs"}`
- `build_context_pack`: Assemble primary files, symbols, tests, and boundaries. `{"format":"markdown","limit":12,"task":"token"}`
- `find_tests_for_change`: Find indexed validation candidates for the primary source file. `{"limit":8,"path":"src/auth.rs"}`

