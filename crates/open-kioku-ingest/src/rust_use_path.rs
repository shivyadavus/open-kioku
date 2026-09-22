//! Rust `use` paths followed through the module tree of the importing file's own crate.
//!
//! Only paths the file layout can answer are mapped: `crate::` names the crate root, and
//! `self::`/`super::` name modules relative to the importer. The crate root is `src/` under the
//! caller-supplied package directory (the nearest `Cargo.toml`); an importer outside it (`tests/`,
//! `examples/`, `build.rs`) or under `src/bin/` is not mapped, and neither is a path that starts
//! with an extern crate or a 2015-edition bare module name. The importer's module comes from its
//! file path alone, so `self::` and `super::` are wrong for a `use` nested in an inline `mod`
//! block, and a mapped path is only a candidate until the caller checks the `mod` declarations.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustUsePath {
    /// Repository-relative directory holding the crate's module tree (`src`, `crates/app/src`).
    pub(crate) src_root: String,
    /// Path below the crate root with the `crate`/`self`/`super` prefix applied. A glob import
    /// keeps its trailing `*`.
    pub(crate) segments: Vec<String>,
    /// The importing file's module as its path implies (`["auth", "keys"]` for `auth/keys.rs`).
    pub(crate) importer_module: Vec<String>,
    /// The path is `self::`/`super::`, so it is only as sound as `importer_module`, which a
    /// `#[path]` declaration or a missing `mod` declaration makes wrong.
    pub(crate) relative: bool,
    /// `lib` or `main` when the importer is that crate root file itself.
    pub(crate) importer_root: Option<&'static str>,
}

impl RustUsePath {
    /// Extension-less paths of the files that can hold `module`: `<dir>.rs` and `<dir>/mod.rs`,
    /// or `lib.rs` and `main.rs` for the crate root.
    pub(crate) fn module_file_stems(&self, module: &[String]) -> [String; 2] {
        if module.is_empty() {
            [
                format!("{}/lib", self.src_root),
                format!("{}/main", self.src_root),
            ]
        } else {
            let dir = format!("{}/{}", self.src_root, module.join("/"));
            [format!("{dir}/mod"), dir]
        }
    }
}

/// Maps `use_path` from `importer`, both repository-relative, in the package at `crate_dir`.
pub(crate) fn map_rust_use_path(
    crate_dir: &Path,
    importer: &Path,
    use_path: &str,
) -> Option<RustUsePath> {
    let src_root = src_root_of(crate_dir);
    let importer = importer.to_string_lossy().replace('\\', "/");
    let module_file = importer
        .strip_prefix(src_root.as_str())?
        .strip_prefix('/')?;
    let mut importer_module = module_file
        .strip_suffix(".rs")?
        .split('/')
        .collect::<Vec<_>>();
    // Each file under `src/bin/` is the root of its own binary crate.
    if matches!(importer_module.as_slice(), ["bin", _, ..]) {
        return None;
    }
    let importer_root = match importer_module.as_slice() {
        ["lib"] => Some("lib"),
        ["main"] => Some("main"),
        _ => None,
    };
    if importer_root.is_some() {
        importer_module.clear();
    } else if importer_module.last() == Some(&"mod") {
        importer_module.pop();
    }

    let importer_module_names = importer_module
        .iter()
        .map(|segment| segment.to_string())
        .collect::<Vec<_>>();
    let mut parts = use_path.split("::");
    let first = parts.next()?;
    let mut segments = match first {
        "crate" => Vec::new(),
        "self" => importer_module,
        "super" => {
            importer_module.pop()?;
            importer_module
        }
        _ => return None,
    };
    let mut rest = parts.peekable();
    while rest.peek() == Some(&"super") {
        if first == "crate" {
            return None;
        }
        rest.next();
        segments.pop()?;
    }
    let rest = rest.collect::<Vec<_>>();
    let last = rest.len().checked_sub(1)?;
    for (position, part) in rest.iter().enumerate() {
        if !(is_path_identifier(part) || (*part == "*" && position == last)) {
            return None;
        }
    }
    segments.extend(rest);
    Some(RustUsePath {
        src_root,
        segments: segments.into_iter().map(str::to_string).collect(),
        importer_module: importer_module_names,
        relative: first != "crate",
        importer_root,
    })
}

/// Maps `use_path` naming the importing file's own package by its crate name
/// (`demo_crate::auth::issue_token`), which is absolute in that package's library crate.
///
/// Unlike `crate::`, a crate-name path is legal from a file outside the module tree — an
/// integration test under `tests/`, an example, a binary — so the importer's own path says nothing
/// and the path is followed from `lib.rs` alone.
pub(crate) fn map_rust_crate_name_path(
    crate_dir: &Path,
    package_name: &str,
    use_path: &str,
) -> Option<RustUsePath> {
    let mut parts = use_path.split("::");
    if parts.next()? != package_name.replace('-', "_") {
        return None;
    }
    let rest = parts.collect::<Vec<_>>();
    let last = rest.len().checked_sub(1)?;
    for (position, part) in rest.iter().enumerate() {
        if !(is_path_identifier(part) || (*part == "*" && position == last)) {
            return None;
        }
    }
    Some(RustUsePath {
        src_root: src_root_of(crate_dir),
        segments: rest.into_iter().map(str::to_string).collect(),
        importer_module: Vec::new(),
        relative: false,
        // A crate name names the library crate, whichever file writes the path.
        importer_root: Some("lib"),
    })
}

/// The directory holding the module tree of the package at `crate_dir`.
fn src_root_of(crate_dir: &Path) -> String {
    let crate_dir = crate_dir.to_string_lossy().replace('\\', "/");
    match crate_dir.trim_end_matches('/') {
        "" => "src".to_string(),
        dir => format!("{dir}/src"),
    }
}

fn is_path_identifier(part: &str) -> bool {
    let name = part.strip_prefix("r#").unwrap_or(part);
    !name.is_empty()
        && !matches!(part, "crate" | "self" | "super")
        && name.chars().all(|ch| ch == '_' || ch.is_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapped(crate_dir: &str, importer: &str, use_path: &str) -> Option<(String, Vec<String>)> {
        map_rust_use_path(Path::new(crate_dir), Path::new(importer), use_path)
            .map(|path| (path.src_root, path.segments))
    }

    fn segments(root: &str, parts: &[&str]) -> Option<(String, Vec<String>)> {
        Some((
            root.to_string(),
            parts.iter().map(|part| part.to_string()).collect(),
        ))
    }

    #[test]
    fn mapped_paths_record_the_importer_module_and_whether_they_are_relative() {
        let relative = map_rust_use_path(
            Path::new(""),
            Path::new("src/auth/keys.rs"),
            "super::issue_token",
        )
        .expect("super path maps");
        assert!(relative.relative);
        assert_eq!(relative.importer_module, vec!["auth", "keys"]);

        let absolute = map_rust_use_path(
            Path::new(""),
            Path::new("src/auth/mod.rs"),
            "crate::session::open",
        )
        .expect("crate path maps");
        assert!(!absolute.relative);
        assert_eq!(absolute.importer_module, vec!["auth"]);
    }

    #[test]
    fn crate_paths_map_from_the_packages_src_root() {
        assert_eq!(
            mapped("", "src/session.rs", "crate::auth::issue_token"),
            segments("src", &["auth", "issue_token"])
        );
        assert_eq!(
            mapped(
                "crates/app",
                "crates/app/src/api/session.rs",
                "crate::auth::*"
            ),
            segments("crates/app/src", &["auth", "*"])
        );
    }

    #[test]
    fn self_and_super_paths_map_from_the_importing_module() {
        assert_eq!(
            mapped("", "src/lib.rs", "self::auth::issue_token"),
            segments("src", &["auth", "issue_token"])
        );
        assert_eq!(
            mapped("", "src/auth/mod.rs", "self::keys::rotate"),
            segments("src", &["auth", "keys", "rotate"])
        );
        assert_eq!(
            mapped("", "src/auth/keys.rs", "super::issue_token"),
            segments("src", &["auth", "issue_token"])
        );
        assert_eq!(
            mapped("", "src/auth/keys.rs", "super::super::session::open"),
            segments("src", &["session", "open"])
        );
    }

    #[test]
    fn paths_outside_the_packages_module_tree_are_not_mapped() {
        assert_eq!(mapped("", "src/lib.rs", "super::helper"), None);
        assert_eq!(mapped("", "src/session.rs", "std::fmt::Display"), None);
        assert_eq!(
            mapped("", "src/session.rs", "other_crate::auth::issue_token"),
            None
        );
        assert_eq!(mapped("", "src/session.rs", "crate::super::auth"), None);
        assert_eq!(mapped("", "src/session.rs", "crate::auth::*::x"), None);
        assert_eq!(mapped("", "src/session.rs", "crate"), None);
        assert_eq!(
            mapped("", "src/bin/tool.rs", "crate::auth::issue_token"),
            None
        );
        assert_eq!(
            mapped("", "tests/flow.rs", "crate::auth::issue_token"),
            None
        );
        // An integration test of a package nested under the root `src/` is its own crate.
        assert_eq!(
            mapped(
                "src/tools/xtask",
                "src/tools/xtask/tests/common.rs",
                "crate::helper"
            ),
            None
        );
    }

    #[test]
    fn crate_name_paths_map_from_the_packages_library_root() {
        let path =
            map_rust_crate_name_path(Path::new(""), "demo-crate", "demo_crate::auth::issue_token")
                .expect("a package's own crate name maps");
        assert_eq!(path.src_root, "src");
        assert_eq!(path.segments, vec!["auth", "issue_token"]);
        assert_eq!(path.importer_root, Some("lib"));
        assert!(!path.relative);
        assert!(path.importer_module.is_empty());

        let member = map_rust_crate_name_path(Path::new("crates/app"), "app", "app::auth::*")
            .expect("a workspace member's crate name maps");
        assert_eq!(member.src_root, "crates/app/src");
        assert_eq!(member.segments, vec!["auth", "*"]);
    }

    #[test]
    fn crate_name_paths_of_another_package_or_without_a_tail_are_not_mapped() {
        for path in ["other_crate::auth", "demo_crate", "demo_crate::*::x"] {
            assert_eq!(
                map_rust_crate_name_path(Path::new(""), "demo-crate", path),
                None,
                "`{path}`"
            );
        }
    }
}
