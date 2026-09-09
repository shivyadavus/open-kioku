# @open-kioku/darwin-x64

**This package is not published.** Open Kioku ships no prebuilt binary for macOS on Intel x64
on any channel — not npm, not GitHub release assets, not `cargo binstall`, and not Homebrew.
The `open-kioku` wrapper does not list it as an optional dependency, and it fails on an Intel
Mac with an explanatory message rather than resolving to something that does not exist.

The supported path on Intel macOS is to build from source:

```sh
cargo install open-kioku-cli
```

The last release that shipped a prebuilt Intel binary was 2.4.0
(`npm install -g open-kioku@2.4.0`).

This directory is kept so the name stays reserved and this explanation has somewhere to live.
Adding a `package.json` here would not be enough to publish it: `release-metadata.json`, the
wrapper's `optionalDependencies`, the `build` matrix in `.github/workflows/release.yml`, the
`x86_64-apple-darwin` prohibition in `scripts/validate-release-metadata.py`, and the
`depends_on arch: :arm64` line in `Formula/open-kioku.rb` would all have to change together.

Repository: https://github.com/shivyadavus/open-kioku
