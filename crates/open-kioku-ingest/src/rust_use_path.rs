//! Rust `use` paths followed through the module tree of the importing file's own crate.
//!
//! Only paths the file layout can answer are mapped: `crate::` names the crate root, and
//! `self::`/`super::` name modules relative to the importer. A package (the nearest `Cargo.toml`)
//! holds several crate module trees: its library and default binary in `src/`, and the binaries,
//! integration tests, examples and benches Cargo discovers under `src/bin/`, `tests/`,
//! `examples/` and `benches/`. A crate root's modules live in the root file's own directory. An
//! importer outside every tree (`build.rs`) is not mapped, and neither is a path that starts with
//! an extern crate or a 2015-edition bare module name. The importer's module comes from its file
//! path alone, so `self::` and `super::` are wrong for a `use` nested in an inline `mod` block, and
//! a mapped path is only a candidate until the caller checks the `mod` declarations.

use open_kioku_semantic_model::ProjectRoot;
use std::collections::HashMap;
use std::path::Path;

/// Where a Rust package keeps the module trees of its crates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustPackageLayout {
    /// Repository-relative directory of the package (`""`, `crates/app`).
    package_dir: String,
    /// Repository-relative directory holding the library's module tree (`src`, `crates/app/src`).
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
        let package_dir = crate_dir
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_string();
        let src_root = join_dir(&package_dir, "src");
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
        Self {
            package_dir,
            src_root,
            library,
        }
    }

    /// Whether the library crate root, when the package has one, is in the module tree.
    pub(crate) fn places_library(&self) -> bool {
        self.library.is_some()
    }

    fn library_stem(&self) -> Option<String> {
        Some(format!("{}/{}", self.src_root, self.library.as_deref()?))
    }

    /// The module tree of the library and the default binary, `src/lib.rs` and `src/main.rs`.
    pub(crate) fn main_tree(&self) -> RustCrateTree {
        RustCrateTree {
            module_dir: self.src_root.clone(),
            roots: self
                .library_stem()
                .into_iter()
                .chain([format!("{}/main", self.src_root)])
                .collect(),
        }
    }

    /// [`RustPackageLayout::crate_tree`] for a repository-relative Rust file path.
    pub(crate) fn crate_tree_of(
        &self,
        file: &Path,
        stems_in_dir: &HashMap<String, Vec<String>>,
    ) -> Option<RustCrateTree> {
        let file = file.to_string_lossy().replace('\\', "/");
        self.crate_tree(file.strip_suffix(".rs")?, stems_in_dir)
    }

    /// The crate module tree holding `file`, repository-relative with `/` separators, given the
    /// extension-less paths of the indexed Rust files in each directory. Under a target directory
    /// (`src/bin/`, `tests/`, `examples/`, `benches/`) each file directly in it is a crate root of
    /// its own, whose modules live in that directory, and `<dir>/<name>/main.rs` is one whose
    /// modules live in `<dir>/<name>/`. `None` for a file outside every tree, such as `build.rs`.
    pub(crate) fn crate_tree(
        &self,
        file: &str,
        stems_in_dir: &HashMap<String, Vec<String>>,
    ) -> Option<RustCrateTree> {
        let target_dirs = [
            join_dir(&self.src_root, "bin"),
            join_dir(&self.package_dir, "tests"),
            join_dir(&self.package_dir, "examples"),
            join_dir(&self.package_dir, "benches"),
        ];
        for dir in target_dirs {
            let Some(rest) = strip_dir(file, &dir) else {
                continue;
            };
            if let Some((name, _)) = rest.split_once('/') {
                let own = format!("{dir}/{name}");
                let main = format!("{own}/main");
                if stems_in_dir
                    .get(&own)
                    .is_some_and(|stems| stems.contains(&main))
                {
                    return Some(RustCrateTree {
                        module_dir: own,
                        roots: vec![main],
                    });
                }
            }
            let roots = stems_in_dir.get(&dir).cloned().unwrap_or_default();
            return Some(RustCrateTree {
                module_dir: dir,
                roots,
            });
        }
        strip_dir(file, &self.src_root)?;
        Some(self.main_tree())
    }
}

/// The module tree of one or more crates: the directory their modules live in and the crate root
/// files in it whose modules do. The binaries directly in `src/bin/` share one tree, as do the
/// integration tests directly in `tests/`; which of them a module belongs to is whichever declares
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustCrateTree {
    /// Repository-relative directory holding the modules (`src`, `crates/app/src/bin/tool`).
    pub(crate) module_dir: String,
    /// Extension-less paths of the crate root files, indexed or not.
    pub(crate) roots: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RustUsePath {
    /// The crate module tree that holds the importer.
    pub(crate) tree: RustCrateTree,
    /// Path below the crate root with the `crate`/`self`/`super` prefix applied. A glob import
    /// keeps its trailing `*`, and a raw identifier keeps its `r#`.
    pub(crate) segments: Vec<String>,
    /// The importing file's module as its path implies (`["auth", "keys"]` for `auth/keys.rs`).
    pub(crate) importer_module: Vec<String>,
    /// The path is `self::`/`super::`, so it is only as sound as `importer_module`, which a
    /// `#[path]` declaration or a missing `mod` declaration makes wrong.
    pub(crate) relative: bool,
    /// Extension-less path of the crate root the importer is, when it is that root file itself.
    pub(crate) importer_root: Option<String>,
}

impl RustUsePath {
    /// Extension-less paths of the files that can hold `module`: `<dir>.rs` and `<dir>/mod.rs`,
    /// or the crate root files for the crate root.
    pub(crate) fn module_file_stems(&self, module: &[String]) -> Vec<String> {
        if module.is_empty() {
            self.tree.roots.clone()
        } else {
            let dir = format!(
                "{}/{}",
                self.tree.module_dir,
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

/// Maps `use_path` from `importer`, both repository-relative, in `tree`.
pub(crate) fn map_rust_use_path(
    tree: &RustCrateTree,
    importer: &Path,
    use_path: &str,
) -> Option<RustUsePath> {
    let file = map_rust_module_file(tree, importer)?;
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

/// The module that `file`, repository-relative, holds in `tree` as its path implies, as a path
/// with no segments: `src/auth/keys.rs` is `["auth", "keys"]` and a crate root file the root.
/// `None` for a file outside `tree`'s directory.
pub(crate) fn map_rust_module_file(tree: &RustCrateTree, file: &Path) -> Option<RustUsePath> {
    let file = file.to_string_lossy().replace('\\', "/");
    let stem = file.strip_suffix(".rs")?;
    let module_file = strip_dir(stem, &tree.module_dir)?;
    let (importer_root, module) = if tree.roots.iter().any(|root| root == stem) {
        (Some(stem.to_string()), Vec::new())
    } else {
        let mut module = module_file.split('/').collect::<Vec<_>>();
        if module.last() == Some(&"mod") {
            module.pop();
        }
        (None, module)
    };
    Some(RustUsePath {
        tree: tree.clone(),
        segments: Vec::new(),
        importer_module: module.into_iter().map(str::to_string).collect(),
        relative: false,
        importer_root,
    })
}

/// `path` below `dir`, both repository-relative; any path is below the repository root `""`.
fn strip_dir<'p>(path: &'p str, dir: &str) -> Option<&'p str> {
    if dir.is_empty() {
        return Some(path);
    }
    path.strip_prefix(dir)?.strip_prefix('/')
}

fn join_dir(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
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
    let library = package.library_stem()?;
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
        tree: package.main_tree(),
        segments: rest.into_iter().map(str::to_string).collect(),
        importer_module: Vec::new(),
        relative: false,
        // A crate name names the library crate, whichever file writes the path.
        importer_root: Some(library),
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

    /// `use_path` from `importer` in the package at `crate_dir`, where the indexed Rust files are
    /// `importer` and the package's default crate roots.
    fn mapped(crate_dir: &str, importer: &str, use_path: &str) -> Option<(String, Vec<String>)> {
        let tree = layout(crate_dir).crate_tree(importer.strip_suffix(".rs")?, &HashMap::new())?;
        map_rust_use_path(&tree, Path::new(importer), use_path)
            .map(|path| (path.tree.module_dir, path.segments))
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
            &layout("").main_tree(),
            Path::new("src/auth/keys.rs"),
            "super::issue_token",
        )
        .expect("super path maps");
        assert!(relative.relative);
        assert_eq!(relative.importer_module, vec!["auth", "keys"]);

        let absolute = map_rust_use_path(
            &layout("").main_tree(),
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
        assert_eq!(mapped("", "build.rs", "crate::auth::issue_token"), None);
    }

    fn stems_in_dir(stems: &[&str]) -> HashMap<String, Vec<String>> {
        let mut by_dir = HashMap::<String, Vec<String>>::new();
        for stem in stems {
            let dir = stem.rsplit_once('/').map_or("", |(dir, _)| dir);
            by_dir
                .entry(dir.to_string())
                .or_default()
                .push(stem.to_string());
        }
        by_dir
    }

    #[test]
    fn target_directory_files_are_crate_roots_whose_modules_live_beside_them() {
        let indexed = stems_in_dir(&[
            "src/lib",
            "src/bin/tool",
            "src/bin/other",
            "src/bin/multi/main",
            "src/bin/multi/inner",
            "src/bin/helpers/mod",
            "tests/it",
            "tests/common/mod",
            "examples/demo",
        ]);
        let tree = |file: &str| layout("").crate_tree(file, &indexed);

        // `src/bin/tool.rs` is a root whose `mod helpers;` is `src/bin/helpers/mod.rs`, a tree
        // it shares with the other binaries directly in `src/bin/`.
        let bin = tree("src/bin/tool").expect("a binary is a crate root");
        assert_eq!(bin.module_dir, "src/bin");
        assert_eq!(bin.roots, vec!["src/bin/tool", "src/bin/other"]);
        assert_eq!(tree("src/bin/helpers/mod"), Some(bin.clone()));
        let helpers = map_rust_module_file(&bin, Path::new("src/bin/helpers/mod.rs"))
            .expect("a module of the binaries maps");
        assert_eq!(helpers.importer_module, vec!["helpers"]);
        assert_eq!(
            map_rust_module_file(&bin, Path::new("src/bin/tool.rs"))
                .expect("the binary maps")
                .importer_root
                .as_deref(),
            Some("src/bin/tool")
        );

        // `src/bin/multi/main.rs` keeps its modules in `src/bin/multi/`.
        let multi = tree("src/bin/multi/inner").expect("a module of a binary directory");
        assert_eq!(multi.module_dir, "src/bin/multi");
        assert_eq!(multi.roots, vec!["src/bin/multi/main"]);
        assert_eq!(
            map_rust_use_path(&multi, Path::new("src/bin/multi/inner.rs"), "super::run")
                .expect("super maps to the binary root")
                .segments,
            vec!["run"]
        );

        let tests = tree("tests/common/mod").expect("a module of the integration tests");
        assert_eq!(tests.module_dir, "tests");
        assert_eq!(tests.roots, vec!["tests/it"]);
        assert_eq!(
            tree("examples/demo").map(|tree| tree.module_dir).as_deref(),
            Some("examples")
        );
        assert_eq!(
            tree("src/auth").map(|tree| tree.module_dir).as_deref(),
            Some("src")
        );
        assert_eq!(tree("build"), None);

        // A package nested under the root `src/` has its own integration tests.
        let nested = RustPackageLayout::new(Path::new("src/tools/xtask"), None)
            .crate_tree("src/tools/xtask/tests/common", &HashMap::new())
            .expect("the nested package's test tree");
        assert_eq!(nested.module_dir, "src/tools/xtask/tests");
    }

    #[test]
    fn crate_name_paths_map_from_the_packages_library_root() {
        let path =
            map_rust_crate_name_path(&layout(""), "demo-crate", "demo_crate::auth::issue_token")
                .expect("a package's own crate name maps");
        assert_eq!(path.tree.module_dir, "src");
        assert_eq!(path.segments, vec!["auth", "issue_token"]);
        assert_eq!(path.importer_root.as_deref(), Some("src/lib"));
        assert!(!path.relative);
        assert!(path.importer_module.is_empty());

        let member = map_rust_crate_name_path(&layout("crates/app"), "app", "app::auth::*")
            .expect("a workspace member's crate name maps");
        assert_eq!(member.tree.module_dir, "crates/app/src");
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
        let moved = moved.main_tree();
        assert_eq!(
            moved.roots,
            vec!["crates/app/src/app_lib", "crates/app/src/main"]
        );
        let root = map_rust_module_file(&moved, Path::new("crates/app/src/app_lib.rs"))
            .expect("the library root maps");
        assert_eq!(
            root.importer_root.as_deref(),
            Some("crates/app/src/app_lib")
        );
        // With the root moved, a `lib.rs` beside it is an ordinary module file.
        let lib = map_rust_module_file(&moved, Path::new("crates/app/src/lib.rs"))
            .expect("lib.rs maps as a module");
        assert_eq!(lib.importer_root, None);
        assert_eq!(lib.importer_module, vec!["lib"]);

        for outside in ["crates/app/lib.rs", "crates/app/src/nested/lib.rs"] {
            let layout = RustPackageLayout::new(Path::new("crates/app"), Some(Path::new(outside)));
            assert_eq!(
                layout.main_tree().roots,
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
        let path = map_rust_use_path(
            &layout("").main_tree(),
            Path::new("src/lib.rs"),
            "crate::r#type::ty",
        )
        .expect("a raw identifier is a path identifier");
        assert_eq!(path.segments, vec!["r#type", "ty"]);
        assert_eq!(
            path.module_file_stems(&path.segments[..1]),
            vec!["src/type/mod", "src/type"]
        );
    }
}
