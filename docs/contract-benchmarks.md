# Contract Benchmarks

`ok contract-bench` scores contract generation and verification against a
contract-specific JSON corpus. The checked-in suite lives at
`benchmarks/contract-cases.json`, runs against `benchmarks/contract-fixture`,
and is enforced by `.github/workflows/bench.yml`.

Run it locally:

```sh
ok contract-bench benchmarks/contract-fixture \
  --cases-file benchmarks/contract-cases.json \
  --min-cases 7 \
  --min-verdict-accuracy 0.95 \
  --min-verification-precision 0.95 \
  --min-boundary-precision 0.97 \
  --min-boundary-recall 0.90 \
  --min-toon-reduction 0.35
```

Use `--json` to inspect per-case reports:

```sh
ok --json contract-bench benchmarks/contract-fixture \
  --cases-file benchmarks/contract-cases.json
```

## Case Format

Each case declares one Epic #53 contract rule family:

```json
{
  "id": "api-surface-addition",
  "rule_family": "api_surface_delta",
  "task": "update api receipt formatting in src/api/mod.rs",
  "expected_verdict": "fail",
  "expected_contract": {
    "primary_files": ["src/api/mod.rs"],
    "allowed_boundary": ["src/api/mod.rs"],
    "min_required_tests": 1,
    "min_traceability": 5,
    "min_architecture_constraints": 1,
    "min_evidence_refs": 1
  },
  "contract_overlay": {
    "api_surface_constraints": [
      {
        "scope": "src/api/mod.rs",
        "allowed_changes": [],
        "severity": "forbidden",
        "reason": "public API additions require explicit contract approval",
        "evidence_refs": []
      }
    ]
  },
  "edits": [
    {
      "path": "src/api/mod.rs",
      "content": "pub fn new_public_api() {}\n"
    }
  ],
  "check_api_surface": true,
  "expected_findings": ["api_surface_violation"],
  "explanation_terms": ["api", "traceability"]
}
```

Supported `rule_family` values are `allowed_edit`, `forbidden_edit`,
`missing_tests`, `architecture_violation`, `dependency_delta`,
`api_surface_delta`, and `explanation_quality`. The runner fails before
scoring if any required family is absent.

## Metrics

The report includes:

- `verdict_accuracy`: exact expected-vs-actual verification verdict match rate.
- `verification_precision`: non-pass verification precision.
- `boundary_precision`: generated allowed boundary paths that avoid forbidden
  paths declared by the case.
- `boundary_recall`: expected allowed boundary paths found in the generated
  contract.
- `min_toon_reduction` and `mean_toon_reduction`: byte reduction for TOON
  contract export compared with pretty JSON for the same contract content.
- `mean_generation_ms` and `mean_verification_ms`: per-case contract generation
  and verification timing.

Each case runs in a temporary copy of the fixture repository. The runner indexes
the baseline fixture, builds a contract from the generated plan, applies
case-specific contract constraints, mutates the temporary working tree, and then
verifies the changed files or diff against the contract. The checked-in fixture
contains API, domain, storage, tests, and architecture policy layers so contract
benchmarks do not rely on the main Open Kioku source tree to answer themselves.
