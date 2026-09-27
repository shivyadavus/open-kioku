//! How the index stores the Cargo package model: analysis facts on each indexed `Cargo.toml`,
//! written by ingest from the manifests it parsed and read back at query time, so a consumer
//! answers from the model the import resolver used instead of parsing manifests itself.
//!
//! Every fact's `source` starts with [`CARGO_MANIFEST_SOURCE_PREFIX`] and its confidence is exact.
//! A manifest that did not parse states nothing.

/// Prefix of every Cargo manifest fact's `source`.
pub const CARGO_MANIFEST_SOURCE_PREFIX: &str = "open-kioku-cargo-manifest/";
/// The manifest declares a package: a `DEFINES` fact whose target is a package label.
pub const CARGO_PACKAGE_SOURCE: &str = "open-kioku-cargo-manifest/package";
/// The manifest declares a procedural-macro package (`[lib] proc-macro = true`).
pub const CARGO_PROC_MACRO_PACKAGE_SOURCE: &str = "open-kioku-cargo-manifest/proc-macro-package";
/// The root file of the package's library crate, `src/lib.rs` unless `[lib] path` moves it: a
/// `DEFINES` fact whose target is that file's repository-relative path, indexed or not.
pub const CARGO_LIBRARY_ROOT_SOURCE: &str = "open-kioku-cargo-manifest/library-root";
/// The manifest declares a workspace and no package: a `DEFINES` fact whose target is a package
/// label with the crate name `workspace`.
pub const CARGO_VIRTUAL_WORKSPACE_SOURCE: &str = "open-kioku-cargo-manifest/virtual-workspace";
/// A workspace `members` entry: a `CONTAINS` fact whose target is the member's repository-relative
/// path prefix, cut at its first wildcard, and ending in `/` unless a glob cut it mid-name.
pub const CARGO_WORKSPACE_MEMBER_SOURCE: &str = "open-kioku-cargo-manifest/workspace-member";
/// A `[dependencies]` entry on a package of the repository: a `DEPENDS_ON` fact whose target is
/// the dependency's repository-relative `Cargo.toml` path.
pub const CARGO_DEPENDENCY_SOURCE: &str = "open-kioku-cargo-manifest/dependency";
/// A `[dev-dependencies]` entry on a package of the repository.
pub const CARGO_DEV_DEPENDENCY_SOURCE: &str = "open-kioku-cargo-manifest/dev-dependency";
/// A `[build-dependencies]` entry on a package of the repository.
pub const CARGO_BUILD_DEPENDENCY_SOURCE: &str = "open-kioku-cargo-manifest/build-dependency";

/// The label of the package whose manifest is in `dir`: `<crate>@<dir>`, where `crate` is the
/// name other crates write for its library and `dir` is repository-relative, `.` for the root.
pub fn cargo_package_label(crate_name: &str, dir: &str) -> String {
    let dir = if dir.is_empty() { "." } else { dir };
    format!("{crate_name}@{dir}")
}

/// The crate name and repository-relative directory (`""` for the root) a package label names.
pub fn parse_cargo_package_label(label: &str) -> Option<(&str, &str)> {
    let (crate_name, dir) = label.split_once('@')?;
    Some((crate_name, if dir == "." { "" } else { dir }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_labels_round_trip_with_the_repository_root_spelled_dot() {
        assert_eq!(
            cargo_package_label("engine", "crates/engine"),
            "engine@crates/engine"
        );
        assert_eq!(cargo_package_label("workspace", ""), "workspace@.");
        assert_eq!(
            parse_cargo_package_label("engine@crates/a@b"),
            Some(("engine", "crates/a@b"))
        );
        assert_eq!(
            parse_cargo_package_label("workspace@."),
            Some(("workspace", ""))
        );
        assert_eq!(parse_cargo_package_label("engine"), None);
    }
}
