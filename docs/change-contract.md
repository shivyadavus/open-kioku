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
