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

use open_kioku_semantic_model::{CargoTargetKind, CargoTargets, ProjectRoot};
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
    /// The target directories (`src/bin`, `tests`, `examples`, `benches`), whether Cargo
    /// discovers the crate roots directly in each.
    target_dirs: Vec<(String, bool)>,
    /// Extension-less crate root files the manifest names whose module trees the layout follows:
    /// one in `src_root`, directly in a target directory or a `<dir>/<name>/` below one, or in a
    /// directory outside all of them.
    target_roots: Vec<String>,
    /// Crate roots the manifest names whose module trees the layout does not follow, such as a
    /// `[[bin]] path` in a subdirectory of `src/`, with the directory of the tree that holds the
    /// root file (`None` when no tree does). Their modules can be files of that tree which no
    /// indexed root declares.
    unmodeled_roots: Vec<(Option<String>, String)>,
}

impl RustPackageLayout {
    /// The layout of the package at `root`, or of a top-level `src/` when no manifest is above.
    pub(crate) fn of(root: Option<&ProjectRoot>) -> Self {
        match root {
            Some(root) => Self::new(
                &root.path,
                root.library_root.as_deref(),
                &root.cargo_targets,
            ),
            None => Self::new(Path::new(""), None, &CargoTargets::default()),
        }
    }

    /// The package at `crate_dir`, whose manifest may set its library root with `[lib] path` and
    /// name other crate roots in its target tables.
    pub(crate) fn new(
        crate_dir: &Path,
        library_root: Option<&Path>,
        targets: &CargoTargets,
    ) -> Self {
        let package_dir = slash_path(crate_dir).trim_end_matches('/').to_string();
        let src_root = join_dir(&package_dir, "src");
        let target_dirs = [
            (join_dir(&src_root, "bin"), CargoTargetKind::Bin),
            (join_dir(&package_dir, "tests"), CargoTargetKind::Test),
            (join_dir(&package_dir, "examples"), CargoTargetKind::Example),
            (join_dir(&package_dir, "benches"), CargoTargetKind::Bench),
        ]
        .into_iter()
        .map(|(dir, kind)| (dir, targets.autodiscovers(kind)))
        .collect::<Vec<_>>();
        let mut layout = Self {
            package_dir,
            src_root,
            library: None,
            target_dirs,
            target_roots: Vec::new(),
            unmodeled_roots: Vec::new(),
        };
        match library_root {
            None => layout.library = Some("lib".to_string()),
            Some(file) => {
                let stem = slash_path(file);
                let stem = stem.strip_suffix(".rs").unwrap_or(&stem);
                match strip_dir(stem, &layout.src_root) {
                    Some(name) if !name.contains('/') && name != "main" => {
                        layout.library = Some(name.to_string());
                    }
                    // A library root deeper in `src/` has modules there that no placed root
                    // declares; one elsewhere leaves the library unplaced, which is reported.
                    Some(_) => layout
                        .unmodeled_roots
                        .push((Some(layout.holding_tree_dir(stem)), stem.to_string())),
                    None => {}
                }
            }
        }
        for root in &targets.roots {
            let root = slash_path(root);
            let modeled = root
                .strip_suffix(".rs")
                .filter(|stem| layout.follows_target_root(stem))
                .map(str::to_string);
            match modeled {
                Some(stem) if !layout.target_roots.contains(&stem) => {
                    layout.target_roots.push(stem);
                }
                Some(_) => {}
                None => {
                    let stem = root.strip_suffix(".rs").unwrap_or(&root).to_string();
                    let tree = (!stem.split('/').any(|part| matches!(part, "" | "." | ".."))
                        && strip_dir(&stem, &layout.package_dir).is_some())
                    .then(|| layout.holding_tree_dir(&stem));
                    layout.unmodeled_roots.push((tree, stem));
                }
            }
        }
        layout
    }

    /// Whether the module tree of the target root `stem` is one the layout follows: the root is
    /// in the package, and its directory is `src/`, a target directory, a `<dir>/<name>/` below
    /// one, or a directory inside none of them, where no other crate's modules live.
    fn follows_target_root(&self, stem: &str) -> bool {
        if stem.split('/').any(|part| matches!(part, "" | "." | ".."))
            || strip_dir(stem, &self.package_dir).is_none()
        {
            return false;
        }
        let dir = parent_dir(stem);
        if dir == self.src_root || self.target_dirs.iter().any(|(target, _)| *target == dir) {
            return true;
        }
        if self
            .target_dirs
            .iter()
            .any(|(target, _)| parent_dir(dir) == target)
        {
            return true;
        }
        strip_dir(dir, &self.src_root).is_none()
            && self
                .target_dirs
                .iter()
                .all(|(target, _)| strip_dir(dir, target).is_none())
    }

    /// The tree directory an unmodeled root file sits in: the target directory that holds it,
    /// or `src/`, or else its own directory.
    fn holding_tree_dir(&self, stem: &str) -> String {
        self.target_dirs
            .iter()
            .map(|(dir, _)| dir)
            .chain([&self.src_root])
            .find(|dir| strip_dir(stem, dir).is_some())
            .cloned()
            .unwrap_or_else(|| parent_dir(stem).to_string())
    }

    /// Whether every crate root of the package the manifest names is in a module tree the
    /// layout follows.
    pub(crate) fn places_all_roots(&self) -> bool {
        self.library.is_some() && self.unmodeled_roots.is_empty()
    }

    fn library_stem(&self) -> Option<String> {
        Some(format!("{}/{}", self.src_root, self.library.as_deref()?))
    }

    /// The module tree of `src/`: the library, the default binary `src/main.rs` unless binary
    /// auto-discovery is off, and any target the manifest roots directly in `src/`.
    pub(crate) fn main_tree(&self) -> RustCrateTree {
        let default_binary = self
            .target_dirs
            .first()
            .is_some_and(|(_, discovered)| *discovered)
            .then(|| format!("{}/main", self.src_root));
        let mut roots = self
            .library_stem()
            .into_iter()
            .chain(default_binary)
            .collect::<Vec<_>>();
        for root in self.own_target_roots(&self.src_root) {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        RustCrateTree {
            module_dir: self.src_root.clone(),
            roots,
            is_src: true,
            discovers_roots: false,
            unmodeled_roots: self.unmodeled_roots_in(&self.src_root),
        }
    }

    fn own_target_roots(&self, dir: &str) -> Vec<String> {
        self.target_roots
            .iter()
            .filter(|root| parent_dir(root) == dir)
            .cloned()
            .collect()
    }

    fn unmodeled_roots_in(&self, dir: &str) -> Vec<String> {
        self.unmodeled_roots
            .iter()
            .filter(|(tree, _)| tree.as_deref() == Some(dir))
            .map(|(_, root)| root.clone())
            .collect()
    }

    /// [`RustPackageLayout::crate_tree`] for a repository-relative Rust file path.
    pub(crate) fn crate_tree_of(
        &self,
        file: &Path,
        stems_in_dir: &HashMap<String, Vec<String>>,
    ) -> Option<RustCrateTree> {
        let file = slash_path(file);
        self.crate_tree(file.strip_suffix(".rs")?, stems_in_dir)
    }

    /// The crate module tree holding `file`, repository-relative with `/` separators, given the
    /// extension-less paths of the indexed Rust files in each directory: the tree of the nearest
    /// directory above it that holds one. Under a target directory (`src/bin/`, `tests/`,
    /// `examples/`, `benches/`) each file directly in it is a crate root of its own unless the
    /// manifest turns that off, whose modules live in that directory, and `<dir>/<name>/main.rs`
    /// is one whose modules live in `<dir>/<name>/`. A root the manifest names roots a tree in
    /// its own directory. `None` for a file outside every tree, such as `build.rs`.
    pub(crate) fn crate_tree(
        &self,
        file: &str,
        stems_in_dir: &HashMap<String, Vec<String>>,
    ) -> Option<RustCrateTree> {
        strip_dir(file, &self.package_dir)?;
        let mut dir = parent_dir(file);
        loop {
            if let Some(tree) = self.tree_at(dir, stems_in_dir) {
                return Some(tree);
            }
            if dir == self.package_dir || dir.is_empty() {
                return None;
            }
            dir = parent_dir(dir);
        }
    }

    /// The crate module tree whose modules live in `dir`, if one does.
    fn tree_at(
        &self,
        dir: &str,
        stems_in_dir: &HashMap<String, Vec<String>>,
    ) -> Option<RustCrateTree> {
        if dir == self.src_root {
            return Some(self.main_tree());
        }
        let own = self.own_target_roots(dir);
        if let Some((_, discovered)) = self.target_dirs.iter().find(|(target, _)| target == dir) {
            let mut roots = if *discovered {
                stems_in_dir.get(dir).cloned().unwrap_or_default()
            } else {
                Vec::new()
            };
            for root in own {
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
            return Some(RustCrateTree {
                module_dir: dir.to_string(),
                roots,
                is_src: false,
                discovers_roots: *discovered,
                unmodeled_roots: self.unmodeled_roots_in(dir),
            });
        }
        let mut roots = Vec::new();
        if let Some((_, discovered)) = self
            .target_dirs
            .iter()
            .find(|(target, _)| target == parent_dir(dir))
        {
            let main = format!("{dir}/main");
            if *discovered
                && stems_in_dir
                    .get(dir)
                    .is_some_and(|stems| stems.contains(&main))
            {
                roots.push(main);
            }
        }
        for root in own {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        (!roots.is_empty()).then(|| RustCrateTree {
            module_dir: dir.to_string(),
            roots,
            is_src: false,
            discovers_roots: false,
            unmodeled_roots: Vec::new(),
        })
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
    /// The tree is the package's `src/`, of the library and the default binary.
    pub(crate) is_src: bool,
    /// Every Rust file directly in `module_dir` is a crate root: a target directory Cargo
    /// discovers, whose `roots` are only the indexed ones.
    pub(crate) discovers_roots: bool,
    /// Crate roots the manifest names in this tree's directory whose own module trees the layout
    /// does not follow.
    pub(crate) unmodeled_roots: Vec<String>,
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

fn slash_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The directory of a repository-relative path, `""` at the repository root.
fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
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
    use std::path::PathBuf;

    /// `use_path` from `importer` in the package at `crate_dir`, where the indexed Rust files are
    /// `importer` and the package's default crate roots.
    fn mapped(crate_dir: &str, importer: &str, use_path: &str) -> Option<(String, Vec<String>)> {
        let tree = layout(crate_dir).crate_tree(importer.strip_suffix(".rs")?, &HashMap::new())?;
        map_rust_use_path(&tree, Path::new(importer), use_path)
            .map(|path| (path.tree.module_dir, path.segments))
    }

    fn layout(crate_dir: &str) -> RustPackageLayout {
        RustPackageLayout::new(Path::new(crate_dir), None, &CargoTargets::default())
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
        let nested =
            RustPackageLayout::new(Path::new("src/tools/xtask"), None, &CargoTargets::default())
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
            &CargoTargets::default(),
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
            let layout = RustPackageLayout::new(
                Path::new("crates/app"),
                Some(Path::new(outside)),
                &CargoTargets::default(),
            );
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

    #[test]
    fn crate_roots_a_manifest_names_root_trees_in_their_own_directories() {
        let targets = CargoTargets {
            roots: [
                "crates/app/src/cli.rs",
                "crates/app/tools/gen.rs",
                "crates/app/examples/deep/demo.rs",
                "crates/app/src/nested/extra.rs",
                "crates/app/../shared/main.rs",
            ]
            .map(PathBuf::from)
            .to_vec(),
            not_autodiscovered: vec![CargoTargetKind::Bin],
        };
        let layout = RustPackageLayout::new(Path::new("crates/app"), None, &targets);
        let indexed = stems_in_dir(&[
            "crates/app/src/lib",
            "crates/app/src/bin/other",
            "crates/app/tests/it",
        ]);
        let tree = |file: &str| {
            layout
                .crate_tree(file, &indexed)
                .expect("the file is in a crate module tree")
        };

        // `src/cli.rs` is a root of `src/`, and with binaries not discovered `src/main.rs` is not.
        let main = tree("crates/app/src/util");
        assert_eq!(main.roots, vec!["crates/app/src/lib", "crates/app/src/cli"]);
        // A root in a subdirectory of `src/` shares its files with the library's modules.
        assert_eq!(main.unmodeled_roots, vec!["crates/app/src/nested/extra"]);
        assert!(!layout.places_all_roots());
        // `src/bin/other.rs` is not a binary; integration tests are still discovered.
        let bins = tree("crates/app/src/bin/other");
        assert!(bins.roots.is_empty());
        assert!(!bins.discovers_roots);
        assert_eq!(
            tree("crates/app/tests/common").roots,
            vec!["crates/app/tests/it"]
        );
        // A root outside every tree keeps its modules beside it, as does one a level below a
        // target directory.
        let tools = tree("crates/app/tools/args");
        assert_eq!(tools.module_dir, "crates/app/tools");
        assert_eq!(tools.roots, vec!["crates/app/tools/gen"]);
        let deep = tree("crates/app/examples/deep/scene");
        assert_eq!(deep.module_dir, "crates/app/examples/deep");
        assert_eq!(deep.roots, vec!["crates/app/examples/deep/demo"]);
        // A root outside the package names no tree.
        assert_eq!(layout.crate_tree("crates/app/build", &indexed), None);
        assert_eq!(layout.crate_tree("crates/shared/main", &indexed), None);
    }
}
