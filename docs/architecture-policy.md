# Architecture Policy

Architecture policy is an experimental repository-owned contract for layers,
bounded contexts, dependency rules, public API boundaries, and explicit
exemptions. Policy loading, validation, and dependency edge evaluation are
available now, including public API boundary enforcement and explainable
exemptions. Integration into plans, contracts, and verification remains
follow-up work.

## Policy Location

The canonical file is:

```text
.open-kioku/architecture.toml
```

For compatibility, the same policy may be embedded in `ok.toml` below
`[architecture.policy]`. Nested arrays use the corresponding fully qualified
TOML paths:

```toml
[architecture.policy]
version = "v1"

[[architecture.policy.layers]]
id = "api"
paths = ["crates/*-api/**"]
```

The existing `architecture.rules` setting remains optional and retains its
legacy default of `.ok/architecture-rules.yml`.

Loading is deterministic:

- canonical only: load `.open-kioku/architecture.toml`
- compatibility only: load `[architecture.policy]` from `ok.toml`
- both and identical: use the canonical definition and report both sources
- both and different: fail with a conflict diagnostic; never merge silently
- neither: return no policy and preserve existing heuristic architecture behavior

The standalone example is
[`examples/architecture-policy.toml`](../examples/architecture-policy.toml).

## Schema

Every policy declares `version = "v1"` and may define:

- `layers`: named components selected by repository-relative path globs
- `contexts`: named bounded contexts selected by path globs
- `dependency_rules`: allowed or forbidden component edges with severity
- `public_api_rules`: public and internal path boundaries for a component
- `internal_only_rules`: internal path boundaries for a component
- `exemptions`: evidence-bearing exceptions tied to named policy rules and
  scoped to configured paths, tests, vendor code, or generated code

Component and rule identifiers must be unique. Rule references must resolve to
declared components, exemption references must resolve to declared rules, and
all path globs must be valid, repository-relative, and slash-normalized.
Unknown fields and severities are rejected rather than ignored.

## CLI

Validate the repository policy without requiring an index:

```bash
ok --repo /path/to/repo architecture policy validate
```

Validate an explicit standalone policy:

```bash
ok --repo /path/to/repo architecture policy validate \
  --path .open-kioku/architecture.toml
```

Print the resolved policy and its source:

```bash
ok --repo /path/to/repo architecture policy print
```

Evaluate indexed dependency edges against the resolved policy:

```bash
ok --repo /path/to/repo architecture policy check
```

The check evaluates indexed `imports`, `references`, and `calls` graph edges.
It reports deterministic counts for allowed edges, forbidden violations, and
unknown edges. It also evaluates public API boundaries over the same graph
evidence: cross-component consumers may only depend on configured public API
globs, and internal-only targets produce structured violations unless an
explicit exemption matches. Violations include the matching rule id, source
component, target component, severity, paths, edge type, and graph-edge
evidence. Unknown edges are counted exactly, while returned unknown samples are
bounded so large repositories do not produce unbounded output.

Explain a public API boundary decision for a file or symbol:

```bash
ok --repo /path/to/repo architecture policy explain --symbol crate::api::handler
ok --repo /path/to/repo architecture policy explain --file src/api/internal/session.rs
```

The explanation reports policy component matches plus any boundary violations
or exemptions involving the queried file or symbol. Exemptions are returned as
evidence records with their exemption id, rule id, scope, reason, paths, and
graph edge evidence; they do not silently remove unrelated findings.

Add global `--json` for stable structured output. Repositories with no policy
remain valid and explicitly report that heuristic architecture detection is
still active.

The MCP tool `architecture_policy_check` returns the same structured policy
check report for indexed repositories. `architecture_policy_explain` accepts
exactly one `file` or `symbol` argument and returns the same explanation shape
as the CLI. Plan/impact/contract integration is intentionally out of scope.
