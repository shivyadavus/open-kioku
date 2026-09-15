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
    let crate_dir = crate_dir.to_string_lossy().replace('\\', "/");
    let src_root = match crate_dir.trim_end_matches('/') {
        "" => "src".to_string(),
        dir => format!("{dir}/src"),
    };
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
    if matches!(importer_module.as_slice(), ["lib"] | ["main"]) {
        importer_module.clear();
    } else if importer_module.last() == Some(&"mod") {
        importer_module.pop();
    }

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
    })
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
}
