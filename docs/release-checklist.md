# Release Checklist

Open Kioku release metadata is canonicalized by `release-metadata.json` and checked by `scripts/validate-versions.sh`. Run the checklist from a clean checkout before publishing a tag.

## Preflight

```sh
scripts/validate-versions.sh
scripts/validate-docs.sh
scripts/check-no-ignored-tests.py
scripts/validate-release-metadata.py
scripts/validate-trust-gates.py
OK_BIN=target/debug/ok scripts/validate-public-quickstart.sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
scripts/verify-release-readiness.sh
scripts/verify-npm-package.sh
```

The release workflow repeats `scripts/validate-versions.sh` and then verifies
both publishing credentials before any build starts, because a dead token only
surfaces after the builds and, for crates.io, after the tag and GitHub release
are already public. The `preflight` job runs `npm whoami` against `NPM_TOKEN`
(a repository secret). The `preflight-crates` job authenticates
`CARGO_REGISTRY_TOKEN` (an environment secret in the `crates-io` environment,
which the job declares in order to read it) with a read-only crates.io
request. A failed check names the secret, where it lives, and the
`gh secret set` command that rotates it. Neither check can prove a token's
publish scope; the registries only exercise that on the publish itself.

For a release that changes the reusable GitHub Action, also run its independent
`npm test` and `npm run check`, publish an immutable action tag, and verify the
`v1` major tag points at that reviewed release. The action publishes no source
snippets by default; re-check [`docs/github-action.md`](github-action.md) when
its privacy behavior changes.

Review `docs/release-trust.md` before tagging. It documents the checksums,
SBOM, provenance, third-party notices, local processing threat model, and
install audit evidence expected on every release.

## Version And Tag

- Confirm `Cargo.toml` `[workspace.package]` version is `4.0.0`.
- Confirm `release-metadata.json` uses tag `v4.0.0`.
- Confirm the GitHub release tag is exactly `v4.0.0`.
- Confirm `CHANGELOG.md` has a `4.0.0` section and a matching `[4.0.0]` release link.

## Crates.io Publication

Keep crates.io credentials local. Normally the release workflow's own
`publish-crates` job publishes the workspace; the commands below are the local
fallback. `EXPECTED_VERSION` is an independent guard against publishing the
wrong version, so state it explicitly rather than deriving it from `Cargo.toml`. From a clean checkout of the exact release commit, first run:

```sh
EXPECTED_VERSION=<VERSION> scripts/publish-crates.sh --dry-run
```

After the `v<VERSION>` tag is final and the GitHub release gate has succeeded,
publish locally using Cargo credentials from `cargo login` or
`CARGO_REGISTRY_TOKEN`:

```sh
EXPECTED_VERSION=<VERSION> scripts/publish-crates.sh --publish
```

Do not commit or upload the crates.io token.

The workflow's `CARGO_REGISTRY_TOKEN` lives in the `crates-io` GitHub
environment, not in the repository secrets; `gh secret list` will not show it
without `--env crates-io`. Rotate it yourself, so the token never enters a
transcript or a commit:

```sh
gh secret set CARGO_REGISTRY_TOKEN --repo shivyadavus/open-kioku --env crates-io
```

A `publish-crates` failure of `403 Forbidden: authentication failed` is this
token being revoked, expired, or never issued. The `preflight-crates` job
catches it before the builds; if it slips through anyway, publish locally as
above and re-run the failed job, which skips crates that are already
published.

## Install Channels

Each channel must report the same `ok --version` value.

```sh
npm install -g open-kioku
ok --version

cargo binstall open-kioku-cli
ok --version

cargo install open-kioku-cli
ok --version
```

Inspect the wrapper package before publishing; this confirms the package name,
version, entrypoint, README, and generated tarball name without publishing:

```sh
scripts/verify-npm-package.sh
```

## Release Artifacts

GitHub release notes, the release workflow, in-repo Homebrew formula URLs, cargo-binstall metadata, and npm platform packages must reference the same artifact set. Do not advertise Homebrew as a public install channel until a `shivyadavus/homebrew-open-kioku` tap exists and the install command has been verified.

- `ok-linux-x86_64`
- `ok-linux-x86_64.sha256`
- `ok-linux-arm64`
- `ok-linux-arm64.sha256`
- `ok-macos-arm64`
- `ok-macos-arm64.sha256`
- `ok-windows-x86_64.exe`
- `ok-windows-x86_64.exe.sha256`
- `SHA256SUMS`
- `SBOM.cargo-metadata.json`
- `PROVENANCE.json`
- `THIRD_PARTY_NOTICES.md`
- `release-metadata.json`

`scripts/generate-release-trust-artifacts.sh dist` generates the aggregate
release trust artifacts after the platform binaries have been downloaded into
`dist/`. GitHub Actions also publishes build provenance attestations for the
four binary artifacts.

The hash-pin commit (`release: pin <VERSION> artifact hashes`) writes the
built binaries' sha256 values into `release-metadata.json`,
`Formula/open-kioku.rb`, and the root `Dockerfile` (`OK_SHA256`, the
`ok-linux-x86_64` hash; `OK_VERSION` is synced earlier with every other
manifest). MCP directories build that image from the release tag, so the tag
must be at or after this commit. `scripts/validate-release-metadata.py` fails
when the Dockerfile disagrees with the workspace version or with the
`ok-linux-x86_64` entry in `release-metadata.json`.

## Post-Publish Smoke

```sh
ok demo --force
ok prove ./open-kioku-demo --task token
ok init ./open-kioku-demo
ok index ./open-kioku-demo
ok plan "change token expiration"
ok mcp install cursor --repo "$PWD/open-kioku-demo"
ok mcp install claude --repo "$PWD/open-kioku-demo"
ok mcp install codex --repo "$PWD/open-kioku-demo"
```
