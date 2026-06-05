# Contributing to Open Kioku

Thank you for your interest in contributing to Open Kioku!

## Start Here

1. Read the [crate map](docs/crate-map.md) to understand the workspace architecture.
2. Browse [good first issues](https://github.com/shivyadavus/open-kioku/labels/good%20first%20issue) for contributor-friendly tasks.
3. Read the [contributor guide](docs/contributor-guide.md) for fixture conventions, signal checklist, and smoke expectations.
4. Check the [stability policy](STABILITY.md) to understand which APIs are stable.

## Development Environment

- Rust toolchain (1.78+)
- Cargo (for building and testing)

## Testing

Please ensure all tests pass before submitting a pull request:

```bash
cargo test --all
```

## Linting and Formatting

We enforce strict linting and formatting rules:

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
```

## Architecture

The workspace is composed of 43 focused crates (e.g., `open-kioku-core`, `open-kioku-storage-sqlite`, `open-kioku-mcp`). When adding a new feature, place it in the most specific crate or create a new crate if it represents a distinct architectural component.

Use [`docs/contributor-guide.md`](docs/contributor-guide.md) for the current architecture map, crate selection guide, fixture conventions, benchmark authoring guide, signal checklist, label taxonomy, and smoke-test expectations.

## Pull Requests

- Keep PRs focused on a single logical change.
- Ensure the CI workflow passes on your branch.
- Add tests for new functionality or bug fixes.
- Follow the existing module structure: each crate has a single, well-defined responsibility.
