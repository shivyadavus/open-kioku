use crate::cycle_memo::{Begin, CycleMemo};
use crate::rust_use_path::{
    join_dir, map_rust_crate_name_path, map_rust_module_file, map_rust_use_path, module_name,
    normalize_path, parent_dir, strip_dir, RustCrateTree, RustPackageLayout, RustUsePath,
    ScannedModules,
};
use open_kioku_core::{
    File, FileId, ImportSite, Language, ModuleDeclarationSite, ScopeId, ScopeKind, SymbolId,
    SymbolKind, Visibility,
};
use open_kioku_resolution::{
    RustConfiguredModules, RustConfiguredRead, RustCrateNames, RustModuleFiles,
    RustModulePlacement, RustModuleRoute, RustNamespace, RustReexport, RustReexportNamespaces,
    RustReexported,
};
use open_kioku_semantic_model::{
    CargoImporter, ConfiguredImportTargets, ProjectModel, ProjectRoot,
};
pub use open_kioku_semantic_model::{
    ExportBinding, ExportIndex, ImportBinding, ImportBindingRule, ImportIndex, ImportOrigin,
    GLOB_IMPORT_LOCAL_NAME,
};
use std::cell::{OnceCell, RefCell};
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
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
    /// with no body, outside any inline module, and with no `path` attribute or only ones set
    /// through `cfg_attr`, which leave the module at its default location whenever their
    /// conditions do not hold (#608). `mod r#type;` is `type`.
    file_modules: HashSet<(String, String)>,
    /// `(declaring file without `.rs`, module path)` for each `mod name;` with no body declared
    /// inside inline `mod` blocks of the file, the path spelling the blocks from the outermost
    /// down and then the module (`mod b { mod sys; }` is `["b", "sys"]`). Rustc reads such a
    /// module's file below the directory those blocks spell, as it does a file module's: only
    /// one with no `path` attribute, inside blocks with none and inside no function, is kept.
    inline_file_modules: InlineFileModules,
    /// The first block of each path in `inline_file_modules`, by declaring file.
    inline_file_module_tops: HashSet<(String, String)>,
    /// The names each scope of a Rust file imports, by `(file, scope)`, with
    /// [`GLOB_IMPORT_LOCAL_NAME`] for a glob: a name a nearer scope imports, or one its glob may
    /// supply, is not the module the file declares.
    scope_imports: HashSet<(FileId, ScopeId, String)>,
    /// Rust files discovery saw but did not index (over `max_file_size`, excluded, ignored,
    /// unreadable), by repository-relative path without `.rs`. A crate root among them owns
    /// modules the index cannot place.
    unindexed_stems: HashSet<String>,
    /// The module names Rust files discovery skipped for size declare, and the `cfg_attr` paths
    /// on those `mod` items, by extension-less path, as [`scan_module_declarations`] read them:
    /// `None` where the lines could not tell. Only
    /// the files whose `mod` items decide a placement are read: crate roots, and files a
    /// `#[path]` attribute mounts or that sit below one. A root not scanned (one a path policy
    /// excluded is never read) may declare any module, and a mounted file not scanned may mount
    /// any file of its package.
    ///
    /// [`scan_module_declarations`]: crate::rust_use_path::scan_module_declarations
    scanned_files: HashMap<String, Option<ScannedModules>>,
    /// What each `#[path]` attribute mounts, by the extension-less path of the declaring file.
    path_mounts: Vec<(String, PathMount)>,
    /// The file-backed `mod name;` items of each file, by its extension-less path, as
    /// [`RustModuleTree::configured_modules`] reads the files each may be compiled from.
    declared_modules: HashMap<String, Vec<DeclaredModule>>,
    /// [`RustModuleTree::find_shared_files`], computed on first use.
    shared_files: OnceCell<SharedFiles>,
    /// [`RustModuleTree::configured_modules`], computed on first use.
    configured: OnceCell<HashMap<String, RustConfiguredModules>>,
    /// The `use` sites of each Rust file, through which a path naming the file's module reaches
    /// the names they bring in: any of them from the module's own crate, `pub use` from another.
    /// Only those written at the top level of the file count ([`is_module_level`]).
    module_uses: HashMap<FileId, Vec<ImportSite>>,
    /// The Rust files whose top level invokes a macro, which may expand to items and `use`
    /// declarations the parser does not see: no glob of theirs settles a name.
    module_macros: HashSet<FileId>,
    /// The names a top-level `thread_local!` of each Rust file declares: a glob of the file does
    /// not settle them either.
    module_macro_names: HashMap<FileId, HashSet<String>>,
    /// What the `use` sites of a module bring in under a name, as [`RustModuleTree::used_name`]
    /// found it, by the number of `use` declarations followed to reach it. A cycle of `use`
    /// declarations, or [`MAX_REEXPORT_HOPS`], cuts a lookup short, and one that saw a cut
    /// depends on where it was entered from: it is kept only for a lookup that would take the
    /// same path (#659).
    used_names: RefCell<CycleMemo<UsedNameKey, UsedName>>,
}

/// A module, a name, whether the path naming it is written in another crate, and the namespace
/// it is read in (`None` for a `use`, which brings in every namespace's item of the name).
type UsedNameKey = (ModuleAt, String, bool, Option<RustNamespace>);

/// Where the module a Rust path's parent names is, for [`RustModuleTree::used_name`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ModuleAt {
    /// The extension-less paths of the files that may hold a module of the tree.
    Files(Vec<String>),
    /// An inline `mod name { .. }` block of one file, by the file and the block's scope (#641).
    Inline(FileId, ScopeId),
}

/// What the `use` sites at the top level of a module bring in under one name.
#[derive(Debug, Clone, Default)]
struct UsedName {
    /// What each `use` naming it explicitly brings in, `as` alias included.
    named: Vec<RustPathTarget>,
    /// A `use` naming it explicitly that the tree cannot follow.
    named_unresolved: bool,
    /// What each glob `use` brings in under it. The globs are read only while they may settle
    /// the name: not beside a named `use` or a macro, which shadow them, and not past one that
    /// leaves it unsettled (#659), so the list may stop short then.
    globbed: Vec<RustPathTarget>,
    /// A glob `use` the tree cannot follow, which may bring it in too.
    glob_unresolved: bool,
    /// The module's file invokes a macro at its top level, which may expand to a `use` or an item
    /// of the name that shadows a glob.
    macro_expanded: bool,
}

impl UsedName {
    /// A lookup cut short: anything may bring the name in.
    fn unknown() -> Self {
        Self {
            named_unresolved: true,
            glob_unresolved: true,
            ..Self::default()
        }
    }

    /// What the name is, when the `use` sites settle it: a named `use` shadows a glob, and every
    /// one that may supply it must agree. A glob settles nothing beside a macro, which may expand
    /// to an item or a named `use` that shadows it; a written named `use` stands, since another
    /// of the name a macro wrote would collide with it.
    fn target(&self) -> Option<RustPathTarget> {
        let (targets, unresolved) = if self.named.is_empty() && !self.named_unresolved {
            (&self.globbed, self.glob_unresolved || self.macro_expanded)
        } else {
            (&self.named, self.named_unresolved)
        };
        match (targets.split_first(), unresolved) {
            (Some((first, rest)), false) if rest.iter().all(|target| target == first) => {
                Some(first.clone())
            }
            _ => None,
        }
    }

    /// Whether a `use` may bring in an item of the name other than `item`, which the module
    /// defines: a named `use`, which compiles beside the item only under a `cfg` or in another
    /// namespace. A glob never does: an item the module defines shadows what a glob brings in.
    /// What a macro may expand to is not counted either: it would collide with the item as a
    /// written `use` does, and the index has always read an item it sees as the name.
    fn may_override(&self, item: &SymbolId) -> bool {
        self.named
            .iter()
            .any(|target| target.item.as_ref() != Some(item))
            || self.named_unresolved
    }
}

/// How a Rust path is followed through `use` declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReexportWalk {
    /// The path is written in another crate than the module it names, which brings names in
    /// for it only through `pub use`.
    from_other_crate: bool,
    /// How many `use` declarations it has been followed through.
    hops: usize,
    /// The namespace the path reads its last segment in (#643): a call path reads a value and a
    /// path to a type a type. `None` for a `use` path, which brings in the name's item of every
    /// namespace and is read as one item, as before namespaces were told apart.
    namespace: Option<RustNamespace>,
}

impl ReexportWalk {
    /// A `use` path written in a file, through a crate name or not.
    fn start(crate_name: bool) -> Self {
        Self {
            from_other_crate: crate_name,
            hops: 0,
            namespace: None,
        }
    }

    /// The walk reading its name in `namespace`.
    fn in_namespace(self, namespace: RustNamespace) -> Self {
        Self {
            namespace: Some(namespace),
            ..self
        }
    }

    /// Whether `symbol` is an item of the namespace the walk reads, as `scopes` tells.
    fn admits(
        &self,
        symbol: &open_kioku_core::Symbol,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> bool {
        self.namespace
            .is_none_or(|namespace| scopes.rust_in_namespace(symbol, namespace))
    }
}

/// A `mod name;` item with no body outside any inline module, and the files it may compile the
/// module from.
#[derive(Debug, Clone)]
struct DeclaredModule {
    /// The module name, `r#` removed.
    name: String,
    /// It has no `path` attribute, or only `cfg_attr` ones whose conditions may all fail, so its
    /// default location is compiled on some build.
    at_default: bool,
    /// The extension-less file each `path` attribute names, `None` for one the index cannot read.
    paths: Vec<Option<String>>,
}

/// How deep below a module whose file configuration selects the module tree is followed.
const MAX_CONFIGURED_DEPTH: usize = 64;

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
    /// What the path names in each file of a module whose file configuration selects, when it
    /// passes through one the file writing it is not below (#615).
    configured: Option<ConfiguredImportTargets>,
    /// The variant of the enum `item` the path names (`Shape::Circle`, or `Circle` through
    /// `pub use Shape::*;`, #641), which is no item the index records.
    variant: Option<String>,
}

/// What a Rust `use` path names once modules whose file configuration selects are read.
enum ConfiguredPath {
    /// What the placed tree reached, with what the path names in each file that may hold such a
    /// module on a build that compiles the writer, if it passes one.
    Alternatives(Option<RustPathTarget>),
    /// The writer fixes every choice on the path: what the path names in the one file each
    /// choice is compiled from with it, in place of what the placed tree reached.
    Proven(Option<RustPathTarget>),
}

/// Where a Rust `use` path whose first segment is not `crate`, `self` or `super` starts (#632).
#[derive(Debug, Clone, PartialEq, Eq)]
enum UsePathStart {
    /// Not at a module the writing file declares in scope: a crate name, or nothing the module
    /// tree reads.
    Elsewhere,
    /// At such a module: the path is this `self::` path.
    Module(String),
    /// At such a module whose name is also a crate the package can name, so the path may start
    /// from either.
    ModuleOrCrate,
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
    /// Unindexed files in a mounted subtree whose `mod` items were not read and that marked a
    /// file, since each may mount any file of its package.
    unread_mounted_files: usize,
    /// Unindexed files in a mounted subtree with no [`RustModuleTree::scanned_files`] entry:
    /// reading those skipped for size tells which files below them the mount reaches.
    unscanned: HashSet<String>,
}

/// What [`RustModuleTree::mounted_subtree`] finds below a `#[path]`-mounted file.
struct MountedSubtree {
    /// Each file with whether it keeps the place its file path spells.
    files: Vec<(String, bool)>,
    /// The `#[path]` attributes of the subtree the index cannot read, by their index in
    /// `path_mounts`, with the file declaring each.
    unread_mounts: Vec<(usize, String)>,
    /// The unindexed files of the subtree whose `mod` items were not read.
    unread_files: Vec<String>,
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
    /// Files a `#[path]` attribute mounts, or below one, that were not indexed and whose `mod`
    /// items could not be read, each of which marked files of its package as shared.
    pub(crate) unread_mounted_files: usize,
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
                    && (!declaration.has_path_attribute || declaration.path_is_conditional)
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
        let (inline_file_modules, inline_file_module_tops) =
            inline_file_modules(&files, declarations, scopes);
        let mut declared_modules = HashMap::<String, Vec<DeclaredModule>>::new();
        for declaration in declarations.iter().filter(|declaration| {
            !declaration.has_body
                && !declaration
                    .scope_id
                    .as_ref()
                    .is_some_and(|scope| is_inside_inline_module(scope, scopes))
        }) {
            let Some(declaring) = files
                .get(&declaration.file_id)
                .and_then(|path| rust_file_stem(path))
            else {
                continue;
            };
            let paths = if declaration.has_path_attribute && declaration.path_attributes.is_empty()
            {
                vec![None]
            } else {
                declaration
                    .path_attributes
                    .iter()
                    .map(|value| match PathMount::of(&declaring, value, false) {
                        Some(PathMount::File(file)) => Some(file),
                        _ => None,
                    })
                    .collect()
            };
            declared_modules
                .entry(declaring)
                .or_default()
                .push(DeclaredModule {
                    name: module_name(&declaration.name).to_string(),
                    at_default: !declaration.has_path_attribute || declaration.path_is_conditional,
                    paths,
                });
        }
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
            inline_file_modules,
            inline_file_module_tops,
            scope_imports: HashSet::new(),
            unindexed_stems: HashSet::new(),
            scanned_files: HashMap::new(),
            path_mounts,
            declared_modules,
            shared_files: OnceCell::new(),
            configured: OnceCell::new(),
            module_uses: HashMap::new(),
            module_macros: HashSet::new(),
            module_macro_names: HashMap::new(),
            used_names: RefCell::default(),
        }
    }

    /// Records the Rust import sites: the `use` sites a path through a module may reach names
    /// through, and the names each scope imports.
    pub(crate) fn with_import_sites(mut self, sites: &[ImportSite]) -> Self {
        for site in sites
            .iter()
            .filter(|site| self.files.contains_key(&site.file_id))
        {
            if let Some(scope) = &site.scope_id {
                let mut bound = site
                    .bindings
                    .iter()
                    .map(|binding| binding.local.clone())
                    .collect::<Vec<_>>();
                if site.is_glob {
                    bound.push(GLOB_IMPORT_LOCAL_NAME.to_string());
                }
                for local in bound {
                    self.scope_imports
                        .insert((site.file_id.clone(), scope.clone(), local));
                }
            }
            self.module_uses
                .entry(site.file_id.clone())
                .or_default()
                .push(site.clone());
        }
        self.used_names = RefCell::default();
        self
    }

    /// Records the Rust files whose top level invokes a macro that may declare anything, and the
    /// names each file's `thread_local!` declares (see [`RustModuleTree::module_macros`]).
    pub(crate) fn with_module_macros(
        mut self,
        files: HashSet<FileId>,
        names: HashMap<FileId, HashSet<String>>,
    ) -> Self {
        self.module_macros = files;
        self.module_macro_names = names;
        self.used_names = RefCell::default();
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

    /// Where a Rust `use` path written in `file` at `scope` starts when its first segment is not
    /// `crate`, `self` or `super` (#632). That depends on the package's edition.
    ///
    /// In the 2015 edition the path starts at the crate root wherever it is written, so
    /// `use sys::f;` in `src/a.rs` is `use crate::sys::f;` when the crate root declares `mod sys`,
    /// even if `src/a.rs` declares a `mod sys` of its own.
    ///
    /// Since the 2018 edition the segment is looked up in scope first, so `use sys::imp::f;` in a
    /// file declaring `mod sys;` is `use self::sys::imp::f;`. The segment names such a module
    /// when the file declares `mod` items of the name at its top level and no other item of the
    /// name in the type namespace, the path is written at that level or in a function body inside
    /// it, and no scope between declares or imports the name or holds a glob that may supply it.
    /// A site that records no scope is read as written at the top level, and a file-backed
    /// `mod first;` the module tree holds stands for the item when the index holds no symbol of
    /// the name there. A path written inside an inline `mod` block, or at a scope the index does
    /// not hold, is not read. The path may be the module alone (`pub use inner as facade;`), and
    /// an `enum` that is the one item of the name starts a path to its variants
    /// (`pub use Shape::*;`, #641).
    ///
    /// When the index cannot read the edition, only a path written in a crate root is read, where
    /// the crate root and the module in scope are the same module.
    fn use_path_start(
        &self,
        file: &FileId,
        scope: Option<&ScopeId>,
        source: &str,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> UsePathStart {
        // A one-segment path names the module itself: `pub use inner as facade;` (#641).
        let first = source.split("::").next().unwrap_or_default().trim();
        if matches!(first, "" | "crate" | "self" | "super" | "Self") {
            return UsePathStart::Elsewhere;
        }
        let importer = self.files.get(file).copied();
        let module_file = importer.and_then(|path| self.module_file_of(path));
        let at_crate_root = module_file
            .as_ref()
            .is_some_and(|file| file.importer_root.is_some());
        match importer.and_then(|path| crate::project_model::rust_edition(self.project, path)) {
            Some("2015") => {
                let Some(module_file) = module_file else {
                    return UsePathStart::Elsewhere;
                };
                let roots = match &module_file.importer_root {
                    Some(root) => vec![root.clone()],
                    None => self.declared_crate_roots(&module_file),
                };
                return if !roots.is_empty()
                    && roots.iter().all(|root| self.declares_top(root, first))
                {
                    UsePathStart::Module(format!("crate::{}", source.trim()))
                } else {
                    UsePathStart::Elsewhere
                };
            }
            None if !at_crate_root => return UsePathStart::Elsewhere,
            _ => {}
        }
        // The import registry records a site with no scope at `global`.
        let unscoped = scope.is_none_or(|scope| scope.0 == "global");
        let mut current = scope.and_then(|scope| scopes.get(scope));
        let mut at_file_level = unscoped;
        for _ in 0..=scopes.scopes.len() {
            let Some(here) = current else {
                break;
            };
            match here.kind {
                ScopeKind::File => {
                    at_file_level = true;
                    break;
                }
                // A `mod` block does not see the items of the module around it.
                ScopeKind::Module => break,
                _ => {}
            }
            let imports = |local: &str| {
                self.scope_imports
                    .contains(&(file.clone(), here.id.clone(), local.to_string()))
            };
            if !symbols
                .lookup_file_scope_name(file, &here.id, first)
                .is_empty()
                || imports(first)
                || imports(GLOB_IMPORT_LOCAL_NAME)
            {
                return UsePathStart::Elsewhere;
            }
            current = here
                .parent_id
                .as_ref()
                .and_then(|parent| scopes.get(parent));
        }
        if !at_file_level {
            return UsePathStart::Elsewhere;
        }
        // Functions, fields and constants live in the value namespace and never begin a path.
        let items = symbols
            .lookup_file_name(file, first)
            .iter()
            .filter_map(|id| symbols.get(id))
            .filter(|symbol| {
                symbol
                    .scope_id
                    .as_ref()
                    .and_then(|scope| scopes.get(scope))
                    .is_some_and(|scope| scope.kind == ScopeKind::File)
                    && !matches!(
                        symbol.kind,
                        SymbolKind::Function
                            | SymbolKind::Method
                            | SymbolKind::Field
                            | SymbolKind::Variable
                            | SymbolKind::Constant
                            | SymbolKind::Test
                            | SymbolKind::Endpoint
                            | SymbolKind::DatabaseTable
                    )
            })
            .collect::<Vec<_>>();
        let declared = match items.as_slice() {
            [] => self
                .files
                .get(file)
                .and_then(|path| rust_file_stem(path))
                .is_some_and(|stem| {
                    self.file_modules
                        .contains(&(stem, module_name(first).to_string()))
                }),
            // An enum of the module starts a path to its variants (`pub use Shape::*;`, #641).
            [only] if scopes.rust_enum_variants(&only.id).is_some() => source.contains("::"),
            _ => items.iter().all(|item| item.kind == SymbolKind::Module),
        };
        if !declared {
            return UsePathStart::Elsewhere;
        }
        // A crate of the same name may start the path too, and rustc rejects it as ambiguous.
        if scopes.rust_names_crate(file, first) {
            return UsePathStart::ModuleOrCrate;
        }
        UsePathStart::Module(format!("self::{}", source.trim()))
    }

    /// `source`, written in `file` (at `importer`) in `scope`, mapped onto a crate module tree:
    /// through a module the file declares in scope there (see [`RustModuleTree::use_path_start`]),
    /// through a crate name, or `crate::`/`self::`/`super::` in the importer's own crate. The flag
    /// is set for a crate-name path, which is written in another crate than the one it names. A
    /// path whose first segment names both such a module and a crate is mapped onto neither.
    fn rust_path(
        &self,
        (file, importer): (&FileId, &Path),
        scope: Option<&ScopeId>,
        source: &str,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<(RustUsePath, bool)> {
        self.project.nearest_root_for(importer, Language::Rust)?;
        let in_scope = match self.use_path_start(file, scope, source, symbols, scopes) {
            UsePathStart::ModuleOrCrate => return None,
            UsePathStart::Module(path) => Some(path),
            UsePathStart::Elsewhere => None,
        };
        if in_scope.is_none() {
            if let Some(path) = self.crate_name_path(importer, source) {
                return Some((path, true));
            }
        }
        let source = in_scope.as_deref().unwrap_or(source);
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
    /// names the module. When the parent declares no item of the name, the `use` sites of the
    /// parent are followed instead, as `walk` says (#476); one it declares that a `use` beside it
    /// may stand in for leaves the path unresolved.
    ///
    /// A path through a module whose file configuration selects also names what it reaches in
    /// each file of that module (see [`RustModuleTree::configured_path_targets`]); `writer` is
    /// the file writing the path, `None` for a path through a crate name, which is written in
    /// another crate.
    fn rust_path_target(
        &self,
        path: &RustUsePath,
        writer: Option<&Path>,
        walk: ReexportWalk,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<RustPathTarget> {
        let placed = self.placed_path_target(path, walk, symbols, scopes);
        match self.configured_path_targets(path, writer, placed, symbols) {
            ConfiguredPath::Alternatives(target) | ConfiguredPath::Proven(target) => target,
        }
    }

    /// [`RustModuleTree::rust_path_target`] as the module tree places the files.
    fn placed_path_target(
        &self,
        path: &RustUsePath,
        walk: ReexportWalk,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<RustPathTarget> {
        let (item_name, parent) = path.segments.split_last()?;
        // A module is a type: a value of its name is another item (#643).
        if walk.namespace != Some(RustNamespace::Value) {
            if let Some(module_file) = self.module_file(path, &path.segments) {
                return Some(RustPathTarget {
                    module_file: Some(module_file),
                    item: None,
                    reexport: false,
                    configured: None,
                    variant: None,
                });
            }
        }
        if !self.declares_file_modules(path, parent) {
            // A `mod name;` the tree does not declare as a plain file (a `path` attribute, a
            // configuration choice) is no enum, block or renamed module.
            if self.passes_mounted_module(path, parent, symbols, scopes) {
                return None;
            }
            // A path through a name that is not a module the tree declares: an enum's variant,
            // a block of an inline `mod`, or a module a `use` brings in under a name (#641).
            return self
                .enum_variant_target(path, walk, symbols, scopes)
                .or_else(|| self.inline_path_target(path, walk, symbols, scopes))
                .or_else(|| self.aliased_path_target(path, walk, symbols, scopes));
        }
        let stems = self.module_stems(path, parent);
        // An inline `mod` of the name is the type the path names: a path continues through it
        // (#654), and no edge reaches it.
        if walk.namespace == Some(RustNamespace::Type) {
            let modules = rust_module_symbols(&stems, item_name, symbols);
            if !modules.is_empty() {
                return inline_module_target(&modules, scopes);
            }
        }
        let items = rust_module_items(&stems, item_name, symbols)
            .into_iter()
            .filter(|item| {
                symbols
                    .get(item)
                    .is_some_and(|symbol| walk.admits(symbol, scopes))
            })
            .collect::<Vec<_>>();
        self.defined_or_used_target(path, &items, walk, symbols, scopes)
    }

    /// Whether the first module on `parent` the tree does not declare as a file is named by a
    /// `mod` item with no body, or one the index cannot tell a block from, in the module above
    /// it: a `path` attribute or a configuration choice places that module, and no `use`, block
    /// or enum of the name stands for it.
    fn passes_mounted_module(
        &self,
        path: &RustUsePath,
        parent: &[String],
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> bool {
        let Some(at) =
            (0..parent.len()).find(|&end| !self.declares_file_modules(path, &parent[..=end]))
        else {
            return false;
        };
        let stems = self.module_stems(path, &parent[..at]);
        rust_module_symbols(&stems, &parent[at], symbols)
            .iter()
            .any(|module| scopes.inline_module_body(module).is_none())
    }

    /// What the last segment of `path` names given `items`, the items of its name its module
    /// defines in the namespace `walk` reads: the one defined item, shadowing any glob, unless a
    /// named `use` beside it may stand for another, which compiles only under a `cfg` or in
    /// another namespace (#476); with none, what the module's `use` sites bring in.
    fn defined_or_used_target(
        &self,
        path: &RustUsePath,
        items: &[SymbolId],
        walk: ReexportWalk,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<RustPathTarget> {
        match items {
            [item] => (!self
                .used_name(path, symbols, scopes, walk)
                .may_override(item))
            .then(|| RustPathTarget {
                module_file: None,
                item: Some(item.clone()),
                reexport: false,
                configured: None,
                variant: None,
            }),
            [] => self.reexported_target(path, symbols, scopes, walk),
            _ => None,
        }
    }

    /// What `path` names when its parent names a Rust `enum` (#641): the variant of the
    /// enum its last segment names, `Shape::Circle`. The enum is read as a type, and a path
    /// through a module whose file configuration selects is left to that module's reading.
    fn enum_variant_target(
        &self,
        path: &RustUsePath,
        walk: ReexportWalk,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<RustPathTarget> {
        let (variant, parent) = path.segments.split_last()?;
        let enum_item = self.enum_at(path, parent, walk, symbols, scopes)?;
        let variants = scopes.rust_enum_variants(&enum_item.item)?;
        variants
            .iter()
            .any(|declared| declared == variant)
            .then(|| RustPathTarget {
                module_file: None,
                item: Some(enum_item.item.clone()),
                reexport: enum_item.reexport,
                configured: None,
                variant: Some(variant.clone()),
            })
    }

    /// The `enum` the module path `parent` of `path` names, read as a type, and whether it was
    /// reached through a `use`. `None` for anything else, and for a path through a module whose
    /// file configuration selects.
    fn enum_at(
        &self,
        path: &RustUsePath,
        parent: &[String],
        walk: ReexportWalk,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<EnumAt> {
        if parent.is_empty() || self.configured_choice(path, None, parent).is_some() {
            return None;
        }
        let enum_path = RustUsePath {
            segments: parent.to_vec(),
            ..path.clone()
        };
        let target = self.placed_path_target(
            &enum_path,
            walk.in_namespace(RustNamespace::Type),
            symbols,
            scopes,
        )?;
        let item = match target {
            RustPathTarget {
                module_file: None,
                item: Some(item),
                configured: None,
                variant: None,
                ..
            } => item,
            _ => return None,
        };
        scopes.rust_enum_variants(&item)?;
        Some(EnumAt {
            item,
            reexport: target.reexport,
        })
    }

    /// What `path` names when its parent ends in blocks of inline `mod name { .. }` items of
    /// a file the tree places (#641): an item a block defines, or else what the `use` sites
    /// written directly in the block bring in, read as [`RustModuleTree::used_name`] reads a
    /// module's. A path through a module whose file configuration selects is not read.
    fn inline_path_target(
        &self,
        path: &RustUsePath,
        walk: ReexportWalk,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<RustPathTarget> {
        let (name, parent) = path.segments.split_last()?;
        let ModuleAt::Inline(file, block) = self.module_at(path, parent, symbols, scopes) else {
            return None;
        };
        if self.configured_choice(path, None, parent).is_some() {
            return None;
        }
        let defined = symbols
            .lookup_file_scope_name(&file, &block, name)
            .iter()
            .filter_map(|id| symbols.get(id))
            .filter(|symbol| symbol.language == Language::Rust)
            .collect::<Vec<_>>();
        // A nested `mod` of the name is the type the path names, which no edge reaches; a path
        // read in the type namespace continues through it (#654).
        if walk.namespace != Some(RustNamespace::Value)
            && defined
                .iter()
                .any(|symbol| symbol.kind == SymbolKind::Module)
        {
            if walk.namespace != Some(RustNamespace::Type) {
                return None;
            }
            let modules = defined
                .iter()
                .filter(|symbol| symbol.kind == SymbolKind::Module)
                .map(|symbol| symbol.id.clone())
                .collect::<Vec<_>>();
            return inline_module_target(&modules, scopes);
        }
        let items = defined
            .iter()
            .filter(|symbol| symbol.kind != SymbolKind::Module && walk.admits(symbol, scopes))
            .map(|symbol| symbol.id.clone())
            .collect::<Vec<_>>();
        self.defined_or_used_target(path, &items, walk, symbols, scopes)
    }

    /// What `path` names when a module on it is one a `use` brings in under a name the tree
    /// declares no module of (#641): `crate::facade::f` after `pub use inner as facade;` in the
    /// crate root is `crate::inner::f`. The `use` is read as any other in the module holding
    /// the name, and the path continues from the module file it names, in the same crate, with
    /// the walk one step longer. A module of the name the holding module defines, and a path
    /// through a module whose file configuration selects, are not read.
    fn aliased_path_target(
        &self,
        path: &RustUsePath,
        walk: ReexportWalk,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<RustPathTarget> {
        if walk.hops >= MAX_REEXPORT_HOPS {
            return None;
        }
        let (_, parent) = path.segments.split_last()?;
        let at =
            (0..parent.len()).find(|&end| !self.declares_file_modules(path, &parent[..=end]))?;
        let (holder, alias) = (&parent[..at], &parent[at]);
        if self.configured_choice(path, None, holder).is_some() {
            return None;
        }
        let stems = self.module_stems(path, holder);
        if !rust_module_symbols(&stems, alias, symbols).is_empty()
            || rust_module_items(&stems, alias, symbols)
                .iter()
                .filter_map(|item| symbols.get(item))
                .any(|symbol| scopes.rust_in_namespace(symbol, RustNamespace::Type))
        {
            return None;
        }
        let alias_path = RustUsePath {
            segments: parent[..=at].to_vec(),
            ..path.clone()
        };
        let target = self
            .used_name(
                &alias_path,
                symbols,
                scopes,
                walk.in_namespace(RustNamespace::Type),
            )
            .target()?;
        // The module is a file, or an inline `mod` block a glob brings in (#654), read from the
        // top of its file through the blocks around it.
        let (module_file, blocks) = match target {
            RustPathTarget {
                module_file: Some(module_file),
                item: None,
                configured: None,
                variant: None,
                ..
            } => (module_file, Vec::new()),
            RustPathTarget {
                module_file: None,
                item: Some(module),
                configured: None,
                variant: None,
                ..
            } => {
                let body = scopes.inline_module_body(&module)?;
                (
                    body.file_id.clone(),
                    inline_chain(&body.id, symbols, scopes)?,
                )
            }
            _ => return None,
        };
        let module_path = self.files.get(&module_file)?;
        let tree = self.crate_tree(module_path)?;
        if tree != path.tree {
            return None;
        }
        let rest = blocks
            .iter()
            .map(String::as_str)
            .chain(path.segments[at + 1..].iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("::");
        let mapped = map_rust_use_path(&tree, module_path, &format!("self::{rest}"))?;
        if !self.declares_file_modules(&mapped, &mapped.importer_module)
            || self
                .configured_choice(&mapped, None, &mapped.importer_module)
                .is_some()
        {
            return None;
        }
        let walk = ReexportWalk {
            hops: walk.hops + 1,
            ..walk
        };
        let found = self.placed_path_target(&mapped, walk, symbols, scopes)?;
        // What the path reaches below the module is read there; a configuration choice below it
        // is not.
        if found.configured.is_some()
            || self
                .configured_choice(&mapped, None, &mapped.segments)
                .is_some()
        {
            return None;
        }
        Some(RustPathTarget {
            reexport: true,
            ..found
        })
    }

    /// Where the module `parent` of `path` is: the files of a module the tree declares, or a
    /// block of inline `mod` items in the one file of the deepest module on the path the tree
    /// declares, when each remaining segment names one such block of the block above it.
    fn module_at(
        &self,
        path: &RustUsePath,
        parent: &[String],
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> ModuleAt {
        let files = || ModuleAt::Files(self.module_stems(path, parent));
        if parent.is_empty() || self.declares_file_modules(path, parent) {
            return files();
        }
        let Some(declared) = (0..parent.len())
            .rev()
            .find(|&end| end == 0 || self.declares_file_modules(path, &parent[..end]))
        else {
            return files();
        };
        let Some(file) = self.module_or_root_file(path, &parent[..declared]) else {
            return files();
        };
        match inline_block(&file, &parent[declared..], symbols, scopes) {
            Some(block) => ModuleAt::Inline(file, block),
            None => files(),
        }
    }

    /// What `path` names in each file of a module whose file configuration selects, when the
    /// path ends at or below one that `writer` is not below (#615): each file of a module the
    /// path names, or the item of its name in each file of the module holding it, as
    /// `configured` beside `placed`, what the placed tree reached. What `placed` reached is one
    /// of them, including through a `pub use` of one file, and a choice a `pub use` it was
    /// followed through passes, is kept too. A file inside one alternative, `path`-mounted or
    /// not, is compiled only with that alternative's files, so the path names those alone
    /// (#624), and when that leaves one file for every choice on the path, what the path names
    /// there is its proven target, in place of `placed`. `placed` stands alone for a path no such
    /// choice is on.
    fn configured_path_targets(
        &self,
        path: &RustUsePath,
        writer: Option<&Path>,
        placed: Option<RustPathTarget>,
        symbols: &open_kioku_resolution::SymbolIndex,
    ) -> ConfiguredPath {
        let Some((item_name, parent)) = path.segments.split_last() else {
            return ConfiguredPath::Alternatives(placed);
        };
        // A variant is reached only outside any configuration choice (#641).
        if item_name == "*"
            || placed
                .as_ref()
                .is_some_and(|placed| placed.variant.is_some())
        {
            return ConfiguredPath::Alternatives(placed);
        }
        let mut found = ConfiguredImportTargets::default();
        let mut reading = None;
        if let Some(read) = self.configured_choice(path, writer, &path.segments) {
            if let Some(files) = &read.files {
                found.module_files = self.file_ids(&files.files);
                found.files.clone_from(&files.files);
                found.unread = files.unread;
                reading = Some(read);
            }
        }
        if found.module_files.is_empty() {
            if let Some(read) = self.configured_choice(path, writer, parent) {
                if let Some(own) = &read.files {
                    found.items = rust_module_items(&own.files, item_name, symbols);
                }
                let files = read.files.as_ref().unwrap_or(&read.choice_files);
                found.files.clone_from(&files.files);
                found.unread = files.unread;
                reading = Some(read);
            }
        }
        // One file for every choice: the path names its module, or its one item of the name.
        // Several items of the name in that file (`#[cfg]`-gated definitions) stay unproven
        // alternatives below, as they are from outside.
        let several_items = found.module_files.is_empty() && found.items.len() > 1;
        if reading.as_ref().is_some_and(|read| read.proven) && !several_items {
            let target = match (found.module_files.as_slice(), found.items.as_slice()) {
                ([module_file], _) => Some(RustPathTarget {
                    module_file: Some(module_file.clone()),
                    item: None,
                    reexport: false,
                    configured: None,
                    variant: None,
                }),
                ([], [item]) => Some(RustPathTarget {
                    module_file: None,
                    item: Some(item.clone()),
                    reexport: false,
                    configured: None,
                    variant: None,
                }),
                // Not declared in that file, or through its `pub use`, which is not followed
                // there: what the placed tree reached is in a file never compiled with this one.
                _ => None,
            };
            return ConfiguredPath::Proven(target);
        }
        // A file a build that compiles the writer may compile: below no choice, or on a route
        // that agrees with the writer's.
        let may_compile = |file: &FileId| {
            reading
                .as_ref()
                .is_none_or(|read: &RustConfiguredRead<'_>| read.may_compile(file))
        };
        let item_compiles = |item: &SymbolId| {
            symbols
                .get(item)
                .is_some_and(|symbol| may_compile(&symbol.file_id))
        };
        let inner = placed
            .as_ref()
            .and_then(|placed| placed.configured.as_ref());
        if found.files.is_empty() && inner.is_none() {
            return ConfiguredPath::Alternatives(placed);
        }
        let mut target = placed.clone().unwrap_or(RustPathTarget {
            module_file: None,
            item: None,
            reexport: false,
            configured: None,
            variant: None,
        });
        // The placed tree's own target stays on the binding only where a build compiling the
        // writer may compile it.
        target.item = target.item.filter(|item| item_compiles(item));
        target.module_file = target.module_file.filter(|file| may_compile(file));
        found.items.extend(target.item.iter().cloned());
        found
            .module_files
            .extend(target.module_file.iter().cloned());
        if let Some(inner) = inner {
            found.items.extend(
                inner
                    .items
                    .iter()
                    .filter(|item| item_compiles(item))
                    .cloned(),
            );
            found.module_files.extend(
                inner
                    .module_files
                    .iter()
                    .filter(|file| may_compile(file))
                    .cloned(),
            );
            found.files.extend(inner.files.iter().cloned());
            found.unread |= inner.unread;
        }
        found.items.sort_by(|left, right| left.0.cmp(&right.0));
        found.items.dedup();
        found
            .module_files
            .sort_by(|left, right| left.0.cmp(&right.0));
        found.module_files.dedup();
        found.files.sort();
        found.files.dedup();
        if found.items.is_empty() && found.module_files.is_empty() {
            return ConfiguredPath::Alternatives(
                (target.item.is_some() || target.module_file.is_some()).then_some(target),
            );
        }
        target.configured = Some(found);
        ConfiguredPath::Alternatives(Some(target))
    }

    /// The ids of the indexed files at `stems`.
    fn file_ids(&self, stems: &[String]) -> Vec<FileId> {
        stems
            .iter()
            .filter_map(|stem| self.files_by_stem.get(stem).cloned())
            .collect()
    }

    /// What `module` of the crate `path` is read in names, when it is at or below a module whose
    /// file configuration selects, read from `writer`, the file writing the path (`None` for a
    /// path through a crate name, which is written in another crate). A writer the tree does not
    /// place, such as a file a `path` attribute mounts, is read by its route through the choices
    /// (#624). `None` when `writer` is placed below the same choice: it is compiled only with the
    /// file of the choice holding it, so a path that stays below the choice names that file's
    /// modules alone, as the tree places them, unless a `mod` item outside the choice also
    /// reaches it.
    fn configured_choice(
        &self,
        path: &RustUsePath,
        writer: Option<&Path>,
        module: &[String],
    ) -> Option<RustConfiguredRead<'_>> {
        let configured = self.configured();
        if configured.is_empty() {
            return None;
        }
        let placed = writer.and_then(|writer| self.placed_module(writer));
        let stem = writer.and_then(rust_file_stem);
        self.crate_roots(path).iter().find_map(|root| {
            let modules = configured.get(&root.replace('/', "::"))?;
            let from = stem
                .as_deref()
                .and_then(|stem| modules.route_of(stem, placed.as_deref()));
            let read = modules.read(module, from)?;
            // A placed file the walk found no one route to, such as one a `path` attribute
            // outside the choice also mounts, is compiled whichever file the choice takes.
            let walked = stem
                .as_deref()
                .and_then(|stem| self.files_by_stem.get(stem))
                .is_some_and(|id| modules.stems.contains_key(id));
            if placed
                .as_ref()
                .is_some_and(|writer| writer.starts_with(&read.choice))
                && (!walked || from.is_some())
            {
                return None;
            }
            Some(read)
        })
    }

    /// The module the declared tree places `file` at, `None` for a file it does not place.
    fn placed_module(&self, file: &Path) -> Option<Vec<String>> {
        let file = self.module_file_of(file)?;
        let (placed, _) = self.declared_placement(&file);
        placed.then_some(file.importer_module)
    }

    /// What the last segment of `path` names through the `use` sites of its parent module,
    /// followed from the module's file: any of them for a path from the module's own crate, and
    /// only `pub use` for one from another (#476). A named `use` shadows a glob; the name is bound
    /// only when every `use` of it the module holds agrees, and one the tree cannot follow leaves
    /// it unbound.
    fn reexported_target(
        &self,
        path: &RustUsePath,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
        walk: ReexportWalk,
    ) -> Option<RustPathTarget> {
        let target = self.used_name(path, symbols, scopes, walk).target()?;
        Some(RustPathTarget {
            reexport: true,
            ..target
        })
    }

    /// What the `use` sites at the top level of the module holding the last segment of `path`
    /// bring in under that name, each followed from the module's file. A glob brings in only an
    /// item it can see: from another crate a `pub` one, and from the module's own crate one that
    /// is not private to its module; a module or a private item through a glob is left
    /// unresolved, and so is a glob of a file whose top level invokes a macro. A cycle of
    /// `use` declarations, or a chain longer than [`MAX_REEXPORT_HOPS`], is unresolved too.
    fn used_name(
        &self,
        path: &RustUsePath,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
        walk: ReexportWalk,
    ) -> Rc<UsedName> {
        let Some((name, parent)) = path.segments.split_last() else {
            return Rc::new(UsedName::unknown());
        };
        if name == "*" {
            return Rc::new(UsedName::unknown());
        }
        let module = self.module_at(path, parent, symbols, scopes);
        let key = (
            module.clone(),
            name.clone(),
            walk.from_other_crate,
            walk.namespace,
        );
        let begun =
            self.used_names
                .borrow_mut()
                .begin(key, walk.hops, walk.hops >= MAX_REEXPORT_HOPS);
        let reading = match begun {
            Begin::Found(found) => return found,
            Begin::Cut => return Rc::new(UsedName::unknown()),
            Begin::Read(reading) => reading,
        };
        let found = match &module {
            ModuleAt::Files(stems) => self.module_used_name(stems, name, symbols, scopes, walk),
            ModuleAt::Inline(file, block) => {
                self.block_used_name((file, block), name, symbols, scopes, walk)
            }
        };
        self.used_names.borrow_mut().finish(reading, found)
    }

    /// What the `use` sites written directly in the inline `mod` block `block` of `file` bring
    /// in under `name` (#641), each path read from the file's top level as
    /// [`inline_use_source`] rewrites it. Only named `use` sites are followed: a macro the block
    /// invokes, which the parser does not record, may expand to an item that shadows a glob, so
    /// no glob of a block settles a name.
    fn block_used_name(
        &self,
        (file, block): (&FileId, &ScopeId),
        name: &str,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
        walk: ReexportWalk,
    ) -> UsedName {
        let mut found = UsedName::default();
        let (Some(importer), Some(chain)) =
            (self.files.get(file), inline_chain(block, symbols, scopes))
        else {
            return UsedName::unknown();
        };
        let sites = self
            .module_uses
            .get(file)
            .into_iter()
            .flatten()
            .filter(|site| site.scope_id.as_ref() == Some(block))
            .filter(|site| !walk.from_other_crate || site.reexported);
        for site in sites {
            if site.is_glob {
                found.glob_unresolved = true;
                continue;
            }
            for binding in site.bindings.iter().filter(|binding| binding.local == name) {
                let target = inline_use_source(&site.source, &chain).and_then(|source| {
                    // A path rewritten from the file's top level is read there; any other is
                    // read where it is written, which sees no module of the file around it.
                    let scope = if source == site.source {
                        Some(block)
                    } else {
                        None
                    };
                    self.reexport_source_target(
                        (file, importer),
                        scope,
                        &source,
                        &binding.imported,
                        (symbols, scopes),
                        walk,
                    )
                });
                match target {
                    Some(target) => found.named.push(target),
                    None => found.named_unresolved = true,
                }
            }
        }
        found
    }

    /// What the `use` sites at the top level of the files at `stems` bring in under `name`: see
    /// [`RustModuleTree::used_name`].
    fn module_used_name(
        &self,
        stems: &[String],
        name: &str,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
        walk: ReexportWalk,
    ) -> UsedName {
        let mut found = UsedName::default();
        let files = stems
            .iter()
            .filter_map(|stem| self.files_by_stem.get(stem))
            .filter_map(|file| Some((file, self.files.get(file)?)))
            .collect::<Vec<_>>();
        let sites = |file: &FileId| {
            self.module_uses
                .get(file)
                .into_iter()
                .flatten()
                .filter(|site| is_module_level(site.scope_id.as_ref(), scopes))
                .filter(|site| !walk.from_other_crate || site.reexported)
        };
        for &(file, importer) in &files {
            for site in sites(file).filter(|site| !site.is_glob) {
                for binding in site
                    .bindings
                    .iter()
                    .filter(|binding| binding.local == *name)
                {
                    match self.reexport_source_target(
                        (file, importer),
                        site.scope_id.as_ref(),
                        &site.source,
                        &binding.imported,
                        (symbols, scopes),
                        walk,
                    ) {
                        Some(target) => found.named.push(target),
                        None => found.named_unresolved = true,
                    }
                }
            }
            // A macro may expand to an item or a `use` of the name the parser does not see.
            found.macro_expanded |= self.module_macros.contains(file)
                || self
                    .module_macro_names
                    .get(file)
                    .is_some_and(|names| names.contains(name));
        }
        // A named `use` shadows the globs, and a macro may shadow them too, so neither leaves
        // anything a glob brings in to read: see [`UsedName::target`]. A glob that cannot be
        // followed leaves the name unsettled whatever the others bring in. Reading the globs
        // past either would only walk `use` cycles to no effect (#659).
        if !found.named.is_empty() || found.named_unresolved || found.macro_expanded {
            return found;
        }
        for &(file, importer) in &files {
            for site in sites(file).filter(|site| site.is_glob) {
                if found.glob_unresolved {
                    return found;
                }
                let Some(prefix) = site.source.strip_suffix("::*") else {
                    found.glob_unresolved = true;
                    continue;
                };
                let source = format!("{prefix}::{name}");
                let target = self.reexport_source_target(
                    (file, importer),
                    site.scope_id.as_ref(),
                    &source,
                    name,
                    (symbols, scopes),
                    walk,
                );
                match target {
                    Some(target) if glob_brings_in(&target, walk, symbols, scopes) => {
                        found.globbed.push(target);
                    }
                    // A module file, as a type, when its `mod` item is visible (#654).
                    Some(target)
                        if walk.namespace == Some(RustNamespace::Type)
                            && target.item.is_none()
                            && target.configured.is_none()
                            && target.module_file.is_some()
                            && self.glob_module_is_visible(
                                (file, importer),
                                site,
                                name,
                                (symbols, scopes),
                                walk,
                            ) =>
                    {
                        found.globbed.push(target);
                    }
                    // A module the glob opens that holds nothing of the name brings none in.
                    None if self.glob_lacks_name(
                        (file, importer),
                        site,
                        name,
                        (symbols, scopes),
                        walk,
                    ) => {}
                    _ => found.glob_unresolved = true,
                }
            }
        }
        found
    }

    /// Whether the one `mod name` item of the module the glob `site` in `importer` opens is
    /// visible to the glob, as [`glob_brings_in`] reads an item's visibility (#654).
    fn glob_module_is_visible(
        &self,
        importer: (&FileId, &Path),
        site: &ImportSite,
        name: &str,
        (symbols, scopes): (
            &open_kioku_resolution::SymbolIndex,
            &open_kioku_resolution::ScopeIndex,
        ),
        walk: ReexportWalk,
    ) -> bool {
        let Some((path, crate_name)) = self.rust_path(
            importer,
            site.scope_id.as_ref(),
            &site.source,
            symbols,
            scopes,
        ) else {
            return false;
        };
        let Some((_, module)) = path.segments.split_last() else {
            return false;
        };
        if !self.declares_file_modules(&path, module) {
            return false;
        }
        let stems = self.module_stems(&path, module);
        let modules = rust_module_symbols(&stems, name, symbols);
        let [module] = modules.as_slice() else {
            return false;
        };
        symbols.get(module).is_some_and(|symbol| {
            if walk.from_other_crate || crate_name {
                symbol.visibility == Visibility::Public
            } else {
                symbol.visibility != Visibility::Private
            }
        })
    }

    /// Whether the module the glob `site` in `importer` opens provably holds nothing named
    /// `name`: it is a module the tree declares as a file, read outside any configuration choice,
    /// whose files define no item or module of the name and whose top-level `use` sites bring
    /// none in, followed as a glob from `walk` would follow them.
    fn glob_lacks_name(
        &self,
        importer: (&FileId, &Path),
        site: &ImportSite,
        name: &str,
        (symbols, scopes): (
            &open_kioku_resolution::SymbolIndex,
            &open_kioku_resolution::ScopeIndex,
        ),
        walk: ReexportWalk,
    ) -> bool {
        let Some((mut path, crate_name)) = self.rust_path(
            importer,
            site.scope_id.as_ref(),
            &site.source,
            symbols,
            scopes,
        ) else {
            return false;
        };
        let Some((_, module)) = path.segments.split_last() else {
            return false;
        };
        let module = module.to_vec();
        let walk = ReexportWalk {
            from_other_crate: walk.from_other_crate || crate_name,
            hops: walk.hops + 1,
            ..walk
        };
        if !self.declares_file_modules(&path, &module) {
            // A glob of an enum brings in its variants alone (#641).
            return self
                .enum_at(&path, &module, walk, symbols, scopes)
                .and_then(|found| scopes.rust_enum_variants(&found.item))
                .is_some_and(|variants| !variants.iter().any(|variant| variant == name));
        }
        let writer = (!crate_name).then_some(importer.1);
        if self.configured_choice(&path, writer, &module).is_some() {
            return false;
        }
        let stems = self.module_stems(&path, &module);
        if !stems
            .iter()
            .any(|stem| self.files_by_stem.contains_key(stem))
        {
            return false;
        }
        // Only an item of the namespace the walk reads is the name there (#654): a module `f`
        // the glob opens holds brings in no value `f`, and a `fn f` no type.
        let defined = stems.iter().any(|stem| {
            symbols
                .by_qualified
                .get(&format!("{}::{name}", stem.replace('/', "::")))
                .is_some_and(|ids| {
                    ids.iter()
                        .filter_map(|id| symbols.get(id))
                        .any(|symbol| walk.admits(symbol, scopes))
                })
        });
        path.segments = module;
        path.segments.push(name.to_string());
        let module_file = walk.namespace != Some(RustNamespace::Value)
            && self.module_file(&path, &path.segments).is_some();
        if defined || module_file {
            return false;
        }
        let used = self.used_name(&path, symbols, scopes, walk);
        used.named.is_empty()
            && !used.named_unresolved
            && used.globbed.is_empty()
            && !used.glob_unresolved
            && !used.macro_expanded
    }

    /// What the path a `use` in `importer`, written at `scope`, names, when its last segment is
    /// `imported`. Such a path may also be a Rust 2018 path from the module of the `use`
    /// (`pub use auth::Token;` beside `mod auth;`), read as [`RustModuleTree::use_path_start`]
    /// reads it. Past a crate name the walk is in another crate, and stays there.
    fn reexport_source_target(
        &self,
        importer: (&FileId, &Path),
        scope: Option<&ScopeId>,
        source: &str,
        imported: &str,
        (symbols, scopes): (
            &open_kioku_resolution::SymbolIndex,
            &open_kioku_resolution::ScopeIndex,
        ),
        walk: ReexportWalk,
    ) -> Option<RustPathTarget> {
        let (path, crate_name) = self.rust_path(importer, scope, source, symbols, scopes)?;
        let (_, importer) = importer;
        if path.segments.last().map(String::as_str) != Some(imported) {
            return None;
        }
        // `pub use imp::f;` in the module declaring a configuration-selected `imp` makes no
        // choice, so the re-exported name is `f` of each file `imp` may be.
        let writer = (!crate_name).then_some(importer);
        let walk = ReexportWalk {
            from_other_crate: walk.from_other_crate || crate_name,
            hops: walk.hops + 1,
            ..walk
        };
        self.rust_path_target(&path, writer, walk, symbols, scopes)
    }

    /// What each name the `use` sites of a Rust module bring in names, for paths through the
    /// module that the resolver reads (#476), by the qualified name tree-sitter would give an
    /// item of the name in the module's file. Recorded: a name the module brings in and does not
    /// define, when the `use` sites settle what it is, and a name it defines that a named `use`
    /// beside it may stand for instead, which stays ambiguous. Only files the tree places, with `use`
    /// sites at their top level, are read; the names read are those their named `use` sites bind
    /// and those their globs may bring in, that `wanted` admits in some namespace: a path the
    /// resolver reads ends in a name some call writes, as its callee, read as a value, or as a
    /// segment of its receiver, read as a type (#643). Each name is read in the namespaces
    /// `wanted` admits it in.
    pub(crate) fn reexports(
        &self,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
        wanted: impl Fn(&str, RustNamespace) -> bool,
    ) -> HashMap<String, RustReexport> {
        let namespaces = |name: &str| {
            [RustNamespace::Type, RustNamespace::Value]
                .into_iter()
                .filter(|namespace| wanted(name, *namespace))
                .collect::<Vec<_>>()
        };
        let names = self.module_use_names(symbols, scopes);
        let mut reexports = HashMap::new();
        let mut files = names.keys().collect::<Vec<_>>();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        for file in files {
            let (Some(importer), Some(stem)) = (
                self.files.get(file),
                self.files.get(file).and_then(|path| rust_file_stem(path)),
            ) else {
                continue;
            };
            let exports = self
                .module_uses
                .get(file)
                .into_iter()
                .flatten()
                .any(|site| site.reexported && is_module_level(site.scope_id.as_ref(), scopes));
            for name in &names[file] {
                let read = namespaces(name);
                if read.is_empty() {
                    continue;
                }
                let Some((path, _)) = self.rust_path(
                    (file, importer),
                    None,
                    &format!("self::{name}"),
                    symbols,
                    scopes,
                ) else {
                    continue;
                };
                if let Some(reexport) =
                    self.reexport_entry(file, &path, (exports, &read), symbols, scopes)
                {
                    reexports.insert(format!("{}::{name}", stem.replace('/', "::")), reexport);
                }
            }
        }
        // Paths through a module a `use` renames and through inline `mod` blocks, by the names
        // the resolver spells for them from the module path (#641).
        for (key, file, path) in self.renamed_and_inline_paths(&names, symbols, scopes, &wanted) {
            let Some(name) = path.segments.last() else {
                continue;
            };
            let read = namespaces(name);
            if let Some(reexport) =
                self.reexport_entry(&file, &path, (true, &read), symbols, scopes)
            {
                reexports.entry(key).or_insert(reexport);
            }
        }
        reexports
    }

    /// What the name `path` ends in, written from `file`, is through the `use` sites of the
    /// module holding it, in each of `namespaces`, from the module's own crate and, when
    /// `exports`, from another: see [`RustModuleTree::reexports`]. `None` when nothing is
    /// recorded.
    fn reexport_entry(
        &self,
        file: &FileId,
        path: &RustUsePath,
        (exports, namespaces): (bool, &[RustNamespace]),
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<RustReexport> {
        let read = |from_other_crate: bool| {
            let walk = ReexportWalk::start(from_other_crate);
            let read_in = |namespace: RustNamespace| {
                namespaces
                    .contains(&namespace)
                    .then(|| self.reexport_at(path, walk.in_namespace(namespace), symbols, scopes))
                    .flatten()
            };
            RustReexportNamespaces {
                types: read_in(RustNamespace::Type),
                values: read_in(RustNamespace::Value),
            }
        };
        let in_crate = read(false);
        let from_other_crates = if exports {
            read(true)
        } else {
            RustReexportNamespaces::default()
        };
        if in_crate.is_empty() && from_other_crates.is_empty() {
            return None;
        }
        Some(RustReexport {
            file: file.clone(),
            in_crate,
            from_other_crates,
        })
    }

    /// What the name `path` ends in is through the `use` sites of the module holding it, as
    /// `walk` follows them and in the namespace it reads (#643): when the module defines no item
    /// of the name there, what a `use` brings in, and when it defines one that a named `use`
    /// beside it may stand for instead, both, ambiguous. A path through a module a `use`
    /// renames is what it names through that module (#641). See [`RustModuleTree::reexports`].
    fn reexport_at(
        &self,
        path: &RustUsePath,
        walk: ReexportWalk,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> Option<RustReexported> {
        let (name, parent) = path.segments.split_last()?;
        let is_type = walk.namespace != Some(RustNamespace::Value);
        if is_type && self.module_file(path, &path.segments).is_some() {
            return None;
        }
        let admitted = |symbol: &open_kioku_core::Symbol| walk.admits(symbol, scopes);
        let items = match self.module_at(path, parent, symbols, scopes) {
            ModuleAt::Files(stems)
                if parent.is_empty() || self.declares_file_modules(path, parent) =>
            {
                if is_type && !rust_module_symbols(&stems, name, symbols).is_empty() {
                    return None;
                }
                rust_module_items(&stems, name, symbols)
                    .into_iter()
                    .filter(|item| symbols.get(item).is_some_and(admitted))
                    .collect::<Vec<_>>()
            }
            ModuleAt::Inline(file, block) => {
                let defined = symbols
                    .lookup_file_scope_name(&file, &block, name)
                    .iter()
                    .filter_map(|id| symbols.get(id))
                    .filter(|symbol| symbol.language == Language::Rust)
                    .collect::<Vec<_>>();
                if is_type
                    && defined
                        .iter()
                        .any(|symbol| symbol.kind == SymbolKind::Module)
                {
                    return None;
                }
                defined
                    .into_iter()
                    .filter(|symbol| symbol.kind != SymbolKind::Module && admitted(symbol))
                    .map(|symbol| symbol.id.clone())
                    .collect()
            }
            ModuleAt::Files(_) => {
                return reexported_value(self.aliased_path_target(path, walk, symbols, scopes)?);
            }
        };
        match items.as_slice() {
            [] => reexported_value(self.reexported_target(path, symbols, scopes, walk)?),
            [item] => {
                let used = self.used_name(path, symbols, scopes, walk);
                if !used.may_override(item) {
                    return None;
                }
                let mut candidates = vec![item.clone()];
                for target in &used.named {
                    candidates.extend(target.item.iter().cloned());
                    if let Some(configured) = &target.configured {
                        candidates.extend(configured.items.iter().cloned());
                    }
                }
                candidates.sort_by(|left, right| left.0.cmp(&right.0));
                candidates.dedup();
                Some(RustReexported::Ambiguous(candidates))
            }
            _ => None,
        }
    }

    /// The paths the resolver may read through a module a `use` renames, or through a block of
    /// inline `mod` items, keyed by the qualified name it spells for each from the module path
    /// (`src::facade::f` for `crate::facade::f()` after `pub use inner as facade;` in
    /// `src/lib.rs`), with the file whose `use` sites settle it (#641). A renamed module is
    /// read for the names its file defines or brings in, a block for the names its named `use`
    /// sites bind; only names `wanted` admits are read, through a renamed module some call writes in
    /// its receiver, and only from files the tree places.
    fn renamed_and_inline_paths(
        &self,
        names: &HashMap<FileId, BTreeSet<String>>,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
        wanted: &impl Fn(&str, RustNamespace) -> bool,
    ) -> Vec<(String, FileId, RustUsePath)> {
        let wants =
            |name: &str| wanted(name, RustNamespace::Type) || wanted(name, RustNamespace::Value);
        let mut found = Vec::new();
        let mut files = self.module_uses.keys().collect::<Vec<_>>();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        for file in files {
            let Some(importer) = self.files.get(file) else {
                continue;
            };
            let path_to = |segments: &[&str]| {
                let source = std::iter::once("self")
                    .chain(segments.iter().copied())
                    .collect::<Vec<_>>()
                    .join("::");
                self.rust_path((file, importer), None, &source, symbols, scopes)
                    .map(|(path, _)| path)
            };
            let mut renamed = BTreeSet::new();
            let mut blocks = BTreeMap::<Vec<String>, BTreeSet<String>>::new();
            // A module a glob may bring in is read as one a `use` renames (#654).
            if self.module_uses[file]
                .iter()
                .any(|site| site.is_glob && is_module_level(site.scope_id.as_ref(), scopes))
            {
                renamed.extend(names.get(file).into_iter().flatten().cloned());
            }
            for site in self.module_uses[file].iter().filter(|site| !site.is_glob) {
                let locals = site.bindings.iter().map(|binding| binding.local.clone());
                if is_module_level(site.scope_id.as_ref(), scopes) {
                    renamed.extend(locals);
                    continue;
                }
                let Some(chain) = site
                    .scope_id
                    .as_ref()
                    .filter(|scope| {
                        scopes
                            .get(scope)
                            .is_some_and(|scope| scope.kind == ScopeKind::Module)
                    })
                    .and_then(|scope| inline_chain(scope, symbols, scopes))
                else {
                    continue;
                };
                blocks
                    .entry(chain)
                    .or_default()
                    .extend(locals.filter(|name| wants(name)));
            }
            for alias in renamed
                .into_iter()
                .filter(|alias| wanted(alias, RustNamespace::Type))
            {
                let Some(alias_path) = path_to(&[alias.as_str()]) else {
                    continue;
                };
                let Some(target) = self
                    .used_name(
                        &alias_path,
                        symbols,
                        scopes,
                        ReexportWalk::start(false).in_namespace(RustNamespace::Type),
                    )
                    .target()
                else {
                    continue;
                };
                let mut reached = BTreeSet::new();
                if let Some(module_file) = &target.module_file {
                    // What the renamed module's file defines and brings in.
                    reached.extend(names.get(module_file).cloned().unwrap_or_default());
                    reached.extend(
                        symbols
                            .by_file
                            .get(module_file)
                            .into_iter()
                            .flatten()
                            .filter_map(|id| symbols.get(id))
                            .filter(|symbol| {
                                symbol.language == Language::Rust
                                    && symbol.parent_symbol_id.is_none()
                            })
                            .map(|symbol| symbol.name.clone()),
                    );
                } else if let Some(body) = target
                    .item
                    .as_ref()
                    .and_then(|module| scopes.inline_module_body(module))
                {
                    // What an inline block a glob brings in defines and names with `use`.
                    reached.extend(
                        symbols
                            .by_file
                            .get(&body.file_id)
                            .into_iter()
                            .flatten()
                            .filter_map(|id| symbols.get(id))
                            .filter(|symbol| symbol.scope_id.as_ref() == Some(&body.id))
                            .map(|symbol| symbol.name.clone()),
                    );
                    reached.extend(
                        self.module_uses
                            .get(&body.file_id)
                            .into_iter()
                            .flatten()
                            .filter(|site| {
                                !site.is_glob && site.scope_id.as_ref() == Some(&body.id)
                            })
                            .flat_map(|site| {
                                site.bindings.iter().map(|binding| binding.local.clone())
                            }),
                    );
                }
                for name in reached.iter().filter(|name| wants(name)) {
                    if let Some(path) = path_to(&[alias.as_str(), name.as_str()]) {
                        found.push((resolver_qualified_name(&path), file.clone(), path));
                    }
                }
            }
            for (chain, names) in blocks {
                for name in names {
                    let segments = chain
                        .iter()
                        .map(String::as_str)
                        .chain(std::iter::once(name.as_str()))
                        .collect::<Vec<_>>();
                    if let Some(path) = path_to(&segments) {
                        found.push((resolver_qualified_name(&path), file.clone(), path));
                    }
                }
            }
        }
        found
    }

    /// The names each Rust file's top-level `use` sites may bring in: those its named sites bind,
    /// and, through its globs, the names each module a glob opens defines or brings in itself,
    /// read to a fixed point so that globs of globs are covered.
    fn module_use_names(
        &self,
        symbols: &open_kioku_resolution::SymbolIndex,
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> HashMap<FileId, BTreeSet<String>> {
        let mut names = HashMap::<FileId, BTreeSet<String>>::new();
        let mut opens = HashMap::<FileId, Vec<FileId>>::new();
        for (file, sites) in &self.module_uses {
            let Some(importer) = self.files.get(file) else {
                continue;
            };
            let sites = sites
                .iter()
                .filter(|site| is_module_level(site.scope_id.as_ref(), scopes))
                .collect::<Vec<_>>();
            if sites.is_empty() {
                continue;
            }
            let own = names.entry(file.clone()).or_default();
            for site in sites {
                if !site.is_glob {
                    own.extend(site.bindings.iter().map(|binding| binding.local.clone()));
                    continue;
                }
                let Some((path, _)) = self.rust_path(
                    (file, importer),
                    site.scope_id.as_ref(),
                    &site.source,
                    symbols,
                    scopes,
                ) else {
                    continue;
                };
                let Some((_, module)) = path.segments.split_last() else {
                    continue;
                };
                let stems = if module.is_empty() {
                    self.module_stems(&path, module)
                } else {
                    path.module_file_stems(module)
                };
                opens.entry(file.clone()).or_default().extend(
                    stems
                        .iter()
                        .filter_map(|stem| self.files_by_stem.get(stem).cloned()),
                );
            }
        }
        // What a glob may bring in: what the module it opens defines, and what it brings in.
        let supplies = |file: &FileId, names: &HashMap<FileId, BTreeSet<String>>| {
            let mut found = names.get(file).cloned().unwrap_or_default();
            found.extend(
                symbols
                    .by_file
                    .get(file)
                    .into_iter()
                    .flatten()
                    .filter_map(|id| symbols.get(id))
                    .filter(|symbol| {
                        symbol.language == Language::Rust && symbol.parent_symbol_id.is_none()
                    })
                    .map(|symbol| symbol.name.clone()),
            );
            found
        };
        let mut opening = opens.keys().cloned().collect::<Vec<_>>();
        opening.sort_by(|left, right| left.0.cmp(&right.0));
        loop {
            let mut changed = false;
            for file in &opening {
                let mut found = BTreeSet::new();
                for opened in &opens[file] {
                    found.extend(supplies(opened, &names));
                }
                let own = names.entry(file.clone()).or_default();
                let before = own.len();
                own.extend(found);
                changed |= own.len() != before;
            }
            if !changed {
                break;
            }
        }
        names
    }

    /// Records the repository-relative paths discovery skipped; only Rust files matter.
    pub(crate) fn with_unindexed_files<'p>(
        mut self,
        paths: impl IntoIterator<Item = &'p Path>,
    ) -> Self {
        self.unindexed_stems
            .extend(paths.into_iter().filter_map(rust_file_stem));
        self.shared_files = OnceCell::new();
        self.configured = OnceCell::new();
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

    /// The skipped files whose `mod` items decide which files another crate may compile and that
    /// have not been read yet: the [`unread_crate_roots`], and the files a `#[path]` attribute
    /// mounts, or that sit below one, that were not indexed. Reading one can reach another
    /// below it, so a caller reads until none is left that it can read.
    ///
    /// [`unread_crate_roots`]: RustModuleTree::unread_crate_roots
    pub(crate) fn unscanned_module_files(&self) -> HashSet<PathBuf> {
        let mut files = self.unread_crate_roots();
        files.retain(|path| {
            rust_file_stem(path).is_some_and(|stem| !self.scanned_files.contains_key(&stem))
        });
        files.extend(
            self.shared_files()
                .unscanned
                .iter()
                .map(|stem| PathBuf::from(format!("{stem}.rs"))),
        );
        files
    }

    /// Records the module names, and `cfg_attr` paths, Rust files skipped for size declare, as
    /// [`scan_module_declarations`] read them (`None` where it could not tell).
    ///
    /// [`scan_module_declarations`]: crate::rust_use_path::scan_module_declarations
    pub(crate) fn with_scanned_files<'p>(
        mut self,
        files: impl IntoIterator<Item = (&'p Path, Option<ScannedModules>)>,
    ) -> Self {
        self.scanned_files.extend(
            files
                .into_iter()
                .filter_map(|(path, names)| Some((rust_file_stem(path)?, names))),
        );
        self.shared_files = OnceCell::new();
        self.configured = OnceCell::new();
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

    /// [`RustModuleTree::declares_file_modules`] from the crate roots `roots`. A module file may
    /// also be declared inside inline `mod` blocks of the file above it (#633): `mod b { mod sys;
    /// }` in the crate root declares `b::sys` as a file, though `b` is no file.
    fn declared_below(&self, roots: &[String], path: &RustUsePath, module: &[String]) -> bool {
        let mut depth = 0;
        while depth < module.len() {
            // How far each file holding `module[..depth]` declares the path as a file: one
            // module further, or through inline blocks to the module file they declare.
            let next = |stem: &String| {
                let mut found = (depth..module.len()).filter(|&end| {
                    if end == depth {
                        self.file_modules
                            .contains(&(stem.clone(), module_name(&module[depth]).to_string()))
                    } else {
                        let chain = module[depth..=end]
                            .iter()
                            .map(|segment| module_name(segment).to_string())
                            .collect::<Vec<_>>();
                        self.inline_file_modules.contains(&(stem.clone(), chain))
                    }
                });
                match (found.next(), found.next()) {
                    (Some(end), None) => Some(end + 1),
                    _ => None,
                }
            };
            let reached = if depth == 0 {
                // Every crate root that holds the file declares the path the same way.
                let mut reached = roots.iter().map(next);
                match reached.next() {
                    Some(Some(first)) if reached.all(|other| other == Some(first)) => Some(first),
                    _ => None,
                }
            } else {
                let mut reached = path
                    .module_file_stems(&module[..depth])
                    .iter()
                    .filter_map(next)
                    .collect::<Vec<_>>();
                reached.sort_unstable();
                reached.dedup();
                match reached.as_slice() {
                    [only] => Some(*only),
                    _ => None,
                }
            };
            let Some(reached) = reached else {
                return false;
            };
            depth = reached;
        }
        true
    }

    /// Whether the file at `stem` declares a module file whose path starts with `top`: `mod top;`,
    /// or `mod top { .. }` holding such a declaration.
    fn declares_top(&self, stem: &str, top: &str) -> bool {
        let key = (stem.to_string(), module_name(top).to_string());
        self.file_modules.contains(&key) || self.inline_file_module_tops.contains(&key)
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
            .filter(|stem| self.declares_top(stem, top))
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

    /// The modules whose file configuration selects, by the qualified-name prefix of each crate
    /// root whose tree holds them (#613). A choice is made where the `mod name;` items of one
    /// placed file name more than one file for `name` between them: the default location of an
    /// item with no `path` attribute or only `cfg_attr` ones that may all fail, and each file a
    /// `path` names, so `#[cfg_attr(windows, path = "win.rs")] mod imp;` beside `imp.rs` and
    /// `#[cfg(unix)] mod imp;` beside `#[cfg(windows)] #[path = "win.rs"] mod imp;` both choose.
    /// A `path` the index cannot read, or a default location found both as `name.rs` and
    /// `name/mod.rs`, may name another file. Below a choice, the modules each of its files declares
    /// are followed as file modules, so a path below it can be read against every file.
    pub(crate) fn configured_modules(&self) -> HashMap<String, RustConfiguredModules> {
        self.configured().clone()
    }

    /// [`RustModuleTree::configured_modules`], computed on first use.
    fn configured(&self) -> &HashMap<String, RustConfiguredModules> {
        self.configured
            .get_or_init(|| self.find_configured_modules())
    }

    fn find_configured_modules(&self) -> HashMap<String, RustConfiguredModules> {
        let mut configured = HashMap::<String, RustConfiguredModules>::new();
        // The route to each file, by crate root; `None` for a file two routes reach.
        let mut routes = HashMap::<String, HashMap<String, Option<RustModuleRoute>>>::new();
        // The files some walk below a choice reached, and each `mod` item that makes a choice.
        let mut walked = HashSet::<String>::new();
        let mut choice_items = HashSet::<(String, String)>::new();
        let mut declaring = self
            .declared_modules
            .iter()
            .filter(|(_, items)| may_choose(items))
            .map(|(stem, _)| stem)
            .collect::<Vec<_>>();
        declaring.sort();
        for stem in declaring {
            let Some(file) = self
                .files_by_stem
                .get(stem)
                .and_then(|id| self.files.get(id))
                .and_then(|path| self.module_file_of(path))
            else {
                continue;
            };
            // A file the tree does not place is reached only below a choice of its own crate,
            // whose walk reads it.
            let (placed, roots) = self.declared_placement(&file);
            if !placed || roots.is_empty() {
                continue;
            }
            let mut names = self.declared_modules[stem]
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            for name in names {
                let mut module = file.importer_module.clone();
                module.push(name.to_string());
                let default = file.module_file_stems(&module);
                let (candidates, unread) = self.module_candidates(stem, name, &default);
                if candidates.len() + usize::from(unread) < 2 {
                    continue;
                }
                choice_items.insert((stem.clone(), name.to_string()));
                let mut found = RustConfiguredModules::default();
                let mut found_routes = HashMap::new();
                self.configured_subtree(
                    module,
                    (stem, candidates, unread),
                    &mut found,
                    &mut found_routes,
                );
                for root in &roots {
                    let root = root.replace('/', "::");
                    let into = configured.entry(root.clone()).or_default();
                    into.choices.extend(found.choices.iter().cloned());
                    for (module, files) in &found.files {
                        let entry = into.files.entry(module.clone()).or_default();
                        entry.files.extend(files.files.iter().cloned());
                        entry.unread |= files.unread;
                    }
                    into.stems.extend(
                        found
                            .stems
                            .iter()
                            .map(|(id, stem)| (id.clone(), stem.clone())),
                    );
                    for (module, declaring) in &found.unread_by {
                        into.unread_by
                            .entry(module.clone())
                            .or_default()
                            .extend(declaring.iter().cloned());
                    }
                    let into = routes.entry(root).or_default();
                    for (stem, route) in &found_routes {
                        merge_route(into, stem, route.clone());
                    }
                }
                walked.extend(found_routes.into_keys());
            }
        }
        // A file below a choice that a `mod` item outside every walk also reaches, such as
        // `#[path = "sys/unix/util.rs"] mod uu;` in the crate root, is compiled on every build
        // that compiles that item, whichever file the choice takes: it, and every file whose
        // route runs through it, has no route.
        let mounted_outside = self.reached_outside(&walked, &choice_items);
        for known in routes.values_mut() {
            for route in known.values_mut() {
                if route.as_ref().is_some_and(|route| {
                    route
                        .files
                        .values()
                        .any(|file| mounted_outside.contains(file))
                }) {
                    *route = None;
                }
            }
        }
        for (root, modules) in &mut configured {
            for files in modules.files.values_mut() {
                files.files.sort();
                files.files.dedup();
            }
            modules.routes = routes
                .remove(root)
                .into_iter()
                .flatten()
                .filter_map(|(stem, route)| Some((stem, route?)))
                .collect();
        }
        configured
    }

    /// The files the `mod` items of files no walk below a choice reached (`walked`) may compile
    /// their modules from, other than the items that make a choice (`choice_items`), whose
    /// files are the choice's own. A default location is read both from the file's place in the
    /// tree and from its own directory, since a file a `path` attribute mounts reads its children
    /// from there: a file found in more places than rustc compiles only loses a proof.
    fn reached_outside(
        &self,
        walked: &HashSet<String>,
        choice_items: &HashSet<(String, String)>,
    ) -> HashSet<String> {
        let mut reached = HashSet::new();
        for (declaring, items) in &self.declared_modules {
            if walked.contains(declaring) {
                continue;
            }
            let file = self
                .files_by_stem
                .get(declaring)
                .and_then(|id| self.files.get(id))
                .and_then(|path| self.module_file_of(path));
            let mut names = items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            for name in names {
                if choice_items.contains(&(declaring.clone(), name.to_string())) {
                    continue;
                }
                let mut default = file
                    .as_ref()
                    .map(|file| {
                        let mut module = file.importer_module.clone();
                        module.push(name.to_string());
                        file.module_file_stems(&module)
                    })
                    .unwrap_or_default();
                for dir in [parent_dir(declaring), declaring.as_str()] {
                    default.push(join_dir(dir, name));
                    default.push(join_dir(dir, &format!("{name}/mod")));
                }
                default.sort();
                default.dedup();
                // `name.rs` beside `name/mod.rs` reads as unread there; either may be compiled.
                reached.extend(
                    default
                        .iter()
                        .filter(|stem| {
                            items
                                .iter()
                                .any(|item| item.name == name && item.at_default)
                                && self.files_by_stem.contains_key(*stem)
                        })
                        .cloned(),
                );
                let (candidates, _) = self.module_candidates(declaring, name, &[]);
                reached.extend(candidates.into_iter().map(|(stem, _)| stem));
            }
        }
        // A `mod name;` inside inline blocks reaches its file below the directory they spell.
        for (declaring, chain) in &self.inline_file_modules {
            if walked.contains(declaring) {
                continue;
            }
            let below = chain.join("/");
            let mut default = self
                .files_by_stem
                .get(declaring)
                .and_then(|id| self.files.get(id))
                .and_then(|path| self.module_file_of(path))
                .map(|file| {
                    let mut module = file.importer_module.clone();
                    module.extend(chain.iter().cloned());
                    file.module_file_stems(&module)
                })
                .unwrap_or_default();
            for dir in [parent_dir(declaring), declaring.as_str()] {
                default.push(join_dir(dir, &below));
                default.push(join_dir(dir, &format!("{below}/mod")));
            }
            reached.extend(
                default
                    .into_iter()
                    .filter(|stem| self.files_by_stem.contains_key(stem)),
            );
        }
        reached
    }

    /// The files the `mod name;` items of `declaring` may compile module `name` from, each with
    /// whether a `path` attribute mounts it, given `default`, the files of its default location;
    /// and whether one of them the index cannot read or did not index may name another.
    fn module_candidates(
        &self,
        declaring: &str,
        name: &str,
        default: &[String],
    ) -> (Vec<(String, bool)>, bool) {
        let mut candidates = Vec::<(String, bool)>::new();
        let mut unread = false;
        let mut add = |stem: &str, mounted: bool, unread: &mut bool| {
            if self.files_by_stem.contains_key(stem) {
                if !candidates.iter().any(|(known, _)| known == stem) {
                    candidates.push((stem.to_string(), mounted));
                }
            } else if self.unindexed_stems.contains(stem) {
                *unread = true;
            }
        };
        for item in self
            .declared_modules
            .get(declaring)
            .into_iter()
            .flatten()
            .filter(|item| item.name == name)
        {
            if item.at_default {
                let found = default
                    .iter()
                    .filter(|stem| {
                        self.files_by_stem.contains_key(*stem)
                            || self.unindexed_stems.contains(*stem)
                    })
                    .collect::<Vec<_>>();
                match found.as_slice() {
                    [stem] => add(stem, false, &mut unread),
                    // `name.rs` beside `name/mod.rs`, which rustc rejects.
                    [_, _, ..] => unread = true,
                    [] => {}
                }
            }
            for path in &item.paths {
                match path {
                    Some(stem) => add(stem, true, &mut unread),
                    None => unread = true,
                }
            }
        }
        (candidates, unread)
    }

    /// Records `module`, which `declaring` gives a file configuration selects from `candidates`
    /// (with whether one of them the index cannot read or did not index may name another), and
    /// the modules its candidates declare below it, each with the files that may hold it, in
    /// `out`. A file a `path` mounts has its own `mod` items read from its directory, as a
    /// `mod.rs` file does. The route to each file the walk reaches by one route alone is recorded
    /// in `routes`, and `None` for one it reaches by more (#624).
    fn configured_subtree(
        &self,
        module: Vec<String>,
        (declaring, candidates, unread): (&str, Vec<(String, bool)>, bool),
        out: &mut RustConfiguredModules,
        routes: &mut HashMap<String, Option<RustModuleRoute>>,
    ) {
        out.choices.insert(module.clone());
        if unread {
            out.unread_by
                .entry(module.clone())
                .or_default()
                .insert(declaring.to_string());
        }
        // Each (module, file) the walk reached, with those it was reached from: a candidate of
        // the choice itself is reached from none.
        let mut parents = WalkParents::new();
        let mut pending = Vec::new();
        for (stem, mounted) in candidates {
            if parents
                .insert((module.clone(), stem.clone()), BTreeSet::new())
                .is_none()
            {
                pending.push((module.clone(), stem, mounted, unread));
            }
        }
        let tops = parents.keys().cloned().collect::<HashSet<_>>();
        while let Some((module, stem, mounted, unread)) = pending.pop() {
            let entry = out.files.entry(module.clone()).or_default();
            entry.files.push(stem.clone());
            entry.unread |= unread;
            // An unindexed file's `mod` items are not known: a path below it is read as below
            // the choice, against the files the tree places.
            let Some(id) = self.files_by_stem.get(&stem) else {
                continue;
            };
            out.stems.insert(id.clone(), stem.clone());
            if module.len() >= MAX_CONFIGURED_DEPTH {
                continue;
            }
            let dir = if mounted || is_mod_rs(&stem) {
                parent_dir(&stem)
            } else {
                stem.as_str()
            };
            let mut names = self
                .declared_modules
                .get(&stem)
                .into_iter()
                .flatten()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            for name in names {
                let default = [join_dir(dir, name), join_dir(dir, &format!("{name}/mod"))];
                let (candidates, unread) = self.module_candidates(&stem, name, &default);
                let mut child = module.clone();
                child.push(name.to_string());
                if candidates.len() + usize::from(unread) >= 2 {
                    out.choices.insert(child.clone());
                }
                if unread {
                    out.unread_by
                        .entry(child.clone())
                        .or_default()
                        .insert(stem.clone());
                }
                for (candidate, mounted) in candidates {
                    let from = parents
                        .entry((child.clone(), candidate.clone()))
                        .or_insert_with(|| {
                            pending.push((child.clone(), candidate.clone(), mounted, unread));
                            BTreeSet::new()
                        });
                    from.insert((module.clone(), stem.clone()));
                }
            }
        }
        let mut memo = HashMap::new();
        let mut by_stem = BTreeMap::<&str, Vec<&WalkNode>>::new();
        for node in parents.keys() {
            by_stem.entry(node.1.as_str()).or_default().push(node);
        }
        for (stem, nodes) in by_stem {
            let route = match nodes.as_slice() {
                [node] => {
                    node_route(node, &parents, &tops, &mut memo, 0).map(|files| RustModuleRoute {
                        module: node.0.clone(),
                        files,
                    })
                }
                // One file held as two modules is compiled with no one choice made.
                _ => None,
            };
            merge_route(routes, stem, route);
        }
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
        // The module files each file declares, by their path below its directory: `name`, or
        // `b/sys` for a `mod sys;` inside its inline `mod b`.
        let mut modules_by_file = HashMap::<&str, Vec<String>>::new();
        for (file, name) in &self.file_modules {
            modules_by_file.entry(file).or_default().push(name.clone());
        }
        for (file, module) in &self.inline_file_modules {
            modules_by_file
                .entry(file)
                .or_default()
                .push(module.join("/"));
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
        let mut unread_files = Vec::new();
        let mut unscanned = HashSet::new();
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
                // A mount below the mounted file the index cannot read, or an unindexed file
                // there whose `mod` items were not read, may mount any file of its package into
                // these crates too.
                for (below, declaring) in subtree.unread_mounts {
                    let package = self.package_of_stem(&declaring);
                    unread_mounts.push((below, package, crates.clone(), false));
                }
                for file in subtree.unread_files {
                    if !self.scanned_files.contains_key(&file) {
                        unscanned.insert(file.clone());
                    }
                    let package = self.package_of_stem(&file);
                    unread_files.push((file, package, crates.clone(), false));
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
                match self.scanned_files.get(root) {
                    Some(Some(scan)) if scan.conditional_paths.is_empty() => {
                        let top = below.split('/').next().unwrap_or(below);
                        if scan.names.contains(module_name(top)) {
                            reasons.push(true);
                        }
                    }
                    // Its lines may hold a `path` attribute, which mounts a file anywhere; a
                    // root's `cfg_attr` paths are not followed.
                    Some(_) => reasons.push(false),
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
            // A mount the index cannot read, or a mounted file it could not read, is read within
            // its own package.
            let unread = unread_mounts
                .iter_mut()
                .map(|(_, declaring, crates, fired)| (&*declaring, &*crates, fired))
                .chain(
                    unread_files
                        .iter_mut()
                        .map(|(_, declaring, crates, fired)| (&*declaring, &*crates, fired)),
                );
            for (declaring, crates, fired) in unread {
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
            unread_mounted_files: unread_files
                .iter()
                .filter(|(_, _, _, fired)| *fired)
                .map(|(file, ..)| file)
                .collect::<HashSet<_>>()
                .len(),
            unscanned,
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
                    .chain(&self.unindexed_stems)
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
    /// relative to the declaring file just as in its own crate. A module whose every `path` is a
    /// `cfg_attr` is also followed to its default location, which it compiles from whenever no
    /// condition holds (#608).
    ///
    /// A file of the subtree discovery saw but did not index has its `mod` items, and their
    /// `cfg_attr` paths, read from [`RustModuleTree::scanned_files`]; where they were not read,
    /// or could not tell, the file is recorded as unread, since it may mount any file of its
    /// package (#610).
    fn mounted_subtree(
        &self,
        mounted: &str,
        modules_by_file: &HashMap<&str, Vec<String>>,
        mounts_by_file: &HashMap<&str, Vec<(usize, &PathMount)>>,
    ) -> MountedSubtree {
        let mut subtree = MountedSubtree {
            files: vec![(mounted.to_string(), is_mod_rs(mounted))],
            unread_mounts: Vec::new(),
            unread_files: Vec::new(),
        };
        let known = |stem: &str| {
            self.files_by_stem.contains_key(stem) || self.unindexed_stems.contains(stem)
        };
        let mut seen = HashSet::from([mounted.to_string()]);
        let mut pending = vec![(mounted.to_string(), parent_dir(mounted).to_string())];
        while let Some((file, dir)) = pending.pop() {
            // The `cfg_attr` paths a scanned file's `mod` items set, which an indexed file's
            // `mounts_by_file` hold instead.
            let mut scanned_mounts = Vec::new();
            let names = if self.files_by_stem.contains_key(&file) {
                modules_by_file
                    .get(file.as_str())
                    .into_iter()
                    .flatten()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            } else if !self.unindexed_stems.contains(&file) {
                // A path naming no file discovery saw mounts nothing the index holds.
                Vec::new()
            } else if let Some(Some(scan)) = self.scanned_files.get(&file) {
                scanned_mounts = scan
                    .conditional_paths
                    .iter()
                    .filter_map(|value| PathMount::of(&file, value, false))
                    .collect();
                scan.names.iter().map(String::as_str).collect()
            } else {
                subtree.unread_files.push(file.clone());
                Vec::new()
            };
            for name in names {
                for child in [join_dir(&dir, name), join_dir(&dir, &format!("{name}/mod"))] {
                    if !known(&child) || !seen.insert(child.clone()) {
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
            let mut mounted_children = Vec::new();
            for (at, mount) in mounts_by_file.get(file.as_str()).into_iter().flatten() {
                match self.mounted_files(&file, mount) {
                    Some(children) => mounted_children.extend(children),
                    None => subtree.unread_mounts.push((*at, file.clone())),
                }
            }
            for mount in &scanned_mounts {
                match self.mounted_files(&file, mount) {
                    Some(children) => mounted_children.extend(children),
                    // A scanned path the index cannot follow leaves the file's mounts unread.
                    None => subtree.unread_files.push(file.clone()),
                }
            }
            for child in mounted_children {
                if !known(&child) || !seen.insert(child.clone()) {
                    continue;
                }
                let child_dir = parent_dir(&child).to_string();
                subtree.files.push((child.clone(), is_mod_rs(&child)));
                pending.push((child, child_dir));
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
            unread_mounted_files: self.shared_files().unread_mounted_files,
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

/// Whether the `mod` items of one file may choose between files for a module: two items of one
/// name, or one naming a file beside its default location, more than one file, or a file the
/// index cannot read. Only such a file is placed to find out.
fn may_choose(items: &[DeclaredModule]) -> bool {
    items.iter().enumerate().any(|(at, item)| {
        items[..at].iter().any(|other| other.name == item.name)
            || item.paths.len() + usize::from(item.at_default) >= 2
            || item.paths.iter().any(Option::is_none)
    })
}

/// A route through the configured subtree, keyed by module: the file each module is compiled
/// from.
type RouteFiles = BTreeMap<Vec<String>, String>;

/// A (module, file) the walk below a choice reached.
type WalkNode = (Vec<String>, String);

/// Each node the walk below a choice reached, with the nodes it was reached from.
type WalkParents = HashMap<WalkNode, BTreeSet<WalkNode>>;

/// The route to `node`, a (module, file) the walk below a choice reached: the files from the
/// choice down to it. `None` when it is reached from more than one (module, file), or from one
/// whose route is `None`, or a candidate of the choice is also reached from below it.
fn node_route(
    node: &WalkNode,
    parents: &WalkParents,
    tops: &HashSet<WalkNode>,
    memo: &mut HashMap<WalkNode, Option<RouteFiles>>,
    depth: usize,
) -> Option<RouteFiles> {
    if let Some(route) = memo.get(node) {
        return route.clone();
    }
    // Deeper than the walk goes only through a cycle of `path` attributes.
    if depth > MAX_CONFIGURED_DEPTH {
        return None;
    }
    let from = parents.get(node)?;
    let route = match (
        tops.contains(node),
        from.iter().collect::<Vec<_>>().as_slice(),
    ) {
        (true, []) => Some(BTreeMap::new()),
        (false, [parent]) => node_route(parent, parents, tops, memo, depth + 1),
        _ => None,
    }
    .map(|mut files| {
        files.insert(node.0.clone(), node.1.clone());
        files
    });
    memo.insert(node.clone(), route.clone());
    route
}

/// Records `route` for the file at `stem` in `routes`. Two walks may reach one file, such as the
/// walk below a choice and the walk below a choice nested in a placed file of it: the longer
/// route stands when it holds the shorter one; routes that disagree leave the file none.
fn merge_route(
    routes: &mut HashMap<String, Option<RustModuleRoute>>,
    stem: &str,
    route: Option<RustModuleRoute>,
) {
    let Some(known) = routes.get_mut(stem) else {
        routes.insert(stem.to_string(), route);
        return;
    };
    let holds = |outer: &RustModuleRoute, inner: &RustModuleRoute| {
        outer.module == inner.module
            && inner
                .files
                .iter()
                .all(|(module, file)| outer.files.get(module) == Some(file))
    };
    *known = match (known.take(), route) {
        (Some(left), Some(right)) if holds(&left, &right) => Some(left),
        (Some(left), Some(right)) if holds(&right, &left) => Some(right),
        _ => None,
    };
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
            configured_targets: None,
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
                binding.configured_targets = target.configured;
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
        (&binding.file_id, importer),
        Some(&binding.scope_id),
        &binding.source_module,
        symbols,
        scopes,
    )?;
    let (item_name, _) = path.segments.split_last()?;
    if *item_name != binding.imported_name {
        return None;
    }
    // A path through a crate name is written in another crate, where no choice of that crate's
    // configuration-selected modules is made.
    let writer = (!crate_name).then_some(importer);
    modules
        .rust_path_target(
            &path,
            writer,
            ReexportWalk::start(crate_name),
            symbols,
            scopes,
        )
        // An enum's variant is no item to bind (#641); its `IMPORTS` edge reaches the enum's
        // file all the same.
        .filter(|target| target.variant.is_none())
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

/// The module-level Rust `mod` items named `name` in the files at `module_stems`, which an inline
/// block of the name declares beside them as well as `mod name;` does.
fn rust_module_symbols(
    module_stems: &[String],
    name: &str,
    symbols: &open_kioku_resolution::SymbolIndex,
) -> Vec<SymbolId> {
    module_stems
        .iter()
        .filter_map(|stem| {
            symbols
                .by_qualified
                .get(&format!("{}::{name}", stem.replace('/', "::")))
        })
        .flatten()
        .filter(|id| {
            symbols.get(id).is_some_and(|symbol| {
                symbol.language == Language::Rust
                    && symbol.parent_symbol_id.is_none()
                    && symbol.kind == SymbolKind::Module
            })
        })
        .cloned()
        .collect()
}

/// The scope of the block of inline `mod` items `chain` names in `file`, from its top level
/// down: `["n", "inner"]` for `mod n { mod inner { .. } }`. `None` unless each segment names
/// exactly one `mod` item of the block above it, and that item has a body.
fn inline_block(
    file: &FileId,
    chain: &[String],
    symbols: &open_kioku_resolution::SymbolIndex,
    scopes: &open_kioku_resolution::ScopeIndex,
) -> Option<ScopeId> {
    let mut block: Option<ScopeId> = None;
    for segment in chain {
        let found = symbols
            .lookup_file_name(file, module_name(segment))
            .iter()
            .filter_map(|id| symbols.get(id))
            .filter(|symbol| {
                symbol.kind == SymbolKind::Module
                    && match &block {
                        Some(block) => symbol.scope_id.as_ref() == Some(block),
                        None => symbol
                            .scope_id
                            .as_ref()
                            .and_then(|scope| scopes.get(scope))
                            .is_some_and(|scope| scope.kind == ScopeKind::File),
                    }
            })
            .collect::<Vec<_>>();
        let [module] = found.as_slice() else {
            return None;
        };
        block = Some(scopes.inline_module_body(&module.id)?.id.clone());
    }
    block
}

/// The names of the inline `mod` blocks from the top level of a file down to `block`, `None`
/// for a scope inside a function or another non-module scope.
fn inline_chain(
    block: &ScopeId,
    symbols: &open_kioku_resolution::SymbolIndex,
    scopes: &open_kioku_resolution::ScopeIndex,
) -> Option<Vec<String>> {
    let mut chain = Vec::new();
    let mut current = scopes.get(block)?;
    for _ in 0..=scopes.scopes.len() {
        match current.kind {
            ScopeKind::File => {
                chain.reverse();
                return Some(chain);
            }
            ScopeKind::Module => {
                let owner = symbols.get(current.owner_symbol_id.as_ref()?)?;
                chain.push(owner.name.clone());
                current = scopes.get(current.parent_id.as_ref()?)?;
            }
            _ => return None,
        }
    }
    None
}

/// What a path read in the type namespace reaches when it ends at the `mod` items `modules`:
/// the one module of the name, when it has an inline block, through which a path continues
/// (#654). `None` for a `mod name;` and for several modules of the name.
fn inline_module_target(
    modules: &[SymbolId],
    scopes: &open_kioku_resolution::ScopeIndex,
) -> Option<RustPathTarget> {
    let [module] = modules else {
        return None;
    };
    scopes.inline_module_body(module)?;
    Some(RustPathTarget {
        module_file: None,
        item: Some(module.clone()),
        reexport: false,
        configured: None,
        variant: None,
    })
}

/// A `use` path written directly in the inline `mod` block `chain` names, read from the top level
/// of its file instead (#641): `super::m::g` in `mod n` is `self::m::g`, and `self::x` there is
/// `self::n::x`. A path that does not start with `self` or `super` is returned as written, and
/// `None` for one that names a block itself.
fn inline_use_source(source: &str, chain: &[String]) -> Option<String> {
    let mut segments = source.split("::").map(str::trim).peekable();
    let climbs = match segments.peek().copied() {
        Some("self") => 0,
        Some("super") => {
            let mut climbs = 0;
            while segments.peek() == Some(&"super") {
                segments.next();
                climbs += 1;
            }
            climbs
        }
        _ => return Some(source.to_string()),
    };
    if climbs == 0 {
        segments.next();
    }
    let rest = segments.collect::<Vec<_>>();
    if rest.is_empty() {
        return None;
    }
    let prefix = match chain.len().checked_sub(climbs) {
        Some(kept) => std::iter::once("self")
            .chain(chain[..kept].iter().map(String::as_str))
            .collect::<Vec<_>>(),
        None => vec!["super"; climbs - chain.len()],
    };
    Some(
        prefix
            .into_iter()
            .chain(rest)
            .collect::<Vec<_>>()
            .join("::"),
    )
}

/// What the resolver reads off `target`, a name a Rust module brings in with `use`: one item,
/// or the items of each file a module configuration selects. An enum's variant is no item the
/// index records, and is not recorded (#641).
fn reexported_value(target: RustPathTarget) -> Option<RustReexported> {
    if target.variant.is_some() {
        return None;
    }
    match (target.configured, target.item) {
        (Some(configured), _) if !configured.items.is_empty() => {
            Some(RustReexported::Alternatives {
                items: configured.items,
                files: RustModuleFiles {
                    files: configured.files,
                    unread: configured.unread,
                },
            })
        }
        (None, Some(item)) => Some(RustReexported::Item(item)),
        _ => None,
    }
}

/// The qualified name the resolver spells for a Rust path from its crate's module directory
/// and the modules on it (`src::facade::f` for `crate::facade::f` in a crate at `src/`), as it
/// does for a module the tree does not declare as a file.
fn resolver_qualified_name(path: &RustUsePath) -> String {
    path.tree
        .module_dir
        .split('/')
        .filter(|segment| !segment.is_empty())
        .chain(path.segments.iter().map(|segment| module_name(segment)))
        .collect::<Vec<_>>()
        .join("::")
}

/// The enum a Rust path names, and whether it was reached through a `use`.
struct EnumAt {
    item: SymbolId,
    reexport: bool,
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
/// A path whose item is reached by following `use` re-exports, of the importer's own crate or
/// of the crate a crate name names, reaches the file that defines the item, or the module file
/// it names (#476).
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
/// - an item a path reaches only through `use` re-exports, of its own crate or of the crate a
///   crate name names, reaches the file declaring it, with its own strategy (#476);
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
        // A path starting at a module the file declares in scope is of its own crate (#632),
        // and stays unresolved when its first segment may name a crate instead.
        let in_crate = is_rust_in_crate_path(&site.source, root.package_name.as_deref())
            || modules.use_path_start(
                &site.file_id,
                site.scope_id.as_ref(),
                &site.source,
                symbols,
                scopes,
            ) != UsePathStart::Elsewhere;
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
                .rust_path(
                    (&site.file_id, importer),
                    site.scope_id.as_ref(),
                    &site.source,
                    symbols,
                    scopes,
                )
                .and_then(|(path, crate_name)| {
                    // A path through a crate name is written in another crate, where no choice
                    // of that crate's configuration-selected modules is made.
                    let writer = (!crate_name).then_some(*importer);
                    let walk = ReexportWalk::start(crate_name);
                    rust_import_edge(&path, walk, writer, symbols, scopes, modules)
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
    walk: ReexportWalk,
    writer: Option<&Path>,
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
    // The file-level edge is read off the tree as placed; the alternatives a
    // configuration-selected module adds bind the names, not this edge. A writer inside one
    // alternative imports from that alternative alone, and from its own file when that leaves one
    // for every choice on the path (#624).
    let placed = modules.placed_path_target(path, walk, symbols, scopes);
    let target = match modules.configured_path_targets(path, writer, placed, symbols) {
        ConfiguredPath::Alternatives(target) | ConfiguredPath::Proven(target) => target,
    }?;
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

/// `(declaring file without `.rs`, module path)` of each `mod name;` inline blocks declare.
type InlineFileModules = HashSet<(String, Vec<String>)>;

/// The `mod name;` items with no body that inline `mod` blocks declare, as
/// [`RustModuleTree::inline_file_modules`] keeps them, and the first block of each. A block's
/// `mod` item is found as the resolver's scope index finds it, by the scope enclosing it and its
/// range. Left out: an item with a `path` attribute, or inside a block that has one, since the
/// attribute moves the files below it, and an item inside a function body or a block the index
/// cannot name.
fn inline_file_modules(
    files: &HashMap<FileId, &Path>,
    declarations: &[ModuleDeclarationSite],
    scopes: &open_kioku_resolution::ScopeIndex,
) -> (InlineFileModules, HashSet<(String, String)>) {
    let range = |range: &open_kioku_core::SourceRange| {
        (
            range.start_line,
            range.start_column,
            range.end_line,
            range.end_column,
        )
    };
    let blocks = declarations
        .iter()
        .filter(|declaration| declaration.has_body)
        .map(|declaration| {
            (
                (declaration.scope_id.as_ref(), range(&declaration.range)),
                declaration,
            )
        })
        .collect::<HashMap<_, _>>();
    let mut modules = HashSet::new();
    let mut tops = HashSet::new();
    for declaration in declarations
        .iter()
        .filter(|declaration| !declaration.has_body && !declaration.has_path_attribute)
    {
        let Some(scope_id) = declaration.scope_id.as_ref() else {
            continue;
        };
        if !is_inside_inline_module(scope_id, scopes) {
            continue;
        }
        let Some(declaring) = files
            .get(&declaration.file_id)
            .and_then(|path| rust_file_stem(path))
        else {
            continue;
        };
        let mut module = vec![module_name(&declaration.name).to_string()];
        let mut current = scopes.get(scope_id);
        let mut complete = false;
        for _ in 0..=scopes.scopes.len() {
            let Some(scope) = current else {
                break;
            };
            match scope.kind {
                ScopeKind::File => {
                    complete = true;
                    break;
                }
                ScopeKind::Module => {
                    let Some(block) = blocks.get(&(scope.parent_id.as_ref(), range(&scope.range)))
                    else {
                        break;
                    };
                    if block.has_path_attribute {
                        break;
                    }
                    module.push(module_name(&block.name).to_string());
                }
                _ => break,
            }
            current = scope
                .parent_id
                .as_ref()
                .and_then(|parent| scopes.get(parent));
        }
        if !complete {
            continue;
        }
        module.reverse();
        tops.insert((declaring.clone(), module[0].clone()));
        modules.insert((declaring, module));
    }
    (modules, tops)
}

/// Whether a Rust `use` written at `scope` brings names into the module of its file: it is
/// written at the file's top level, not in a function, block or inline `mod`.
fn is_module_level(scope: Option<&ScopeId>, scopes: &open_kioku_resolution::ScopeIndex) -> bool {
    scope.is_none_or(|scope| {
        scopes
            .get(scope)
            .is_some_and(|scope| scope.kind == ScopeKind::File)
    })
}

/// Whether a glob `use` brings in `target`, which the path through the module it opens names: an
/// item visible where the glob is, from another crate a `pub` one and from the module's own crate
/// one not private to its module, or the items a module configuration selects, none proven. A
/// module, and a private item, whose visibility the glob's module may or may not have, are not
/// settled. An item is one of the namespace `walk` reads, or a variant of an enum, which is as
/// visible as its enum (#641, #643).
fn glob_brings_in(
    target: &RustPathTarget,
    walk: ReexportWalk,
    symbols: &open_kioku_resolution::SymbolIndex,
    scopes: &open_kioku_resolution::ScopeIndex,
) -> bool {
    match &target.item {
        Some(item) => symbols.get(item).is_some_and(|symbol| {
            let visible = if walk.from_other_crate {
                symbol.visibility == Visibility::Public
            } else {
                symbol.visibility != Visibility::Private
            };
            visible && (target.variant.is_some() || walk.admits(symbol, scopes))
        }),
        None => target.module_file.is_none() && target.configured.is_some(),
    }
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
            alias_of: None,
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
            alias_of: None,
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
            path_is_conditional: false,
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
        // declared inside an inline `mod inner` whose own `mod` item the index did not record,
        // and `orphan.rs` by nothing.
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
                path_is_conditional: false,
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

    /// A scope of `file` spanning `lines`.
    fn file_scope(
        id: &str,
        file: &str,
        parent: Option<&str>,
        kind: ScopeKind,
        lines: (u32, u32),
    ) -> Scope {
        Scope {
            id: ScopeId::new(id),
            file_id: FileId::new(format!("file:{file}")),
            parent_id: parent.map(ScopeId::new),
            owner_symbol_id: None,
            kind,
            range: SourceRange {
                start_line: lines.0,
                start_column: 1,
                end_line: lines.1,
                end_column: 1,
            },
        }
    }

    /// The `mod` item `name` in `file`, written at `scope`, with a body or not, spanning `lines`.
    fn mod_item(
        file: &str,
        scope: &str,
        name: &str,
        has_body: bool,
        lines: (u32, u32),
    ) -> ModuleDeclarationSite {
        ModuleDeclarationSite {
            scope_id: Some(ScopeId::new(scope)),
            has_body,
            range: SourceRange {
                start_line: lines.0,
                start_column: 1,
                end_line: lines.1,
                end_column: 1,
            },
            ..mod_decl(file, name)
        }
    }

    /// The module symbol a `mod name` item of `file` declares at `scope`.
    fn module_symbol(file: &str, name: &str, scope: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(format!("symbol:{file}:mod:{name}")),
            kind: SymbolKind::Module,
            scope_id: Some(ScopeId::new(scope)),
            ..rust_symbol(file, name)
        }
    }

    /// Binds the Rust `use` sites and follows them for the file-level `IMPORTS` edge as indexing
    /// does, in one package at the repository root of `edition` (`None` for a manifest the index
    /// could not read) that names the crates `external`.
    fn bind_and_follow_rust_imports(
        files: &[&str],
        declarations: Vec<ModuleDeclarationSite>,
        sites: &[ImportSite],
        (symbols, scopes): (Vec<Symbol>, Vec<Scope>),
        external: &[&str],
        edition: Option<&str>,
    ) -> (ImportRegistry, RustImportEdgeTargets) {
        let files = files.iter().copied().map(source_file).collect::<Vec<_>>();
        let mut registry = ImportRegistry::default();
        for site in sites {
            registry.insert_unresolved_site(site);
        }
        let symbols = open_kioku_resolution::SymbolIndex::build(symbols);
        let mut project = rust_project(&[("", None)]);
        if let Some(edition) = edition {
            project.roots[0].cargo_manifest = Some(CargoManifest {
                package: Some("fx".into()),
                edition: Some(edition.into()),
                ..Default::default()
            });
        }
        let mut scopes = open_kioku_resolution::ScopeIndex::build(scopes);
        scopes.record_module_declarations(&declarations);
        let names = Arc::new(external.iter().map(ToString::to_string).collect());
        scopes.record_rust_external_crates(
            files
                .iter()
                .map(|file| (file.id.clone(), Arc::clone(&names)))
                .collect(),
        );
        let modules =
            RustModuleTree::new(&files, &project, &declarations, &scopes).with_import_sites(sites);
        registry.resolve_rust_imports(&symbols, &scopes, &modules);
        let edges = rust_import_edge_targets(sites, &symbols, &scopes, &modules);
        (registry, edges)
    }

    #[test]
    fn rust_use_paths_starting_at_a_module_in_scope_bind_as_self_paths() {
        // `src/lib.rs` declares `mod sys;` and `mod a;`, and holds `use sys::imp::f;`, a
        // `use sys::imp::g;` in `fn go`'s body, and `fn shadow() { use crate::a as sys;
        // use sys::imp::h; }`; `src/a.rs` declares no `sys` and holds `use sys::imp::f;` (#632).
        let files = ["src/lib.rs", "src/a.rs", "src/sys/mod.rs", "src/sys/imp.rs"];
        let declarations = vec![
            mod_decl("src/lib.rs", "sys"),
            mod_decl("src/lib.rs", "a"),
            mod_decl("src/sys/mod.rs", "imp"),
        ];
        let scopes = vec![
            file_scope("scope:lib", "src/lib.rs", None, ScopeKind::File, (1, 40)),
            file_scope(
                "scope:go",
                "src/lib.rs",
                Some("scope:lib"),
                ScopeKind::Function,
                (5, 9),
            ),
            file_scope(
                "scope:go:body",
                "src/lib.rs",
                Some("scope:go"),
                ScopeKind::Block,
                (5, 9),
            ),
            file_scope(
                "scope:shadow",
                "src/lib.rs",
                Some("scope:lib"),
                ScopeKind::Function,
                (10, 14),
            ),
            file_scope("scope:a", "src/a.rs", None, ScopeKind::File, (1, 10)),
        ];
        let symbols = vec![
            module_symbol("src/lib.rs", "sys", "scope:lib"),
            module_symbol("src/lib.rs", "a", "scope:lib"),
            rust_symbol("src/sys/imp.rs", "f"),
            rust_symbol("src/sys/imp.rs", "g"),
            rust_symbol("src/sys/imp.rs", "h"),
        ];
        let sites = [
            rust_use_site("src/lib.rs", "sys::imp::f", "f", Some("scope:lib")),
            rust_use_site("src/lib.rs", "sys::imp::g", "g", Some("scope:go:body")),
            rust_use_site("src/lib.rs", "crate::a", "sys", Some("scope:shadow")),
            rust_use_site("src/lib.rs", "sys::imp::h", "h", Some("scope:shadow")),
            rust_use_site("src/a.rs", "sys::imp::f", "f", Some("scope:a")),
        ];
        let (registry, edges) = bind_and_follow_rust_imports(
            &files,
            declarations.clone(),
            &sites,
            (symbols.clone(), scopes.clone()),
            &[],
            Some("2021"),
        );
        assert_eq!(
            bound_target(&registry, "src/lib.rs", "f").as_deref(),
            Some("symbol:src/sys/imp.rs:f")
        );
        assert_eq!(
            binding(&registry, "src/lib.rs", "f").rule,
            ImportBindingRule::RustModulePath
        );
        assert_eq!(
            bound_target(&registry, "src/lib.rs", "g").as_deref(),
            Some("symbol:src/sys/imp.rs:g")
        );
        // The function's own import of `sys` shadows the module, and `a.rs` declares none.
        assert_eq!(bound_target(&registry, "src/lib.rs", "h"), None);
        assert_eq!(bound_target(&registry, "src/a.rs", "f"), None);
        assert_eq!(
            edge_target(&edges, "src/lib.rs", "sys::imp::f").as_deref(),
            Some("rust-item-module:file:src/sys/imp.rs")
        );
        assert!(!edges.is_in_crate(&FileId::new("file:src/a.rs"), "sys::imp::f"));

        // A crate the package can name `sys` may start the path too: nothing is bound, and the
        // file-level edge stays unresolved rather than matched against repository paths.
        let (registry, edges) = bind_and_follow_rust_imports(
            &files,
            declarations,
            &sites,
            (symbols, scopes),
            &["sys"],
            Some("2021"),
        );
        assert_eq!(bound_target(&registry, "src/lib.rs", "f"), None);
        assert_eq!(binding(&registry, "src/lib.rs", "f").target_file, None);
        let lib = FileId::new("file:src/lib.rs");
        assert!(edges.is_in_crate(&lib, "sys::imp::f"));
        assert_eq!(edge_target(&edges, "src/lib.rs", "sys::imp::f"), None);
    }

    #[test]
    fn rust_use_paths_start_at_the_crate_root_in_the_2015_edition() {
        // `src/lib.rs` declares `mod sys;` and `mod a;`; `src/a.rs` declares a `mod sys;` of its
        // own and holds `use sys::f;`, and `src/lib.rs` holds `use sys::g;` (#632).
        let files = ["src/lib.rs", "src/a.rs", "src/sys.rs", "src/a/sys.rs"];
        let declarations = vec![
            mod_decl("src/lib.rs", "sys"),
            mod_decl("src/lib.rs", "a"),
            mod_decl("src/a.rs", "sys"),
        ];
        let scopes = vec![
            file_scope("scope:lib", "src/lib.rs", None, ScopeKind::File, (1, 10)),
            file_scope("scope:a", "src/a.rs", None, ScopeKind::File, (1, 10)),
        ];
        let symbols = vec![
            module_symbol("src/lib.rs", "sys", "scope:lib"),
            module_symbol("src/lib.rs", "a", "scope:lib"),
            module_symbol("src/a.rs", "sys", "scope:a"),
            rust_symbol("src/sys.rs", "f"),
            rust_symbol("src/sys.rs", "g"),
            rust_symbol("src/a/sys.rs", "f"),
        ];
        let sites = [
            rust_use_site("src/a.rs", "sys::f", "f", Some("scope:a")),
            rust_use_site("src/lib.rs", "sys::g", "g", Some("scope:lib")),
        ];
        let run = |edition: Option<&str>| {
            bind_and_follow_rust_imports(
                &files,
                declarations.clone(),
                &sites,
                (symbols.clone(), scopes.clone()),
                &[],
                edition,
            )
        };
        let edge = |edges: &RustImportEdgeTargets, importer: &str, path: &str| {
            edge_target(edges, importer, path)
        };
        // 2015: from the crate root, wherever the path is written.
        let (registry, edges) = run(Some("2015"));
        assert_eq!(
            bound_target(&registry, "src/a.rs", "f").as_deref(),
            Some("symbol:src/sys.rs:f")
        );
        assert_eq!(
            edge(&edges, "src/a.rs", "sys::f").as_deref(),
            Some("rust-item-module:file:src/sys.rs")
        );
        // 2018 and later: in scope, so `a.rs`'s own `sys`.
        let (registry, edges) = run(Some("2021"));
        assert_eq!(
            bound_target(&registry, "src/a.rs", "f").as_deref(),
            Some("symbol:src/a/sys.rs:f")
        );
        assert_eq!(
            edge(&edges, "src/a.rs", "sys::f").as_deref(),
            Some("rust-item-module:file:src/a/sys.rs")
        );
        // An edition the index cannot read leaves the path below the crate root unread; in the
        // crate root both readings name the same module.
        let (registry, edges) = run(None);
        assert_eq!(bound_target(&registry, "src/a.rs", "f"), None);
        assert_eq!(edge(&edges, "src/a.rs", "sys::f"), None);
        for edition in [Some("2015"), Some("2021"), None] {
            let (registry, _) = run(edition);
            assert_eq!(
                bound_target(&registry, "src/lib.rs", "g").as_deref(),
                Some("symbol:src/sys.rs:g"),
                "{edition:?}"
            );
        }
    }

    #[test]
    fn module_placements_follow_a_mod_item_inside_inline_blocks() {
        // `src/lib.rs` holds `mod a;`, `pub mod b { mod sys; }` and
        // `#[path = "elsewhere"] mod x { mod sys; }`; `src/a.rs` holds `mod c { mod d; }` (#633).
        let files = [
            "src/lib.rs",
            "src/a.rs",
            "src/b/sys.rs",
            "src/a/c/d.rs",
            "src/x/sys.rs",
            "src/elsewhere/sys.rs",
        ]
        .map(source_file);
        let declarations = vec![
            mod_decl("src/lib.rs", "a"),
            mod_item("src/lib.rs", "scope:lib", "b", true, (3, 9)),
            mod_item("src/lib.rs", "scope:lib:b", "sys", false, (4, 4)),
            ModuleDeclarationSite {
                has_path_attribute: true,
                path_attributes: vec!["elsewhere".into()],
                ..mod_item("src/lib.rs", "scope:lib", "x", true, (10, 15))
            },
            mod_item("src/lib.rs", "scope:lib:x", "sys", false, (12, 12)),
            mod_item("src/a.rs", "scope:a", "c", true, (1, 4)),
            mod_item("src/a.rs", "scope:a:c", "d", false, (2, 2)),
        ];
        let mut scopes = open_kioku_resolution::ScopeIndex::build(vec![
            file_scope("scope:lib", "src/lib.rs", None, ScopeKind::File, (1, 40)),
            file_scope(
                "scope:lib:b",
                "src/lib.rs",
                Some("scope:lib"),
                ScopeKind::Module,
                (3, 9),
            ),
            file_scope(
                "scope:lib:x",
                "src/lib.rs",
                Some("scope:lib"),
                ScopeKind::Module,
                (10, 15),
            ),
            file_scope("scope:a", "src/a.rs", None, ScopeKind::File, (1, 10)),
            file_scope(
                "scope:a:c",
                "src/a.rs",
                Some("scope:a"),
                ScopeKind::Module,
                (1, 4),
            ),
        ]);
        scopes.record_module_declarations(&declarations);
        let project = rust_project(&[("", None)]);
        let modules = RustModuleTree::new(&files, &project, &declarations, &scopes);
        let placements = modules.module_placements();
        let module = |file: &str| {
            placements[&FileId::new(format!("file:{file}"))]
                .module
                .clone()
                .map(|module| module.join("::"))
        };
        assert_eq!(module("src/b/sys.rs").as_deref(), Some("b::sys"));
        assert_eq!(module("src/a/c/d.rs").as_deref(), Some("a::c::d"));
        // A `path` on the block moves the module files below it, wherever it mounts them.
        assert_eq!(module("src/x/sys.rs"), None);
        assert_eq!(module("src/elsewhere/sys.rs"), None);
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
            path_is_conditional: false,
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
                unread_mounted_files: 0,
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
                unread_mounted_files: 0,
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
            path_is_conditional: false,
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
        let scanned = modules().with_scanned_files([(
            Path::new("src/main.rs"),
            Some(ScannedModules {
                names: declared,
                ..ScannedModules::default()
            }),
        )]);
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
        let unknown = modules().with_scanned_files([(Path::new("src/main.rs"), None)]);
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
            path_is_conditional: false,
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
            path_is_conditional: false,
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
    fn a_module_whose_file_configuration_selects_is_read_with_every_file() {
        let path_decl =
            |file: &str, name: &str, paths: &[&str], conditional: bool| ModuleDeclarationSite {
                has_path_attribute: true,
                path_attributes: paths.iter().map(|path| path.to_string()).collect(),
                path_is_conditional: conditional,
                ..mod_decl(file, name)
            };
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let project = rust_project(&[("", None)]);
        let files = [
            "src/lib.rs",
            "src/sys/mod.rs",
            "src/sys/imp.rs",
            "src/sys/imp/inner.rs",
            "src/sys/win.rs",
            "src/sys/inner.rs",
            "src/sys/u.rs",
            "src/sys/o.rs",
            "src/sys/x.rs",
            "src/sys/plain.rs",
        ]
        .map(source_file);
        let configured = |imp: Vec<ModuleDeclarationSite>| {
            let mut declarations = vec![
                mod_decl("src/lib.rs", "sys"),
                mod_decl("src/sys/mod.rs", "plain"),
                mod_decl("src/sys/imp.rs", "inner"),
                mod_decl("src/sys/win.rs", "inner"),
            ];
            declarations.extend(imp);
            let mut modules = RustModuleTree::new(&files, &project, &declarations, &scopes)
                .configured_modules()
                .into_iter()
                .collect::<Vec<_>>();
            modules.sort_by(|left, right| left.0.cmp(&right.0));
            modules
                .into_iter()
                .map(|(root, modules)| {
                    let files = modules
                        .files
                        .into_iter()
                        .map(|(module, files)| (module.join("::"), files.files, files.unread))
                        .collect::<Vec<_>>();
                    let choices = modules
                        .choices
                        .into_iter()
                        .map(|module| module.join("::"))
                        .collect::<Vec<_>>();
                    (root, choices, files)
                })
                .collect::<Vec<_>>()
        };
        let strings = |values: &[&str]| values.iter().map(|value| value.to_string()).collect();
        // `imp` is `sys/imp.rs` or `sys/win.rs`, and its `inner` is below whichever it is.
        let windows = vec![(
            "src::lib".to_string(),
            strings(&["sys::imp"]),
            vec![
                (
                    "sys::imp".to_string(),
                    strings(&["src/sys/imp", "src/sys/win"]),
                    false,
                ),
                (
                    "sys::imp::inner".to_string(),
                    strings(&["src/sys/imp/inner", "src/sys/inner"]),
                    false,
                ),
            ],
        )];
        // `#[cfg_attr(windows, path = "win.rs")] mod imp;`
        assert_eq!(
            configured(vec![path_decl("src/sys/mod.rs", "imp", &["win.rs"], true)]),
            windows
        );
        // `#[cfg(not(windows))] mod imp;` and `#[cfg(windows)] #[path = "win.rs"] mod imp;`
        assert_eq!(
            configured(vec![
                mod_decl("src/sys/mod.rs", "imp"),
                path_decl("src/sys/mod.rs", "imp", &["win.rs"], false),
            ]),
            windows
        );
        // `unix` beside `not(unix)`: never the default location.
        assert_eq!(
            configured(vec![path_decl(
                "src/sys/mod.rs",
                "imp",
                &["u.rs", "o.rs"],
                false
            )]),
            vec![(
                "src::lib".to_string(),
                strings(&["sys::imp"]),
                vec![(
                    "sys::imp".to_string(),
                    strings(&["src/sys/o", "src/sys/u"]),
                    false
                )],
            )]
        );
        // A `path` the index cannot read may name another file than the default location.
        assert_eq!(
            configured(vec![path_decl("src/sys/mod.rs", "imp", &[], true)]),
            vec![(
                "src::lib".to_string(),
                strings(&["sys::imp"]),
                vec![
                    ("sys::imp".to_string(), strings(&["src/sys/imp"]), true),
                    (
                        "sys::imp::inner".to_string(),
                        strings(&["src/sys/imp/inner"]),
                        false
                    ),
                ],
            )]
        );
        // One file, however the module is spelled, chooses nothing: `all()` read as always, a
        // `#[cfg]` on a single item, and two items at the default location.
        for one_file in [
            vec![path_decl("src/sys/mod.rs", "imp", &["x.rs"], false)],
            vec![mod_decl("src/sys/mod.rs", "imp")],
            vec![
                mod_decl("src/sys/mod.rs", "imp"),
                mod_decl("src/sys/mod.rs", "imp"),
            ],
        ] {
            assert_eq!(configured(one_file.clone()), Vec::new(), "{one_file:?}");
        }
    }

    #[test]
    fn a_module_whose_only_path_is_a_cfg_attr_is_also_at_its_default_location() {
        // `sys/mod.rs` declares `#[cfg_attr(windows, path = "../common.rs")] mod imp;`: the
        // module is `common.rs` on Windows and `sys/imp.rs` everywhere else (#608).
        let path_decl =
            |file: &str, name: &str, paths: &[&str], conditional: bool| ModuleDeclarationSite {
                has_path_attribute: true,
                path_attributes: paths.iter().map(|path| path.to_string()).collect(),
                path_is_conditional: conditional,
                ..mod_decl(file, name)
            };
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let project = rust_project(&[("", None)]);
        let files = [
            "src/lib.rs",
            "src/sys/mod.rs",
            "src/sys/imp.rs",
            "src/common.rs",
            "src/other.rs",
            "tests/it.rs",
        ]
        .map(source_file);
        let own = |conditional: bool| {
            vec![
                mod_decl("src/lib.rs", "sys"),
                mod_decl("src/lib.rs", "common"),
                mod_decl("src/lib.rs", "other"),
                path_decl("src/sys/mod.rs", "imp", &["../common.rs"], conditional),
            ]
        };
        let mounted = |conditional: bool| {
            let mut declarations = own(conditional);
            declarations.push(path_decl(
                "tests/it.rs",
                "sys",
                &["../src/sys/mod.rs"],
                false,
            ));
            declarations
        };
        let placed = |declarations: &[ModuleDeclarationSite]| {
            RustModuleTree::new(&files, &project, declarations, &scopes).module_placements()
                [&FileId::new("file:src/sys/imp.rs")]
                .module
                .clone()
        };
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

        // The library places `sys/imp.rs` at `crate::sys::imp`; a `path` that always applies
        // leaves it a stale file no module path names.
        assert_eq!(
            placed(&own(true)),
            Some(vec!["sys".to_string(), "imp".to_string()])
        );
        assert_eq!(placed(&own(false)), None);

        // `tests/it.rs` mounts `sys/mod.rs`, so the test crate compiles `sys/imp.rs` as well as
        // `common.rs`, and `crate::helper` in either is no one crate's item.
        let modules = RustModuleTree::new(&files, &project, &mounted(true), &scopes);
        assert_eq!(
            marked(&modules),
            vec![
                ("file:src/common.rs".to_string(), false),
                ("file:src/sys/imp.rs".to_string(), true),
                ("file:src/sys/mod.rs".to_string(), true),
            ]
        );
        let registry = bind_crate_paths(
            &modules,
            &["src/sys/imp.rs", "src/common.rs", "src/other.rs"],
            &["src/lib.rs"],
        );
        assert_eq!(bound_target(&registry, "src/sys/imp.rs", "helper"), None);
        assert_eq!(bound_target(&registry, "src/common.rs", "helper"), None);
        assert_eq!(
            bound_target(&registry, "src/other.rs", "helper").as_deref(),
            Some("symbol:src/lib.rs:helper")
        );

        // Control: under an unconditional `#[path]` the default file is not compiled into the
        // test crate, and keeps its binding.
        let modules = RustModuleTree::new(&files, &project, &mounted(false), &scopes);
        assert_eq!(
            marked(&modules),
            vec![
                ("file:src/common.rs".to_string(), false),
                ("file:src/sys/mod.rs".to_string(), true),
            ]
        );
        let registry = bind_crate_paths(&modules, &["src/sys/imp.rs"], &["src/lib.rs"]);
        assert_eq!(
            bound_target(&registry, "src/sys/imp.rs", "helper").as_deref(),
            Some("symbol:src/lib.rs:helper")
        );
    }

    #[test]
    fn a_mounted_file_skipped_for_size_follows_its_cfg_attr_paths_and_default_locations() {
        // As above, with `sys/mod.rs` over `max_file_size`: its lines declare `imp` with only a
        // `cfg_attr` path, so the scan names `imp` and that path (#608).
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let project = rust_project(&[("", None)]);
        let files = [
            "src/lib.rs",
            "src/sys/imp.rs",
            "src/common.rs",
            "src/other.rs",
            "tests/it.rs",
        ]
        .map(source_file);
        let declarations = vec![
            mod_decl("src/lib.rs", "sys"),
            mod_decl("src/lib.rs", "common"),
            mod_decl("src/lib.rs", "other"),
            ModuleDeclarationSite {
                has_path_attribute: true,
                path_attributes: vec!["../src/sys/mod.rs".to_string()],
                path_is_conditional: false,
                ..mod_decl("tests/it.rs", "sys")
            },
        ];
        let modules = |scan: Option<ScannedModules>| {
            RustModuleTree::new(&files, &project, &declarations, &scopes)
                .with_unindexed_files([Path::new("src/sys/mod.rs")])
                .with_scanned_files([(Path::new("src/sys/mod.rs"), scan)])
        };
        let marked = |modules: &RustModuleTree<'_>| {
            let mut marked = modules
                .module_placements()
                .into_iter()
                .filter(|(_, placement)| placement.in_other_crates)
                .map(|(id, _)| id.0)
                .collect::<Vec<_>>();
            marked.sort();
            marked
        };

        let scanned = modules(Some(ScannedModules {
            names: HashSet::from(["imp".to_string()]),
            conditional_paths: vec!["../common.rs".to_string()],
        }));
        assert_eq!(
            marked(&scanned),
            vec![
                "file:src/common.rs".to_string(),
                "file:src/sys/imp.rs".to_string(),
            ]
        );
        assert_eq!(scanned.placement_gaps().unread_mounted_files, 0);

        // A scanned path the index cannot follow leaves the file unread, which may mount any
        // file of the package.
        let absolute = modules(Some(ScannedModules {
            names: HashSet::from(["imp".to_string()]),
            conditional_paths: vec!["/abs/common.rs".to_string()],
        }));
        assert_eq!(
            marked(&absolute),
            vec![
                "file:src/common.rs".to_string(),
                "file:src/other.rs".to_string(),
                "file:src/sys/imp.rs".to_string(),
            ]
        );
        assert_eq!(absolute.placement_gaps().unread_mounted_files, 1);
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
                path_is_conditional: false,
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
    fn a_mounted_file_skipped_for_size_shares_the_modules_its_mod_lines_declare() {
        // `tests/it.rs` mounts `src/m.rs`, which was over `max_file_size`, with `#[path]`; read
        // from `src/`, as a mounted file's items are, its `mod b;` is the library's `src/b.rs`,
        // so the test crate compiles that file too (#610).
        let files = ["src/lib.rs", "src/b.rs", "src/c.rs", "tests/it.rs"].map(source_file);
        let project = rust_project(&[("", None)]);
        let declarations = vec![
            mod_decl("src/lib.rs", "b"),
            mod_decl("src/lib.rs", "c"),
            ModuleDeclarationSite {
                has_path_attribute: true,
                path_attributes: vec!["../src/m.rs".to_string()],
                path_is_conditional: false,
                ..mod_decl("tests/it.rs", "m")
            },
        ];
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = || {
            RustModuleTree::new(&files, &project, &declarations, &scopes)
                .with_unindexed_files([Path::new("src/m.rs")])
        };
        let marked = |modules: &RustModuleTree<'_>| {
            let mut marked = modules
                .module_placements()
                .into_iter()
                .filter(|(_, placement)| placement.in_other_crates)
                .map(|(id, _)| id.0)
                .collect::<Vec<_>>();
            marked.sort();
            marked
        };
        // Its lines are asked for before anything is marked from them.
        assert_eq!(
            modules().unscanned_module_files(),
            HashSet::from([PathBuf::from("src/m.rs")])
        );

        let declared = HashSet::from(["b".to_string()]);
        let scanned = modules().with_scanned_files([(
            Path::new("src/m.rs"),
            Some(ScannedModules {
                names: declared,
                ..ScannedModules::default()
            }),
        )]);
        assert!(scanned.unscanned_module_files().is_empty());
        assert_eq!(marked(&scanned), vec!["file:src/b.rs".to_string()]);
        let gaps = scanned.placement_gaps();
        assert_eq!((gaps.shared_files, gaps.unread_mounted_files), (1, 0));
        let registry = bind_crate_paths(&scanned, &["src/b.rs"], &["src/lib.rs"]);
        assert_eq!(bound_target(&registry, "src/b.rs", "helper"), None);

        // Unread (a path policy excluded it) or unreadable (a `path` attribute of its own), it
        // may mount any file of the package, and says so.
        for modules in [
            modules(),
            modules().with_scanned_files([(Path::new("src/m.rs"), None)]),
        ] {
            assert_eq!(
                marked(&modules),
                vec!["file:src/b.rs".to_string(), "file:src/c.rs".to_string()]
            );
            let gaps = modules.placement_gaps();
            assert_eq!((gaps.shared_files, gaps.unread_mounted_files), (2, 1));
        }

        // A mount of a file discovery never saw reaches nothing and reads nothing.
        let absent = RustModuleTree::new(&files, &project, &declarations, &scopes);
        assert!(absent.unscanned_module_files().is_empty());
        assert!(marked(&absent).is_empty());
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
        // `[[bin]] path = "src/.env.rs"` is skipped as secret-like (an environment-file
        // name), and its skip is redacted, so no unindexed path names it; the
        // manifest still does. It declares `cli`.
        let files = ["src/lib.rs", "src/cli.rs"].map(source_file);
        let project = rust_package_with_targets(CargoTargets {
            roots: vec![PathBuf::from("src/.env.rs")],
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
                unread_mounted_files: 0,
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
                    path_is_conditional: false,
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
                    path_is_conditional: false,
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
                    path_is_conditional: false,
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
            RustModuleTree::new(&files, &project, &declarations, &scopes).with_import_sites(&sites);
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
            RustModuleTree::new(&files, &project, &declarations, &scopes).with_import_sites(&sites);
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

    /// The modules of a package at the repository root (`src/lib.rs`), indexed with the `use`
    /// sites of every file, and with `macros` the files whose top level invokes a macro.
    fn module_tree_with_uses<'a>(
        files: &'a [File],
        project: &'a ProjectModel,
        declarations: &[ModuleDeclarationSite],
        sites: &[ImportSite],
        macros: &[&str],
        scopes: &open_kioku_resolution::ScopeIndex,
    ) -> RustModuleTree<'a> {
        RustModuleTree::new(files, project, declarations, scopes)
            .with_import_sites(sites)
            .with_module_macros(
                macros
                    .iter()
                    .filter(|file| !file.contains(':'))
                    .map(|file| FileId::new(format!("file:{file}")))
                    .collect(),
                // `file:name`: `file`'s `thread_local!` declares `name`.
                macros
                    .iter()
                    .filter_map(|entry| entry.split_once(':'))
                    .map(|(file, name)| {
                        (
                            FileId::new(format!("file:{file}")),
                            HashSet::from([name.to_string()]),
                        )
                    })
                    .collect(),
            )
    }

    /// `pub use <source>;` in `importer`, binding `local`.
    fn rust_pub_use(importer: &str, source: &str, local: &str) -> ImportSite {
        ImportSite {
            reexported: true,
            ..rust_use_site(importer, source, local, None)
        }
    }

    /// Binds every `use` site of `importer` in the package, as indexing does, and returns what
    /// each `local` name is bound to with the rule that bound it.
    fn bind_through_uses(
        files: &[&str],
        declarations: Vec<ModuleDeclarationSite>,
        sites: &[ImportSite],
        symbols: Vec<Symbol>,
        macros: &[&str],
    ) -> ImportRegistry {
        let files = files.iter().copied().map(source_file).collect::<Vec<_>>();
        let project = rust_project(&[("", None)]);
        let symbols = open_kioku_resolution::SymbolIndex::build(symbols);
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules =
            module_tree_with_uses(&files, &project, &declarations, sites, macros, &scopes);
        let mut registry = ImportRegistry::default();
        for site in sites {
            registry.insert_unresolved_site(site);
        }
        registry.resolve_rust_imports(&symbols, &scopes, &modules);
        registry
    }

    /// `(target, rule)` of the binding of `local` in `importer`, `None` where it is unbound.
    fn bound_with_rule(
        registry: &ImportRegistry,
        importer: &str,
        local: &str,
    ) -> Option<(String, ImportBindingRule)> {
        let binding = binding(registry, importer, local);
        binding
            .target_symbol
            .as_ref()
            .map(|target| (target.0.clone(), binding.rule))
    }

    const REEXPORT_FILES: [&str; 10] = [
        "src/lib.rs",
        "src/api.rs",
        "src/auth.rs",
        "src/facade.rs",
        "src/prelude.rs",
        "src/store.rs",
        "src/ledger.rs",
        "src/ring.rs",
        "src/loop_back.rs",
        "src/vault.rs",
    ];

    fn reexport_declarations() -> Vec<ModuleDeclarationSite> {
        [
            "api",
            "auth",
            "facade",
            "prelude",
            "store",
            "ledger",
            "ring",
            "loop_back",
            "vault",
        ]
        .into_iter()
        .map(|module| mod_decl("src/lib.rs", module))
        .collect()
    }

    fn reexport_symbols() -> Vec<Symbol> {
        vec![
            rust_symbol("src/auth.rs", "issue_token"),
            rust_symbol("src/auth.rs", "Token"),
            rust_symbol("src/auth.rs", "revoke"),
            rust_symbol("src/store.rs", "open"),
            rust_symbol("src/store.rs", "close"),
            rust_symbol("src/ledger.rs", "open"),
            rust_symbol("src/ledger.rs", "post"),
        ]
    }

    #[test]
    fn rust_item_imports_follow_use_reexports_of_the_module_they_name() {
        // `lib.rs`: `pub use auth::issue_token;`, `pub use auth::{Token, revoke as cancel};`,
        // and a private `use crate::store::close;`. `facade.rs` re-exports the root's
        // re-export again, so `crate::facade::mint` takes two steps.
        let sites = vec![
            rust_pub_use("src/lib.rs", "auth::issue_token", "issue_token"),
            rust_pub_use("src/lib.rs", "auth::Token", "Token"),
            rust_pub_use("src/lib.rs", "auth::revoke", "cancel"),
            rust_use_site("src/lib.rs", "crate::store::close", "close", None),
            ImportSite {
                reexported: true,
                ..rust_use_site("src/facade.rs", "crate::issue_token", "mint", None)
            },
            rust_use_site("src/api.rs", "crate::issue_token", "issue_token", None),
            rust_use_site("src/api.rs", "crate::Token", "Token", None),
            rust_use_site("src/api.rs", "crate::cancel", "cancel", None),
            rust_use_site("src/api.rs", "crate::close", "close", None),
            rust_use_site("src/api.rs", "crate::facade::mint", "mint", None),
            rust_use_site("src/api.rs", "crate::revoke", "revoke", None),
        ];
        let registry = bind_through_uses(
            &REEXPORT_FILES,
            reexport_declarations(),
            &sites,
            reexport_symbols(),
            &[],
        );
        let reexported = |target: &str| {
            Some((
                format!("symbol:src/auth.rs:{target}"),
                ImportBindingRule::RustReexport,
            ))
        };
        let api = "src/api.rs";
        assert_eq!(
            bound_with_rule(&registry, api, "issue_token"),
            reexported("issue_token"),
            "a single `pub use` in the crate root"
        );
        assert_eq!(
            bound_with_rule(&registry, api, "Token"),
            reexported("Token"),
            "one path of a grouped `pub use`"
        );
        assert_eq!(
            bound_with_rule(&registry, api, "cancel"),
            reexported("revoke"),
            "an aliased one, by its alias"
        );
        assert_eq!(
            bound_with_rule(&registry, api, "revoke"),
            None,
            "and not by the name it renames"
        );
        assert_eq!(
            bound_with_rule(&registry, api, "close"),
            Some((
                "symbol:src/store.rs:close".into(),
                ImportBindingRule::RustReexport
            )),
            "a private `use` of the crate root, reached from a module below it"
        );
        assert_eq!(
            bound_with_rule(&registry, api, "mint"),
            reexported("issue_token"),
            "a chain of two re-exports"
        );
    }

    #[test]
    fn rust_glob_reexports_bind_a_name_only_one_item_supplies() {
        // `prelude.rs`: `pub use crate::store::*;` and `pub use crate::auth::*;`, and a named
        // `pub use crate::ledger::open;` that shadows the globs' `open`. `facade.rs`:
        // `pub use crate::store::*;` and `pub use crate::ledger::*;`, which both bring in
        // `open`.
        let glob = |importer: &str, source: &str| ImportSite {
            reexported: true,
            ..rust_glob_site(importer, source, None)
        };
        let mut sites = vec![
            glob("src/prelude.rs", "crate::store::*"),
            glob("src/prelude.rs", "crate::auth::*"),
            rust_pub_use("src/prelude.rs", "crate::ledger::open", "open"),
            glob("src/facade.rs", "crate::store::*"),
            glob("src/facade.rs", "crate::ledger::*"),
        ];
        for (source, local) in [
            ("crate::prelude::close", "close"),
            ("crate::prelude::issue_token", "issue_token"),
            ("crate::prelude::open", "open"),
            ("crate::facade::open", "open_either"),
            ("crate::facade::post", "post"),
        ] {
            sites.push(rust_use_site("src/api.rs", source, local, None));
        }
        let registry = bind_through_uses(
            &REEXPORT_FILES,
            reexport_declarations(),
            &sites,
            reexport_symbols(),
            &[],
        );
        let api = "src/api.rs";
        let reexport = |target: &str| Some((target.to_string(), ImportBindingRule::RustReexport));
        assert_eq!(
            bound_with_rule(&registry, api, "close"),
            reexport("symbol:src/store.rs:close"),
            "the one glob that brings the name in"
        );
        assert_eq!(
            bound_with_rule(&registry, api, "issue_token"),
            reexport("symbol:src/auth.rs:issue_token")
        );
        assert_eq!(
            bound_with_rule(&registry, api, "open"),
            reexport("symbol:src/ledger.rs:open"),
            "a named re-export shadows the globs"
        );
        assert_eq!(
            bound_with_rule(&registry, api, "open_either"),
            None,
            "two globs bring in two items of the name"
        );
        assert_eq!(
            bound_with_rule(&registry, api, "post"),
            reexport("symbol:src/ledger.rs:post")
        );
    }

    #[test]
    fn rust_reexports_that_cannot_be_settled_bind_nothing() {
        let glob = |importer: &str, source: &str| ImportSite {
            reexported: true,
            ..rust_glob_site(importer, source, None)
        };
        let sites = vec![
            // A cycle: `ring.rs` and `loop_back.rs` re-export `spin` from each other.
            rust_pub_use("src/ring.rs", "crate::loop_back::spin", "spin"),
            rust_pub_use("src/loop_back.rs", "crate::ring::spin", "spin"),
            // Two named re-exports of one name, as two `cfg`s may write them.
            rust_pub_use("src/facade.rs", "crate::store::open", "open"),
            rust_pub_use("src/facade.rs", "crate::ledger::open", "open"),
            // A glob bringing in `post`, beside a `post` the module defines, which shadows it.
            glob("src/store.rs", "crate::ledger::*"),
            // A `thread_local!` of `ring.rs` declares a `revoke` its glob also brings in.
            glob("src/ring.rs", "crate::auth::*"),
            // A glob of a crate the index does not hold may bring the name in too.
            glob("src/prelude.rs", "crate::auth::*"),
            glob("src/prelude.rs", "outside::*"),
            // A `use` inside a function brings nothing into the module.
            rust_use_site(
                "src/auth.rs",
                "crate::store::close",
                "close",
                Some("fn-scope"),
            ),
            // A macro at the top of `vault.rs` may define the name the glob brings in.
            glob("src/vault.rs", "crate::store::*"),
            rust_use_site("src/api.rs", "crate::ring::spin", "spin", None),
            rust_use_site("src/api.rs", "crate::facade::open", "open", None),
            rust_use_site("src/api.rs", "crate::store::post", "post", None),
            rust_use_site("src/api.rs", "crate::store::close", "close_store", None),
            rust_use_site("src/api.rs", "crate::prelude::revoke", "revoke", None),
            rust_use_site("src/api.rs", "crate::auth::close", "close", None),
            rust_use_site("src/api.rs", "crate::vault::close", "close_vault", None),
            rust_use_site("src/api.rs", "crate::ring::revoke", "revoke_ring", None),
            rust_use_site("src/api.rs", "crate::ring::Token", "Token", None),
        ];
        let mut symbols = reexport_symbols();
        symbols.push(rust_symbol("src/store.rs", "post"));
        let registry = bind_through_uses(
            &REEXPORT_FILES,
            reexport_declarations(),
            &sites,
            symbols,
            &["src/vault.rs", "src/ring.rs:revoke"],
        );
        let api = "src/api.rs";
        for (local, why) in [
            ("spin", "a cycle"),
            ("open", "two re-exports of one name"),
            ("revoke", "a glob the index cannot follow"),
            ("close", "a `use` inside a function"),
            ("close_vault", "a module whose top level invokes a macro"),
            ("revoke_ring", "a name a `thread_local!` declares"),
        ] {
            assert_eq!(bound_with_rule(&registry, api, local), None, "{why}");
        }
        assert_eq!(
            bound_with_rule(&registry, api, "close_store"),
            Some((
                "symbol:src/store.rs:close".into(),
                ImportBindingRule::RustModulePath
            )),
            "an item the module defines, which no `use` beside it may stand for"
        );
        assert_eq!(
            bound_with_rule(&registry, api, "post"),
            Some((
                "symbol:src/store.rs:post".into(),
                ImportBindingRule::RustModulePath
            )),
            "an item the module defines shadows what a glob beside it brings in"
        );
        assert_eq!(
            bound_with_rule(&registry, api, "Token"),
            Some((
                "symbol:src/auth.rs:Token".into(),
                ImportBindingRule::RustReexport
            )),
            "a `thread_local!` settles the glob's other names"
        );
    }

    #[test]
    fn rust_paths_from_another_crate_follow_only_pub_use_and_pub_items() {
        // `engine`'s root: `pub use plan::PlanEngine;` (Rust 2018 path), a private
        // `use crate::plan::draft;`, and `pub use crate::util::*;` where `util` defines a
        // private `helper` beside the `pub` `shared`.
        let reexport = |source: &str| {
            rust_pub_use(
                "crates/engine/src/lib.rs",
                source,
                source.rsplit("::").next().unwrap(),
            )
        };
        let mut sites = vec![
            reexport("plan::PlanEngine"),
            rust_use_site(
                "crates/engine/src/lib.rs",
                "crate::plan::draft",
                "draft",
                None,
            ),
            ImportSite {
                reexported: true,
                ..rust_glob_site("crates/engine/src/lib.rs", "crate::util::*", None)
            },
        ];
        let main = "crates/app/src/main.rs";
        for source in [
            "engine::PlanEngine",
            "engine::draft",
            "engine::shared",
            "engine::helper",
        ] {
            sites.push(rust_use_site(
                main,
                source,
                source.rsplit("::").next().unwrap(),
                None,
            ));
        }
        let files = CROSS_CRATE_FILES.map(source_file);
        let project = cross_crate_project();
        let declarations = vec![
            mod_decl("crates/engine/src/lib.rs", "plan"),
            mod_decl("crates/engine/src/lib.rs", "util"),
        ];
        let mut helper = rust_symbol("crates/engine/src/util.rs", "helper");
        helper.visibility = Visibility::Private;
        let symbols = open_kioku_resolution::SymbolIndex::build(vec![
            rust_symbol("crates/engine/src/plan.rs", "PlanEngine"),
            rust_symbol("crates/engine/src/plan.rs", "draft"),
            rust_symbol("crates/engine/src/util.rs", "shared"),
            helper,
        ]);
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = module_tree_with_uses(&files, &project, &declarations, &sites, &[], &scopes);
        let mut registry = ImportRegistry::default();
        for site in &sites {
            registry.insert_unresolved_site(site);
        }
        registry.resolve_rust_imports(&symbols, &scopes, &modules);
        let bound = |local: &str| bound_target(&registry, main, local);
        assert_eq!(
            bound("PlanEngine").as_deref(),
            Some("symbol:crates/engine/src/plan.rs:PlanEngine")
        );
        assert_eq!(
            bound("draft"),
            None,
            "a private `use` is not the other crate's"
        );
        assert_eq!(
            bound("shared").as_deref(),
            Some("symbol:crates/engine/src/util.rs:shared")
        );
        assert_eq!(bound("helper"), None, "a glob brings in no private item");
    }

    #[test]
    fn rust_reexport_table_records_what_a_path_through_a_module_reaches() {
        let glob = |importer: &str, source: &str| ImportSite {
            reexported: true,
            ..rust_glob_site(importer, source, None)
        };
        let sites = vec![
            rust_pub_use("src/lib.rs", "auth::issue_token", "issue_token"),
            rust_use_site("src/lib.rs", "crate::store::close", "close", None),
            glob("src/prelude.rs", "crate::ledger::*"),
            glob("src/store.rs", "crate::ledger::*"),
            // `facade` defines `post` and names `ledger`'s in a `use` too, as a `cfg` may.
            rust_pub_use("src/facade.rs", "crate::ledger::post", "post"),
        ];
        let files = REEXPORT_FILES.map(source_file);
        let project = rust_project(&[("", None)]);
        let mut symbols = reexport_symbols();
        symbols.push(rust_symbol("src/store.rs", "post"));
        symbols.push(rust_symbol("src/facade.rs", "post"));
        let symbols = open_kioku_resolution::SymbolIndex::build(symbols);
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let modules = module_tree_with_uses(
            &files,
            &project,
            &reexport_declarations(),
            &sites,
            &[],
            &scopes,
        );
        let table = modules.reexports(&symbols, &scopes, |name, _| name != "close");
        let item = |id: &str| Some(RustReexported::Item(SymbolId::new(id)));
        let root = &table["src::lib::issue_token"];
        assert_eq!(root.file, FileId::new("file:src/lib.rs"));
        assert_eq!(root.in_crate.values, item("symbol:src/auth.rs:issue_token"));
        assert_eq!(root.in_crate.types, None, "a function is no type");
        assert_eq!(
            root.from_other_crates.values,
            item("symbol:src/auth.rs:issue_token")
        );
        assert!(
            !table.contains_key("src::lib::close"),
            "a name no call writes is not read"
        );
        let post = &table["src::prelude::post"];
        assert_eq!(post.in_crate.values, item("symbol:src/ledger.rs:post"));
        assert_eq!(
            table["src::facade::post"].in_crate.values,
            Some(RustReexported::Ambiguous(vec![
                SymbolId::new("symbol:src/facade.rs:post"),
                SymbolId::new("symbol:src/ledger.rs:post"),
            ])),
            "a name the module defines and a named `use` beside it brings in"
        );
        assert!(
            !table.contains_key("src::store::post"),
            "an item the module defines shadows a glob beside it"
        );
    }

    /// The reexport table of a crate whose `modules` each define one function `f<n>` and glob
    /// every other with a private `use crate::<other>::*;`, and how many `use` lookups it read
    /// afresh.
    fn glob_clique_reexports(modules: usize) -> (HashMap<String, RustReexport>, usize) {
        let names = (0..modules).map(|at| format!("m{at}")).collect::<Vec<_>>();
        let mut paths = vec!["src/lib.rs".to_string()];
        paths.extend(names.iter().map(|name| format!("src/{name}.rs")));
        let paths = paths.iter().map(String::as_str).collect::<Vec<_>>();
        let files = paths.iter().copied().map(source_file).collect::<Vec<_>>();
        let declarations = names
            .iter()
            .map(|name| mod_decl("src/lib.rs", name))
            .collect::<Vec<_>>();
        let mut sites = Vec::new();
        let mut symbols = Vec::new();
        for (at, name) in names.iter().enumerate() {
            let importer = format!("src/{name}.rs");
            symbols.push(rust_symbol(&importer, &format!("f{at}")));
            for other in names.iter().filter(|other| *other != name) {
                sites.push(rust_glob_site(
                    &importer,
                    &format!("crate::{other}::*"),
                    None,
                ));
            }
        }
        let project = rust_project(&[("", None)]);
        let symbols = open_kioku_resolution::SymbolIndex::build(symbols);
        let scopes = open_kioku_resolution::ScopeIndex::build(Vec::new());
        let tree = module_tree_with_uses(&files, &project, &declarations, &sites, &[], &scopes);
        let table = tree.reexports(&symbols, &scopes, |_, _| true);
        let reads = tree.used_names.borrow().reads();
        (table, reads)
    }

    #[test]
    fn rust_glob_cycles_are_not_walked_once_per_path() {
        // Two modules globbing each other bring in each other's function.
        let (table, _) = glob_clique_reexports(2);
        let mut keys = table.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        assert_eq!(keys, ["src::m0::f1", "src::m1::f0"]);
        // With three or more, every glob reaches the name through a cycle as well, which cuts
        // the lookup short and leaves the glob unresolved, so nothing is settled.
        for modules in 3..=8 {
            let (table, reads) = glob_clique_reexports(modules);
            assert!(table.is_empty(), "{table:?}");
            // A lookup that saw a cut used not to be kept, and every glob was read past one that
            // left the name unsettled, so each lookup walked every path through the clique to
            // `MAX_REEXPORT_HOPS`: 321 reads for three modules, 41,935 for five (#659).
            assert!(
                reads <= 8 * modules * modules,
                "{modules} modules: {reads} reads"
            );
        }
    }
}
