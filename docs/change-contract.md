# Change Contract Schema

`open-kioku-contract` owns the authoritative, versioned artifact that connects a
task to its indexed evidence, edit boundary, impacted symbols, required tests,
architecture constraints, validation commands, validation requirements, risk,
confidence, and source plan.

The crate is intentionally low-level. It contains schema and validation
primitives only and does not depend on CLI, MCP, patch, plan, or persistence
crates.

## Version 1

`ChangeContractV1` requires every top-level field defined by contract v1.
Deserialization rejects:

- missing or unsupported versions
- empty mandatory sections
- empty identifiers and explanations
- invalid, non-finite, or out-of-range scores
- risk or confidence levels that disagree with their scores
- non-exact confidence without explicit uncertainty
- invalid or decreasing timestamps
- non-normalized, absolute, escaping, duplicate, or overlapping file boundaries

`secondary_files` and `forbidden_files` may be empty because a valid narrow
change does not always have supporting or prohibited files. Unknown additive
top-level fields are retained in `extensions` and survive a v1 round trip.

Validation commands are the human-readable compatibility surface. Newer
contracts may also carry `validation_requirements`, which bind a command,
optional repository-relative working directory, rationale, and evidence refs to
the attestation system. Verification records can attach
`ValidationAttestation` entries that include command, cwd, timestamps, exit
code, allowlist status, normalized outcome, and stdout/stderr summaries.
Detailed validation ledgers are persisted under
`.ok/contracts/validation/{run_id}.json`; contract verification JSONL records
store the corresponding attestation summaries.

Contracts generated from `PlanReport` preserve architecture policy evidence
when a plan or nested impact report includes `architecture_policy`.
`open_kioku_plan::summarize_policy_for_contract` returns a
`PolicySignalSummary` with bounded `PolicyViolationEvidenceRef` entries for
contract consumers. The builder adds a policy summary constraint and bounded
policy violation constraints under `architecture_constraints`, each citing
stable `architecture-policy:*` evidence refs that are also present in the
contract's top-level `evidence_refs`. Verification uses configured repository
policy to classify dependency deltas as allowed, violating, or unknown.

Generated contracts also preserve bounded history intelligence when the source
plan carries it. The builder writes a `history_signal_summary` extension and a
`history_signals` traceability entry for `history_churn`, `ownership_risk`,
`similar_change_overlap`, and `reviewer_affinity` evidence refs. This extension
is advisory and does not alter the v1 schema or the rule that exact code,
boundary, and validation evidence remain authoritative.

## CLI and MCP Workflow

The CLI exposes first-class contract commands without removing the legacy
`plan` and `verify` flow:

```bash
ok --repo /path/to/repo --json contract create "update auth token" --limit 12
ok --repo /path/to/repo contract show <contract-id> --format markdown
ok --repo /path/to/repo contract export <contract-id> --format toon
ok --repo /path/to/repo --json contract verify --id <contract-id> --changed src/auth.rs
ok --repo /path/to/repo contract explain --id <contract-id> --format markdown
```

`contract create` accepts exactly one of a task, `--plan`, or `--plan-json`.
Contracts are stored under `.ok/contracts` by default; pass `--no-store` to
print an inline contract only. `contract verify` accepts exactly one of
`--id`, `--contract`, or `--contract-json`, so callers can use either stored
contract IDs or inline JSON artifacts.

The MCP server exposes the same workflow through `create_change_contract`,
`get_change_contract`, `verify_change_contract`, and `explain_verification`.
`create_change_contract` accepts a task, inline `plan`, or `plan_json`;
`verify_change_contract` accepts `contract_id`, inline `contract`, or
`contract_json`; `get_change_contract` can export JSON, Markdown, or TOON.
Stored contract verification appends JSONL verification records next to the
contract, while inline verification leaves the store untouched.

Use `open_kioku_contract::schema()` to obtain the JSON Schema root. The
canonical JSON example is
[`crates/open-kioku-contract/tests/fixtures/change_contract_v1.json`](../crates/open-kioku-contract/tests/fixtures/change_contract_v1.json).
The runnable Rust example prints both the validated contract and its schema:

```bash
cargo run -p open-kioku-contract --example change_contract_v1
```

Programmatically constructed values must call `ChangeContractV1::validate()`
before they cross a crate or persistence boundary. JSON deserialization invokes
the same validation automatically.

Persistence, plan adapters, builders, verification, and user-facing CLI/MCP
commands live in the plan, patch, CLI, and MCP crates; the contract crate
continues to own only the schema and validation primitives.
