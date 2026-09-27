use crate::rust_use_path::{
    join_dir, map_rust_crate_name_path, map_rust_module_file, map_rust_use_path, module_name,
    normalize_path, parent_dir, strip_dir, RustCrateTree, RustPackageLayout, RustUsePath,
};
use open_kioku_core::{
    File, FileId, ImportSite, Language, ModuleDeclarationSite, ScopeId, ScopeKind, SymbolId,
    SymbolKind,
};
use open_kioku_resolution::{RustCrateNames, RustModulePlacement};
use open_kioku_semantic_model::{CargoImporter, ProjectModel, ProjectRoot};
pub use open_kioku_semantic_model::{
    ExportBinding, ExportIndex, ImportBinding, ImportBindingRule, ImportIndex, ImportOrigin,
    GLOB_IMPORT_LOCAL_NAME,
};
use std::cell::OnceCell;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type FileMap = HashMap<String, Vec<FileId>>;

/// Ambiguity-aware module/file lookup used by V2 import resolution.
/// Implementations must expose all candidates so `unique_file` can fail closed.
/// Ambiguous keys deliberately remain unresolved instead of relying on insertion order.
pub trait FileLookup {
    fn candidates(&self, key: &str) -> Vec<FileId>;

    fn contains_key_candidate(&self, key: &str) -> bool {
        !self.candidates(key).is_empty()
    }

    fn unique_file(&self, key: &str) -> Option<FileId> {
        let candidates = self.candidates(key);
        if candidates.len() == 1 {
            Some(candidates[0].clone())
        } else {
            None
        }
    }
}

impl FileLookup for FileMap {
    fn candidates(&self, key: &str) -> Vec<FileId> {
        self.get(key).cloned().unwrap_or_default()
    }
}

impl FileLookup for HashMap<String, FileId> {
    fn candidates(&self, key: &str) -> Vec<FileId> {
        self.get(key).cloned().into_iter().collect()
    }
}

/// The Rust module tree as declared, which Rust import binding follows instead of trusting file
/// layout alone.
pub(crate) struct RustModuleTree<'a> {
    files: HashMap<FileId, &'a Path>,
    /// Rust files by repository-relative path without `.rs`.
    files_by_stem: HashMap<String, FileId>,
    /// Those paths by the directory holding the file.
    stems_in_dir: HashMap<String, Vec<String>>,
    project: &'a ProjectModel,
    /// `(declaring file without `.rs`, module name)` for each file-backed declaration: `mod name;`
    /// with no body and no `path` attribute, outside any inline module. `mod r#type;` is `type`.
    file_modules: HashSet<(String, String)>,
    /// Rust files discovery saw but did not index (over `max_file_size`, excluded, ignored,
    /// unreadable), by repository-relative path without `.rs`. A crate root among them owns
    /// modules the index cannot place.
    unindexed_stems: HashSet<String>,
    /// The module names crate roots discovery skipped for size declare, by extension-less path,
    /// as [`scan_module_declarations`] read them: `None` where the lines could not tell. A root
    /// not scanned (one a path policy excluded is never read) may declare any module.
    ///
    /// [`scan_module_declarations`]: crate::rust_use_path::scan_module_declarations
    scanned_roots: HashMap<String, Option<HashSet<String>>>,
    /// What each `#[path]` attribute mounts, by the extension-less path of the declaring file.
    path_mounts: Vec<(String, PathMount)>,
    /// [`RustModuleTree::find_shared_files`], computed on first use.
    shared_files: OnceCell<SharedFiles>,
    /// The `pub use` sites of each Rust file, which other crates reach items through.
    reexports: HashMap<FileId, Vec<ImportSite>>,
}

/// How many `pub use` re-exports one path is followed through before it is left unresolved.
const MAX_REEXPORT_HOPS: usize = 8;

/// What a Rust path names, as the declared module tree proves it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RustPathTarget {
    /// The file of the module the path names.
    module_file: Option<FileId>,
    /// The module-level item the path names, when it names no module file.
    item: Option<SymbolId>,
    /// Reached through at least one `pub use` of the crate the path starts in.
    reexport: bool,
}

/// The file a `#[path]` attribute on a `mod` item mounts, as far as the index can tell.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathMount {
    /// The extension-less path of the file.
    File(String),
    /// A file below `dir` whose path ends in `file`, extension-less: a `path` inside an inline
    /// module is read from a directory below the declaring file's that the module names spell.
    Below { dir: String, file: String },
    /// Any file: a `path` the index cannot read, or one on a module with a body.
    Unknown,
}

impl PathMount {
    fn of(declaring: &str, value: &str, in_block: bool) -> Option<Self> {
        let value = value.replace('\\', "/");
        let file = value.strip_suffix(".rs")?;
        if value.starts_with('/') {
            return Some(Self::Unknown);
        }
        let dir = parent_dir(declaring);
        if in_block {
            if file.split('/').any(|part| matches!(part, "" | "." | "..")) {
                return Some(Self::Unknown);
            }
            return Some(Self::Below {
                dir: dir.to_string(),
                file: file.to_string(),
            });
        }
        // A path above the repository root names no indexed file.
        normalize_path(&join_dir(dir, file)).map(Self::File)
    }

    fn may_mount(&self, stem: &str) -> bool {
        match self {
            Self::File(file) => file == stem,
            // At least one module directory lies between the declaring file's directory and
            // the file, so `x.rs` beside the declaring file is not the one mounted.
            Self::Below { dir, file } => strip_dir(stem, dir).is_some_and(|below| {
                below
                    .strip_suffix(file.as_str())
                    .is_some_and(|prefix| prefix.ends_with('/'))
            }),
            Self::Unknown => true,
        }
    }
}

/// Module files an indexed crate root compiles that another crate may compile too.
#[derive(Debug, Default)]
struct SharedFiles {
    /// By [`importer_stem`]: whether every other crate that may compile the file mounts it as a
    /// module file at the same place in its own tree, so the module files below it are the same
    /// files in each crate.
    files: HashMap<String, bool>,
    /// `#[path]` attributes the index could not read that marked a file, since they may mount
    /// any file of their package.
    unread_mounts: usize,
}

/// What [`RustModuleTree::mounted_subtree`] finds below a `#[path]`-mounted file.
struct MountedSubtree {
    /// Each file with whether it keeps the place its file path spells.
    files: Vec<(String, bool)>,
    /// The `#[path]` attributes of the subtree the index cannot read, by their index in
    /// `path_mounts`, with the file declaring each.
    unread_mounts: Vec<(usize, String)>,
}

/// Rust files whose module paths the index leaves unresolved because it cannot tell which crate
/// they belong to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RustPlacementGaps {
    /// Packages with indexed files in a crate module tree that has no indexed crate root, or
    /// whose manifest names a crate root in a tree the layout does not follow.
    pub(crate) unplaced_packages: usize,
    /// Crate roots of `src/` trees discovery skipped or the layout does not follow, where a file
    /// no indexed root declares was withheld.
    pub(crate) unread_roots: usize,
    /// Files of those trees that no indexed crate root declares, read against no crate.
    pub(crate) withheld_files: usize,
    /// Files an indexed crate root declares that may also be compiled into another crate, whose
    /// own module paths are read against no crate.
    pub(crate) shared_files: usize,
    /// `#[path]` attributes the index could not read (a raw string, a macro, a directory on an
    /// inline module), each of which marked files of its package as shared.
    pub(crate) unread_mounts: usize,
}

impl<'a> RustModuleTree<'a> {
    pub(crate) fn new(
        files: &'a [File],
        project: &'a ProjectModel,
        declarations: &[ModuleDeclarationSite],
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Self {
        let files = files
            .iter()
            .filter(|file| file.language == Language::Rust)
            .map(|file| (file.id.clone(), file.path.as_path()))
            .collect::<HashMap<_, _>>();
        let files_by_stem = files
            .iter()
            .filter_map(|(id, path)| Some((rust_file_stem(path)?, id.clone())))
            .collect::<HashMap<_, _>>();
        let mut stems_in_dir = HashMap::<String, Vec<String>>::new();
        for stem in files_by_stem.keys() {
            let dir = stem.rsplit_once('/').map_or("", |(dir, _)| dir);
            stems_in_dir
                .entry(dir.to_string())
                .or_default()
                .push(stem.clone());
        }
        for stems in stems_in_dir.values_mut() {
            stems.sort();
        }
        let file_modules = declarations
            .iter()
            .filter(|declaration| {
                !declaration.has_body
                    && !declaration.has_path_attribute
                    && !declaration
                        .scope_id
                        .as_ref()
                        .is_some_and(|scope| is_inside_inline_module(scope, scopes))
            })
            .filter_map(|declaration| {
                let path = files.get(&declaration.file_id)?;
                Some((
                    rust_file_stem(path)?,
                    module_name(&declaration.name).to_string(),
                ))
            })
            .collect();
        let mut path_mounts = Vec::new();
        for declaration in declarations
            .iter()
            .filter(|declaration| declaration.has_path_attribute)
        {
            let Some(declaring) = files
                .get(&declaration.file_id)
                .and_then(|path| rust_file_stem(path))
            else {
                continue;
            };
            // A `path` on a module with a body moves the modules it declares.
            if declaration.has_body || declaration.path_attributes.is_empty() {
                path_mounts.push((declaring, PathMount::Unknown));
                continue;
            }
            let in_block = declaration
                .scope_id
                .as_ref()
                .is_some_and(|scope| is_inside_inline_module(scope, scopes));
            for value in &declaration.path_attributes {
                if let Some(mount) = PathMount::of(&declaring, value, in_block) {
                    path_mounts.push((declaring.clone(), mount));
                }
            }
        }
        Self {
            files,
            files_by_stem,
            stems_in_dir,
            project,
            file_modules,
            unindexed_stems: HashSet::new(),
            scanned_roots: HashMap::new(),
            path_mounts,
            shared_files: OnceCell::new(),
            reexports: HashMap::new(),
        }
    }

    /// Records the `pub use` sites a path through a crate name may be re-exported by.
    pub(crate) fn with_reexports(mut self, sites: &[ImportSite]) -> Self {
        for site in sites
            .iter()
            .filter(|site| site.reexported && self.files.contains_key(&site.file_id))
        {
            self.reexports
                .entry(site.file_id.clone())
                .or_default()
                .push(site.clone());
        }
        self
    }

    /// Which dependencies code in `importer` can name: a package's build script only its build
    /// dependencies.
    fn cargo_importer(&self, importer: &Path) -> CargoImporter {
        let package = self.package_of(importer);
        let build_script = importer.file_name().is_some_and(|name| name == "build.rs")
            && importer.parent() == package;
        if build_script {
            CargoImporter::BuildScript
        } else {
            CargoImporter::Crate
        }
    }

    /// `source` as a path through a crate name written in `importer`: the importer's own package
    /// (`demo_crate::auth`, legal from its tests, examples and binaries), or a dependency its
    /// manifest declares on a package of the repository, followed from that crate's library root.
    fn crate_name_path(&self, importer: &Path, source: &str) -> Option<RustUsePath> {
        let root = self.project.nearest_root_for(importer, Language::Rust)?;
        let first = source.split("::").next()?;
        if let Some(package) = root.package_name.as_deref() {
            if package.replace('-', "_") == first {
                return map_rust_crate_name_path(
                    &RustPackageLayout::of(Some(root)),
                    package,
                    source,
                );
            }
        }
        let dependency =
            self.project
                .rust_dependency(root, first, self.cargo_importer(importer))?;
        map_rust_crate_name_path(&RustPackageLayout::of(Some(dependency)), first, source)
    }

    /// Whether `source` starts with the name of a dependency `importer`'s package declares on a
    /// package of the repository.
    fn names_dependency(&self, importer: &Path, source: &str) -> bool {
        let Some(root) = self.project.nearest_root_for(importer, Language::Rust) else {
            return false;
        };
        source.split("::").next().is_some_and(|first| {
            self.project
                .rust_dependency(root, first, self.cargo_importer(importer))
                .is_some()
        })
    }

    /// `source`, written in `importer` in `scope`, mapped onto a crate module tree: through a
    /// crate name, or `crate::`/`self::`/`super::` in the importer's own crate. The flag is set for
    /// a crate-name path, whose items may be reached through that crate's `pub use` re-exports.
    fn rust_path(
        &self,
        importer: &Path,
        scope: Option<&ScopeId>,
        source: &str,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<(RustUsePath, bool)> {
        self.project.nearest_root_for(importer, Language::Rust)?;
        if let Some(path) = self.crate_name_path(importer, source) {
            return Some((path, true));
        }
        // `self` and `super` are read off the importer's file path, which cannot see an inline
        // `mod` block: in `mod tests { use super::*; }` `super` is the file's own module.
        if !source.starts_with("crate::")
            && scope.is_some_and(|scope| is_inside_inline_module(scope, scopes))
        {
            return None;
        }
        let path = map_rust_use_path(&self.crate_tree(importer)?, importer, source)?;
        if path.relative && !self.declares_file_modules(&path, &path.importer_module) {
            return None;
        }
        Some((path, false))
    }

    /// What `path` names: the file of a declared module, or else the one module-level item of
    /// its name in the declared parent module. A path naming both a module file and an item
    /// names the module. When the parent declares no item of the name and `follow_reexports` is
    /// set, the `pub use` sites of the parent are followed instead.
    fn rust_path_target(
        &self,
        path: &RustUsePath,
        follow_reexports: bool,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
        hops: usize,
    ) -> Option<RustPathTarget> {
        let (item_name, parent) = path.segments.split_last()?;
        if let Some(module_file) = self.module_file(path, &path.segments) {
            return Some(RustPathTarget {
                module_file: Some(module_file),
                item: None,
                reexport: false,
            });
        }
        if !self.declares_file_modules(path, parent) {
            return None;
        }
        let items = rust_module_items(&self.module_stems(path, parent), item_name, symbols);
        match items.as_slice() {
            [item] => Some(RustPathTarget {
                module_file: None,
                item: Some(item.clone()),
                reexport: false,
            }),
            // An item defined in the module is the name; a `pub use` of the same name beside it
            // does not compile, so it is followed only where the module defines none.
            [] if follow_reexports => self.reexported_target(path, symbols, scopes, hops),
            _ => None,
        }
    }

    /// What the last segment of `path` names through the `pub use` sites of its parent module,
    /// followed from the re-exporting file. A named re-export shadows a glob one; the name is
    /// bound only when every re-export of it the module holds agrees, and one the tree cannot
    /// follow leaves it unbound.
    fn reexported_target(
        &self,
        path: &RustUsePath,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
        hops: usize,
    ) -> Option<RustPathTarget> {
        if hops >= MAX_REEXPORT_HOPS {
            return None;
        }
        let (name, parent) = path.segments.split_last()?;
        if name == "*" {
            return None;
        }
        let mut named = Vec::new();
        let mut named_unresolved = false;
        let mut globbed = Vec::new();
        let mut glob_unresolved = false;
        for file in self
            .module_stems(path, parent)
            .iter()
            .filter_map(|stem| self.files_by_stem.get(stem))
        {
            let Some(importer) = self.files.get(file) else {
                continue;
            };
            for site in self.reexports.get(file).into_iter().flatten() {
                // A `pub use` inside an inline `mod` re-exports from that module, not this one.
                if site
                    .scope_id
                    .as_ref()
                    .is_some_and(|scope| is_inside_inline_module(scope, scopes))
                {
                    continue;
                }
                if site.is_glob {
                    let Some(prefix) = site.source.strip_suffix("::*") else {
                        continue;
                    };
                    let source = format!("{prefix}::{name}");
                    match self
                        .reexport_source_target(importer, &source, name, symbols, scopes, hops)
                    {
                        Some(target) => globbed.push(target),
                        None => glob_unresolved = true,
                    }
                    continue;
                }
                for binding in site
                    .bindings
                    .iter()
                    .filter(|binding| binding.local == *name)
                {
                    match self.reexport_source_target(
                        importer,
                        &site.source,
                        &binding.imported,
                        symbols,
                        scopes,
                        hops,
                    ) {
                        Some(target) => named.push(target),
                        None => named_unresolved = true,
                    }
                }
            }
        }
        // A glob the tree cannot follow may supply the name too, unless a named one shadows it.
        let (mut targets, unresolved) = if named.is_empty() && !named_unresolved {
            (globbed, glob_unresolved)
        } else {
            (named, named_unresolved)
        };
        targets.dedup();
        match (targets.as_slice(), unresolved) {
            ([target], false) => Some(RustPathTarget {
                reexport: true,
                ..target.clone()
            }),
            _ => None,
        }
    }

    /// What the path a `pub use` in `importer` names, when its last segment is `imported`. Such a
    /// path may also be a Rust 2018 path from the re-exporting module (`pub use auth::Token;`
    /// beside `mod auth;`).
    fn reexport_source_target(
        &self,
        importer: &Path,
        source: &str,
        imported: &str,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
        hops: usize,
    ) -> Option<RustPathTarget> {
        let first = source.split("::").next()?;
        let declares_first = rust_file_stem(importer).is_some_and(|stem| {
            self.file_modules
                .contains(&(stem, module_name(first).to_string()))
        });
        let (path, _) = if declares_first {
            self.rust_path(importer, None, &format!("self::{source}"), scopes)?
        } else {
            self.rust_path(importer, None, source, scopes)?
        };
        if path.segments.last().map(String::as_str) != Some(imported) {
            return None;
        }
        self.rust_path_target(&path, true, symbols, scopes, hops + 1)
    }

    /// Records the repository-relative paths discovery skipped; only Rust files matter.
    pub(crate) fn with_unindexed_files<'p>(
        mut self,
        paths: impl IntoIterator<Item = &'p Path>,
    ) -> Self {
        self.unindexed_stems
            .extend(paths.into_iter().filter_map(rust_file_stem));
        self.shared_files = OnceCell::new();
        self
    }

    /// The skipped files that are crate roots of a module tree holding an indexed file, whose
    /// `mod` declarations decide which of the tree's files another crate may compile.
    pub(crate) fn unread_crate_roots(&self) -> HashSet<PathBuf> {
        let mut trees = HashSet::new();
        let mut roots = HashSet::new();
        for path in self.files.values() {
            let Some(file) = self.module_file_of(path) else {
                continue;
            };
            if trees.insert(file.tree.module_dir.clone()) {
                roots.extend(
                    self.unreadable_roots(&file.tree)
                        .into_iter()
                        .filter(|root| self.unindexed_stems.contains(root))
                        .map(|root| PathBuf::from(format!("{root}.rs"))),
                );
            }
        }
        roots
    }

    /// Records the module names crate roots skipped for size declare, as
    /// [`scan_module_declarations`] read them (`None` where it could not tell).
    ///
    /// [`scan_module_declarations`]: crate::rust_use_path::scan_module_declarations
    pub(crate) fn with_scanned_roots<'p>(
        mut self,
        roots: impl IntoIterator<Item = (&'p Path, Option<HashSet<String>>)>,
    ) -> Self {
        self.scanned_roots.extend(
            roots
                .into_iter()
                .filter_map(|(path, names)| Some((rust_file_stem(path)?, names))),
        );
        self.shared_files = OnceCell::new();
        self
    }

    /// The crate roots of `tree` whose modules the index cannot place: roots discovery skipped,
    /// roots the manifest names that were not indexed (a skipped secret-like path is recorded
    /// without one), and roots the manifest names whose own module trees the layout does not
    /// follow. A file of the tree that no indexed root declares may belong to one of them.
    fn unreadable_roots(&self, tree: &RustCrateTree) -> Vec<String> {
        let discovered = tree
            .discovers_roots
            .then(|| {
                self.unindexed_stems.iter().filter(|stem| {
                    stem.rsplit_once('/').map_or("", |(dir, _)| dir) == tree.module_dir
                })
            })
            .into_iter()
            .flatten();
        let mut roots = tree
            .roots
            .iter()
            .filter(|root| {
                self.unindexed_stems.contains(*root)
                    || (tree.declared_roots.contains(*root)
                        && !self.files_by_stem.contains_key(*root))
            })
            .chain(discovered)
            .chain(&tree.unmodeled_roots)
            .cloned()
            .collect::<Vec<_>>();
        roots.sort();
        roots.dedup();
        roots
    }

    /// The crate roots a file of `tree` that no indexed root declares is read against: the
    /// indexed roots of `src/`, whose library and default binary hold its modules unless
    /// declared otherwise. None when a root of the tree cannot be read, since the file may be
    /// that crate's alone; none for a file where Cargo would find a crate root it does not build
    /// (`src/main.rs` under `autobins = false`), which no crate compiles; and none in a target
    /// directory, whose crate roots are independent crates that do not stand for one another.
    fn undeclared_file_roots(&self, path: &RustUsePath) -> Vec<String> {
        if !path.tree.is_src
            || !self.unreadable_roots(&path.tree).is_empty()
            || path.tree.unbuilt_files.contains(&importer_stem(path))
        {
            return Vec::new();
        }
        self.indexed_crate_roots(path)
    }

    /// Whether every module on `module`, from the crate root down, is declared as a file by the
    /// module above it. A stale `auth.rs` beside `#[path = "auth_v2.rs"] mod auth;` or an inline
    /// `mod auth { ... }` is not the module `crate::auth` names.
    fn declares_file_modules(&self, path: &RustUsePath, module: &[String]) -> bool {
        module.is_empty() || self.declared_below(&self.crate_roots(path), path, module)
    }

    /// [`RustModuleTree::declares_file_modules`] from the crate roots `roots`.
    fn declared_below(&self, roots: &[String], path: &RustUsePath, module: &[String]) -> bool {
        (0..module.len()).all(|depth| {
            let name = module_name(&module[depth]);
            let declares = |stem: &String| {
                self.file_modules
                    .contains(&(stem.clone(), name.to_string()))
            };
            if depth == 0 {
                !roots.is_empty() && roots.iter().all(declares)
            } else {
                path.module_file_stems(&module[..depth])
                    .iter()
                    .any(declares)
            }
        })
    }

    /// The crate root files whose module tree holds the importer: the root file itself, or the
    /// `lib.rs` and `main.rs` that declare the importer's top-level module. A package with both
    /// roots is two crates, and `crate::` names only the modules of the importer's own root; when
    /// both roots declare the importer's module, a path must be declared under both. When no
    /// indexed root declares it, those of [`RustModuleTree::undeclared_file_roots`]. None for a
    /// file that may also be compiled into another crate (see
    /// [`RustModuleTree::find_shared_files`]), where a path names an item of each, unless the
    /// path is `self::` and every crate mounts the file at the same place: it then ends in the
    /// same file in each.
    fn crate_roots(&self, path: &RustUsePath) -> Vec<String> {
        if path.importer_root.is_none() {
            match self.shared_files().files.get(&importer_stem(path)) {
                Some(true) if path.within_importer => {}
                Some(_) => return Vec::new(),
                None => {}
            }
        }
        self.declared_crate_roots(path)
    }

    /// [`RustModuleTree::crate_roots`] as the indexed crate roots declare them, whatever other
    /// crate may compile the file too.
    fn declared_crate_roots(&self, path: &RustUsePath) -> Vec<String> {
        if let Some(root) = &path.importer_root {
            return vec![root.clone()];
        }
        let roots = self.indexed_crate_roots(path);
        let Some(top) = path.importer_module.first() else {
            return roots;
        };
        let declaring = roots
            .iter()
            .filter(|stem| self.file_modules.contains(&((*stem).clone(), top.clone())))
            .cloned()
            .collect::<Vec<_>>();
        if declaring.is_empty() {
            self.undeclared_file_roots(path)
        } else {
            declaring
        }
    }

    /// The module tree layout of the package holding `file`: the nearest `Cargo.toml` above it,
    /// or a top-level `src/` when there is none.
    fn package_layout(&self, file: &Path) -> RustPackageLayout {
        RustPackageLayout::of(self.project.nearest_root_for(file, Language::Rust))
    }

    /// The crate module tree of its package that holds `file`, if any.
    fn crate_tree(&self, file: &Path) -> Option<RustCrateTree> {
        self.package_layout(file)
            .crate_tree_of(file, &self.stems_in_dir)
    }

    /// Where the declared module tree places each Rust file of a crate module tree, for paths
    /// the resolver spells from file paths. A file is placed at its path when it is a crate root,
    /// or when every module from the crate root down is declared as a file by the module above
    /// it; its crate roots are those that declare its top-level module. A file of a tree that is
    /// not placed there (mounted by `#[path]`, at the default location of a `#[path]` or inline
    /// module, or declared by no `mod` the parser sees, such as one inside a macro) in `src/` is
    /// recorded with no module and the indexed `lib.rs`/`main.rs`, so a `crate::` path written
    /// there is still read against its own package, unless a crate root of `src/` cannot be read
    /// (see [`RustModuleTree::undeclared_file_roots`]). One in a target directory (`src/bin/`,
    /// `tests/`, `examples/`, `benches/`), whose crate roots are independent crates, a file
    /// outside every tree (`build.rs`), and every file of a tree whose crate roots are not
    /// indexed, is not recorded.
    pub(crate) fn module_placements(&self) -> HashMap<FileId, RustModulePlacement> {
        self.files
            .iter()
            .filter_map(|(id, path)| {
                let file = self.module_file_of(path)?;
                let (placed, roots) = self.declared_placement(&file);
                if roots.is_empty() {
                    return None;
                }
                let shared = file
                    .importer_root
                    .is_none()
                    .then(|| {
                        self.shared_files()
                            .files
                            .get(&importer_stem(&file))
                            .copied()
                    })
                    .flatten();
                let qualified = |stem: &str| stem.replace('/', "::");
                Some((
                    id.clone(),
                    RustModulePlacement {
                        crate_dir: qualified(&file.tree.module_dir),
                        crate_roots: roots.iter().map(|root| qualified(root)).collect(),
                        module: placed.then(|| file.importer_module.clone()),
                        in_other_crates: shared.is_some(),
                        own_subtree_in_every_crate: shared.unwrap_or(true),
                    },
                ))
            })
            .collect()
    }

    /// The library crates each Rust file names by crate name: the dependencies its package
    /// declares that are visible to it, each placed at its crate root, and its own package's
    /// library for a file only another crate of the package compiles (a binary, integration test,
    /// example or bench). Library code cannot name its own crate that way, nor can a build script,
    /// and a file the library may compile is treated as library code. Shared by every file of one
    /// package, importer kind and library membership.
    pub(crate) fn crate_names(&self) -> HashMap<FileId, Arc<RustCrateNames>> {
        let mut by_package = HashMap::<(&Path, CargoImporter, bool), Arc<RustCrateNames>>::new();
        let mut names = HashMap::new();
        for (id, path) in &self.files {
            let Some(root) = self.project.nearest_root_for(path, Language::Rust) else {
                continue;
            };
            let kind = self.cargo_importer(path);
            let names_own_library =
                kind == CargoImporter::Crate && self.outside_the_library(root, path);
            let crates = by_package
                .entry((root.path.as_path(), kind, names_own_library))
                .or_insert_with(|| {
                    let mut crates = RustCrateNames::new();
                    let dependencies = root
                        .cargo_manifest
                        .iter()
                        .flat_map(|manifest| &manifest.dependencies)
                        .filter(|dependency| dependency.visible_to(kind))
                        .filter_map(|dependency| dependency.crate_name.as_deref());
                    for name in dependencies {
                        if let Some(placement) = self
                            .project
                            .rust_dependency(root, name, kind)
                            .and_then(library_placement)
                        {
                            crates.insert(name.to_string(), placement);
                        }
                    }
                    if let Some(package) =
                        root.package_name.as_deref().filter(|_| names_own_library)
                    {
                        if let Some(placement) = library_placement(root) {
                            crates.entry(package.replace('-', "_")).or_insert(placement);
                        }
                    }
                    Arc::new(crates)
                })
                .clone();
            if !crates.is_empty() {
                names.insert(id.clone(), crates);
            }
        }
        names
    }

    /// Whether `path`, a file of the package at `root`, is compiled only into crates other than
    /// the package's library: the crate roots that declare it are known, none is the library's,
    /// and no other crate may compile it too.
    fn outside_the_library(&self, root: &ProjectRoot, path: &Path) -> bool {
        let Some(library) = RustPackageLayout::of(Some(root)).library_stem() else {
            return false;
        };
        let Some(file) = self.module_file_of(path) else {
            return false;
        };
        if file.importer_root.is_none()
            && self
                .shared_files()
                .files
                .contains_key(&importer_stem(&file))
        {
            return false;
        }
        let roots = match &file.importer_root {
            Some(own) => vec![own.clone()],
            None => self.declared_placement(&file).1,
        };
        !roots.is_empty() && !roots.contains(&library)
    }

    /// Whether the declared module tree places `file` at its path, and the crate roots whose
    /// crates the indexed roots compile it into.
    fn declared_placement(&self, file: &RustUsePath) -> (bool, Vec<String>) {
        let declared = self.declared_crate_roots(file);
        let placed = file.importer_root.is_some()
            || self.declared_below(&declared, file, &file.importer_module);
        if placed {
            (true, declared)
        } else {
            (false, self.undeclared_file_roots(file))
        }
    }

    fn shared_files(&self) -> &SharedFiles {
        self.shared_files.get_or_init(|| self.find_shared_files())
    }

    /// Module files an indexed crate root compiles, by [`importer_stem`], that another crate may
    /// compile too, so that `crate::` and `super::` in them name an item of each crate:
    /// - a file below the directory of a crate root the index cannot read (see
    ///   [`RustModuleTree::unreadable_roots`]), which may declare it as well. A root skipped for
    ///   size whose lines were read declares only the modules it names; any other is taken to
    ///   declare every module below its directory, each at the place its file path spells;
    /// - a file a `#[path]` attribute mounts from a file of a crate the declaring roots are not,
    ///   or from a file whose crate is unknown, such as a build script, and the module files the
    ///   mounted file declares below it, through `mod name;` or a `#[path]` of its own. A `#[path]`
    ///   the index cannot follow may mount any file of its package.
    ///
    /// Each is recorded with whether every other crate compiles it at the place its path spells:
    /// not so for a file a `#[path]` mounts other than a `mod.rs`, whose own `mod` items are read
    /// from its directory, nor for one a root that may use `#[path]` or a `#[path]` the index cannot
    /// follow may mount. A crate root file is left out: its own crate is the one it roots.
    fn find_shared_files(&self) -> SharedFiles {
        let mut modules_by_file = HashMap::<&str, Vec<&str>>::new();
        for (file, name) in &self.file_modules {
            modules_by_file.entry(file).or_default().push(name);
        }
        let mut mounts_by_file = HashMap::<&str, Vec<(usize, &PathMount)>>::new();
        for (at, (declaring, mount)) in self.path_mounts.iter().enumerate() {
            mounts_by_file
                .entry(declaring)
                .or_default()
                .push((at, mount));
        }
        // Per mounted file: the crates of each mount, and whether it keeps the file in place.
        let mut mounted = HashMap::<String, Vec<(Vec<String>, bool)>>::new();
        let mut unread_mounts = Vec::new();
        for (at, (declaring, mount)) in self.path_mounts.iter().enumerate() {
            let crates = self.crates_of(declaring);
            let Some(files) = self.mounted_files(declaring, mount) else {
                unread_mounts.push((at, self.package_of_stem(declaring), crates, false));
                continue;
            };
            for file in files {
                let subtree = self.mounted_subtree(&file, &modules_by_file, &mounts_by_file);
                for (stem, in_place) in subtree.files {
                    mounted
                        .entry(stem)
                        .or_default()
                        .push((crates.clone(), in_place));
                }
                // A mount below the mounted file the index cannot read may mount any file of
                // its package into these crates too.
                for (below, declaring) in subtree.unread_mounts {
                    let package = self.package_of_stem(&declaring);
                    unread_mounts.push((below, package, crates.clone(), false));
                }
            }
        }
        let mut unreadable_by_tree = HashMap::<String, Vec<String>>::new();
        let mut shared = HashMap::new();
        for path in self.files.values() {
            let Some(file) = self
                .module_file_of(path)
                .filter(|file| file.importer_root.is_none())
            else {
                continue;
            };
            let (_, roots) = self.declared_placement(&file);
            if roots.is_empty() {
                continue;
            }
            let Some(stem) = rust_file_stem(path) else {
                continue;
            };
            let foreign = |crates: &Vec<String>| crates.iter().any(|root| !roots.contains(root));
            // Whether each other crate that may compile the file keeps it in place.
            let mut reasons = Vec::new();
            let unreadable = unreadable_by_tree
                .entry(file.tree.module_dir.clone())
                .or_insert_with(|| self.unreadable_roots(&file.tree));
            for root in unreadable.iter() {
                let Some(below) = strip_dir(&stem, parent_dir(root)) else {
                    continue;
                };
                match self.scanned_roots.get(root) {
                    Some(Some(names)) => {
                        let top = below.split('/').next().unwrap_or(below);
                        if names.contains(module_name(top)) {
                            reasons.push(true);
                        }
                    }
                    // Its lines may hold a `path` attribute, which mounts a file anywhere.
                    Some(None) => reasons.push(false),
                    None => reasons.push(true),
                }
            }
            reasons.extend(
                mounted
                    .get(stem.as_str())
                    .into_iter()
                    .flatten()
                    .filter(|(crates, _)| foreign(crates))
                    .map(|(_, in_place)| *in_place),
            );
            let package = self.package_of(path);
            // A mount the index cannot read is read within its own package.
            for (_, declaring, crates, fired) in &mut unread_mounts {
                if *declaring == package && foreign(crates) {
                    *fired = true;
                    reasons.push(false);
                }
            }
            if !reasons.is_empty() {
                shared.insert(
                    importer_stem(&file),
                    reasons.iter().all(|in_place| *in_place),
                );
            }
        }
        SharedFiles {
            files: shared,
            // An attribute reached from several mounts is still one attribute.
            unread_mounts: unread_mounts
                .iter()
                .filter(|(_, _, _, fired)| *fired)
                .map(|(at, ..)| at)
                .collect::<HashSet<_>>()
                .len(),
        }
    }

    /// The extension-less paths of the indexed files `mount`, declared in `declaring`, may mount,
    /// or `None` for a `#[path]` the index cannot read, which may mount any file of its package.
    fn mounted_files(&self, declaring: &str, mount: &PathMount) -> Option<Vec<String>> {
        match mount {
            PathMount::File(file) => Some(vec![file.clone()]),
            PathMount::Below { .. } => {
                let package = self.package_of_stem(declaring);
                let mut files = self
                    .files_by_stem
                    .keys()
                    .filter(|stem| mount.may_mount(stem) && self.package_of_stem(stem) == package)
                    .cloned()
                    .collect::<Vec<_>>();
                files.sort();
                Some(files)
            }
            PathMount::Unknown => None,
        }
    }

    /// `mounted`, a file a `#[path]` attribute mounts, and the module files below it that its
    /// `mod name;` items declare, each with whether it keeps the place its file path spells. The
    /// mounted file's own items are read from its directory, as a `mod.rs` file's are, so a file
    /// other than a `mod.rs` is not in place, though the modules it declares are.
    ///
    /// A `#[path]` inside the subtree, including each alternative of a `cfg_attr(.., path = ..)`
    /// since the configuration is unknown, mounts its file into the mounting crate as well, read
    /// relative to the declaring file just as in its own crate.
    fn mounted_subtree(
        &self,
        mounted: &str,
        modules_by_file: &HashMap<&str, Vec<&str>>,
        mounts_by_file: &HashMap<&str, Vec<(usize, &PathMount)>>,
    ) -> MountedSubtree {
        let mut subtree = MountedSubtree {
            files: vec![(mounted.to_string(), is_mod_rs(mounted))],
            unread_mounts: Vec::new(),
        };
        let mut seen = HashSet::from([mounted.to_string()]);
        let mut pending = vec![(mounted.to_string(), parent_dir(mounted).to_string())];
        while let Some((file, dir)) = pending.pop() {
            for name in modules_by_file.get(file.as_str()).into_iter().flatten() {
                for child in [join_dir(&dir, name), join_dir(&dir, &format!("{name}/mod"))] {
                    if !self.files_by_stem.contains_key(&child) || !seen.insert(child.clone()) {
                        continue;
                    }
                    let child_dir = if is_mod_rs(&child) {
                        parent_dir(&child).to_string()
                    } else {
                        child.clone()
                    };
                    subtree.files.push((child.clone(), true));
                    pending.push((child, child_dir));
                }
            }
            for (at, mount) in mounts_by_file.get(file.as_str()).into_iter().flatten() {
                let Some(children) = self.mounted_files(&file, mount) else {
                    subtree.unread_mounts.push((*at, file.clone()));
                    continue;
                };
                for child in children {
                    if !self.files_by_stem.contains_key(&child) || !seen.insert(child.clone()) {
                        continue;
                    }
                    let child_dir = parent_dir(&child).to_string();
                    subtree.files.push((child.clone(), is_mod_rs(&child)));
                    pending.push((child, child_dir));
                }
            }
        }
        subtree
    }

    /// The crate roots of the crates `declaring`, an extension-less path, is compiled into as far
    /// as the indexed roots declare: itself for a crate root or a file of no known crate.
    fn crates_of(&self, declaring: &str) -> Vec<String> {
        let roots = self
            .files_by_stem
            .get(declaring)
            .and_then(|id| self.files.get(id))
            .and_then(|path| self.module_file_of(path))
            .filter(|file| file.importer_root.is_none())
            .map(|file| self.declared_placement(&file).1)
            .unwrap_or_default();
        if roots.is_empty() {
            vec![declaring.to_string()]
        } else {
            roots
        }
    }

    /// The directory of the package holding `path`, if a manifest is above it.
    fn package_of(&self, path: &Path) -> Option<&'a Path> {
        self.project
            .nearest_root_for(path, Language::Rust)
            .map(|root| root.path.as_path())
    }

    fn package_of_stem(&self, stem: &str) -> Option<&'a Path> {
        self.package_of(Path::new(&format!("{stem}.rs")))
    }

    /// `path` as a file of its package's crate module tree, if one holds it.
    fn module_file_of(&self, path: &Path) -> Option<RustUsePath> {
        map_rust_module_file(&self.crate_tree(path)?, path)
    }

    /// What the index could not place, for the `relationship_resolution` quality note. Counts
    /// only: a skipped root may be a path the index must not name.
    pub(crate) fn placement_gaps(&self) -> RustPlacementGaps {
        let mut unplaced_packages = HashSet::new();
        let mut unread_roots = HashSet::new();
        let mut withheld_files = 0;
        for path in self.files.values() {
            let package = self.package_layout(path);
            let file = self.module_file_of(path);
            let rootless = file
                .as_ref()
                .is_some_and(|file| self.indexed_crate_roots(file).is_empty());
            if !package.places_all_roots() || rootless {
                unplaced_packages.insert(package.src_root.clone());
            }
            let Some(file) = file.filter(|file| file.tree.is_src && file.importer_root.is_none())
            else {
                continue;
            };
            let unreadable = self.unreadable_roots(&file.tree);
            // An unfollowed root is not a module of the tree, so it is no file withheld from one.
            if unreadable.is_empty()
                || unreadable.contains(&importer_stem(&file))
                || rootless
                || self.declared_placement(&file).0
            {
                continue;
            }
            withheld_files += 1;
            unread_roots.extend(unreadable);
        }
        RustPlacementGaps {
            unplaced_packages: unplaced_packages.len(),
            unread_roots: unread_roots.len(),
            withheld_files,
            shared_files: self.shared_files().files.len(),
            unread_mounts: self.shared_files().unread_mounts,
        }
    }

    /// The indexed crate root files of the package holding `path`.
    fn indexed_crate_roots(&self, path: &RustUsePath) -> Vec<String> {
        path.module_file_stems(&[])
            .into_iter()
            .filter(|stem| self.files_by_stem.contains_key(stem))
            .collect()
    }

    /// Extension-less paths of the files that can hold `module` in the importer's crate.
    fn module_stems(&self, path: &RustUsePath, module: &[String]) -> Vec<String> {
        if module.is_empty() {
            self.crate_roots(path)
        } else {
            path.module_file_stems(module)
        }
    }

    /// The one indexed file holding `module`, when the module is declared as a file.
    fn module_file(&self, path: &RustUsePath, module: &[String]) -> Option<FileId> {
        if module.is_empty() || !self.declares_file_modules(path, module) {
            return None;
        }
        let mut found = path
            .module_file_stems(module)
            .into_iter()
            .filter_map(|stem| self.files_by_stem.get(&stem).cloned())
            .collect::<Vec<_>>();
        match found.len() {
            1 => found.pop(),
            _ => None,
        }
    }

    /// `module_file`, extended to the crate root itself: `use crate::*;`, and `use super::*;` from a
    /// top-level module, open the root module, whose file is `lib.rs` or `main.rs`.
    fn module_or_root_file(&self, path: &RustUsePath, module: &[String]) -> Option<FileId> {
        if !module.is_empty() {
            return self.module_file(path, module);
        }
        let mut roots = self
            .crate_roots(path)
            .into_iter()
            .filter_map(|stem| self.files_by_stem.get(&stem).cloned())
            .collect::<Vec<_>>();
        match roots.len() {
            1 => roots.pop(),
            _ => None,
        }
    }
}

/// Where the library crate root of the package at `root` sits, for paths through its crate name.
/// `None` when the layout does not follow the library's module tree.
fn library_placement(root: &ProjectRoot) -> Option<RustModulePlacement> {
    let layout = RustPackageLayout::of(Some(root));
    let library = layout.library_stem()?;
    let qualified = |stem: &str| stem.replace('/', "::");
    Some(RustModulePlacement {
        crate_dir: qualified(&layout.src_root),
        crate_roots: vec![qualified(&library)],
        module: Some(Vec::new()),
        in_other_crates: false,
        own_subtree_in_every_crate: true,
    })
}

/// The extension-less path of the file `path` was mapped from, for a file below its crate root.
fn importer_stem(path: &RustUsePath) -> String {
    join_dir(&path.tree.module_dir, &path.importer_module.join("/"))
}

/// Whether the extension-less path is a `mod.rs` file.
fn is_mod_rs(stem: &str) -> bool {
    stem == "mod" || stem.ends_with("/mod")
}

fn rust_file_stem(path: &Path) -> Option<String> {
    path.to_string_lossy()
        .replace('\\', "/")
        .strip_suffix(".rs")
        .map(str::to_string)
}

#[derive(Debug, Clone, Default)]
pub struct ImportRegistry {
    pub index: ImportIndex,
}

impl ImportRegistry {
    pub fn resolve_site<M: FileLookup>(&mut self, site: &ImportSite, file_map: &M) {
        let has_relative_prefix = site.source.starts_with("./") || site.source.starts_with("../");
        let known_internal_key = file_map.contains_key_candidate(&site.source);
        let origin = if has_relative_prefix || known_internal_key {
            ImportOrigin::Internal
        } else {
            ImportOrigin::Unknown
        };
        let target_file = file_map.unique_file(&site.source);
        self.insert_site(site, origin, target_file);
    }

    /// Records a site's bindings with no target. Rust sites take this path: the module-key map
    /// `resolve_site` reads is built from file paths without the owning crate, so
    /// `crate::auth::issue_token` in one workspace member would match another member's
    /// `auth/issue_token.rs`. `resolve_rust_imports` binds them instead.
    pub(crate) fn insert_unresolved_site(&mut self, site: &ImportSite) {
        self.insert_site(site, ImportOrigin::Unknown, None);
    }

    fn insert_site(
        &mut self,
        site: &ImportSite,
        origin: ImportOrigin,
        target_file: Option<FileId>,
    ) {
        let scope_id = site
            .scope_id
            .clone()
            .unwrap_or_else(|| open_kioku_core::ScopeId::new("global"));
        let binding = |local: &str, imported: &str| ImportBinding {
            file_id: site.file_id.clone(),
            scope_id: scope_id.clone(),
            local_name: local.to_string(),
            imported_name: imported.to_string(),
            source_module: site.source.clone(),
            resolved_module: None,
            target_file: target_file.clone(),
            target_symbol: None,
            origin,
            is_type_only: site.is_type_only,
            is_glob: site.is_glob,
            evidence: Vec::new(),
            rule: ImportBindingRule::ModuleKey,
        };
        if site.is_glob && site.bindings.is_empty() {
            // Recorded so name lookup can tell that a glob in a nearer scope may supply a name.
            self.index
                .insert(binding(GLOB_IMPORT_LOCAL_NAME, GLOB_IMPORT_LOCAL_NAME));
        }
        for imported in &site.bindings {
            self.index
                .insert(binding(&imported.local, &imported.imported));
        }
    }

    pub fn resolve_symbols<M: FileLookup>(
        &mut self,
        symbols: &open_kioku_resolution::SymbolIndex,
        module_to_file: &M,
    ) {
        self.resolve_symbols_skipping(symbols, module_to_file, &HashSet::new());
    }

    /// `resolve_symbols` for every binding outside `skip_files`.
    pub(crate) fn resolve_symbols_skipping<M: FileLookup>(
        &mut self,
        symbols: &open_kioku_resolution::SymbolIndex,
        module_to_file: &M,
        skip_files: &HashSet<FileId>,
    ) {
        for list in self.index.by_file_local_name.values_mut() {
            for binding in list
                .iter_mut()
                .filter(|binding| !skip_files.contains(&binding.file_id))
            {
                let target_file_id = binding
                    .target_file
                    .clone()
                    .or_else(|| module_to_file.unique_file(&binding.source_module))
                    .or_else(|| {
                        if binding.source_module.contains('.') {
                            let (pkg, cls) = binding.source_module.rsplit_once('.')?;
                            module_to_file
                                .unique_file(&format!("{pkg}.{cls}"))
                                .or_else(|| module_to_file.unique_file(pkg))
                        } else {
                            None
                        }
                    });

                if let Some(target_fid) = target_file_id {
                    binding.target_file = Some(target_fid.clone());
                    if let Some(file_syms) = symbols.by_file.get(&target_fid) {
                        let candidates: Vec<&open_kioku_core::SymbolId> = file_syms
                            .iter()
                            .filter(|id| {
                                symbols
                                    .get(id)
                                    .map(|s| s.name == binding.imported_name)
                                    .unwrap_or(false)
                            })
                            .collect();
                        if candidates.len() == 1 {
                            binding.target_symbol = Some(candidates[0].clone());
                        }
                    }
                } else if let Some(qualified) = symbols.by_qualified.get(&binding.source_module) {
                    if qualified.len() == 1 {
                        binding.target_symbol = Some(qualified[0].clone());
                    }
                }
            }
        }
    }

    /// Binds Rust imports by following their paths through the declared module tree of the
    /// importing file's crate.
    ///
    /// `crate::` starts at the `src/` of the nearest `Cargo.toml`; `self::` and `super::` start at
    /// the importer's module, which must itself be declared where its path says. Every module on
    /// the path must be declared as a file by the module above it.
    ///
    /// - A path naming a module file binds `target_file` (`use crate::auth;`).
    /// - Otherwise the parent path is the module and the last segment the item, bound when exactly
    ///   one module-level Rust item has that qualified name (`use crate::auth::issue_token;`).
    /// - A path naming both a module file and an item binds only the module: a call through it
    ///   cannot be told apart from a path into the module.
    ///
    /// Nothing is matched by bare name, so an extern-crate path, a method or nested item of the
    /// same name, and an item defined in both `x.rs` and `x/mod.rs` stay unbound. Bindings it sets
    /// carry `ImportBindingRule::RustModulePath`.
    pub(crate) fn resolve_rust_imports(
        &mut self,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
        modules: &RustModuleTree<'_>,
    ) {
        // Scope-aware resolution reads the file map and `ImportIndex::lookup` reads the scope map
        // first; each holds its own copy of a binding, so both copies are bound.
        let index = &mut self.index;
        for list in index
            .by_file_local_name
            .values_mut()
            .chain(index.by_scope_local_name.values_mut())
        {
            for binding in list.iter_mut().filter(|binding| !binding.is_glob) {
                let Some(importer) = modules.files.get(&binding.file_id) else {
                    continue;
                };
                let Some(target) = rust_import_target(binding, importer, symbols, scopes, modules)
                else {
                    continue;
                };
                binding.target_file = target.module_file;
                binding.target_symbol = target.item;
                binding.origin = ImportOrigin::Internal;
                binding.rule = if target.reexport {
                    ImportBindingRule::RustReexport
                } else {
                    ImportBindingRule::RustModulePath
                };
            }
        }
    }
}

fn rust_import_target(
    binding: &ImportBinding,
    importer: &Path,
    symbols: &open_kioku_resolution::SymbolIndex,
    scopes: &open_kioku_resolution::ScopeIndex,
    modules: &RustModuleTree<'_>,
) -> Option<RustPathTarget> {
    let (path, crate_name) = modules.rust_path(
        importer,
        Some(&binding.scope_id),
        &binding.source_module,
        scopes,
    )?;
    let (item_name, _) = path.segments.split_last()?;
    if *item_name != binding.imported_name {
        return None;
    }
    modules.rust_path_target(&path, crate_name, symbols, scopes, 0)
}

/// The module-level Rust items named `item` in the files at `module_stems`.
fn rust_module_items(
    module_stems: &[String],
    item: &str,
    symbols: &open_kioku_resolution::SymbolIndex,
) -> Vec<SymbolId> {
    let mut targets = module_stems
        .iter()
        .filter_map(|stem| {
            symbols
                .by_qualified
                .get(&format!("{}::{item}", stem.replace('/', "::")))
        })
        .flatten()
        .filter(|id| {
            symbols.get(id).is_some_and(|symbol| {
                symbol.language == Language::Rust
                    && symbol.parent_symbol_id.is_none()
                    && symbol.kind != SymbolKind::Module
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| left.0.cmp(&right.0));
    targets.dedup();
    targets
}

/// Where each Rust `use` path in a file points, for that file's `IMPORTS` edge.
///
/// A path inside the importing file's own crate is the module tree's to answer or nobody's: the
/// resolver must not match its text against repository paths or fall back to the crate root, which
/// pointed three of one file's imports at `src/lib.rs` with a binding proof.
#[derive(Debug, Default)]
pub struct RustImportEdgeTargets {
    /// Keyed by importing file and `use` path. A `None` value keeps a path of the importer's own
    /// crate that the module tree cannot answer, or that two sites disagree about, unresolved.
    in_crate: HashMap<(FileId, String), Option<RustImportEdge>>,
}

/// The file a Rust `use` path names, and the rule that reached it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustImportEdge {
    pub file: FileId,
    pub strategy: &'static str,
}

/// A path naming a module, or the module a glob opens, reaches that module's own file.
pub const RUST_MODULE_PATH_STRATEGY: &str = "rust-module-path";
/// A path naming an item reaches the file of the module that declares it.
pub const RUST_ITEM_MODULE_STRATEGY: &str = "rust-item-module";
/// A relative path written inside an inline `mod` block that cannot leave the file it is written
/// in. The file imports its own module, which is no dependency, so no edge is emitted.
pub const RUST_SELF_MODULE_STRATEGY: &str = "rust-self-module";
/// A path through a crate name whose item is reached by following that crate's `pub use`
/// re-exports reaches the file that defines the item, or the module file it names.
pub const RUST_REEXPORT_STRATEGY: &str = "rust-reexport";

impl RustImportEdgeTargets {
    /// The file `path` names in `file`, when the module tree proves one.
    pub fn target(&self, file: &FileId, path: &str) -> Option<&RustImportEdge> {
        self.in_crate
            .get(&(file.clone(), path.to_string()))?
            .as_ref()
    }

    /// Whether `path` is a path of `file`'s own crate, proven or not. An unproven one stays
    /// unresolved instead of falling back to a repository-path match.
    pub fn is_in_crate(&self, file: &FileId, path: &str) -> bool {
        self.in_crate
            .contains_key(&(file.clone(), path.to_string()))
    }

    /// Records what `path` names in `file`. Two sites spelling one path in one file must agree:
    /// `use super::*;` at file level and inside `mod tests` name different modules, and the stored
    /// import row cannot tell them apart, so a disagreement leaves the path unresolved.
    ///
    /// Public because `resolver::resolve_imports` requires these targets: a caller outside this
    /// crate that passes `Default::default()` gets "no path of any importer's own crate resolves",
    /// which is a degraded answer rather than an error, so the constructor must be reachable.
    pub fn record(&mut self, file: FileId, path: &str, edge: Option<RustImportEdge>) {
        match self.in_crate.entry((file, path.to_string())) {
            Entry::Occupied(mut recorded) => {
                if *recorded.get() != edge {
                    recorded.insert(None);
                }
            }
            Entry::Vacant(slot) => {
                slot.insert(edge);
            }
        }
    }
}

/// Follows every Rust `use` path through the declared module tree of the importing file's own
/// crate, or of a crate its package depends on, for the file-level `IMPORTS` edge.
///
/// - a path naming a declared module file reaches that file;
/// - a glob reaches the file of the module it opens;
/// - an item reaches the file of the module declaring it, which is the crate root only when the
///   item is declared there;
/// - an item a crate-name path reaches only through that crate's `pub use` re-exports reaches the
///   file declaring it, with its own strategy; an in-crate path through a re-export stays
///   unresolved;
/// - a path of the importer's own crate the tree cannot answer is left unresolved, including a
///   relative path written inside an inline `mod` block, whose module the importing file's path
///   cannot tell. A path into a dependency the tree cannot answer is not recorded, and resolves
///   as any other path naming another crate does.
///
/// Import sites are read rather than the stored import rows because only a site carries the scope
/// its path is written in.
pub(crate) fn rust_import_edge_targets(
    sites: &[ImportSite],
    symbols: &open_kioku_resolution::SymbolIndex,
    scopes: &open_kioku_resolution::ScopeIndex,
    modules: &RustModuleTree<'_>,
) -> RustImportEdgeTargets {
    let mut targets = RustImportEdgeTargets::default();
    for site in sites {
        let Some(importer) = modules.files.get(&site.file_id) else {
            continue;
        };
        let Some(root) = modules.project.nearest_root_for(importer, Language::Rust) else {
            continue;
        };
        let in_crate = is_rust_in_crate_path(&site.source, root.package_name.as_deref());
        if !in_crate && !modules.names_dependency(importer, &site.source) {
            continue;
        }
        let edge = if in_crate && rust_self_module_site(site, scopes) {
            Some(RustImportEdge {
                file: site.file_id.clone(),
                strategy: RUST_SELF_MODULE_STRATEGY,
            })
        } else {
            modules
                .rust_path(importer, site.scope_id.as_ref(), &site.source, scopes)
                .and_then(|(path, crate_name)| {
                    rust_import_edge(&path, crate_name, symbols, scopes, modules)
                })
        };
        if !in_crate && edge.is_none() {
            continue;
        }
        targets.record(site.file_id.clone(), &site.source, edge);
    }
    targets
}

/// Whether `source` names something in the crate of the file that writes it: `crate::`, `self::`,
/// `super::`, or the package's own crate name.
pub(crate) fn is_rust_in_crate_path(source: &str, package_name: Option<&str>) -> bool {
    let Some(first) = source.split("::").next() else {
        return false;
    };
    matches!(first, "crate" | "self" | "super")
        || package_name.is_some_and(|package| package.replace('-', "_") == first)
}

/// Whether `site` writes a relative path that cannot leave the file it is written in: `self::`,
/// or `super` repeated no more times than the inline `mod` blocks enclosing it.
///
/// In `mod tests { use super::*; }` the parent of `tests` is the module the file already is, so
/// the file imports itself. One more `super` than there are enclosing blocks climbs above the
/// file's own module and names another file, which the importing file's path cannot identify.
fn rust_self_module_site(site: &ImportSite, scopes: &open_kioku_resolution::ScopeIndex) -> bool {
    let Some(scope) = site.scope_id.as_ref() else {
        return false;
    };
    let depth = inline_module_depth(scope, scopes);
    if depth == 0 {
        return false;
    }
    let mut segments = site.source.split("::").peekable();
    let hops = match segments.peek() {
        Some(&"self") => {
            segments.next();
            0
        }
        Some(&"super") => {
            let mut hops = 0;
            while segments.peek() == Some(&"super") {
                segments.next();
                hops += 1;
            }
            hops
        }
        _ => return false,
    };
    // One hop per enclosing block lands on the file's own module; one more climbs past it.
    if hops > depth {
        return false;
    }
    // Only a path that stops at that module, or globs it, stays inside the file.
    // `super::helpers::Thing` names a module below it — a different file, and the module tree's
    // to answer. Treating it as self-referential dropped the edge and the absence with it.
    match segments.next() {
        None => true,
        Some("*") => segments.next().is_none(),
        Some(_) => false,
    }
}

fn rust_import_edge(
    path: &RustUsePath,
    follow_reexports: bool,
    symbols: &open_kioku_resolution::SymbolIndex,
    scopes: &open_kioku_resolution::ScopeIndex,
    modules: &RustModuleTree<'_>,
) -> Option<RustImportEdge> {
    let module_edge = |file| RustImportEdge {
        file,
        strategy: RUST_MODULE_PATH_STRATEGY,
    };
    let (last, parent) = path.segments.split_last()?;
    if last == "*" {
        return modules.module_or_root_file(path, parent).map(module_edge);
    }
    let target = modules.rust_path_target(path, follow_reexports, symbols, scopes, 0)?;
    let strategy = if target.reexport {
        RUST_REEXPORT_STRATEGY
    } else if target.module_file.is_some() {
        RUST_MODULE_PATH_STRATEGY
    } else {
        RUST_ITEM_MODULE_STRATEGY
    };
    // A path naming both a module file and an item names the module: the item is reached through
    // that module, not by this path.
    let file = match (target.module_file, target.item) {
        (Some(file), _) => file,
        (None, Some(item)) => symbols.get(&item)?.file_id.clone(),
        (None, None) => return None,
    };
    Some(RustImportEdge { file, strategy })
}

fn is_inside_inline_module(scope_id: &ScopeId, scopes: &open_kioku_resolution::ScopeIndex) -> bool {
    inline_module_depth(scope_id, scopes) > 0
}

/// How many inline `mod` blocks of the writing file enclose `scope_id`. `super` climbs one module
/// per hop, so a relative path with no more hops than this stays inside that file.
fn inline_module_depth(scope_id: &ScopeId, scopes: &open_kioku_resolution::ScopeIndex) -> usize {
    std::iter::successors(scopes.get(scope_id), |scope| {
        scope
            .parent_id
            .as_ref()
            .and_then(|parent| scopes.get(parent))
    })
    .take(scopes.scopes.len())
    .filter(|scope| matches!(scope.kind, ScopeKind::Module))
    .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, EvidenceSourceType, FileId, ImportSite, ImportedName, Language, RepositoryId,
        Scope, SourceRange, Symbol, SymbolId, SymbolKind,
    };
    use open_kioku_semantic_model::{
        CargoDependency, CargoDependencyKind, CargoManifest, CargoTargetKind, CargoTargets,
        ProjectRoot,
    };
    use std::path::PathBuf;

    fn one_file_map(key: &str, file_id: &str) -> FileMap {
        HashMap::from([(key.to_string(), vec![FileId::new(file_id)])])
    }

    #[test]
    fn resolves_internal_and_external_imports() {
        let mut registry = ImportRegistry::default();
        let file_map = one_file_map("@app/repo", "file:repo.ts");

        let site = ImportSite {
            file_id: FileId::new("file:main.ts"),
            scope_id: None,
            source: "@app/repo".into(),
            bindings: vec![ImportedName {
                imported: "Repository".into(),
                local: "Repo".into(),
            }],
            is_glob: false,
            is_type_only: false,
            reexported: false,
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 40,
            },
        };

        registry.resolve_site(&site, &file_map);
        let lookups = registry
            .index
            .lookup(&FileId::new("file:main.ts"), None, "Repo");
        assert_eq!(lookups.len(), 1);
        assert_eq!(lookups[0].local_name, "Repo");
        assert_eq!(lookups[0].origin, ImportOrigin::Internal);
        assert_eq!(lookups[0].target_file, Some(FileId::new("file:repo.ts")));
    }

    #[test]
    fn ambiguous_internal_module_key_fails_closed() {
        let mut registry = ImportRegistry::default();
        let file_map = HashMap::from([(
            "service".to_string(),
            vec![
                FileId::new("file:a/service.py"),
                FileId::new("file:b/service.py"),
            ],
        )]);
        let site = ImportSite {
            file_id: FileId::new("file:consumer.py"),
            scope_id: None,
            source: "service".into(),
            bindings: vec![ImportedName {
                imported: "run".into(),
                local: "run".into(),
            }],
            is_glob: false,
            is_type_only: false,
            reexported: false,
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 24,
            },
        };

        registry.resolve_site(&site, &file_map);
        let binding = registry
            .index
            .lookup(&FileId::new("file:consumer.py"), None, "run")[0];
        assert_eq!(binding.origin, ImportOrigin::Internal);
        assert_eq!(binding.target_file, None);
    }

    #[test]
    fn legacy_single_value_map_remains_supported_during_indexer_migration() {
        let mut registry = ImportRegistry::default();
        let file_map = HashMap::from([("@app/repo".to_string(), FileId::new("file:repo.ts"))]);
        let site = ImportSite {
            file_id: FileId::new("file:main.ts"),
            scope_id: None,
            source: "@app/repo".into(),
            bindings: vec![ImportedName {
                imported: "Repository".into(),
                local: "Repo".into(),
            }],
            is_glob: false,
            is_type_only: false,
            reexported: false,
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 40,
            },
        };
        registry.resolve_site(&site, &file_map);
        assert_eq!(
            registry
                .index
                .lookup(&FileId::new("file:main.ts"), None, "Repo")[0]
                .target_file,
            Some(FileId::new("file:repo.ts"))
        );
    }

    #[test]
    fn unresolved_external_import_with_unique_internal_type_remains_unresolved() {
        let mut registry = ImportRegistry::default();
        let file_map = FileMap::new(); // External package not in file_map

        let site = ImportSite {
            file_id: FileId::new("file:app.ts"),
            scope_id: None,
            source: "@vendor/unrelated-pkg".into(),
            bindings: vec![ImportedName {
                imported: "Repository".into(),
                local: "Repository".into(),
            }],
            is_glob: false,
            is_type_only: false,
            reexported: false,
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 50,
            },
        };

        registry.resolve_site(&site, &file_map);

        // Suppose the repo happens to have an unrelated internal symbol named "Repository"
        let internal_sym = Symbol {
            id: SymbolId::new("sym:internal:repo"),
            name: "Repository".into(),
            qualified_name: "src/internal::Repository".into(),
            kind: SymbolKind::Class,
            file_id: FileId::new("file:internal/repo.ts"),
            range: None,
            language: Language::TypeScript,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Public,
        };

        let symbol_index = open_kioku_resolution::SymbolIndex::build(vec![internal_sym]);
        registry.resolve_symbols(&symbol_index, &file_map);

        let lookups = registry
            .index
            .lookup(&FileId::new("file:app.ts"), None, "Repository");
        assert_eq!(lookups.len(), 1);
        assert_eq!(
            lookups[0].target_symbol, None,
            "External import must NOT resolve to unrelated unique internal symbol"
        );
    }

    #[test]
    fn glob_import_is_recorded_under_the_glob_local_name() {
        let mut registry = ImportRegistry::default();
        let site = ImportSite {
            is_glob: true,
            bindings: Vec::new(),
            ..rust_use_site("src/auth.rs", "crate::fakes::*", "*", Some("scope:tests"))
        };
        registry.insert_unresolved_site(&site);
        let globs = registry.index.lookup(
            &FileId::new("file:src/auth.rs"),
            None,
            GLOB_IMPORT_LOCAL_NAME,
        );
        assert_eq!(globs.len(), 1);
        assert!(globs[0].is_glob);
        assert_eq!(globs[0].scope_id, ScopeId::new("scope:tests"));
    }

    fn source_file(path: &str) -> File {
        File {
            id: FileId::new(format!("file:{path}")),
            repository_id: RepositoryId::new("repo"),
            path: PathBuf::from(path),
            language: if path.ends_with(".py") {
                Language::Python
            } else {
                Language::Rust
            },
            size_bytes: 0,
            content_hash: "hash".into(),
            is_generated: false,
            is_vendor: false,
        }
    }

    fn rust_symbol(path: &str, name: &str) -> Symbol {
        let stem = path
            .trim_end_matches(".rs")
            .trim_end_matches(".py")
            .replace('/', "::");
        Symbol {
            id: SymbolId::new(format!("symbol:{path}:{name}")),
            name: name.into(),
            qualified_name: format!("{stem}::{name}"),
            kind: SymbolKind::Function,
            file_id: FileId::new(format!("file:{path}")),
            range: None,
            language: Language::Rust,
            confidence: Confidence::High,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: open_kioku_core::Visibility::Public,
        }
    }

    /// `mod name;` in `file`: no body, no `path` attribute, at file scope.
    fn mod_decl(file: &str, name: &str) -> ModuleDeclarationSite {
        ModuleDeclarationSite {
            file_id: FileId::new(format!("file:{file}")),
            scope_id: None,
            name: name.into(),
            has_body: false,
            has_path_attribute: false,
            path_attributes: Vec::new(),
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 20,
            },
        }
    }

    /// The site the parser emits for one imported path of a Rust `use` declaration.
    fn rust_use_site(importer: &str, source: &str, local: &str, scope: Option<&str>) -> ImportSite {
        ImportSite {
            file_id: FileId::new(format!("file:{importer}")),
            scope_id: scope.map(ScopeId::new),
            source: source.into(),
            bindings: vec![ImportedName {
                imported: source.rsplit("::").next().unwrap().into(),
                local: local.into(),
            }],
            is_glob: false,
            is_type_only: false,
            reexported: false,
            range: SourceRange {
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 40,
            },
        }
    }

    fn inline_module_scope(id: &str, file: &str) -> Scope {
        Scope {
            id: ScopeId::new(id),
            file_id: FileId::new(format!("file:{file}")),
            parent_id: None,
            owner_symbol_id: None,
            kind: ScopeKind::Module,
            range: SourceRange {
                start_line: 10,
                start_column: 1,
                end_line: 20,
                end_column: 1,
            },
        }
    }

    /// Runs the registry the way indexing does over `files` in packages rooted at `manifests`
    /// (directories holding a `Cargo.toml`, `""` for the repository root).
    fn bind_rust_imports(
        files: &[&str],
        manifests: &[&str],
        declarations: Vec<ModuleDeclarationSite>,
        sites: &[ImportSite],
        symbols: Vec<Symbol>,
        scopes: Vec<Scope>,
    ) -> ImportRegistry {
        bind_rust_imports_with_file_map(
            files,
            manifests,
            declarations,
            sites,
            symbols,
            scopes,
            FileMap::new(),
        )
    }

    fn bind_rust_imports_with_file_map(
        files: &[&str],
        manifests: &[&str],
        declarations: Vec<ModuleDeclarationSite>,
        sites: &[ImportSite],
        symbols: Vec<Symbol>,
        scopes: Vec<Scope>,
        file_map: FileMap,
    ) -> ImportRegistry {
        let files = files.iter().copied().map(source_file).collect::<Vec<_>>();
        let rust_files = files
            .iter()
            .filter(|file| file.language == Language::Rust)
            .map(|file| file.id.clone())
            .collect::<HashSet<_>>();
        let mut registry = ImportRegistry::default();
        for site in sites {
            if rust_files.contains(&site.file_id) {
                registry.insert_unresolved_site(site);
            } else {
                registry.resolve_site(site, &file_map);
            }
        }
        let symbols = open_kioku_resolution::SymbolIndex::build(symbols);
        registry.resolve_symbols_skipping(&symbols, &file_map, &rust_files);
        let mut project = ProjectModel::new();
        project
            .roots
            .extend(manifests.iter().map(|dir| ProjectRoot {
                path: PathBuf::from(*dir),
                language: Language::Rust,
                package_name: None,
                source_roots: Vec::new(),
                library_root: None,
                cargo_targets: Default::default(),
                cargo_manifest: None,
            }));
        let scopes = open_kioku_resolution::ScopeIndex::build(scopes);
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        registry.resolve_rust_imports(&symbols, &scopes, &modules);
        registry
    }

    fn binding<'r>(registry: &'r ImportRegistry, importer: &str, local: &str) -> &'r ImportBinding {
        let lookups = registry
            .index
            .lookup(&FileId::new(format!("file:{importer}")), None, local);
        assert_eq!(lookups.len(), 1, "one `{local}` binding in {importer}");
        lookups[0]
    }

    fn bound_target(registry: &ImportRegistry, importer: &str, local: &str) -> Option<String> {
        binding(registry, importer, local)
            .target_symbol
            .as_ref()
            .map(|id| id.0.clone())
    }

    #[test]
    fn rust_item_import_binds_the_module_level_symbol_its_parent_path_names() {
        let registry = bind_rust_imports(
            &["src/lib.rs", "src/auth.rs", "src/session.rs"],
            &[""],
            vec![
                mod_decl("src/lib.rs", "auth"),
                mod_decl("src/lib.rs", "session"),
            ],
            &[rust_use_site(
                "src/session.rs",
                "crate::auth::issue_token",
                "issue_token",
                None,
            )],
            vec![rust_symbol("src/auth.rs", "issue_token")],
            Vec::new(),
        );
        assert_eq!(
            bound_target(&registry, "src/session.rs", "issue_token").as_deref(),
            Some("symbol:src/auth.rs:issue_token")
        );
        let bound = binding(&registry, "src/session.rs", "issue_token");
        assert_eq!(bound.origin, ImportOrigin::Internal);
        assert_eq!(bound.rule, ImportBindingRule::RustModulePath);
        assert_eq!(bound.target_file, None);
    }

    #[test]
    fn rust_crate_import_follows_only_the_root_of_the_importers_own_crate() {
        // A package with both roots: `lib.rs` declares `auth` and `session`, `main.rs` declares
        // `cli`, and each root defines its own `run`.
        let registry = bind_rust_imports(
            &[
                "src/lib.rs",
                "src/main.rs",
                "src/auth.rs",
                "src/session.rs",
                "src/cli.rs",
            ],
            &[""],
            vec![
                mod_decl("src/lib.rs", "auth"),
                mod_decl("src/lib.rs", "session"),
                mod_decl("src/main.rs", "cli"),
            ],
            &[
                rust_use_site(
                    "src/session.rs",
                    "crate::auth::issue_token",
                    "issue_token",
                    None,
                ),
                rust_use_site(
                    "src/cli.rs",
                    "crate::auth::issue_token",
                    "issue_token",
                    None,
                ),
                rust_use_site(
                    "src/main.rs",
                    "crate::auth::issue_token",
                    "issue_token",
                    None,
                ),
                rust_use_site("src/cli.rs", "crate::run", "run", None),
                rust_use_site("src/session.rs", "crate::run", "run", None),
            ],
            vec![
                rust_symbol("src/auth.rs", "issue_token"),
                rust_symbol("src/lib.rs", "run"),
                rust_symbol("src/main.rs", "run"),
            ],
            Vec::new(),
        );

        assert_eq!(
            bound_target(&registry, "src/session.rs", "issue_token").as_deref(),
            Some("symbol:src/auth.rs:issue_token")
        );
        assert_eq!(
            bound_target(&registry, "src/cli.rs", "issue_token"),
            None,
            "the binary crate does not declare `auth`"
        );
        assert_eq!(
            bound_target(&registry, "src/main.rs", "issue_token"),
            None,
            "`main.rs` is the binary crate's root"
        );
        assert_eq!(
            bound_target(&registry, "src/cli.rs", "run").as_deref(),
            Some("symbol:src/main.rs:run")
        );
        assert_eq!(
            bound_target(&registry, "src/session.rs", "run").as_deref(),
            Some("symbol:src/lib.rs:run")
        );
    }

    #[test]
    fn rust_module_import_binds_the_declared_module_file() {
        let registry = bind_rust_imports(
            &["src/lib.rs", "src/auth.rs", "src/session.rs"],
            &[""],
            vec![mod_decl("src/lib.rs", "auth")],
            &[rust_use_site("src/session.rs", "crate::auth", "auth", None)],
            Vec::new(),
            Vec::new(),
        );
        let bound = binding(&registry, "src/session.rs", "auth");
        assert_eq!(bound.target_file, Some(FileId::new("file:src/auth.rs")));
        assert_eq!(bound.target_symbol, None);
        assert_eq!(bound.rule, ImportBindingRule::RustModulePath);

        let undeclared = bind_rust_imports(
            &["src/lib.rs", "src/auth.rs", "src/session.rs"],
            &[""],
            Vec::new(),
            &[rust_use_site("src/session.rs", "crate::auth", "auth", None)],
            Vec::new(),
            Vec::new(),
        );
        let unbound = binding(&undeclared, "src/session.rs", "auth");
        assert_eq!(unbound.target_file, None);
        assert_eq!(unbound.rule, ImportBindingRule::ModuleKey);
    }

    #[test]
    fn rust_import_naming_both_a_submodule_and_an_item_binds_no_call_target() {
        // `auth/mod.rs` declares `pub mod target_fn;` and `pub fn target_fn() {}`.
        let registry = bind_rust_imports(
            &[
                "src/lib.rs",
                "src/auth/mod.rs",
                "src/auth/target_fn.rs",
                "src/caller.rs",
            ],
            &[""],
            vec![
                mod_decl("src/lib.rs", "auth"),
                mod_decl("src/auth/mod.rs", "target_fn"),
            ],
            &[rust_use_site(
                "src/caller.rs",
                "crate::auth::target_fn",
                "target_fn",
                None,
            )],
            vec![rust_symbol("src/auth/mod.rs", "target_fn")],
            Vec::new(),
        );
        let bound = binding(&registry, "src/caller.rs", "target_fn");
        assert_eq!(bound.target_symbol, None);
        assert_eq!(
            bound.target_file,
            Some(FileId::new("file:src/auth/target_fn.rs"))
        );
    }

    #[test]
    fn rust_imports_ignore_the_crate_unaware_module_key_map() {
        // The indexing module-key map keys `crates/a/src/auth/issue_token.rs` as
        // `crate::auth::issue_token`, the same text crate `b` imports.
        let files = [
            "crates/a/src/lib.rs",
            "crates/a/src/auth/mod.rs",
            "crates/a/src/auth/issue_token.rs",
            "crates/b/src/lib.rs",
            "crates/b/src/auth.rs",
            "crates/b/src/session.rs",
        ];
        let declarations = || {
            vec![
                mod_decl("crates/a/src/lib.rs", "auth"),
                mod_decl("crates/a/src/auth/mod.rs", "issue_token"),
                mod_decl("crates/b/src/lib.rs", "auth"),
                mod_decl("crates/b/src/lib.rs", "session"),
            ]
        };
        let site = [rust_use_site(
            "crates/b/src/session.rs",
            "crate::auth::issue_token",
            "issue_token",
            None,
        )];
        let file_map = || {
            one_file_map(
                "crate::auth::issue_token",
                "file:crates/a/src/auth/issue_token.rs",
            )
        };
        let crate_a_item = || rust_symbol("crates/a/src/auth/issue_token.rs", "issue_token");

        let with_crate_b_item = bind_rust_imports_with_file_map(
            &files,
            &["crates/a", "crates/b"],
            declarations(),
            &site,
            vec![
                crate_a_item(),
                rust_symbol("crates/b/src/auth.rs", "issue_token"),
            ],
            Vec::new(),
            file_map(),
        );
        assert_eq!(
            bound_target(&with_crate_b_item, "crates/b/src/session.rs", "issue_token").as_deref(),
            Some("symbol:crates/b/src/auth.rs:issue_token")
        );

        let without_crate_b_item = bind_rust_imports_with_file_map(
            &files,
            &["crates/a", "crates/b"],
            declarations(),
            &site,
            vec![crate_a_item()],
            Vec::new(),
            file_map(),
        );
        let unbound = binding(
            &without_crate_b_item,
            "crates/b/src/session.rs",
            "issue_token",
        );
        assert_eq!(unbound.target_symbol, None);
        assert_eq!(unbound.target_file, None);
    }

    #[test]
    fn rust_grouped_item_import_binds_each_path_to_its_own_symbol() {
        // `use crate::auth::{issue_token, Token};` reaches the registry as one site per path.
        let registry = bind_rust_imports(
            &["src/lib.rs", "src/auth.rs", "src/session.rs"],
            &[""],
            vec![mod_decl("src/lib.rs", "auth")],
            &[
                rust_use_site(
                    "src/session.rs",
                    "crate::auth::issue_token",
                    "issue_token",
                    None,
                ),
                rust_use_site("src/session.rs", "crate::auth::Token", "Token", None),
            ],
            vec![
                rust_symbol("src/auth.rs", "issue_token"),
                Symbol {
                    kind: SymbolKind::Class,
                    ..rust_symbol("src/auth.rs", "Token")
                },
            ],
            Vec::new(),
        );
        assert_eq!(
            bound_target(&registry, "src/session.rs", "issue_token").as_deref(),
            Some("symbol:src/auth.rs:issue_token")
        );
        assert_eq!(
            bound_target(&registry, "src/session.rs", "Token").as_deref(),
            Some("symbol:src/auth.rs:Token")
        );
    }

    #[test]
    fn rust_aliased_item_import_binds_the_alias_to_the_item() {
        let registry = bind_rust_imports(
            &["src/lib.rs", "src/auth.rs", "src/session.rs"],
            &[""],
            vec![mod_decl("src/lib.rs", "auth")],
            &[rust_use_site(
                "src/session.rs",
                "crate::auth::issue_token",
                "mint",
                None,
            )],
            vec![rust_symbol("src/auth.rs", "issue_token")],
            Vec::new(),
        );
        assert_eq!(
            bound_target(&registry, "src/session.rs", "mint").as_deref(),
            Some("symbol:src/auth.rs:issue_token")
        );
        assert!(registry
            .index
            .lookup(&FileId::new("file:src/session.rs"), None, "issue_token")
            .is_empty());
    }

    #[test]
    fn rust_self_and_super_item_imports_follow_declared_modules_from_the_importer() {
        let registry = bind_rust_imports(
            &[
                "src/lib.rs",
                "src/auth/mod.rs",
                "src/auth/keys.rs",
                "src/session.rs",
            ],
            &[""],
            vec![
                mod_decl("src/lib.rs", "auth"),
                mod_decl("src/lib.rs", "session"),
                mod_decl("src/auth/mod.rs", "keys"),
            ],
            &[
                rust_use_site("src/lib.rs", "self::session::open", "open", None),
                rust_use_site("src/auth/mod.rs", "self::keys::rotate", "rotate", None),
                rust_use_site(
                    "src/auth/keys.rs",
                    "super::issue_token",
                    "issue_token",
                    None,
                ),
                rust_use_site(
                    "src/auth/keys.rs",
                    "super::super::session::open",
                    "open",
                    None,
                ),
            ],
            vec![
                rust_symbol("src/session.rs", "open"),
                rust_symbol("src/auth/keys.rs", "rotate"),
                rust_symbol("src/auth/mod.rs", "issue_token"),
            ],
            Vec::new(),
        );
        assert_eq!(
            bound_target(&registry, "src/lib.rs", "open").as_deref(),
            Some("symbol:src/session.rs:open")
        );
        assert_eq!(
            bound_target(&registry, "src/auth/mod.rs", "rotate").as_deref(),
            Some("symbol:src/auth/keys.rs:rotate")
        );
        assert_eq!(
            bound_target(&registry, "src/auth/keys.rs", "issue_token").as_deref(),
            Some("symbol:src/auth/mod.rs:issue_token")
        );
        assert_eq!(
            bound_target(&registry, "src/auth/keys.rs", "open").as_deref(),
            Some("symbol:src/session.rs:open")
        );
    }

    #[test]
    fn module_placements_place_only_files_the_module_tree_declares_at_their_path() {
        // `w.rs` mounts `elsewhere.rs` as `w::pathed` with `#[path]`, so neither `elsewhere.rs`
        // nor the `w/pathed.rs` at the default location is the module its path spells. `deep` is
        // declared inside an inline `mod inner` and `orphan.rs` by nothing.
        let files = [
            "src/lib.rs",
            "src/w.rs",
            "src/w/child.rs",
            "src/w/pathed.rs",
            "src/elsewhere.rs",
            "src/nest.rs",
            "src/nest/inner/deep.rs",
            "src/orphan.rs",
            "src/bin/tool.rs",
            "tests/it.rs",
        ]
        .map(source_file);
        let mut project = ProjectModel::new();
        project.roots.push(ProjectRoot {
            path: PathBuf::new(),
            language: Language::Rust,
            package_name: None,
            source_roots: Vec::new(),
            library_root: None,
            cargo_targets: Default::default(),
            cargo_manifest: None,
        });
        let declarations = vec![
            mod_decl("src/lib.rs", "w"),
            mod_decl("src/lib.rs", "nest"),
            mod_decl("src/w.rs", "child"),
            ModuleDeclarationSite {
                has_path_attribute: true,
                path_attributes: Vec::new(),
                ..mod_decl("src/w.rs", "pathed")
            },
            ModuleDeclarationSite {
                scope_id: Some(ScopeId::new("scope:nest:inner")),
                ..mod_decl("src/nest.rs", "deep")
            },
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(vec![inline_module_scope(
            "scope:nest:inner",
            "src/nest.rs",
        )]);
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);

        let placements = modules.module_placements();
        let mut unplaced = placements
            .iter()
            .filter(|(_, placement)| placement.module.is_none())
            .map(|(file, _)| file.0.as_str())
            .collect::<Vec<_>>();
        unplaced.sort();
        assert_eq!(
            unplaced,
            vec![
                "file:src/elsewhere.rs",
                "file:src/nest/inner/deep.rs",
                "file:src/orphan.rs",
                "file:src/w/pathed.rs",
            ]
        );
        let child = &placements[&FileId::new("file:src/w/child.rs")];
        assert_eq!(child.crate_dir, "src");
        assert_eq!(child.crate_roots, vec!["src::lib"]);
        assert_eq!(
            child.module,
            Some(vec!["w".to_string(), "child".to_string()])
        );
        // An unplaced file of the tree still reads `crate::` against its package's roots.
        let orphan = &placements[&FileId::new("file:src/orphan.rs")];
        assert_eq!(orphan.crate_roots, vec!["src::lib"]);
        // A binary under `src/bin/` and an integration test are crate roots of trees of their own.
        for (root, dir) in [("src/bin/tool", "src::bin"), ("tests/it", "tests")] {
            let placement = &placements[&FileId::new(format!("file:{root}.rs"))];
            assert_eq!(placement.crate_dir, dir);
            assert_eq!(placement.crate_roots, vec![root.replace('/', "::")]);
            assert_eq!(placement.module, Some(Vec::new()));
        }
        assert_eq!(modules.placement_gaps().unplaced_packages, 0);

        // Without a `Cargo.toml` the files are read against the top-level `src/`.
        let no_manifest = ProjectModel::new();
        let bare = RustModuleTree::new(&files, &no_manifest, &declarations, &scopes);
        assert_eq!(bare.module_placements(), placements);
    }

    fn rust_project(roots: &[(&str, Option<&str>)]) -> ProjectModel {
        let mut project = ProjectModel::new();
        project
            .roots
            .extend(roots.iter().map(|(dir, library)| ProjectRoot {
                path: PathBuf::from(*dir),
                language: Language::Rust,
                package_name: None,
                source_roots: Vec::new(),
                library_root: library.map(PathBuf::from),
                cargo_targets: Default::default(),
                cargo_manifest: None,
            }));
        project
    }

    #[test]
    fn module_placements_of_a_workspace_member_are_in_its_own_crate() {
        // A root package that is also a workspace, with member `crates/a`: both declare `util`.
        let files = [
            "src/lib.rs",
            "src/util.rs",
            "crates/a/src/lib.rs",
            "crates/a/src/util.rs",
        ]
        .map(source_file);
        let project = rust_project(&[("", None), ("crates/a", None)]);
        let declarations = vec![
            mod_decl("src/lib.rs", "util"),
            mod_decl("crates/a/src/lib.rs", "util"),
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();

        let member = &placements[&FileId::new("file:crates/a/src/util.rs")];
        assert_eq!(member.crate_dir, "crates::a::src");
        assert_eq!(member.crate_roots, vec!["crates::a::src::lib"]);
        assert_eq!(member.module, Some(vec!["util".to_string()]));
        let root = &placements[&FileId::new("file:crates/a/src/lib.rs")];
        assert_eq!(root.crate_roots, vec!["crates::a::src::lib"]);
        assert_eq!(root.module, Some(Vec::new()));
        assert_eq!(
            placements[&FileId::new("file:src/util.rs")].crate_dir,
            "src"
        );
    }

    #[test]
    fn module_placements_of_binaries_and_integration_tests_follow_their_own_roots() {
        // `src/bin/tool.rs` declares `helpers` (in `src/bin/helpers/mod.rs`, beside it) and
        // `src/bin/multi/main.rs` declares `inner` (in `src/bin/multi/`). `src/bin/tool/sub.rs`
        // is where rustc does not look for `tool`'s `mod sub;`. `tests/it.rs` declares `common`.
        let files = [
            "src/lib.rs",
            "src/bin/tool.rs",
            "src/bin/helpers/mod.rs",
            "src/bin/tool/sub.rs",
            "src/bin/multi/main.rs",
            "src/bin/multi/inner.rs",
            "tests/it.rs",
            "tests/common/mod.rs",
        ]
        .map(source_file);
        let project = rust_project(&[("", None)]);
        let declarations = vec![
            mod_decl("src/bin/tool.rs", "helpers"),
            mod_decl("src/bin/tool.rs", "sub"),
            mod_decl("src/bin/multi/main.rs", "inner"),
            mod_decl("tests/it.rs", "common"),
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();
        let placed = |file: &str| {
            let placement = &placements[&FileId::new(format!("file:{file}"))];
            (
                placement.crate_dir.as_str(),
                placement.crate_roots.clone(),
                placement.module.clone(),
            )
        };

        assert_eq!(
            placed("src/bin/helpers/mod.rs"),
            (
                "src::bin",
                vec!["src::bin::tool".to_string()],
                Some(vec!["helpers".to_string()])
            )
        );
        assert_eq!(
            placed("src/bin/multi/inner.rs"),
            (
                "src::bin::multi",
                vec!["src::bin::multi::main".to_string()],
                Some(vec!["inner".to_string()])
            )
        );
        assert_eq!(
            placed("tests/common/mod.rs"),
            (
                "tests",
                vec!["tests::it".to_string()],
                Some(vec!["common".to_string()])
            )
        );
        assert!(!placements.contains_key(&FileId::new("file:src/bin/tool/sub.rs")));
        assert_eq!(modules.placement_gaps().unplaced_packages, 0);
    }

    #[test]
    fn unplaced_files_of_a_target_directory_are_read_against_no_crate() {
        // `tests/a.rs` mounts `tests/support/util.rs` with `#[path]`; `tests/c.rs` is another
        // test crate. `crate::` in `util.rs` is `a`'s crate, which no placement can tell from
        // `c`'s, so the file is not recorded. In `src/` an unplaced file keeps the package's roots.
        let files = [
            "src/lib.rs",
            "src/stray.rs",
            "tests/a.rs",
            "tests/c.rs",
            "tests/support/util.rs",
        ]
        .map(source_file);
        let project = rust_project(&[("", None)]);
        let declarations = vec![ModuleDeclarationSite {
            has_path_attribute: true,
            path_attributes: Vec::new(),
            ..mod_decl("tests/a.rs", "util")
        }];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();

        assert!(!placements.contains_key(&FileId::new("file:tests/support/util.rs")));
        let stray = &placements[&FileId::new("file:src/stray.rs")];
        assert_eq!(stray.module, None);
        assert_eq!(stray.crate_roots, vec!["src::lib"]);
        assert_eq!(
            placements[&FileId::new("file:tests/c.rs")].crate_roots,
            vec!["tests::c"]
        );
    }

    #[test]
    fn module_placements_follow_raw_identifiers_and_a_library_root_in_src() {
        // `pub mod r#type;` is `type.rs`, and `[lib] path = "src/mylib.rs"` is the crate root.
        let files = ["src/mylib.rs", "src/type.rs", "src/a.rs"].map(source_file);
        let project = rust_project(&[("", Some("src/mylib.rs"))]);
        let declarations = vec![
            mod_decl("src/mylib.rs", "r#type"),
            mod_decl("src/mylib.rs", "a"),
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();

        assert_eq!(
            placements[&FileId::new("file:src/type.rs")].module,
            Some(vec!["type".to_string()])
        );
        let a = &placements[&FileId::new("file:src/a.rs")];
        assert_eq!(a.crate_roots, vec!["src::mylib"]);
        assert_eq!(a.module, Some(vec!["a".to_string()]));
        assert_eq!(
            placements[&FileId::new("file:src/mylib.rs")].module,
            Some(Vec::new())
        );
        assert_eq!(modules.placement_gaps().unplaced_packages, 0);
    }

    #[test]
    fn packages_whose_crate_root_cannot_be_placed_are_counted() {
        // `a` has no indexed `lib.rs` (skipped as oversized, say), `b` sets `[lib] path` outside
        // `src/`, and `c` is placed.
        let files = [
            "crates/a/src/util.rs",
            "crates/b/lib.rs",
            "crates/b/util.rs",
            "crates/c/src/lib.rs",
            "crates/c/src/util.rs",
        ]
        .map(source_file);
        let project = rust_project(&[
            ("crates/a", None),
            ("crates/b", Some("crates/b/lib.rs")),
            ("crates/c", None),
        ]);
        let declarations = vec![
            mod_decl("crates/b/lib.rs", "util"),
            mod_decl("crates/c/src/lib.rs", "util"),
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);

        assert_eq!(modules.placement_gaps().unplaced_packages, 2);
        let placements = modules.module_placements();
        let mut placed = placements
            .keys()
            .map(|file| file.0.as_str())
            .collect::<Vec<_>>();
        placed.sort();
        assert_eq!(
            placed,
            vec!["file:crates/c/src/lib.rs", "file:crates/c/src/util.rs"]
        );
    }

    #[test]
    fn a_file_no_indexed_root_declares_is_read_against_no_crate_when_a_src_root_was_skipped() {
        // `main.rs` (over `max_file_size`, say) declares `cli`; `lib.rs` declares `auth`. Read
        // against `lib.rs`, `crate::helper` in `cli.rs` would name the library's `helper`, but
        // rustc compiles `cli.rs` into the binary alone.
        let files = ["src/lib.rs", "src/auth.rs", "src/cli.rs"].map(source_file);
        let project = rust_project(&[("", None)]);
        let declarations = vec![mod_decl("src/lib.rs", "auth")];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let skipped = [Path::new("src/main.rs"), Path::new("README.md")];
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes)
            .with_unindexed_files(skipped);
        let placements = modules.module_placements();

        assert!(!placements.contains_key(&FileId::new("file:src/cli.rs")));
        // A file the indexed root declares is still that crate's, but `main.rs` may declare it
        // too, so paths are not read from it (#572).
        let auth = &placements[&FileId::new("file:src/auth.rs")];
        assert_eq!(auth.crate_roots, vec!["src::lib"]);
        assert_eq!(auth.module, Some(vec!["auth".to_string()]));
        assert!(auth.in_other_crates);
        assert_eq!(
            modules.placement_gaps(),
            RustPlacementGaps {
                unplaced_packages: 0,
                unread_roots: 1,
                withheld_files: 1,
                shared_files: 1,
                unread_mounts: 0,
            }
        );

        // With nothing skipped the file is read against the package's indexed root, as before.
        let indexed = RustModuleTree::new(&files, &project, &declarations, &scopes);
        assert_eq!(
            indexed.module_placements()[&FileId::new("file:src/cli.rs")].crate_roots,
            vec!["src::lib"]
        );
        assert_eq!(indexed.placement_gaps(), RustPlacementGaps::default());
    }

    #[test]
    fn a_crate_import_from_a_file_no_indexed_root_declares_stays_unbound_when_a_root_is_skipped() {
        let files = [
            "src/lib.rs",
            "src/cli.rs",
            "src/bin/tool.rs",
            "src/bin/tool/sub.rs",
        ]
        .map(source_file);
        let sites = [
            rust_use_site("src/cli.rs", "crate::helper", "helper", None),
            // No root declares `tool/sub.rs`, and `tool.rs`'s `mod sub;` would not look there.
            rust_use_site("src/bin/tool/sub.rs", "crate::run", "run", None),
        ];
        let symbols = open_kioku_resolution::SymbolIndex::build(vec![
            rust_symbol("src/lib.rs", "helper"),
            rust_symbol("src/bin/tool.rs", "run"),
        ]);
        let project = rust_project(&[("", None)]);
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let bind = |skipped: &[&str]| {
            let mut registry = ImportRegistry::default();
            for site in &sites {
                registry.insert_unresolved_site(site);
            }
            let modules = RustModuleTree::new(&files, &project, &[], &scopes)
                .with_unindexed_files(skipped.iter().map(Path::new));
            registry.resolve_rust_imports(&symbols, &scopes, &modules);
            registry
        };

        let skipped = bind(&["src/main.rs"]);
        assert_eq!(bound_target(&skipped, "src/cli.rs", "helper"), None);
        let indexed = bind(&[]);
        assert_eq!(
            bound_target(&indexed, "src/cli.rs", "helper").as_deref(),
            Some("symbol:src/lib.rs:helper")
        );
        assert_eq!(bound_target(&indexed, "src/bin/tool/sub.rs", "run"), None);
    }

    fn rust_package_with_targets(targets: CargoTargets) -> ProjectModel {
        let mut project = rust_project(&[("", None)]);
        project.roots[0].cargo_targets = targets;
        project
    }

    #[test]
    fn module_placements_follow_the_crate_roots_a_manifest_names() {
        // `[[bin]] path = "src/cli.rs"` declares `util`; `[[test]] path = "it/main.rs"` declares
        // `common`; the library declares nothing.
        let files = [
            "src/lib.rs",
            "src/cli.rs",
            "src/util.rs",
            "it/main.rs",
            "it/common.rs",
        ]
        .map(source_file);
        let project = rust_package_with_targets(CargoTargets {
            roots: vec![PathBuf::from("src/cli.rs"), PathBuf::from("it/main.rs")],
            not_autodiscovered: Vec::new(),
        });
        let declarations = vec![
            mod_decl("src/cli.rs", "util"),
            mod_decl("it/main.rs", "common"),
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();

        let util = &placements[&FileId::new("file:src/util.rs")];
        assert_eq!(util.crate_roots, vec!["src::cli"]);
        assert_eq!(util.module, Some(vec!["util".to_string()]));
        assert_eq!(
            placements[&FileId::new("file:src/cli.rs")].module,
            Some(Vec::new())
        );
        let common = &placements[&FileId::new("file:it/common.rs")];
        assert_eq!(common.crate_dir, "it");
        assert_eq!(common.crate_roots, vec!["it::main"]);
        assert_eq!(modules.placement_gaps(), RustPlacementGaps::default());

        // Without the manifest's targets `cli.rs` is a module no root declares.
        let plain = rust_project(&[("", None)]);
        let unread = RustModuleTree::new(&files, &plain, &declarations, &scopes);
        let unread = unread.module_placements();
        assert_eq!(unread[&FileId::new("file:src/util.rs")].module, None);
        assert!(!unread.contains_key(&FileId::new("file:it/common.rs")));
    }

    #[test]
    fn a_target_root_in_a_tree_the_layout_does_not_follow_withholds_and_is_reported() {
        // `[[bin]] path = "src/tools/cli.rs"` roots a crate whose modules live in `src/tools/`,
        // which is also where the library's `tools` module would keep its own.
        let files = ["src/lib.rs", "src/tools/cli.rs", "src/tools/args.rs"].map(source_file);
        let project = rust_package_with_targets(CargoTargets {
            roots: vec![PathBuf::from("src/tools/cli.rs")],
            not_autodiscovered: Vec::new(),
        });
        let declarations = vec![mod_decl("src/tools/cli.rs", "args")];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();

        assert!(!placements.contains_key(&FileId::new("file:src/tools/args.rs")));
        // The unfollowed root itself is not counted as a file withheld from `src/`.
        assert_eq!(
            modules.placement_gaps(),
            RustPlacementGaps {
                unplaced_packages: 1,
                unread_roots: 1,
                withheld_files: 1,
                shared_files: 0,
                unread_mounts: 0,
            }
        );
    }

    #[test]
    fn a_library_root_outside_src_beside_a_target_root_shares_that_roots_module_tree() {
        // `[lib] path = "lib.rs"` beside `[[bin]] path = "main.rs"`: both declare `util`, so
        // `crate::helper` in `util.rs` is the library's `helper` in one crate and the binary's in
        // the other. `libonly.rs` is the library's alone.
        let files = ["lib.rs", "main.rs", "util.rs", "libonly.rs"].map(source_file);
        let mut project = rust_project(&[("", Some("lib.rs"))]);
        project.roots[0].cargo_targets = CargoTargets {
            roots: vec![PathBuf::from("main.rs")],
            not_autodiscovered: Vec::new(),
        };
        let declarations = vec![
            mod_decl("lib.rs", "util"),
            mod_decl("lib.rs", "libonly"),
            mod_decl("main.rs", "util"),
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();

        let util = &placements[&FileId::new("file:util.rs")];
        assert_eq!(util.crate_roots, vec!["main", "lib"]);
        assert_eq!(util.module, Some(vec!["util".to_string()]));
        let libonly = &placements[&FileId::new("file:libonly.rs")];
        assert_eq!(libonly.crate_roots, vec!["lib"]);
        assert_eq!(libonly.module, Some(vec!["libonly".to_string()]));
        assert_eq!(
            placements[&FileId::new("file:lib.rs")].crate_roots,
            vec!["lib"]
        );

        let symbols = open_kioku_resolution::SymbolIndex::build(vec![
            rust_symbol("lib.rs", "helper"),
            rust_symbol("main.rs", "helper"),
        ]);
        let mut registry = ImportRegistry::default();
        for site in [
            rust_use_site("util.rs", "crate::helper", "helper", None),
            rust_use_site("libonly.rs", "crate::helper", "helper", None),
        ] {
            registry.insert_unresolved_site(&site);
        }
        registry.resolve_rust_imports(&symbols, &scopes, &modules);
        assert_eq!(bound_target(&registry, "util.rs", "helper"), None);
        assert_eq!(
            bound_target(&registry, "libonly.rs", "helper").as_deref(),
            Some("symbol:lib.rs:helper")
        );
        // Crate-name paths still cannot be followed into a library root outside `src/`.
        assert_eq!(modules.placement_gaps().unplaced_packages, 1);

        // With no other crate root beside it the library roots no tree, as before.
        let alone = rust_project(&[("", Some("lib.rs"))]);
        let alone = RustModuleTree::new(&files, &alone, &declarations, &scopes);
        assert!(alone.module_placements().is_empty());
    }

    /// `crate::helper` bound from each `importer`, and `crate::util::u` from `main.rs` and
    /// `lib.rs`, whichever of them are indexed.
    fn bind_crate_paths(
        modules: &RustModuleTree<'_>,
        importers: &[&str],
        roots: &[&str],
    ) -> ImportRegistry {
        let mut symbols = roots
            .iter()
            .map(|root| rust_symbol(root, "helper"))
            .collect::<Vec<_>>();
        symbols.extend(importers.iter().map(|importer| rust_symbol(importer, "u")));
        let symbols = open_kioku_resolution::SymbolIndex::build(symbols);
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let mut registry = ImportRegistry::default();
        for importer in importers {
            registry.insert_unresolved_site(&rust_use_site(
                importer,
                "crate::helper",
                "helper",
                None,
            ));
        }
        for root in roots {
            registry.insert_unresolved_site(&rust_use_site(root, "crate::util::u", "u", None));
        }
        registry.resolve_rust_imports(&symbols, &scopes, modules);
        registry
    }

    #[test]
    fn a_module_a_skipped_crate_root_may_also_declare_is_read_against_no_crate() {
        // `main.rs` declares `util`; `lib.rs` was not indexed (over `max_file_size`, say) and may
        // declare it too, where `crate::helper` in `util.rs` is the library's `helper` (#572).
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        for (dir, project) in [
            ("src/", rust_project(&[("", None)])),
            ("", {
                let mut project = rust_project(&[("", Some("lib.rs"))]);
                project.roots[0].cargo_targets = CargoTargets {
                    roots: vec![PathBuf::from("main.rs")],
                    not_autodiscovered: Vec::new(),
                };
                project
            }),
        ] {
            let at = |name: &str| format!("{dir}{name}");
            let files = [at("main.rs"), at("util.rs"), at("cli.rs")].map(|path| source_file(&path));
            let declarations = vec![mod_decl(&at("main.rs"), "util")];
            let skipped = [at("lib.rs")];
            let modules = RustModuleTree::new(&files, &project, &declarations, &scopes)
                .with_unindexed_files(skipped.iter().map(Path::new));
            let placements = modules.module_placements();
            let util = &placements[&FileId::new(format!("file:{}", at("util.rs")))];
            assert_eq!(
                util.crate_roots,
                vec![at("main").replace('/', "::")],
                "{dir}"
            );
            assert!(util.in_other_crates, "{dir}");
            assert!(
                !placements[&FileId::new(format!("file:{}", at("main.rs")))].in_other_crates,
                "{dir}"
            );
            assert_eq!(modules.placement_gaps().shared_files, 1, "{dir}");

            let registry = bind_crate_paths(&modules, &[&at("util.rs")], &[&at("main.rs")]);
            assert_eq!(
                bound_target(&registry, &at("util.rs"), "helper"),
                None,
                "{dir}"
            );
            // The binary's own path into `util.rs` still binds.
            assert_eq!(
                bound_target(&registry, &at("main.rs"), "u"),
                Some(format!("symbol:{}:u", at("util.rs"))),
                "{dir}"
            );

            // Indexed, `lib.rs` declares nothing, so `util.rs` is the binary's alone.
            let mut files = files.to_vec();
            files.push(source_file(&at("lib.rs")));
            let indexed = RustModuleTree::new(&files, &project, &declarations, &scopes);
            let util =
                &indexed.module_placements()[&FileId::new(format!("file:{}", at("util.rs")))];
            assert!(!util.in_other_crates, "{dir}");
            let registry = bind_crate_paths(&indexed, &[&at("util.rs")], &[&at("main.rs")]);
            assert_eq!(
                bound_target(&registry, &at("util.rs"), "helper"),
                Some(format!("symbol:{}:helper", at("main.rs"))),
                "{dir}"
            );
        }
    }

    #[test]
    fn a_skipped_crate_root_shares_only_the_modules_below_its_directory() {
        // `[[bin]] path = "src/tools/cli.rs"`, not indexed, keeps its modules in `src/tools/`.
        let files = [
            "src/lib.rs",
            "src/util.rs",
            "src/tools/mod.rs",
            "src/tools/x.rs",
        ]
        .map(source_file);
        let project = rust_package_with_targets(CargoTargets {
            roots: vec![PathBuf::from("src/tools/cli.rs")],
            not_autodiscovered: Vec::new(),
        });
        let declarations = vec![
            mod_decl("src/lib.rs", "util"),
            mod_decl("src/lib.rs", "tools"),
            mod_decl("src/tools/mod.rs", "x"),
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes)
            .with_unindexed_files([Path::new("src/tools/cli.rs")]);
        let placements = modules.module_placements();
        assert!(!placements[&FileId::new("file:src/util.rs")].in_other_crates);
        assert!(placements[&FileId::new("file:src/tools/x.rs")].in_other_crates);
    }

    #[test]
    fn a_file_another_crate_mounts_with_path_is_read_against_no_crate() {
        // `lib.rs` declares `mod util;` and `main.rs` mounts the same file with
        // `#[path = "util.rs"] mod u2;`: `util.rs` is compiled into both crates (#572).
        let path_decl = |file: &str, name: &str, paths: &[&str]| ModuleDeclarationSite {
            has_path_attribute: true,
            path_attributes: paths.iter().map(|path| path.to_string()).collect(),
            ..mod_decl(file, name)
        };
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let files = [
            "src/lib.rs",
            "src/main.rs",
            "src/util.rs",
            "src/cli.rs",
            "src/process.rs",
            "src/process/unix.rs",
            "src/process/imp.rs",
        ]
        .map(source_file);
        let project = rust_project(&[("", None)]);
        let own = vec![
            mod_decl("src/lib.rs", "util"),
            mod_decl("src/lib.rs", "process"),
            mod_decl("src/main.rs", "cli"),
            mod_decl("src/process.rs", "imp"),
            // A mount inside the library's own crate shares nothing.
            path_decl("src/process.rs", "imp", &["process/unix.rs"]),
        ];
        let modules = RustModuleTree::new(&files, &project, &own, &scopes);
        let placements = modules.module_placements();
        assert!(placements
            .values()
            .all(|placement| !placement.in_other_crates));

        let mut declarations = own.clone();
        declarations.push(path_decl("src/main.rs", "u2", &["./util.rs"]));
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();
        let util = &placements[&FileId::new("file:src/util.rs")];
        assert_eq!(util.crate_roots, vec!["src::lib"]);
        assert!(util.in_other_crates);
        for other in ["src/cli.rs", "src/process.rs", "src/process/unix.rs"] {
            assert!(
                !placements[&FileId::new(format!("file:{other}"))].in_other_crates,
                "{other}"
            );
        }
        assert_eq!(modules.placement_gaps().shared_files, 1);
        let registry = bind_crate_paths(&modules, &["src/util.rs"], &["src/lib.rs"]);
        assert_eq!(bound_target(&registry, "src/util.rs", "helper"), None);
        assert_eq!(
            bound_target(&registry, "src/lib.rs", "u").as_deref(),
            Some("symbol:src/util.rs:u")
        );

        // A build script mounting `cli.rs` compiles it into a crate of its own.
        let files_with_build = files
            .iter()
            .cloned()
            .chain([source_file("build.rs")])
            .collect::<Vec<_>>();
        let mut declarations = own.clone();
        declarations.push(path_decl("build.rs", "cli", &["src/cli.rs"]));
        let modules = RustModuleTree::new(&files_with_build, &project, &declarations, &scopes);
        let placements = modules.module_placements();
        assert!(placements[&FileId::new("file:src/cli.rs")].in_other_crates);
        assert!(!placements[&FileId::new("file:src/util.rs")].in_other_crates);

        // Inside an inline module a `path` is read below the declaring file's directory, along
        // the module names: `mod process { #[path = "imp.rs"] mod imp; }` in `main.rs` mounts
        // the library's `src/process/imp.rs`.
        let block_scopes = open_kioku_resolution::ScopeIndex::build(vec![inline_module_scope(
            "scope:main:m",
            "src/main.rs",
        )]);
        let mut declarations = own.clone();
        declarations.push(ModuleDeclarationSite {
            scope_id: Some(ScopeId::new("scope:main:m")),
            ..path_decl("src/main.rs", "imp", &["imp.rs"])
        });
        let modules = RustModuleTree::new(&files, &project, &declarations, &block_scopes);
        let placements = modules.module_placements();
        assert!(placements[&FileId::new("file:src/process/imp.rs")].in_other_crates);
        assert!(!placements[&FileId::new("file:src/util.rs")].in_other_crates);

        // `mod a { #[path = "x.rs"] mod b; }` in `main.rs` mounts `src/a/x.rs`, not the
        // library's `src/x.rs` beside `main.rs`.
        let nested_files = [
            "src/lib.rs",
            "src/main.rs",
            "src/x.rs",
            "src/a/mod.rs",
            "src/a/x.rs",
        ]
        .map(source_file);
        let block_scopes = open_kioku_resolution::ScopeIndex::build(vec![inline_module_scope(
            "scope:main:a",
            "src/main.rs",
        )]);
        let nested = vec![
            mod_decl("src/lib.rs", "x"),
            mod_decl("src/lib.rs", "a"),
            mod_decl("src/a/mod.rs", "x"),
            ModuleDeclarationSite {
                scope_id: Some(ScopeId::new("scope:main:a")),
                ..path_decl("src/main.rs", "b", &["x.rs"])
            },
        ];
        let modules = RustModuleTree::new(&nested_files, &project, &nested, &block_scopes);
        let placements = modules.module_placements();
        assert!(placements[&FileId::new("file:src/a/x.rs")].in_other_crates);
        assert!(!placements[&FileId::new("file:src/x.rs")].in_other_crates);

        // A `#[path]` the index cannot read may mount any file of the package outside its crate.
        let mut declarations = own;
        declarations.push(path_decl("src/main.rs", "hidden", &[]));
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();
        assert!(placements[&FileId::new("file:src/util.rs")].in_other_crates);
        assert!(!placements[&FileId::new("file:src/cli.rs")].in_other_crates);
    }

    #[test]
    fn a_self_path_below_a_file_every_crate_declares_in_place_is_still_read() {
        // `main.rs` was not indexed (a path policy excluded it), so `a.rs` and `a/inner.rs` may be
        // the binary's too; wherever `a` is declared, `a/inner.rs` is the same file (#576).
        let files = ["src/lib.rs", "src/a.rs", "src/a/inner.rs"].map(source_file);
        let project = rust_project(&[("", None)]);
        let declarations = vec![mod_decl("src/lib.rs", "a"), mod_decl("src/a.rs", "inner")];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes)
            .with_unindexed_files([Path::new("src/main.rs")]);
        let placements = modules.module_placements();
        for file in ["src/a.rs", "src/a/inner.rs"] {
            let placement = &placements[&FileId::new(format!("file:{file}"))];
            assert!(placement.in_other_crates, "{file}");
            assert!(placement.own_subtree_in_every_crate, "{file}");
        }

        let symbols = open_kioku_resolution::SymbolIndex::build(vec![
            rust_symbol("src/lib.rs", "helper"),
            rust_symbol("src/a.rs", "u"),
            rust_symbol("src/a/inner.rs", "i"),
        ]);
        let mut registry = ImportRegistry::default();
        for (importer, source, local) in [
            ("src/a.rs", "self::inner::i", "i"),
            ("src/a.rs", "crate::helper", "helper"),
            ("src/a/inner.rs", "super::u", "u"),
        ] {
            registry.insert_unresolved_site(&rust_use_site(importer, source, local, None));
        }
        registry.resolve_rust_imports(&symbols, &scopes, &modules);
        assert_eq!(
            bound_target(&registry, "src/a.rs", "i").as_deref(),
            Some("symbol:src/a/inner.rs:i")
        );
        assert_eq!(bound_target(&registry, "src/a.rs", "helper"), None);
        assert_eq!(bound_target(&registry, "src/a/inner.rs", "u"), None);
    }

    #[test]
    fn a_crate_root_skipped_for_size_shares_only_the_modules_its_lines_declare() {
        // `main.rs` was over `max_file_size`; its lines declare `util` but not `other` (#576).
        let files = [
            "src/lib.rs",
            "src/util.rs",
            "src/other.rs",
            "src/other/deep.rs",
        ]
        .map(source_file);
        let project = rust_project(&[("", None)]);
        let declarations = vec![
            mod_decl("src/lib.rs", "util"),
            mod_decl("src/lib.rs", "other"),
            mod_decl("src/other.rs", "deep"),
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = || {
            RustModuleTree::new(&files, &project, &declarations, &scopes)
                .with_unindexed_files([Path::new("src/main.rs")])
        };
        assert_eq!(
            modules().unread_crate_roots(),
            HashSet::from([PathBuf::from("src/main.rs")])
        );
        let marked = |modules: &RustModuleTree<'_>| {
            let mut marked = modules
                .module_placements()
                .into_iter()
                .filter(|(_, placement)| placement.in_other_crates)
                .map(|(id, placement)| (id.0, placement.own_subtree_in_every_crate))
                .collect::<Vec<_>>();
            marked.sort();
            marked
        };
        let declared = HashSet::from(["util".to_string()]);
        let scanned = modules().with_scanned_roots([(Path::new("src/main.rs"), Some(declared))]);
        assert_eq!(
            marked(&scanned),
            vec![("file:src/util.rs".to_string(), true)]
        );
        assert_eq!(scanned.placement_gaps().shared_files, 1);

        // Unscanned (a path policy excluded it), it may declare every module below `src/`.
        assert_eq!(
            marked(&modules()),
            vec![
                ("file:src/other.rs".to_string(), true),
                ("file:src/other/deep.rs".to_string(), true),
                ("file:src/util.rs".to_string(), true),
            ]
        );
        // Lines that could not tell (a `path` attribute, say) may mount any of them anywhere.
        let unknown = modules().with_scanned_roots([(Path::new("src/main.rs"), None)]);
        assert_eq!(
            marked(&unknown),
            vec![
                ("file:src/other.rs".to_string(), false),
                ("file:src/other/deep.rs".to_string(), false),
                ("file:src/util.rs".to_string(), false),
            ]
        );
    }

    #[test]
    fn the_module_files_a_path_mounted_file_declares_are_shared_too() {
        let path_decl = |file: &str, name: &str, paths: &[&str]| ModuleDeclarationSite {
            has_path_attribute: true,
            path_attributes: paths.iter().map(|path| path.to_string()).collect(),
            ..mod_decl(file, name)
        };
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let project = rust_project(&[("", None)]);
        // `tests/it.rs` mounts the library's `sub/mod.rs`, so rustc compiles `sub/child.rs` and
        // `sub/child/leaf.rs` into the test crate too, each at the place its path spells (#576).
        let files = [
            "src/lib.rs",
            "src/sub/mod.rs",
            "src/sub/child.rs",
            "src/sub/child/leaf.rs",
            "src/other.rs",
            "tests/it.rs",
        ]
        .map(source_file);
        let declarations = vec![
            mod_decl("src/lib.rs", "sub"),
            mod_decl("src/lib.rs", "other"),
            mod_decl("src/sub/mod.rs", "child"),
            mod_decl("src/sub/child.rs", "leaf"),
            path_decl("tests/it.rs", "sub", &["../src/sub/mod.rs"]),
        ];
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();
        for file in [
            "src/sub/mod.rs",
            "src/sub/child.rs",
            "src/sub/child/leaf.rs",
        ] {
            let placement = &placements[&FileId::new(format!("file:{file}"))];
            assert!(placement.in_other_crates, "{file}");
            assert!(placement.own_subtree_in_every_crate, "{file}");
        }
        assert!(!placements[&FileId::new("file:src/other.rs")].in_other_crates);
        let registry = bind_crate_paths(&modules, &["src/sub/child.rs"], &["src/lib.rs"]);
        assert_eq!(bound_target(&registry, "src/sub/child.rs", "helper"), None);

        // `#[path = "../src/util.rs"] mod util;` reads `util.rs`'s `mod child;` from `src/`, the
        // directory of the file, where the library's own `child.rs` is: that file is shared and
        // in place, `util.rs` is not in place, and `util/child.rs` is the library's alone.
        let files = [
            "src/lib.rs",
            "src/util.rs",
            "src/util/child.rs",
            "src/child.rs",
            "tests/it.rs",
        ]
        .map(source_file);
        let declarations = vec![
            mod_decl("src/lib.rs", "util"),
            mod_decl("src/lib.rs", "child"),
            mod_decl("src/util.rs", "child"),
            path_decl("tests/it.rs", "util", &["../src/util.rs"]),
        ];
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();
        let at = |file: &str| {
            let placement = &placements[&FileId::new(format!("file:{file}"))];
            (
                placement.in_other_crates,
                placement.own_subtree_in_every_crate,
            )
        };
        assert_eq!(at("src/util.rs"), (true, false));
        assert_eq!(at("src/child.rs"), (true, true));
        assert_eq!(at("src/util/child.rs"), (false, true));
        let registry = bind_crate_paths(&modules, &["src/util.rs"], &["src/lib.rs"]);
        assert_eq!(bound_target(&registry, "src/util.rs", "helper"), None);
    }

    #[test]
    fn cfg_attr_path_modules_below_a_mounted_file_are_shared_too() {
        let path_decl = |file: &str, name: &str, paths: &[&str]| ModuleDeclarationSite {
            has_path_attribute: true,
            path_attributes: paths.iter().map(|path| path.to_string()).collect(),
            ..mod_decl(file, name)
        };
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let project = rust_project(&[("", None)]);
        let files = [
            "src/lib.rs",
            "src/sys/mod.rs",
            "src/sys/unix.rs",
            "src/sys/windows.rs",
            "src/sys/fd.rs",
            "src/other.rs",
            "tests/it.rs",
            "tests/more.rs",
        ]
        .map(source_file);
        // `sys/mod.rs` declares `#[cfg_attr(unix, path = "unix.rs")]
        // #[cfg_attr(windows, path = "windows.rs")] mod imp;`, and `unix.rs`, read from `sys/`
        // as a mounted file is, declares `mod fd;`.
        let own = vec![
            mod_decl("src/lib.rs", "sys"),
            mod_decl("src/lib.rs", "other"),
            path_decl("src/sys/mod.rs", "imp", &["unix.rs", "windows.rs"]),
            mod_decl("src/sys/unix.rs", "fd"),
        ];
        let modules = RustModuleTree::new(&files, &project, &own, &scopes);
        assert!(modules
            .module_placements()
            .values()
            .all(|placement| !placement.in_other_crates));

        // `tests/it.rs` mounts `sys/mod.rs`, so the test crate compiles every file its `imp`
        // may be, whichever configuration it is built for (#604).
        let mut declarations = own.clone();
        declarations.push(path_decl("tests/it.rs", "sys", &["../src/sys/mod.rs"]));
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();
        let at = |file: &str| {
            let placement = &placements[&FileId::new(format!("file:{file}"))];
            (
                placement.in_other_crates,
                placement.own_subtree_in_every_crate,
            )
        };
        assert_eq!(at("src/sys/mod.rs"), (true, true));
        assert_eq!(at("src/sys/unix.rs"), (true, false));
        assert_eq!(at("src/sys/windows.rs"), (true, false));
        assert_eq!(at("src/sys/fd.rs"), (true, true));
        assert_eq!(at("src/other.rs"), (false, true));
        assert_eq!(modules.placement_gaps().shared_files, 4);
        let registry = bind_crate_paths(&modules, &["src/sys/unix.rs"], &["src/lib.rs"]);
        assert_eq!(bound_target(&registry, "src/sys/unix.rs", "helper"), None);

        // Each alternative of a `cfg_attr` path on the mount itself is followed as well.
        let mut declarations = own.clone();
        declarations.push(path_decl(
            "tests/it.rs",
            "imp",
            &["../src/sys/unix.rs", "../src/sys/windows.rs"],
        ));
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();
        for file in ["src/sys/unix.rs", "src/sys/windows.rs", "src/sys/fd.rs"] {
            assert!(
                placements[&FileId::new(format!("file:{file}"))].in_other_crates,
                "{file}"
            );
        }
        assert!(!placements[&FileId::new("file:src/sys/mod.rs")].in_other_crates);

        // A `path` below the mounted file the index cannot read may mount any file of the
        // package into the test crate; mounted from two test crates it is still one attribute.
        let mut declarations = own.clone();
        declarations.push(path_decl("src/sys/mod.rs", "raw", &[]));
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        assert_eq!(modules.placement_gaps().unread_mounts, 0);
        declarations.push(path_decl("tests/it.rs", "sys", &["../src/sys/mod.rs"]));
        declarations.push(path_decl("tests/more.rs", "sys", &["../src/sys/mod.rs"]));
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        assert_eq!(modules.placement_gaps().unread_mounts, 1);
        assert!(modules.module_placements()[&FileId::new("file:src/other.rs")].in_other_crates);
    }

    #[test]
    fn path_attributes_the_index_cannot_read_are_counted() {
        // `#[path = r"support.rs"]` in `tests/it.rs` may mount any file of the package, so each
        // library file is marked, and the attribute is counted for the quality note (#576).
        let files = ["src/lib.rs", "src/a.rs", "tests/it.rs", "tests/support.rs"].map(source_file);
        let project = rust_project(&[("", None)]);
        let declarations = vec![
            mod_decl("src/lib.rs", "a"),
            ModuleDeclarationSite {
                has_path_attribute: true,
                path_attributes: Vec::new(),
                ..mod_decl("tests/it.rs", "support")
            },
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placement = &modules.module_placements()[&FileId::new("file:src/a.rs")];
        assert!(placement.in_other_crates);
        assert!(!placement.own_subtree_in_every_crate);
        let gaps = modules.placement_gaps();
        assert_eq!((gaps.shared_files, gaps.unread_mounts), (1, 1));
    }

    #[test]
    fn modules_below_a_crate_tree_at_the_repository_root_are_placed() {
        // The package is the repository root, so the module tree of `lib.rs` and `main.rs` is
        // `""`: `lib.rs`'s `pub mod tools;` is `tools/mod.rs`, whose `pub mod inner;` is
        // `tools/inner.rs`.
        let files = ["lib.rs", "main.rs", "tools/mod.rs", "tools/inner.rs"].map(source_file);
        let mut project = rust_project(&[("", Some("lib.rs"))]);
        project.roots[0].cargo_targets = CargoTargets {
            roots: vec![PathBuf::from("main.rs")],
            not_autodiscovered: Vec::new(),
        };
        let declarations = vec![
            mod_decl("lib.rs", "tools"),
            mod_decl("tools/mod.rs", "inner"),
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();

        let inner = &placements[&FileId::new("file:tools/inner.rs")];
        assert_eq!(inner.crate_dir, "");
        assert_eq!(inner.crate_roots, vec!["lib"]);
        assert_eq!(
            inner.module,
            Some(vec!["tools".to_string(), "inner".to_string()])
        );

        let symbols =
            open_kioku_resolution::SymbolIndex::build(vec![rust_symbol("tools/mod.rs", "t")]);
        let mut registry = ImportRegistry::default();
        registry.insert_unresolved_site(&rust_use_site("lib.rs", "crate::tools::t", "t", None));
        registry.resolve_rust_imports(&symbols, &scopes, &modules);
        assert_eq!(
            bound_target(&registry, "lib.rs", "t").as_deref(),
            Some("symbol:tools/mod.rs:t")
        );
    }

    #[test]
    fn target_auto_discovery_turned_off_leaves_only_the_roots_the_manifest_names() {
        // `autobins = false` with `[[bin]] name = "tool"`: `src/main.rs` and `src/bin/other.rs`
        // are not binaries, so neither roots a crate.
        let files = [
            "src/lib.rs",
            "src/main.rs",
            "src/bin/tool.rs",
            "src/bin/other.rs",
        ]
        .map(source_file);
        let project = rust_package_with_targets(CargoTargets {
            roots: vec![PathBuf::from("src/bin/tool.rs")],
            not_autodiscovered: vec![CargoTargetKind::Bin],
        });
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &[], &scopes);
        let placements = modules.module_placements();

        assert_eq!(
            placements[&FileId::new("file:src/bin/tool.rs")].crate_roots,
            vec!["src::bin::tool"]
        );
        assert!(!placements.contains_key(&FileId::new("file:src/bin/other.rs")));
        // No crate compiles an unnamed `src/main.rs`, so it is not read against the library.
        assert!(!placements.contains_key(&FileId::new("file:src/main.rs")));
        assert_eq!(modules.placement_gaps(), RustPlacementGaps::default());
    }

    #[test]
    fn a_manifest_named_root_discovery_skipped_without_a_path_still_withholds() {
        // `[[bin]] path = "src/id_rsa_tool.rs"` is skipped as secret-like, and its skip is
        // redacted, so no unindexed path names it; the manifest still does. It declares `cli`.
        let files = ["src/lib.rs", "src/cli.rs"].map(source_file);
        let project = rust_package_with_targets(CargoTargets {
            roots: vec![PathBuf::from("src/id_rsa_tool.rs")],
            not_autodiscovered: Vec::new(),
        });
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &[], &scopes);

        assert!(!modules
            .module_placements()
            .contains_key(&FileId::new("file:src/cli.rs")));
        assert_eq!(
            modules.placement_gaps(),
            RustPlacementGaps {
                unplaced_packages: 0,
                unread_roots: 1,
                withheld_files: 1,
                shared_files: 0,
                unread_mounts: 0,
            }
        );
    }

    #[test]
    fn rust_relative_import_from_an_importer_not_declared_where_its_path_says_stays_unbound() {
        // `lib.rs` mounts `legacy/session.rs` as `crate::session` with `#[path]`, so `super` there
        // is the crate root, not `legacy`, although `legacy/mod.rs` defines `target_fn` too.
        let files = ["src/lib.rs", "src/legacy/mod.rs", "src/legacy/session.rs"];
        let site = [rust_use_site(
            "src/legacy/session.rs",
            "super::target_fn",
            "target_fn",
            None,
        )];
        let symbols = || {
            vec![
                rust_symbol("src/lib.rs", "target_fn"),
                rust_symbol("src/legacy/mod.rs", "target_fn"),
            ]
        };
        let path_mounted = bind_rust_imports(
            &files,
            &[""],
            vec![
                mod_decl("src/lib.rs", "legacy"),
                ModuleDeclarationSite {
                    has_path_attribute: true,
                    path_attributes: Vec::new(),
                    ..mod_decl("src/lib.rs", "session")
                },
            ],
            &site,
            symbols(),
            Vec::new(),
        );
        assert_eq!(
            bound_target(&path_mounted, "src/legacy/session.rs", "target_fn"),
            None
        );

        let declared = bind_rust_imports(
            &files,
            &[""],
            vec![
                mod_decl("src/lib.rs", "legacy"),
                mod_decl("src/legacy/mod.rs", "session"),
            ],
            &site,
            symbols(),
            Vec::new(),
        );
        assert_eq!(
            bound_target(&declared, "src/legacy/session.rs", "target_fn").as_deref(),
            Some("symbol:src/legacy/mod.rs:target_fn")
        );
    }

    #[test]
    fn rust_crate_item_import_resolves_inside_the_importers_workspace_member() {
        let registry = bind_rust_imports(
            &[
                "crates/app/src/lib.rs",
                "crates/app/src/session.rs",
                "crates/app/src/auth.rs",
                "crates/other/src/lib.rs",
                "crates/other/src/auth.rs",
            ],
            &["crates/app", "crates/other"],
            vec![
                mod_decl("crates/app/src/lib.rs", "auth"),
                mod_decl("crates/app/src/lib.rs", "session"),
                mod_decl("crates/other/src/lib.rs", "auth"),
            ],
            &[rust_use_site(
                "crates/app/src/session.rs",
                "crate::auth::issue_token",
                "issue_token",
                None,
            )],
            vec![
                rust_symbol("crates/app/src/auth.rs", "issue_token"),
                rust_symbol("crates/other/src/auth.rs", "issue_token"),
            ],
            Vec::new(),
        );
        assert_eq!(
            bound_target(&registry, "crates/app/src/session.rs", "issue_token").as_deref(),
            Some("symbol:crates/app/src/auth.rs:issue_token")
        );
    }

    #[test]
    fn rust_crate_root_comes_from_the_nearest_manifest() {
        let registry = bind_rust_imports(
            &[
                "src/lib.rs",
                "src/tools/xtask/tests/common.rs",
                "src/tools/xtask/src/main.rs",
                "src/tools/xtask/src/helper.rs",
                "src/tools/xtask/src/run.rs",
            ],
            &["", "src/tools/xtask"],
            vec![
                mod_decl("src/tools/xtask/src/main.rs", "helper"),
                mod_decl("src/tools/xtask/src/main.rs", "run"),
            ],
            &[
                // `crate` in the nested package's integration test is the test crate itself.
                rust_use_site(
                    "src/tools/xtask/tests/common.rs",
                    "crate::helper",
                    "helper",
                    None,
                ),
                rust_use_site(
                    "src/tools/xtask/src/run.rs",
                    "crate::helper::go",
                    "go",
                    None,
                ),
            ],
            vec![
                rust_symbol("src/lib.rs", "helper"),
                rust_symbol("src/tools/xtask/src/helper.rs", "go"),
            ],
            Vec::new(),
        );
        assert_eq!(
            bound_target(&registry, "src/tools/xtask/tests/common.rs", "helper"),
            None
        );
        assert_eq!(
            bound_target(&registry, "src/tools/xtask/src/run.rs", "go").as_deref(),
            Some("symbol:src/tools/xtask/src/helper.rs:go")
        );
    }

    #[test]
    fn rust_item_import_without_exactly_one_module_level_target_stays_unbound() {
        let registry = bind_rust_imports(
            &[
                "src/lib.rs",
                "src/session.rs",
                "src/auth.rs",
                "src/auth/mod.rs",
                "src/token.rs",
                "src/policy.py",
                "src/vendor/token.rs",
            ],
            &[""],
            vec![
                mod_decl("src/lib.rs", "auth"),
                mod_decl("src/lib.rs", "token"),
                mod_decl("src/lib.rs", "policy"),
                mod_decl("src/lib.rs", "vendor"),
            ],
            &[
                // Defined in both `auth.rs` and `auth/mod.rs`.
                rust_use_site(
                    "src/session.rs",
                    "crate::auth::issue_token",
                    "issue_token",
                    None,
                ),
                // Only a method of that name exists.
                rust_use_site("src/session.rs", "crate::token::refresh", "refresh", None),
                // Named nowhere.
                rust_use_site("src/session.rs", "crate::token::revoke", "revoke", None),
                // The same qualified name from a Python file.
                rust_use_site("src/session.rs", "crate::policy::allow", "allow", None),
                // An extern crate whose path matches an internal module.
                rust_use_site("src/session.rs", "vendor::token::mint", "mint", None),
            ],
            vec![
                rust_symbol("src/auth.rs", "issue_token"),
                rust_symbol("src/auth/mod.rs", "issue_token"),
                Symbol {
                    kind: SymbolKind::Method,
                    parent_symbol_id: Some(SymbolId::new("symbol:src/token.rs:Token")),
                    ..rust_symbol("src/token.rs", "refresh")
                },
                Symbol {
                    language: Language::Python,
                    ..rust_symbol("src/policy.py", "allow")
                },
                rust_symbol("src/vendor/token.rs", "mint"),
            ],
            Vec::new(),
        );
        for local in ["issue_token", "refresh", "revoke", "allow", "mint"] {
            assert_eq!(
                bound_target(&registry, "src/session.rs", local),
                None,
                "`{local}` must stay unbound"
            );
        }
    }

    #[test]
    fn rust_item_import_through_an_undeclared_or_redirected_module_stays_unbound() {
        let scenarios = [
            ("no `mod auth;` declaration", Vec::new(), Vec::new()),
            (
                "`#[path = \"auth_v2.rs\"] mod auth;`",
                vec![ModuleDeclarationSite {
                    has_path_attribute: true,
                    path_attributes: Vec::new(),
                    ..mod_decl("src/lib.rs", "auth")
                }],
                Vec::new(),
            ),
            (
                "inline `mod auth { ... }`",
                vec![ModuleDeclarationSite {
                    has_body: true,
                    ..mod_decl("src/lib.rs", "auth")
                }],
                Vec::new(),
            ),
            (
                "`mod auth;` nested in an inline module",
                vec![ModuleDeclarationSite {
                    scope_id: Some(ScopeId::new("scope:lib:outer")),
                    ..mod_decl("src/lib.rs", "auth")
                }],
                vec![inline_module_scope("scope:lib:outer", "src/lib.rs")],
            ),
        ];
        for (layout, declarations, scopes) in scenarios {
            let registry = bind_rust_imports(
                &["src/lib.rs", "src/auth.rs", "src/session.rs"],
                &[""],
                declarations,
                &[rust_use_site(
                    "src/session.rs",
                    "crate::auth::issue_token",
                    "issue_token",
                    None,
                )],
                vec![rust_symbol("src/auth.rs", "issue_token")],
                scopes,
            );
            assert_eq!(
                bound_target(&registry, "src/session.rs", "issue_token"),
                None,
                "{layout}: `src/auth.rs` is not the declared module"
            );
        }
    }

    #[test]
    fn rust_relative_item_import_inside_an_inline_module_stays_unbound() {
        let registry = bind_rust_imports(
            &["src/lib.rs", "src/auth.rs"],
            &[""],
            vec![mod_decl("src/lib.rs", "auth")],
            &[
                // `mod tests { use super::helper; }` names `auth::helper`, not `crate::helper`.
                rust_use_site(
                    "src/auth.rs",
                    "super::helper",
                    "helper",
                    Some("scope:auth:tests"),
                ),
                rust_use_site(
                    "src/auth.rs",
                    "crate::issue_token",
                    "issue_token",
                    Some("scope:auth:tests"),
                ),
            ],
            vec![
                rust_symbol("src/lib.rs", "helper"),
                rust_symbol("src/lib.rs", "issue_token"),
            ],
            vec![inline_module_scope("scope:auth:tests", "src/auth.rs")],
        );
        assert_eq!(bound_target(&registry, "src/auth.rs", "helper"), None);
        // `crate::` is absolute, so an inline module does not change what it names.
        assert_eq!(
            bound_target(&registry, "src/auth.rs", "issue_token").as_deref(),
            Some("symbol:src/lib.rs:issue_token")
        );
    }

    /// Runs the import-edge pass the way indexing does, over `files` in the packages rooted at
    /// `manifests` (`(directory, package name)`).
    fn rust_import_edges(
        files: &[&str],
        manifests: &[(&str, Option<&str>)],
        declarations: Vec<ModuleDeclarationSite>,
        sites: &[ImportSite],
        symbols: Vec<Symbol>,
        scopes: Vec<Scope>,
    ) -> RustImportEdgeTargets {
        let files = files.iter().copied().map(source_file).collect::<Vec<_>>();
        let mut project = ProjectModel::new();
        project
            .roots
            .extend(manifests.iter().map(|(dir, package)| ProjectRoot {
                path: PathBuf::from(*dir),
                language: Language::Rust,
                package_name: package.map(str::to_string),
                source_roots: Vec::new(),
                library_root: None,
                cargo_targets: Default::default(),
                cargo_manifest: None,
            }));
        let symbols = open_kioku_resolution::SymbolIndex::build(symbols);
        let scopes = open_kioku_resolution::ScopeIndex::build(scopes);
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        rust_import_edge_targets(sites, &symbols, &scopes, &modules)
    }

    /// `strategy:file` for the file a path names, or `None` where it stays unresolved.
    fn edge_target(targets: &RustImportEdgeTargets, importer: &str, path: &str) -> Option<String> {
        targets
            .target(&FileId::new(format!("file:{importer}")), path)
            .map(|edge| format!("{}:{}", edge.strategy, edge.file.0))
    }

    /// The site the parser emits for `use <source>;` where the path ends in `*`.
    fn rust_glob_site(importer: &str, source: &str, scope: Option<&str>) -> ImportSite {
        ImportSite {
            is_glob: true,
            bindings: Vec::new(),
            ..rust_use_site(importer, source, GLOB_IMPORT_LOCAL_NAME, scope)
        }
    }

    #[test]
    fn rust_import_edges_name_the_file_declaring_the_module_or_the_item() {
        // The reported package: the crate root declares the modules and re-exports one item.
        let targets = rust_import_edges(
            &[
                "src/lib.rs",
                "src/api.rs",
                "src/auth.rs",
                "src/auth/keys.rs",
                "src/session.rs",
            ],
            &[("", Some("demo-crate"))],
            vec![
                mod_decl("src/lib.rs", "api"),
                mod_decl("src/lib.rs", "auth"),
                mod_decl("src/lib.rs", "session"),
                mod_decl("src/auth.rs", "keys"),
            ],
            &[
                rust_use_site(
                    "src/session.rs",
                    "crate::auth::issue_token",
                    "issue_token",
                    None,
                ),
                rust_use_site("src/session.rs", "crate::auth::Token", "Token", None),
                rust_use_site("src/session.rs", "crate::auth::keys", "keys", None),
                rust_glob_site("src/session.rs", "crate::auth::*", None),
                rust_use_site("src/api.rs", "crate::issue_token", "issue_token", None),
            ],
            vec![
                rust_symbol("src/auth.rs", "issue_token"),
                rust_symbol("src/auth.rs", "Token"),
            ],
            Vec::new(),
        );

        for path in ["crate::auth::issue_token", "crate::auth::Token"] {
            assert_eq!(
                edge_target(&targets, "src/session.rs", path).as_deref(),
                Some("rust-item-module:file:src/auth.rs"),
                "`{path}`"
            );
        }
        assert_eq!(
            edge_target(&targets, "src/session.rs", "crate::auth::keys").as_deref(),
            Some("rust-module-path:file:src/auth/keys.rs")
        );
        assert_eq!(
            edge_target(&targets, "src/session.rs", "crate::auth::*").as_deref(),
            Some("rust-module-path:file:src/auth.rs")
        );
        // The crate root only re-exports `issue_token`, and a re-export is not a declaration.
        assert!(targets.is_in_crate(&FileId::new("file:src/api.rs"), "crate::issue_token"));
        assert_eq!(
            edge_target(&targets, "src/api.rs", "crate::issue_token"),
            None
        );
    }

    #[test]
    fn a_crate_root_item_import_reaches_the_root_that_declares_it() {
        let targets = rust_import_edges(
            &["src/lib.rs", "src/auth.rs"],
            &[("", None)],
            vec![mod_decl("src/lib.rs", "auth")],
            &[rust_use_site(
                "src/auth.rs",
                "crate::RequestContext",
                "RequestContext",
                None,
            )],
            vec![rust_symbol("src/lib.rs", "RequestContext")],
            Vec::new(),
        );
        assert_eq!(
            edge_target(&targets, "src/auth.rs", "crate::RequestContext").as_deref(),
            Some("rust-item-module:file:src/lib.rs")
        );
    }

    #[test]
    fn crate_name_paths_reach_the_library_crate_from_outside_its_module_tree() {
        let targets = rust_import_edges(
            &["src/lib.rs", "src/auth.rs", "tests/auth_flow.rs"],
            &[("", Some("open-kioku-demo"))],
            vec![mod_decl("src/lib.rs", "auth")],
            &[
                rust_use_site("tests/auth_flow.rs", "open_kioku_demo::auth", "auth", None),
                rust_use_site(
                    "tests/auth_flow.rs",
                    "open_kioku_demo::handle_login",
                    "handle_login",
                    None,
                ),
                // An integration test is its own crate, so its `crate::` is not the library's.
                rust_use_site("tests/auth_flow.rs", "crate::helper", "helper", None),
            ],
            vec![rust_symbol("src/lib.rs", "handle_login")],
            Vec::new(),
        );
        assert_eq!(
            edge_target(&targets, "tests/auth_flow.rs", "open_kioku_demo::auth").as_deref(),
            Some("rust-module-path:file:src/auth.rs")
        );
        assert_eq!(
            edge_target(
                &targets,
                "tests/auth_flow.rs",
                "open_kioku_demo::handle_login"
            )
            .as_deref(),
            Some("rust-item-module:file:src/lib.rs")
        );
        assert_eq!(
            edge_target(&targets, "tests/auth_flow.rs", "crate::helper"),
            None
        );
    }

    #[test]
    fn relative_paths_follow_the_scope_that_writes_them() {
        let file_level = rust_import_edges(
            &["src/lib.rs", "src/auth.rs"],
            &[("", None)],
            vec![mod_decl("src/lib.rs", "auth")],
            &[rust_glob_site("src/auth.rs", "super::*", None)],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            edge_target(&file_level, "src/auth.rs", "super::*").as_deref(),
            Some("rust-module-path:file:src/lib.rs")
        );

        // One file writing the path at both levels names two different modules: at the top the
        // parent file, inside a `mod` block this file. The stored import row carries the path and
        // not the scope it was written in, so the two cannot be told apart and neither wins.
        let both = rust_import_edges(
            &["src/lib.rs", "src/auth.rs"],
            &[("", None)],
            vec![mod_decl("src/lib.rs", "auth")],
            &[
                rust_glob_site("src/auth.rs", "super::*", None),
                rust_glob_site("src/auth.rs", "super::*", Some("scope:auth:tests")),
            ],
            Vec::new(),
            vec![inline_module_scope("scope:auth:tests", "src/auth.rs")],
        );
        assert_eq!(edge_target(&both, "src/auth.rs", "super::*"), None);
    }

    fn nested_module_scope(id: &str, file: &str, parent: &str) -> Scope {
        Scope {
            parent_id: Some(ScopeId::new(parent)),
            ..inline_module_scope(id, file)
        }
    }

    #[test]
    fn super_from_an_inline_module_names_the_file_it_is_written_in() {
        // `mod tests { use super::*; }`: the parent of `tests` is the module `src/auth.rs` already
        // is. The import is known and self-referential, not unknown.
        let targets = rust_import_edges(
            &["src/lib.rs", "src/auth.rs"],
            &[("", None)],
            vec![mod_decl("src/lib.rs", "auth")],
            &[rust_glob_site(
                "src/auth.rs",
                "super::*",
                Some("scope:auth:tests"),
            )],
            Vec::new(),
            vec![inline_module_scope("scope:auth:tests", "src/auth.rs")],
        );
        assert_eq!(
            edge_target(&targets, "src/auth.rs", "super::*").as_deref(),
            Some("rust-self-module:file:src/auth.rs")
        );

        // Nesting stays inside the file: `mod a { mod b { use super::*; } }` names `a`.
        let nested = rust_import_edges(
            &["src/lib.rs", "src/auth.rs"],
            &[("", None)],
            vec![mod_decl("src/lib.rs", "auth")],
            &[rust_glob_site(
                "src/auth.rs",
                "super::*",
                Some("scope:auth:a:b"),
            )],
            Vec::new(),
            vec![
                inline_module_scope("scope:auth:a", "src/auth.rs"),
                nested_module_scope("scope:auth:a:b", "src/auth.rs", "scope:auth:a"),
            ],
        );
        assert_eq!(
            edge_target(&nested, "src/auth.rs", "super::*").as_deref(),
            Some("rust-self-module:file:src/auth.rs")
        );
    }

    #[test]
    fn a_relative_path_naming_something_below_the_module_is_not_self_referential() {
        // `mod tests { use super::helpers::Thing; }` in `src/auth.rs`: `super` is the file's own
        // module, but `helpers` below it is `src/auth/helpers.rs`, a different file. Calling this
        // self-referential emitted no edge and no unresolved fact, so the dependency vanished
        // without the absence being reported.
        let files = &["src/lib.rs", "src/auth.rs", "src/auth/helpers.rs"];
        let decls = || {
            vec![
                mod_decl("src/lib.rs", "auth"),
                mod_decl("src/auth.rs", "helpers"),
            ]
        };
        let scopes = || vec![inline_module_scope("scope:auth:tests", "src/auth.rs")];

        for path in [
            "super::helpers::Thing",
            "super::helpers",
            "self::helpers::Thing",
        ] {
            let targets = rust_import_edges(
                files,
                &[("", None)],
                decls(),
                &[rust_use_site(
                    "src/auth.rs",
                    path,
                    path.rsplit("::").next().unwrap(),
                    Some("scope:auth:tests"),
                )],
                Vec::new(),
                scopes(),
            );
            assert!(
                targets.is_in_crate(&FileId::new("file:src/auth.rs"), path),
                "`{path}` is a path of the importer's own crate"
            );
            assert_ne!(
                edge_target(&targets, "src/auth.rs", path).as_deref(),
                Some("rust-self-module:file:src/auth.rs"),
                "`{path}` names something below the module, not the module itself"
            );
        }

        // Globbing the module the hops land on does stay inside the file.
        let glob = rust_import_edges(
            files,
            &[("", None)],
            decls(),
            &[rust_glob_site(
                "src/auth.rs",
                "self::*",
                Some("scope:auth:tests"),
            )],
            Vec::new(),
            scopes(),
        );
        assert_eq!(
            edge_target(&glob, "src/auth.rs", "self::*").as_deref(),
            Some("rust-self-module:file:src/auth.rs")
        );
    }

    #[test]
    fn one_super_too_many_climbs_out_of_the_file_and_stays_unresolved() {
        // `mod tests { use super::super::*; }` names the parent of `crate::auth`, a different
        // file, which the importing file's path cannot identify.
        let targets = rust_import_edges(
            &["src/lib.rs", "src/auth.rs"],
            &[("", None)],
            vec![mod_decl("src/lib.rs", "auth")],
            &[rust_glob_site(
                "src/auth.rs",
                "super::super::*",
                Some("scope:auth:tests"),
            )],
            Vec::new(),
            vec![inline_module_scope("scope:auth:tests", "src/auth.rs")],
        );
        assert!(targets.is_in_crate(&FileId::new("file:src/auth.rs"), "super::super::*"));
        assert_eq!(
            edge_target(&targets, "src/auth.rs", "super::super::*"),
            None
        );
    }

    #[test]
    fn paths_the_module_tree_cannot_answer_stay_unresolved() {
        // A stale `auth.rs` beside `#[path = "auth_v2.rs"] mod auth;` is not `crate::auth`.
        let redirected = rust_import_edges(
            &[
                "src/lib.rs",
                "src/auth.rs",
                "src/auth_v2.rs",
                "src/session.rs",
            ],
            &[("", None)],
            vec![
                ModuleDeclarationSite {
                    has_path_attribute: true,
                    path_attributes: Vec::new(),
                    ..mod_decl("src/lib.rs", "auth")
                },
                mod_decl("src/lib.rs", "session"),
            ],
            &[rust_use_site(
                "src/session.rs",
                "crate::auth::issue_token",
                "issue_token",
                None,
            )],
            vec![rust_symbol("src/auth.rs", "issue_token")],
            Vec::new(),
        );
        assert_eq!(
            edge_target(&redirected, "src/session.rs", "crate::auth::issue_token"),
            None
        );

        // Each workspace member has its own `src/auth.rs`; `crate::` never leaves the importer's.
        let workspace = rust_import_edges(
            &[
                "crates/a/src/lib.rs",
                "crates/a/src/auth.rs",
                "crates/b/src/lib.rs",
                "crates/b/src/session.rs",
            ],
            &[("crates/a", Some("a")), ("crates/b", Some("b"))],
            vec![
                mod_decl("crates/a/src/lib.rs", "auth"),
                mod_decl("crates/b/src/lib.rs", "session"),
            ],
            &[rust_use_site(
                "crates/b/src/session.rs",
                "crate::auth::issue_token",
                "issue_token",
                None,
            )],
            vec![rust_symbol("crates/a/src/auth.rs", "issue_token")],
            Vec::new(),
        );
        assert_eq!(
            edge_target(
                &workspace,
                "crates/b/src/session.rs",
                "crate::auth::issue_token"
            ),
            None
        );
    }

    /// A dependency of the package being built on the package in `dir`, written `crate_name`.
    fn path_dependency(crate_name: &str, dir: &str, kind: CargoDependencyKind) -> CargoDependency {
        CargoDependency {
            crate_name: Some(crate_name.into()),
            key: crate_name.into(),
            package: crate_name.into(),
            manifest_dir: PathBuf::from(dir),
            kind,
            target_specific: false,
            inherited: false,
        }
    }

    /// Two workspaces each with a package named `engine`, and packages depending on the first:
    /// `crates/app` (with dev and build dependencies too), `crates/renamed` (writing it
    /// `core_alias`), and `crates/nodep`, which declares no dependency at all.
    fn cross_crate_project() -> ProjectModel {
        let package = |dir: &str, name: &str, dependencies: Vec<CargoDependency>| ProjectRoot {
            path: PathBuf::from(dir),
            language: Language::Rust,
            package_name: Some(name.into()),
            source_roots: Vec::new(),
            library_root: None,
            cargo_targets: Default::default(),
            cargo_manifest: Some(CargoManifest {
                package: Some(name.into()),
                dependencies,
                ..Default::default()
            }),
        };
        let mut project = ProjectModel::new();
        project.roots.extend([
            package("crates/engine", "engine", Vec::new()),
            package("other/engine", "engine", Vec::new()),
            package("crates/fixtures", "fixtures", Vec::new()),
            package(
                "crates/app",
                "app",
                vec![
                    path_dependency("engine", "crates/engine", CargoDependencyKind::Normal),
                    path_dependency("fixtures", "crates/fixtures", CargoDependencyKind::Build),
                ],
            ),
            package(
                "crates/renamed",
                "renamed",
                vec![path_dependency(
                    "core_alias",
                    "crates/engine",
                    CargoDependencyKind::Normal,
                )],
            ),
            package("crates/nodep", "nodep", Vec::new()),
        ]);
        project
    }

    const CROSS_CRATE_FILES: [&str; 11] = [
        "crates/engine/src/lib.rs",
        "crates/engine/src/plan.rs",
        "crates/engine/src/util.rs",
        "crates/engine/src/util/deep.rs",
        "other/engine/src/lib.rs",
        "other/engine/src/plan.rs",
        "crates/fixtures/src/lib.rs",
        "crates/app/src/main.rs",
        "crates/app/build.rs",
        "crates/renamed/src/lib.rs",
        "crates/nodep/src/lib.rs",
    ];

    /// `engine`'s root declares `plan` and `util`, defines `run`, re-exports `PlanEngine` by a
    /// Rust 2018 path and everything of `util` by a glob; `util` re-exports `deep::Deep`. The
    /// other workspace's `engine` defines a `PlanEngine` and a `run` of its own.
    fn cross_crate_fixture() -> (Vec<ModuleDeclarationSite>, Vec<ImportSite>, Vec<Symbol>) {
        let reexport = |importer: &str, source: &str| ImportSite {
            reexported: true,
            ..rust_use_site(importer, source, source.rsplit("::").next().unwrap(), None)
        };
        let declarations = vec![
            mod_decl("crates/engine/src/lib.rs", "plan"),
            mod_decl("crates/engine/src/lib.rs", "util"),
            mod_decl("crates/engine/src/util.rs", "deep"),
            mod_decl("other/engine/src/lib.rs", "plan"),
        ];
        let reexports = vec![
            reexport("crates/engine/src/lib.rs", "plan::PlanEngine"),
            ImportSite {
                reexported: true,
                ..rust_glob_site("crates/engine/src/lib.rs", "crate::util::*", None)
            },
            reexport("crates/engine/src/util.rs", "self::deep::Deep"),
            reexport("other/engine/src/lib.rs", "plan::PlanEngine"),
        ];
        let symbols = vec![
            rust_symbol("crates/engine/src/lib.rs", "run"),
            rust_symbol("crates/engine/src/plan.rs", "PlanEngine"),
            rust_symbol("crates/engine/src/util.rs", "helper"),
            rust_symbol("crates/engine/src/util/deep.rs", "Deep"),
            rust_symbol("other/engine/src/lib.rs", "run"),
            rust_symbol("other/engine/src/plan.rs", "PlanEngine"),
            rust_symbol("crates/fixtures/src/lib.rs", "sample"),
        ];
        (declarations, reexports, symbols)
    }

    #[test]
    fn rust_imports_through_a_declared_dependency_bind_that_crates_items() {
        let (declarations, mut sites, symbols) = cross_crate_fixture();
        let uses = [
            ("crates/app/src/main.rs", "engine::run"),
            ("crates/app/src/main.rs", "engine::PlanEngine"),
            ("crates/app/src/main.rs", "engine::plan::PlanEngine"),
            ("crates/app/src/main.rs", "engine::helper"),
            ("crates/app/src/main.rs", "engine::Deep"),
            ("crates/app/src/main.rs", "engine::missing"),
            ("crates/app/src/main.rs", "fixtures::sample"),
            ("crates/app/build.rs", "fixtures::sample"),
            ("crates/app/build.rs", "engine::run"),
            ("crates/renamed/src/lib.rs", "core_alias::run"),
            ("crates/renamed/src/lib.rs", "engine::PlanEngine"),
            ("crates/nodep/src/lib.rs", "engine::run"),
        ];
        sites.extend(uses.iter().map(|(importer, source)| {
            rust_use_site(importer, source, source.rsplit("::").next().unwrap(), None)
        }));
        let files = CROSS_CRATE_FILES.map(source_file);
        let project = cross_crate_project();
        let symbols = open_kioku_resolution::SymbolIndex::build(symbols);
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules =
            RustModuleTree::new(&files, &project, &declarations, &scopes).with_reexports(&sites);
        let mut registry = ImportRegistry::default();
        for site in &sites {
            registry.insert_unresolved_site(site);
        }
        registry.resolve_rust_imports(&symbols, &scopes, &modules);
        let bound = |importer: &str, local: &str| {
            let binding = registry
                .index
                .lookup(&FileId::new(format!("file:{importer}")), None, local)
                .into_iter()
                .find(|binding| !binding.source_module.starts_with("crate::"))
                .map(|binding| (binding.target_symbol.clone(), binding.rule))
                .unwrap();
            binding.0.map(|id| (id.0, binding.1))
        };
        let main = "crates/app/src/main.rs";
        assert_eq!(
            bound(main, "run"),
            Some((
                "symbol:crates/engine/src/lib.rs:run".into(),
                ImportBindingRule::RustModulePath
            )),
            "an item of the dependency's crate root"
        );
        assert_eq!(
            bound(main, "PlanEngine"),
            Some((
                "symbol:crates/engine/src/plan.rs:PlanEngine".into(),
                ImportBindingRule::RustReexport
            )),
            "a crate-root `pub use` by a Rust 2018 path, in the dependency's own workspace"
        );
        assert_eq!(
            bound(main, "helper"),
            Some((
                "symbol:crates/engine/src/util.rs:helper".into(),
                ImportBindingRule::RustReexport
            )),
            "a glob re-export"
        );
        assert_eq!(
            bound(main, "Deep"),
            Some((
                "symbol:crates/engine/src/util/deep.rs:Deep".into(),
                ImportBindingRule::RustReexport
            )),
            "a glob re-export of a module that re-exports the item itself"
        );
        assert_eq!(bound(main, "missing"), None);
        assert_eq!(
            bound(main, "sample"),
            None,
            "a build dependency is not the binary's to name"
        );
        assert_eq!(
            bound("crates/app/build.rs", "sample"),
            Some((
                "symbol:crates/fixtures/src/lib.rs:sample".into(),
                ImportBindingRule::RustModulePath
            ))
        );
        assert_eq!(bound("crates/app/build.rs", "run"), None);
        assert_eq!(
            bound("crates/renamed/src/lib.rs", "run"),
            Some((
                "symbol:crates/engine/src/lib.rs:run".into(),
                ImportBindingRule::RustModulePath
            )),
            "a renamed dependency is named by its new name"
        );
        assert_eq!(
            bound("crates/renamed/src/lib.rs", "PlanEngine"),
            None,
            "and not by its package's"
        );
        assert_eq!(
            bound("crates/nodep/src/lib.rs", "run"),
            None,
            "a package that declares no dependency on `engine` names no `engine` crate"
        );
    }

    #[test]
    fn rust_import_edges_through_a_declared_dependency_reach_the_file_declaring_the_item() {
        let (declarations, mut sites, symbols) = cross_crate_fixture();
        let main = "crates/app/src/main.rs";
        for source in [
            "engine::run",
            "engine::plan",
            "engine::PlanEngine",
            "engine::Deep",
            "engine::missing",
        ] {
            sites.push(rust_use_site(
                main,
                source,
                source.rsplit("::").next().unwrap(),
                None,
            ));
        }
        sites.push(rust_glob_site(main, "engine::util::*", None));
        sites.push(rust_use_site(
            "crates/nodep/src/lib.rs",
            "engine::run",
            "run",
            None,
        ));
        let files = CROSS_CRATE_FILES.map(source_file);
        let project = cross_crate_project();
        let symbols = open_kioku_resolution::SymbolIndex::build(symbols);
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules =
            RustModuleTree::new(&files, &project, &declarations, &scopes).with_reexports(&sites);
        let targets = rust_import_edge_targets(&sites, &symbols, &scopes, &modules);

        let edge = |importer: &str, path: &str| edge_target(&targets, importer, path);
        assert_eq!(
            edge(main, "engine::run").as_deref(),
            Some("rust-item-module:file:crates/engine/src/lib.rs")
        );
        assert_eq!(
            edge(main, "engine::plan").as_deref(),
            Some("rust-module-path:file:crates/engine/src/plan.rs")
        );
        assert_eq!(
            edge(main, "engine::PlanEngine").as_deref(),
            Some("rust-reexport:file:crates/engine/src/plan.rs")
        );
        assert_eq!(
            edge(main, "engine::Deep").as_deref(),
            Some("rust-reexport:file:crates/engine/src/util/deep.rs")
        );
        assert_eq!(
            edge(main, "engine::util::*").as_deref(),
            Some("rust-module-path:file:crates/engine/src/util.rs")
        );
        // A dependency path the tree cannot answer is left to the resolver's other rules, as is
        // one naming a crate the importer's package does not declare.
        let main_id = FileId::new(format!("file:{main}"));
        assert!(!targets.is_in_crate(&main_id, "engine::missing"));
        assert!(!targets.is_in_crate(&FileId::new("file:crates/nodep/src/lib.rs"), "engine::run"));
    }

    #[test]
    fn a_package_names_its_own_library_only_from_its_other_crates() {
        // `crates/engine` has a library (`lib.rs` declaring `util`), a binary, an integration
        // test and a build script; `crates/app` depends on it.
        let files = [
            "crates/engine/src/lib.rs",
            "crates/engine/src/util.rs",
            "crates/engine/src/main.rs",
            "crates/engine/tests/it.rs",
            "crates/engine/build.rs",
            "crates/app/src/lib.rs",
        ]
        .map(source_file);
        let project = cross_crate_project();
        let declarations = vec![mod_decl("crates/engine/src/lib.rs", "util")];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let names = modules.crate_names();
        let named = |path: &str| {
            names
                .get(&FileId::new(format!("file:{path}")))
                .map(|crates| crates.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default()
        };
        // Library code cannot write its own crate's name, and a build script names only its
        // build dependencies.
        assert!(named("crates/engine/src/lib.rs").is_empty());
        assert!(named("crates/engine/src/util.rs").is_empty());
        assert!(named("crates/engine/build.rs").is_empty());
        assert_eq!(named("crates/engine/src/main.rs"), vec!["engine"]);
        assert_eq!(named("crates/engine/tests/it.rs"), vec!["engine"]);
        assert_eq!(named("crates/app/src/lib.rs"), vec!["engine"]);
    }
}
