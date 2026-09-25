//! Rust `use` paths followed through the module tree of the importing file's own crate.
//!
//! Only paths the file layout can answer are mapped: `crate::` names the crate root, and
//! `self::`/`super::` name modules relative to the importer. The crate root is `src/` under the
//! caller-supplied package directory (the nearest `Cargo.toml`); an importer outside it (`tests/`,
//! `examples/`, `build.rs`) or under `src/bin/` is not mapped, and neither is a path that starts
//! with an extern crate or a 2015-edition bare module name. The importer's module comes from its
//! file path alone, so `self::` and `super::` are wrong for a `use` nested in an inline `mod`
//! block, and a mapped path is only a candidate until the caller checks the `mod` declarations.

use open_kioku_semantic_model::ProjectRoot;
use std::path::Path;

/// Where a Rust package keeps the module tree of its library and default binary crates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustPackageLayout {
    /// Repository-relative directory holding the module tree (`src`, `crates/app/src`).
    pub(crate) src_root: String,
    /// The library crate root's file name in `src_root`, without `.rs`: `lib`, or the file that
    /// `[lib] path` names there. `None` when `[lib] path` puts the root outside `src_root`, whose
    /// modules this layout cannot follow.
    library: Option<String>,
}

impl RustPackageLayout {
    /// The layout of the package at `root`, or of a top-level `src/` when no manifest is above.
    pub(crate) fn of(root: Option<&ProjectRoot>) -> Self {
        match root {
            Some(root) => Self::new(&root.path, root.library_root.as_deref()),
            None => Self::new(Path::new(""), None),
        }
    }

    /// The package at `crate_dir`, whose manifest may set its library root with `[lib] path`.
    pub(crate) fn new(crate_dir: &Path, library_root: Option<&Path>) -> Self {
        let src_root = src_root_of(crate_dir);
        let library = match library_root {
            None => Some("lib".to_string()),
            Some(file) => {
                let file = file.to_string_lossy().replace('\\', "/");
                file.strip_prefix(src_root.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    .and_then(|rest| rest.strip_suffix(".rs"))
                    .filter(|name| !name.is_empty() && !name.contains('/') && *name != "main")
                    .map(str::to_string)
            }
        };
        Self { src_root, library }
    }

    /// Whether the library crate root, when the package has one, is in the module tree.
    pub(crate) fn places_library(&self) -> bool {
        self.library.is_some()
    }

    /// Extension-less path of the file holding `root`, when this layout can place it.
    pub(crate) fn root_stem(&self, root: RustCrateRoot) -> Option<String> {
        let name = match root {
            RustCrateRoot::Library => self.library.as_deref()?,
            RustCrateRoot::Binary => "main",
        };
        Some(format!("{}/{name}", self.src_root))
    }

    /// Extension-less paths of the crate root files this layout places.
    pub(crate) fn root_stems(&self) -> Vec<String> {
        [RustCrateRoot::Library, RustCrateRoot::Binary]
            .into_iter()
            .filter_map(|root| self.root_stem(root))
            .collect()
    }
}

/// A crate root file of a package's module tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RustCrateRoot {
    /// `lib.rs`, or the file `[lib] path` names.
    Library,
    /// `main.rs`.
    Binary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustUsePath {
    /// The package whose module tree holds the importer.
    pub(crate) package: RustPackageLayout,
    /// Path below the crate root with the `crate`/`self`/`super` prefix applied. A glob import
    /// keeps its trailing `*`, and a raw identifier keeps its `r#`.
    pub(crate) segments: Vec<String>,
    /// The importing file's module as its path implies (`["auth", "keys"]` for `auth/keys.rs`).
    pub(crate) importer_module: Vec<String>,
    /// The path is `self::`/`super::`, so it is only as sound as `importer_module`, which a
    /// `#[path]` declaration or a missing `mod` declaration makes wrong.
    pub(crate) relative: bool,
    /// The crate root the importer is, when it is that crate root file itself.
    pub(crate) importer_root: Option<RustCrateRoot>,
}

impl RustUsePath {
    /// Extension-less paths of the files that can hold `module`: `<dir>.rs` and `<dir>/mod.rs`,
    /// or the crate root files for the crate root.
    pub(crate) fn module_file_stems(&self, module: &[String]) -> Vec<String> {
        if module.is_empty() {
            self.package.root_stems()
        } else {
            let dir = format!(
                "{}/{}",
                self.package.src_root,
                module
                    .iter()
                    .map(|segment| module_name(segment))
                    .collect::<Vec<_>>()
                    .join("/")
            );
            vec![format!("{dir}/mod"), dir]
        }
    }
}

/// The name a module path segment or `mod` declaration gives a module's file: `r#type` is the
/// module `type`, in `type.rs`.
pub(crate) fn module_name(segment: &str) -> &str {
    segment.strip_prefix("r#").unwrap_or(segment)
}

/// Maps `use_path` from `importer`, both repository-relative, in `package`.
pub(crate) fn map_rust_use_path(
    package: &RustPackageLayout,
    importer: &Path,
    use_path: &str,
) -> Option<RustUsePath> {
    let file = map_rust_module_file(package, importer)?;
    let mut importer_module = file
        .importer_module
        .iter()
        .map(String::as_str)
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
        segments: segments.into_iter().map(str::to_string).collect(),
        relative: first != "crate",
        ..file
    })
}

/// The module that `file`, repository-relative, holds in `package` as its path implies, as a
/// path with no segments: `src/auth/keys.rs` is `["auth", "keys"]` and `src/lib.rs` the crate
/// root. `None` outside `src/` and under `src/bin/`, as for [`map_rust_use_path`].
pub(crate) fn map_rust_module_file(
    package: &RustPackageLayout,
    file: &Path,
) -> Option<RustUsePath> {
    let file = file.to_string_lossy().replace('\\', "/");
    let module_file = file
        .strip_prefix(package.src_root.as_str())?
        .strip_prefix('/')?;
    let mut module = module_file
        .strip_suffix(".rs")?
        .split('/')
        .collect::<Vec<_>>();
    // Each file under `src/bin/` is the root of its own binary crate.
    if matches!(module.as_slice(), ["bin", _, ..]) {
        return None;
    }
    let importer_root = match module.as_slice() {
        [name] if package.library.as_deref() == Some(*name) => Some(RustCrateRoot::Library),
        ["main"] => Some(RustCrateRoot::Binary),
        _ => None,
    };
    if importer_root.is_some() {
        module.clear();
    } else if module.last() == Some(&"mod") {
        module.pop();
    }
    Some(RustUsePath {
        package: package.clone(),
        segments: Vec::new(),
        importer_module: module.into_iter().map(str::to_string).collect(),
        relative: false,
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
    package: &RustPackageLayout,
    package_name: &str,
    use_path: &str,
) -> Option<RustUsePath> {
    // A library root outside the module tree has modules this layout cannot follow.
    package.library.as_ref()?;
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
        package: package.clone(),
        segments: rest.into_iter().map(str::to_string).collect(),
        importer_module: Vec::new(),
        relative: false,
        // A crate name names the library crate, whichever file writes the path.
        importer_root: Some(RustCrateRoot::Library),
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
        map_rust_use_path(&layout(crate_dir), Path::new(importer), use_path)
            .map(|path| (path.package.src_root, path.segments))
    }

    fn layout(crate_dir: &str) -> RustPackageLayout {
        RustPackageLayout::new(Path::new(crate_dir), None)
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
            &layout(""),
            Path::new("src/auth/keys.rs"),
            "super::issue_token",
        )
        .expect("super path maps");
        assert!(relative.relative);
        assert_eq!(relative.importer_module, vec!["auth", "keys"]);

        let absolute = map_rust_use_path(
            &layout(""),
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
            map_rust_crate_name_path(&layout(""), "demo-crate", "demo_crate::auth::issue_token")
                .expect("a package's own crate name maps");
        assert_eq!(path.package.src_root, "src");
        assert_eq!(path.segments, vec!["auth", "issue_token"]);
        assert_eq!(path.importer_root, Some(RustCrateRoot::Library));
        assert!(!path.relative);
        assert!(path.importer_module.is_empty());

        let member = map_rust_crate_name_path(&layout("crates/app"), "app", "app::auth::*")
            .expect("a workspace member's crate name maps");
        assert_eq!(member.package.src_root, "crates/app/src");
        assert_eq!(member.segments, vec!["auth", "*"]);
    }

    #[test]
    fn crate_name_paths_of_another_package_or_without_a_tail_are_not_mapped() {
        for path in ["other_crate::auth", "demo_crate", "demo_crate::*::x"] {
            assert_eq!(
                map_rust_crate_name_path(&layout(""), "demo-crate", path),
                None,
                "`{path}`"
            );
        }
    }

    #[test]
    fn a_library_root_set_in_src_is_the_crate_root_and_one_elsewhere_is_not_placed() {
        let moved = RustPackageLayout::new(
            Path::new("crates/app"),
            Some(Path::new("crates/app/src/app_lib.rs")),
        );
        assert_eq!(
            moved.root_stems(),
            vec!["crates/app/src/app_lib", "crates/app/src/main"]
        );
        let root = map_rust_module_file(&moved, Path::new("crates/app/src/app_lib.rs"))
            .expect("the library root maps");
        assert_eq!(root.importer_root, Some(RustCrateRoot::Library));
        // With the root moved, a `lib.rs` beside it is an ordinary module file.
        let lib = map_rust_module_file(&moved, Path::new("crates/app/src/lib.rs"))
            .expect("lib.rs maps as a module");
        assert_eq!(lib.importer_root, None);
        assert_eq!(lib.importer_module, vec!["lib"]);

        for outside in ["crates/app/lib.rs", "crates/app/src/nested/lib.rs"] {
            let layout = RustPackageLayout::new(Path::new("crates/app"), Some(Path::new(outside)));
            assert_eq!(
                layout.root_stems(),
                vec!["crates/app/src/main"],
                "{outside}"
            );
            assert_eq!(
                map_rust_crate_name_path(&layout, "app", "app::auth::issue_token"),
                None,
                "{outside}"
            );
        }
    }

    #[test]
    fn raw_identifier_segments_name_the_module_file_without_the_prefix() {
        let path = map_rust_use_path(&layout(""), Path::new("src/lib.rs"), "crate::r#type::ty")
            .expect("a raw identifier is a path identifier");
        assert_eq!(path.segments, vec!["r#type", "ty"]);
        assert_eq!(
            path.module_file_stems(&path.segments[..1]),
            vec!["src/type/mod", "src/type"]
        );
    }
}
