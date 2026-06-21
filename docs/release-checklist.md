# Release Checklist

Open Kioku release metadata is canonicalized by `release-metadata.json` and checked by `scripts/validate-versions.sh`. Run the checklist from a clean checkout before publishing a tag.

## Preflight

```sh
scripts/validate-versions.sh
scripts/validate-docs.sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
scripts/verify-release-readiness.sh
```

## Version And Tag

- Confirm `Cargo.toml` `[workspace.package]` version is `2.1.0`.
- Confirm `release-metadata.json` uses tag `v2.1.0`.
- Confirm the GitHub release tag is exactly `v2.1.0`.
- Confirm `CHANGELOG.md` has a `2.1.0` section and a matching `[2.1.0]` release link.

## Install Channels

Each channel must report the same `ok --version` value.

```sh
npm install -g open-kioku
ok --version

brew install shivyadavus/open-kioku/open-kioku
ok --version

cargo binstall open-kioku-cli
ok --version

cargo install open-kioku-cli
ok --version
```

## Release Artifacts

GitHub release notes, the release workflow, Homebrew formula URLs, cargo-binstall metadata, and npm platform packages must reference the same artifact set:

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

## Post-Publish Smoke

```sh
ok demo --force
ok prove ./open-kioku-demo --task token
ok mcp install cursor --repo "$PWD/open-kioku-demo"
ok mcp install claude --repo "$PWD/open-kioku-demo"
ok mcp install codex --repo "$PWD/open-kioku-demo"
```
