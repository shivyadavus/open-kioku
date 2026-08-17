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

For a release that changes the reusable GitHub Action, also run its independent
`npm test` and `npm run check`, publish an immutable action tag, and verify the
`v1` major tag points at that reviewed release. The action publishes no source
snippets by default; re-check [`docs/github-action.md`](github-action.md) when
its privacy behavior changes.

Review `docs/release-trust.md` before tagging. It documents the checksums,
SBOM, provenance, third-party notices, local processing threat model, and
install audit evidence expected on every release.

## Version And Tag

- Confirm `Cargo.toml` `[workspace.package]` version is `3.0.0`.
- Confirm `release-metadata.json` uses tag `v3.0.0`.
- Confirm the GitHub release tag is exactly `v3.0.0`.
- Confirm `CHANGELOG.md` has a `3.0.0` section and a matching `[3.0.0]` release link.

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
- `ok-macos-x86_64`
- `ok-macos-x86_64.sha256`
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
five binary artifacts.

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
