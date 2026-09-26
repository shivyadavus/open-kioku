use crate::rust_use_path::{
    map_rust_crate_name_path, map_rust_module_file, map_rust_use_path, module_name, RustCrateTree,
    RustPackageLayout, RustUsePath,
};
use open_kioku_core::{
    File, FileId, ImportSite, Language, ModuleDeclarationSite, ScopeId, ScopeKind, SymbolId,
    SymbolKind,
};
use open_kioku_resolution::RustModulePlacement;
pub use open_kioku_semantic_model::{
    ExportBinding, ExportIndex, ImportBinding, ImportBindingRule, ImportIndex, ImportOrigin,
    GLOB_IMPORT_LOCAL_NAME,
};
use open_kioku_semantic_model::{ProjectModel, ProjectRoot};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::Path;

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
        Self {
            files,
            files_by_stem,
            stems_in_dir,
            project,
            file_modules,
            unindexed_stems: HashSet::new(),
        }
    }

    /// Records the repository-relative paths discovery skipped; only Rust files matter.
    pub(crate) fn with_unindexed_files<'p>(
        mut self,
        paths: impl IntoIterator<Item = &'p Path>,
    ) -> Self {
        self.unindexed_stems
            .extend(paths.into_iter().filter_map(rust_file_stem));
        self
    }

    /// The crate roots of `tree` whose modules the index cannot place: roots discovery skipped,
    /// and roots the manifest names whose own module trees the layout does not follow. A file
    /// of the tree that no indexed root declares may belong to one of them.
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
            .filter(|root| self.unindexed_stems.contains(*root))
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
    /// that crate's alone, and none in a target directory, whose crate roots are independent
    /// crates that do not stand for one another.
    fn undeclared_file_roots(&self, path: &RustUsePath) -> Vec<String> {
        if !path.tree.is_src || !self.unreadable_roots(&path.tree).is_empty() {
            return Vec::new();
        }
        self.indexed_crate_roots(path)
    }

    /// Whether every module on `module`, from the crate root down, is declared as a file by the
    /// module above it. A stale `auth.rs` beside `#[path = "auth_v2.rs"] mod auth;` or an inline
    /// `mod auth { ... }` is not the module `crate::auth` names.
    fn declares_file_modules(&self, path: &RustUsePath, module: &[String]) -> bool {
        (0..module.len()).all(|depth| {
            let name = module_name(&module[depth]);
            let declares = |stem: String| self.file_modules.contains(&(stem, name.to_string()));
            if depth == 0 {
                let roots = self.crate_roots(path);
                !roots.is_empty() && roots.into_iter().all(declares)
            } else {
                path.module_file_stems(&module[..depth])
                    .into_iter()
                    .any(declares)
            }
        })
    }

    /// The crate root files whose module tree holds the importer: the root file itself, or the
    /// `lib.rs` and `main.rs` that declare the importer's top-level module. A package with both
    /// roots is two crates, and `crate::` names only the modules of the importer's own root; when
    /// both roots declare the importer's module, a path must be declared under both. When no
    /// indexed root declares it, those of [`RustModuleTree::undeclared_file_roots`].
    fn crate_roots(&self, path: &RustUsePath) -> Vec<String> {
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
                let placed = file.importer_root.is_some()
                    || self.declares_file_modules(&file, &file.importer_module);
                let roots = if placed {
                    self.crate_roots(&file)
                } else {
                    self.undeclared_file_roots(&file)
                };
                if roots.is_empty() {
                    return None;
                }
                let qualified = |stem: &str| stem.replace('/', "::");
                Some((
                    id.clone(),
                    RustModulePlacement {
                        crate_dir: qualified(&file.tree.module_dir),
                        crate_roots: roots.iter().map(|root| qualified(root)).collect(),
                        module: placed.then(|| file.importer_module.clone()),
                    },
                ))
            })
            .collect()
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
            if unreadable.is_empty()
                || rootless
                || self.declares_file_modules(&file, &file.importer_module)
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
                binding.rule = ImportBindingRule::RustModulePath;
            }
        }
    }
}

struct RustImportTarget {
    module_file: Option<FileId>,
    item: Option<SymbolId>,
}

fn rust_import_target(
    binding: &ImportBinding,
    importer: &Path,
    symbols: &open_kioku_resolution::SymbolIndex,
    scopes: &open_kioku_resolution::ScopeIndex,
    modules: &RustModuleTree<'_>,
) -> Option<RustImportTarget> {
    // `self` and `super` are read off the importer's file path, which cannot see an inline `mod`
    // block: in `mod tests { use super::helper; }` `super` is the file's own module.
    if !binding.source_module.starts_with("crate::")
        && is_inside_inline_module(&binding.scope_id, scopes)
    {
        return None;
    }
    modules.project.nearest_root_for(importer, Language::Rust)?;
    let path = map_rust_use_path(
        &modules.crate_tree(importer)?,
        importer,
        &binding.source_module,
    )?;
    if path.relative && !modules.declares_file_modules(&path, &path.importer_module) {
        return None;
    }
    let (item_name, parent) = path.segments.split_last()?;
    if *item_name != binding.imported_name {
        return None;
    }

    let module_file = modules.module_file(&path, &path.segments);
    let item = if modules.declares_file_modules(&path, parent) {
        rust_module_item(&modules.module_stems(&path, parent), item_name, symbols)
    } else {
        None
    };
    match (module_file, item) {
        (None, None) => None,
        (Some(module_file), _) => Some(RustImportTarget {
            module_file: Some(module_file),
            item: None,
        }),
        (None, Some(item)) => Some(RustImportTarget {
            module_file: None,
            item: Some(item),
        }),
    }
}

/// The one module-level Rust item named `item` in the files at `module_stems`.
fn rust_module_item(
    module_stems: &[String],
    item: &str,
    symbols: &open_kioku_resolution::SymbolIndex,
) -> Option<SymbolId> {
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
    match targets.as_slice() {
        [target] => Some(target.clone()),
        _ => None,
    }
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
/// crate, for the file-level `IMPORTS` edge.
///
/// - a path naming a declared module file reaches that file;
/// - a glob reaches the file of the module it opens;
/// - an item reaches the file of the module declaring it, which is the crate root only when the
///   item is declared there; an item reachable only through a re-export stays unresolved;
/// - a path the tree cannot answer is left unresolved, including a relative path written inside an
///   inline `mod` block, whose module the importing file's path cannot tell.
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
        if !is_rust_in_crate_path(&site.source, root.package_name.as_deref()) {
            continue;
        }
        let edge = if rust_self_module_site(site, scopes) {
            Some(RustImportEdge {
                file: site.file_id.clone(),
                strategy: RUST_SELF_MODULE_STRATEGY,
            })
        } else {
            rust_use_path_for_site(site, importer, root, scopes, modules)
                .and_then(|path| rust_import_edge(&path, symbols, modules))
        };
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

fn rust_use_path_for_site(
    site: &ImportSite,
    importer: &Path,
    root: &ProjectRoot,
    scopes: &open_kioku_resolution::ScopeIndex,
    modules: &RustModuleTree<'_>,
) -> Option<RustUsePath> {
    let layout = RustPackageLayout::of(Some(root));
    if let Some(package) = root.package_name.as_deref() {
        if let Some(path) = map_rust_crate_name_path(&layout, package, &site.source) {
            return Some(path);
        }
    }
    // `self` and `super` are read off the importer's file path, which cannot see an inline `mod`
    // block: in `mod tests { use super::*; }` `super` is the file's own module.
    if !site.source.starts_with("crate::")
        && site
            .scope_id
            .as_ref()
            .is_some_and(|scope| is_inside_inline_module(scope, scopes))
    {
        return None;
    }
    let path = map_rust_use_path(&modules.crate_tree(importer)?, importer, &site.source)?;
    if path.relative && !modules.declares_file_modules(&path, &path.importer_module) {
        return None;
    }
    Some(path)
}

fn rust_import_edge(
    path: &RustUsePath,
    symbols: &open_kioku_resolution::SymbolIndex,
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
    // A path naming both a module file and an item names the module: the item is reached through
    // that module, not by this path.
    if let Some(file) = modules.module_file(path, &path.segments) {
        return Some(module_edge(file));
    }
    if !modules.declares_file_modules(path, parent) {
        return None;
    }
    let item = rust_module_item(&modules.module_stems(path, parent), last, symbols)?;
    Some(RustImportEdge {
        file: symbols.get(&item)?.file_id.clone(),
        strategy: RUST_ITEM_MODULE_STRATEGY,
    })
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
    use open_kioku_semantic_model::{CargoTargetKind, CargoTargets, ProjectRoot};
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
        });
        let declarations = vec![
            mod_decl("src/lib.rs", "w"),
            mod_decl("src/lib.rs", "nest"),
            mod_decl("src/w.rs", "child"),
            ModuleDeclarationSite {
                has_path_attribute: true,
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
        // A file the indexed root declares is still that crate's.
        let auth = &placements[&FileId::new("file:src/auth.rs")];
        assert_eq!(auth.crate_roots, vec!["src::lib"]);
        assert_eq!(auth.module, Some(vec!["auth".to_string()]));
        assert_eq!(
            modules.placement_gaps(),
            RustPlacementGaps {
                unplaced_packages: 0,
                unread_roots: 1,
                withheld_files: 1,
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
        assert_eq!(
            modules.placement_gaps(),
            RustPlacementGaps {
                unplaced_packages: 1,
                unread_roots: 1,
                withheld_files: 2,
            }
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
        assert_eq!(
            placements[&FileId::new("file:src/main.rs")].crate_roots,
            vec!["src::lib"]
        );
        assert_eq!(placements[&FileId::new("file:src/main.rs")].module, None);
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
}
