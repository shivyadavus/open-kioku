# Plan: token

Found 5 primary context item(s), 0 direct impact candidate(s), 5 validation candidate(s), 0 repo memory fact(s); risk is low.

## Risk

- Level: `low`
- Score: `0.10`
- bounded context built from persisted search results

### Score Signals

- `plan_risk_score` contribution `0.100`: plan risk is `low` from merged context and impact risk

## Confidence

- Overall: `High` (`0.81`)
- Caveats:
  - exact symbol/reference evidence is absent
  - runtime corroboration is absent
- Components:
  - `boundary_tightness` score `1.00`, weight `0.15`, contribution `0.15`: how narrowly allowed edit files bound the proposed change
  - `evidence_density` score `1.00`, weight `0.20`, contribution `0.20`: amount of independent indexed evidence near the selected context
  - `exact_references` score `0.25`, weight `0.20`, contribution `0.05`: explicit exact symbol references or SCIP signals
  - `negative_evidence` score `1.00`, weight `0.15`, contribution `0.15`: absence of low-confidence, missing-anchor, or no-match evidence
  - `runtime_corroboration` score `0.25`, weight `0.05`, contribution `0.01`: runtime traces, incidents, or error signals that support the context
  - `test_coverage` score `1.00`, weight `0.10`, contribution `0.10`: selected tests with runnable commands
  - `validation_availability` score `1.00`, weight `0.15`, contribution `0.15`: presence of validation targets for the likely change

## Negative Evidence

- `exact_references`: no explicit exact symbol reference or SCIP evidence was found (`0.85`)
  - query: `token`; inspected: `search_result.evidence, search_result.match_reason`
  - next probe: `Run `ok scip setup .` and re-index with `ok index . --with-scip auto`.`
- `git_history`: no git co-change or historical validation evidence was available (`0.70`)
  - query: `token`; inspected: `plan.evidence, search_result.evidence`
  - next probe: `Run `git log --name-only -- src/auth.rs` to inspect historical co-change manually.`
- `runtime`: no runtime trace, incident, or error artifact corroborated the selected context (`0.75`)
  - query: `token`; inspected: `runtime_signals, search_result.evidence`
  - next probe: `Import or configure runtime artifacts, then rerun `ok plan`.`

## Evidence Provenance

### Section References

- `boundary`: `search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1, search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1, search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1`
- `impact`: `context:bounded-search`
- `negative_evidence`: `negative:exact_references:token-no-explicit-exact-symbol-reference-or-scip, negative:git_history:token-no-git-co-change-or-historical-validation, negative:runtime:token-no-runtime-trace-incident-or-error-artifact`
- `primary_context`: `search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1, search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1, search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1`
- `validation`: `314d20ce2e07fafa14267b0cf2fc46952eefae3c809afb228c50c863f45a6e76, 9f8fd72a3efef98669b10543eb7c7fa7a75584a4c3d16a705c7616ea05306fd1, ae205155363199d68c9b593ef5680df439ba9213c0536a84d77f326fd6fdbcfd, aeb43ec5523f20785b5dde21a67defb7d9c9416205bcf3a941778e0b2c88cb58, cdd809b815a1af2f7081c8aad676fc1c8ee04497063e945415dec9b4d1dd0a10`

### Evidence Items

- `context:src/auth.rs` `open-kioku-search` (Lexical): BM25 lexical match from local Tantivy index
- `context:src/auth.rs` `open-kioku-search` (Lexical): query variant `token` matched local index
- `context:tests/auth_flow.rs` `open-kioku-search` (Lexical): BM25 lexical match from local Tantivy index
- `context:tests/auth_flow.rs` `open-kioku-search` (Lexical): query variant `token` matched local index
- `context:src/auth.rs` `open-kioku-search` (Lexical): BM25 lexical match from local Tantivy index
- `context:src/auth.rs` `open-kioku-search` (Lexical): query variant `token` matched local index
- `context:src/auth.rs` `open-kioku-search` (Lexical): BM25 lexical match from local Tantivy index
- `context:src/auth.rs` `open-kioku-search` (Lexical): query variant `token` matched local index
- `context:src/lib.rs` `open-kioku-search` (Lexical): BM25 lexical match from local Tantivy index
- `context:src/lib.rs` `open-kioku-search` (Lexical): query variant `token` matched local index
- `context:bounded-search` `open-kioku-context` (Lexical): context pack used persisted search results without full-table impact expansion
- `context:bounded-search` `open-kioku-context` (Lexical): context pack used persisted search results without full-table impact expansion

## Primary Context

- `src/auth.rs`:7-11: pub fn validate_token(token: &str) -> bool {
  - score: `2.990`; signals: `text_relevance` +2.833, `exact_reference` +0.150, `boundary_fit` +0.007
  - evidence: `search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1`
- `tests/auth_flow.rs`:4-7: fn login_returns_valid_token() {
  - score: `2.917`; signals: `text_relevance` +2.717, `exact_reference` +0.150, `validation_proximity` +0.050
  - evidence: `search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1`
- `src/auth.rs`:3-6: pub fn issue_token(context: &RequestContext, ttl_seconds: u64) -> String {
  - score: `2.709`; signals: `text_relevance` +2.551, `exact_reference` +0.150, `boundary_fit` +0.007
  - evidence: `search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1`
- `src/auth.rs`:17-23: fn issues_token_with_user_id() {
  - score: `2.561`; signals: `text_relevance` +2.404, `exact_reference` +0.150, `boundary_fit` +0.007
  - evidence: `search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1`
- `src/lib.rs`:7-12: auth::issue_token(&context, 3600)
  - score: `0.725`; signals: `text_relevance` +0.567, `exact_reference` +0.150, `boundary_fit` +0.007
  - evidence: `search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1`

## Relevant Symbols

- `src::auth::validate_token` (Function)
- `tests::auth_flow::login_returns_valid_token` (Function)
- `src::auth::issue_token` (Function)
- `src::auth::issues_token_with_user_id` (Function)
- `src::lib::handle_login` (Function)

## Impact Candidates

- None found

## Runtime Signals

- None found

## Validation Candidates

- `issue_token` via `cargo test`; signals: `test_selection_score` +0.600, `indexed_test_confidence` +0.600, `score_reconciliation` -0.600; evidence: `9f8fd72a3efef98669b10543eb7c7fa7a75584a4c3d16a705c7616ea05306fd1`
- `issues_token_with_user_id` via `cargo test`; signals: `test_selection_score` +0.600, `indexed_test_confidence` +0.600, `score_reconciliation` -0.600; evidence: `cdd809b815a1af2f7081c8aad676fc1c8ee04497063e945415dec9b4d1dd0a10`
- `login_returns_valid_token` via `cargo test`; signals: `test_selection_score` +0.600, `indexed_test_confidence` +0.600, `score_reconciliation` -0.600; evidence: `314d20ce2e07fafa14267b0cf2fc46952eefae3c809afb228c50c863f45a6e76`
- `tests` via `cargo test`; signals: `test_selection_score` +0.600, `indexed_test_confidence` +0.600, `score_reconciliation` -0.600; evidence: `ae205155363199d68c9b593ef5680df439ba9213c0536a84d77f326fd6fdbcfd`
- `validate_token` via `cargo test`; signals: `test_selection_score` +0.600, `indexed_test_confidence` +0.600, `score_reconciliation` -0.600; evidence: `aeb43ec5523f20785b5dde21a67defb7d9c9416205bcf3a941778e0b2c88cb58`

## Repo Memory

- None matched

## Edit Boundary

Allowed files:
- `src/auth.rs`
  - reason: primary context matched the requested edit intent
  - evidence: `search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1`
  - symbols: `src::auth::issue_token, src::auth::issues_token_with_user_id, src::auth::validate_token`
- `src/lib.rs`
  - reason: primary context matched the requested edit intent
  - evidence: `search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1`
  - symbols: `src::lib::handle_login`
- `tests/auth_flow.rs`
  - reason: primary context matched the requested edit intent
  - evidence: `search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1`
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
- Any edit outside allowed_files must cite concrete evidence from search, impact, references, tests, architecture, ownership, or co-change analysis.
  - required evidence refs: `search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1, search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1, search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1`

Signal hooks:
- architecture: `architecture_boundaries, architecture_violations`
- ownership: `CODEOWNERS, git_history`
- co-change: `git_cochange, historical_prs`

Boundary evidence: `search:src/auth.rs:17-23:0, search:src/auth.rs:17-23:1, search:src/auth.rs:3-6:0, search:src/auth.rs:3-6:1, search:src/auth.rs:7-11:0, search:src/auth.rs:7-11:1, search:src/lib.rs:7-12:0, search:src/lib.rs:7-12:1, search:tests/auth_flow.rs:4-7:0, search:tests/auth_flow.rs:4-7:1`

## Recommended Next Steps

- Inspect the primary context files and symbol ranges before editing.
- Run the recommended validation commands after the change.
- Keep edits within allowed files unless new evidence justifies expanding scope.

## Agent Tool Calls

- `search_code`: Find indexed evidence for the task. `{"limit":12,"query":"token"}`
- `impact_analysis`: Estimate likely downstream files for the primary source file. `{"path":"src/auth.rs"}`
- `build_context_pack`: Assemble primary files, symbols, tests, and boundaries. `{"format":"markdown","limit":12,"task":"token"}`
- `find_tests_for_change`: Find indexed validation candidates for the primary source file. `{"limit":8,"path":"src/auth.rs"}`
- `search_memory`: Check whether prior repo facts exist for this task. `{"limit":8,"query":"token"}`

