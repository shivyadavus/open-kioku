# Release Trust

Open Kioku release claims are backed by repeatable checks and release artifacts.
The release workflow builds platform binaries from the tagged source, publishes
per-binary `.sha256` files, and then generates the aggregate trust bundle with:

```sh
scripts/generate-release-trust-artifacts.sh dist
```

The generated bundle contains:

- `SHA256SUMS`: aggregate checksums for all release binaries.
- `SBOM.cargo-metadata.json`: dependency bill of materials from
  `cargo metadata --locked`.
- `PROVENANCE.json`: tag, version, workflow run, source ref, builder, and
  binary checksums.
- `THIRD_PARTY_NOTICES.md`: third-party notices copied into the release.
- `release-metadata.json`: canonical release-channel metadata.

The release job also runs `actions/attest-build-provenance` for each binary so
GitHub publishes provenance attestations tied to the release workflow identity.

## CI Gates

Every pull request runs:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
cargo test -p open-kioku-tests
scripts/check-no-ignored-tests.py
scripts/validate-release-metadata.py
scripts/validate-trust-gates.py
cargo audit --ignore RUSTSEC-2024-0437
cargo deny check
scripts/verify-release-readiness.sh
```

CI also calls named edge-case tests directly for MCP protocol snapshots, fixture
language coverage, local-mode network denial, snapshot export/import rebuilds,
semantic disabled state, and parser/runtime secret redaction. These explicit
commands make skipped coverage visible when a test is renamed or removed.

## Release Workflow Gates

`.github/workflows/release.yml` gates its own run before spending build time,
because every publish failure it has had so far was a credential failure that
surfaced only after the builds:

- `preflight` runs `scripts/validate-versions.sh` and `npm whoami` against
  `NPM_TOKEN`. npm reports a dead token as `E404 Not Found - PUT` on a package
  that exists, so the check exists to turn that into a plain answer.
- `preflight-crates` authenticates `CARGO_REGISTRY_TOKEN` against crates.io
  with a read-only request, `GET /api/v1/crates?following=1`, the one GET that
  accepts an API token (`/api/v1/me` accepts only a browser session). crates.io
  authenticates before it checks scopes, so a live token answers `200` (legacy
  token) or a scope refusal (scoped token), and a revoked, expired, or never
  issued token answers `403 authentication failed`, which is what the 4.0.0
  release hit in `publish-crates` after the tag and GitHub release were
  public. The secret is an environment secret in the `crates-io` environment,
  so the job declares that environment, exactly as `publish-crates` does. The
  check proves the token is live; only the publish itself exercises the
  publish scope.

After the builds, the `publish` job pins the sha256 of every binary into
`release-metadata.json`, `Formula/open-kioku.rb`, and the root `Dockerfile` in
one commit, verifies the built binaries against those pins with
`scripts/verify-release-artifact-hashes.py`, and tags that commit. The
Dockerfile pin matters because MCP directories build the image from the
release tag and verify the downloaded binary against `OK_SHA256`.

## Local Processing Threat Model

The local processing threat model is documented in
`docs/guides/security-threat-model.md`. Release readiness must continue to show
that Open Kioku can run local setup, plan, proof, UI, architecture, MCP install,
and verification flows without requiring a network service.

## Install Audit

Before tagging, run the install audit from `docs/release-checklist.md`. After
publishing, verify each advertised install channel reports the tagged version:

```sh
ok --version
npm install -g open-kioku
cargo binstall open-kioku-cli
cargo install open-kioku-cli
```

Homebrew remains metadata-only until a public tap is verified; the release
checklist intentionally says not to advertise it as a public channel.
