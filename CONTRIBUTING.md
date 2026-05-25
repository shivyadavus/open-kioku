# Contributing to Open Code Factory

Thank you for your interest in contributing to Open Code Factory (OCF)!

## Development Environment
- Rust toolchain (1.77+)
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
The workspace is composed of many focused crates (e.g., `open-kioku-core`, `open-kioku-storage-sqlite`, `open-kioku-mcp`). When adding a new feature, try to place it in the most specific crate or create a new crate if it represents a distinct architectural component.

## Pull Requests
- Keep PRs focused on a single logical change.
- Ensure the CI workflow passes on your branch.
- Add tests for new functionality or bug fixes.
