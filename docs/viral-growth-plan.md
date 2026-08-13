# Open Kioku activation and sharing execution plan

## Objective

Make Open Kioku's first successful use:

```text
one command -> agent is correctly connected -> one real task gets a concise,
grounded pre-edit verdict -> a safe PR-ready proof can be shared
```

The public promise becomes:

> Stop coding agents from changing the wrong files and running the wrong tests.

## Delivery plan

Work this as one issue and PR per phase, in this order.

| Phase | Deliverable | Completion gate |
|---|---|---|
| 0 | Fix current product-truth drift | Every public command/version is executable and matches releases |
| 1 | One-command onboarding for Claude Code and Cursor | A clean-machine smoke test activates a repo without manual config pasting |
| 2 | Canonical "preflight" experience | One task returns files, risks, tests, and evidence quality in a shareable concise result |
| 3 | Credible demo and quality floor | Demo produces high-confidence output with zero spurious unresolved-import noise |
| 4 | Agent-native habit layer | Installed skills/rules cause appropriate pre-edit use without users pasting a ritual prompt |
| 5 | PR evidence and sharing loop | A privacy-safe GitHub Action produces a reviewable evidence artifact/check |
| 6 | Pi integration package | Pi users can install Open Kioku as a focused extension/package |
| 7 | Docs/site/release hardening | All claims, screenshots, commands, metadata, and packages are validated in CI |

### Phase 0 — repair the trust leaks first

1. Replace the nonexistent `ok setup --agent` site demo with either the shipped command or Phase 1's new command. The current site shows it, while the current CLI rejects it.
2. Establish one release source of truth:
   - Rust workspace version
   - npm wrapper and platform package versions
   - plugin manifests
   - Homebrew formula
   - release metadata
   - landing-page version
3. Extend `scripts/validate-versions.sh` to fail if any public surface differs.
4. Add a release test that executes every command advertised in the quickstart from a clean temporary repo.
5. Do not promote v2.4.0 until npm, binaries, docs, and the website all resolve to that same shipped version.

### Phase 1 — build real one-command onboarding

Add:

```sh
ok setup agent cursor --repo . --apply
ok setup agent claude --repo . --apply
```

Default behavior is `--dry-run`; `--apply` is explicit. It will:

- detect a supported client configuration;
- create `.ok/` and index the repo;
- atomically add only the Open Kioku MCP entry;
- install the agent skill/rules;
- preserve existing user configuration and create a reversible backup;
- validate the MCP server with a real stdio initialize/tool call;
- emit a concise result and an uninstall command;
- never enable write mode, network access, or background watch automatically.

Keep `ok mcp install` as a backward-compatible config-printer.

Tests:

- unit tests for each config adapter and malformed-config recovery;
- integration tests using temporary home/config directories;
- idempotency, rollback, symlink/path-traversal, and unrelated-config preservation tests;
- Cursor and Claude end-to-end smoke tests in CI;
- JSON output contract snapshots for automation.

### Phase 2 — replace the tool catalogue with one memorable outcome

Add a canonical command and MCP tool:

```sh
ok preflight "add token expiration"
preflight_change(task)
```

It composes existing planning, impact, test-selection, and verification evidence into a short decision:

```text
SAFE TO START — medium confidence
Edit: src/auth.rs, src/lib.rs
Likely affected: tests/auth_flow.rs
Run: cargo test auth_flow
Caveat: exact references unavailable; no false precision.
```

Details remain expandable, but the default must be legible in seconds.

Rules:

- no self-congratulatory "proof score" as the headline;
- clearly distinguish confirmed facts, heuristics, and unknowns;
- include exact evidence references and index freshness;
- add `--format json|markdown|html` from one report schema;
- retain existing lower-level tools for experts.

Tests:

- report-schema compatibility tests;
- CLI/MCP parity snapshots;
- fixture cases for safe, risky, and insufficient-evidence verdicts;
- regression cases where the tool must say "unknown" rather than invent certainty.

### Phase 3 — make the demo earn the promise

The current demo is technically small but product-hostile: the initial plan returns medium confidence and 169 unresolved imports. Fix the underlying analysis issue; do not hide it.

Build a new adversarial demo fixture containing:

- a realistic cross-file bug;
- an obvious tempting but incorrect edit path;
- an actual dependency/test relationship Open Kioku finds;
- expected preflight result and a deliberately bounded verification result.

Acceptance criteria:

- zero unexpected unresolved imports;
- no failed optional pass presented as a mysterious warning in the default happy path;
- a user can understand the "saved mistake" in under 30 seconds;
- the README GIF, site animation, CLI demo, and checked-in golden output derive from the same fixture.

### Phase 4 — turn first use into a habit

For Claude Code and Cursor, ship a minimal installed workflow:

- unfamiliar code -> search/definition first;
- rename, deletion, public API change -> impact first;
- multi-file work -> preflight;
- before finishing -> verify against preflight.

Important: MCP cannot force an LLM to call a tool. The implementation must use each client's native skill/rule mechanism and be honest about that limitation. Do not claim enforcement where the client only provides guidance.

Reduce the user-facing default from "59 tools" to four named actions:

```text
Explore -> Preflight -> Edit -> Verify
```

### Phase 5 — create the share loop through PRs, not social posting

Ship an opt-in GitHub Action:

```yaml
- uses: shivyadavus/open-kioku-action@v1
  with:
    task: "..."
    verify: true
```

It must:

- run locally in CI against the checked-out revision;
- emit a redacted JSON/Markdown/HTML evidence artifact;
- optionally post a concise PR check/comment with explicit permissions;
- include CLI version, commit SHA, index manifest hash, plan hash, changed files, validations, and caveats;
- never upload source snippets by default;
- support artifact retention and no-comment mode.

This produces a reviewable message such as:

> Open Kioku verified this PR stayed within the proposed edit boundary and ran the selected tests.

No automatic social posting.

### Phase 6 — package for Pi, without becoming Pi

Create a separately versioned TypeScript package:

```sh
pi install npm:@open-kioku/pi
```

It should provide:

- `/ok-preflight <task>`;
- an index freshness check when a trusted project session begins;
- a lightweight skill directing Pi to use preflight for risky edits;
- a locally executed `ok` integration only—no source upload and no provider credentials;
- package-level mocked-CLI and integration tests.

Pi's extension/package model and built-in command discovery are the useful pattern here; its general-agent surface is not the product to replicate.

### Phase 7 — documentation, site, and release correctness

Update together, not as an afterthought:

- README: one primary first-win path, then advanced paths below it;
- landing page: exact shipped command, one sharp wedge, actual demo result, no aspirational UI;
- client-specific setup pages;
- GIF and Open Graph image;
- `docs/mcp-tools.md`, roadmap, release checklist, examples, package READMEs, and plugin descriptions;
- GitHub action README and security/privacy documentation.

Add automated protection:

- command-contract manifest used by docs and CLI smoke tests;
- link checker and version consistency checker;
- Playwright desktop/mobile checks for the landing page;
- screenshot approval for the hero/demo/OG card;
- release test in a clean npm install environment;
- `npm pack --dry-run`, Cargo package validation, and platform wrapper verification.

## Validation bundle before every release

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo test -p open-kioku-tests
scripts/validate-versions.sh
scripts/validate-docs.sh
scripts/verify-release-readiness.sh
npm pack --dry-run --json
cargo build --release
```

Plus:

- fresh-machine onboarding smoke for Claude and Cursor;
- demo quality assertion;
- action artifact/redaction test;
- Pi extension tests;
- live site browser checks;
- real-repository preflight/verify smoke on at least Rust, TypeScript, Python, Go, and Java fixtures.

## Success criteria

Do not call this complete based on installs or stars. It is complete when:

1. A new user reaches an agent-connected, validated first preflight in one command.
2. The demo proves a concrete avoided mistake with no credibility noise.
3. A PR can carry a privacy-safe, independently inspectable evidence result.
4. Users no longer need to paste a ritual prompt to get normal pre-edit behavior.
5. Public docs, site, release version, and executable commands remain continuously identical.

## Publishing scope

Publishing is included in Phase 7 and is a required release deliverable, not an optional follow-up:

- GitHub `main`, release tag, native binaries, checksums, release notes, and GitHub Pages;
- crates.io publication, package inspection, and clean-install verification;
- npm wrapper and each native platform package, published in dependency order and clean-install verified;
- Homebrew Formula update and validation after immutable release artifacts exist;
- Claude, Cursor, Codex, and Pi package/plugin metadata, with cached third-party listings verified separately from package publication.

The final release checklist must report each surface independently. A merge to GitHub does not count as publishing to crates.io, npm, Homebrew, or cached plugin directories.
