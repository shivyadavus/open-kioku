use open_kioku_core::{
    Binding, FileId, ModuleDeclarationSite, ModuleId, Scope, ScopeId, ScopeKind, SourceRange,
    Symbol, SymbolId,
};
use smallvec::SmallVec;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct SymbolIndex {
    pub by_id: HashMap<SymbolId, Symbol>,
    pub by_name: HashMap<String, SmallVec<[SymbolId; 4]>>,
    pub by_qualified: HashMap<String, SmallVec<[SymbolId; 2]>>,
    pub by_file: HashMap<FileId, Vec<SymbolId>>,
    pub by_file_name: HashMap<(FileId, String), SmallVec<[SymbolId; 2]>>,
    pub by_file_scope_name: HashMap<(FileId, ScopeId, String), SmallVec<[SymbolId; 2]>>,
    pub by_module: HashMap<ModuleId, Vec<SymbolId>>,
    pub by_parent: HashMap<SymbolId, Vec<SymbolId>>,
}

impl SymbolIndex {
    pub fn build(symbols: Vec<Symbol>) -> Self {
        let mut index = Self::default();
        for sym in symbols {
            index
                .by_name
                .entry(sym.name.clone())
                .or_default()
                .push(sym.id.clone());
            index
                .by_qualified
                .entry(sym.qualified_name.clone())
                .or_default()
                .push(sym.id.clone());
            index
                .by_file
                .entry(sym.file_id.clone())
                .or_default()
                .push(sym.id.clone());
            index
                .by_file_name
                .entry((sym.file_id.clone(), sym.name.clone()))
                .or_default()
                .push(sym.id.clone());
            if let Some(scope_id) = &sym.scope_id {
                index
                    .by_file_scope_name
                    .entry((sym.file_id.clone(), scope_id.clone(), sym.name.clone()))
                    .or_default()
                    .push(sym.id.clone());
            }
            if let Some(mod_id) = &sym.module_id {
                index
                    .by_module
                    .entry(mod_id.clone())
                    .or_default()
                    .push(sym.id.clone());
            }
            if let Some(parent_id) = &sym.parent_symbol_id {
                index
                    .by_parent
                    .entry(parent_id.clone())
                    .or_default()
                    .push(sym.id.clone());
            }
            index.by_id.insert(sym.id.clone(), sym);
        }
        for values in index.by_name.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_qualified.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_file.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_file_name.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_file_scope_name.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_module.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        for values in index.by_parent.values_mut() {
            values.sort_by(|left, right| left.0.cmp(&right.0));
            values.dedup();
        }
        index
    }

    pub fn get(&self, id: &SymbolId) -> Option<&Symbol> {
        self.by_id.get(id)
    }

    pub fn lookup_name(&self, name: &str) -> &[SymbolId] {
        self.by_name.get(name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn lookup_file_scope_name(
        &self,
        file_id: &FileId,
        scope_id: &ScopeId,
        name: &str,
    ) -> &[SymbolId] {
        self.by_file_scope_name
            .get(&(file_id.clone(), scope_id.clone(), name.to_owned()))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn lookup_file_name(&self, file_id: &FileId, name: &str) -> &[SymbolId] {
        self.by_file_name
            .get(&(file_id.clone(), name.to_owned()))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScopeIndex {
    pub scopes: HashMap<ScopeId, Scope>,
    /// The scope of each Rust `mod` item, by the module symbol that owns it. The parser gives a
    /// bodiless `mod name;` a scope too, so only [`ScopeIndex::module_body`] says which is a block.
    modules_by_owner: HashMap<SymbolId, ScopeId>,
    /// Whether each `mod` scope has a body, from the declarations the parser recorded.
    module_has_body: HashMap<ScopeId, bool>,
    /// `mod` scopes that enclose another scope, which only a block can.
    modules_with_children: HashSet<ScopeId>,
    /// Where the declared module tree places each Rust file of a crate's module tree.
    rust_module_placements: HashMap<FileId, RustModulePlacement>,
    /// The library crates each Rust file can name by crate name, by that name.
    rust_crate_names: HashMap<FileId, Arc<RustCrateNames>>,
    /// The crates outside the repository each Rust file can name by crate name.
    rust_external_crates: HashMap<FileId, Arc<BTreeSet<String>>>,
    /// The modules configuration selects a file for, by the qualified-name prefix of the crate
    /// root whose tree holds them.
    rust_configured_modules: HashMap<String, RustConfiguredModules>,
}

/// The library crates code of one crate names by crate name: the dependencies its package's
/// manifest declares on packages of the repository, and its own package's library. Each is placed
/// as its library crate root is, with `module` the crate root's.
pub type RustCrateNames = BTreeMap<String, RustModulePlacement>;

/// Where a Rust file of a package's module tree sits, for paths the resolver spells from file
/// paths as tree-sitter spells qualified names (`crates/app/src/auth.rs` is `crates::app::src::auth`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustModulePlacement {
    /// Qualified-name prefix of the directory holding the crate's module tree: `crates::app::src`.
    pub crate_dir: String,
    /// Qualified-name prefixes of the crate root files whose module tree holds the file, such as
    /// `crates::app::src::lib`. A package with both `lib.rs` and `main.rs` is two crates, and a
    /// module either declares belongs to that one only.
    pub crate_roots: Vec<String>,
    /// The file's module below the crate root (`["auth", "keys"]`, empty for a crate root) when
    /// every module from the root down is declared as a file by the module above it, or by a
    /// `mod name;` inside inline `mod` blocks of the file above it (#633). `None` for a file the
    /// tree does not place there, such as the default location of a `#[path]` module, a file a
    /// `#[path]` mounts, one below an inline `mod` block that has a `path` attribute, or one
    /// declared inside a macro: its `self::` and `super::` paths cannot be read off its path, and
    /// a path must not end in it.
    pub module: Option<Vec<String>>,
    /// The file may also be compiled into a crate other than those of `crate_roots`: a crate
    /// root the index could not read may declare it, or another crate mounts it with `#[path]`.
    /// A `crate::`, `self::` or `super::` path written in it names an item of each crate, so none
    /// is read from it; a path from one of `crate_roots` still ends in it.
    pub in_other_crates: bool,
    /// Every crate that may compile the file mounts it at the place `module` spells, so the
    /// module files below it are the same files in each crate: a `self::` or `super::` path that
    /// ends below the file's own module names the same item wherever the file is compiled, and is
    /// read even when `in_other_crates` is set. Not so for a file another crate mounts with
    /// `#[path]` (other than a `mod.rs`), whose `mod` items that crate reads from its directory.
    pub own_subtree_in_every_crate: bool,
}

/// The modules of one crate whose file configuration selects (#613): a `mod` item whose `path`
/// attributes are `cfg_attr`s beside its default location, or several `#[cfg]`-gated `mod` items
/// of one name, naming more than one file between them. Rustc compiles one of those files on a
/// given build, and the index does not model the build, so a path into such a module, or below
/// it, reaches an item of each file but proves none. Modules are spelled below the crate root, as
/// [`RustModulePlacement::module`] is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RustConfiguredModules {
    /// The modules whose `mod` items choose between files.
    pub choices: BTreeSet<Vec<String>>,
    /// Each module at or below a choice, with the files that may hold it.
    pub files: BTreeMap<Vec<String>, RustModuleFiles>,
    /// The route to each file at or below a choice that the module tree reaches by one route
    /// alone, by its repository-relative path without `.rs` (#624). A file two routes reach, such
    /// as one both alternatives mount, has none.
    pub routes: BTreeMap<String, RustModuleRoute>,
    /// The repository-relative path without `.rs` of each indexed file at or below a choice.
    pub stems: BTreeMap<FileId, String>,
    /// For each module whose files the index may not all know, the files whose `mod` items for
    /// it hold a `path` the index cannot read or name a file it did not index. A choice's own
    /// declaring file is placed, so it is below no choice.
    pub unread_by: BTreeMap<Vec<String>, BTreeSet<String>>,
}

/// Where a file at or below a configuration choice sits: a build compiles it only with the file
/// this route names for each module above it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RustModuleRoute {
    /// The module the file holds, below the crate root.
    pub module: Vec<String>,
    /// The file each module from the outermost choice above the file down to `module` is
    /// compiled from on a build that compiles the file, repository-relative without `.rs`.
    pub files: BTreeMap<Vec<String>, String>,
}

impl RustModuleRoute {
    /// Whether one build may compile the files of both routes: they name the same file for
    /// every module both hold.
    pub fn agrees_with(&self, other: &Self) -> bool {
        self.files.iter().all(|(module, file)| {
            other
                .files
                .get(module)
                .is_none_or(|other_file| other_file == file)
        })
    }
}

/// The files a module at or below a configuration choice may be compiled from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RustModuleFiles {
    /// Repository-relative paths without `.rs`, sorted.
    pub files: Vec<String>,
    /// A `path` attribute of the choice the index cannot read, or a file of it that was not
    /// indexed, may name another file.
    pub unread: bool,
}

impl RustModuleFiles {
    /// One file, which the index read.
    fn is_single(&self) -> bool {
        self.files.len() == 1 && !self.unread
    }
}

/// What a module path at or below a module whose file configuration selects names, read from
/// one file (#613, #624).
#[derive(Debug, Clone)]
pub struct RustConfiguredRead<'s> {
    modules: &'s RustConfiguredModules,
    from: Option<&'s RustModuleRoute>,
    /// The innermost choice the module is at or below.
    pub choice: Vec<String>,
    /// The files that may hold the module on a build that compiles the reading file; `None` for a
    /// module below the choice that no followed file module is, such as a type or an inline `mod`
    /// block.
    pub files: Option<RustModuleFiles>,
    /// The files that may hold the choice on such a build.
    pub choice_files: RustModuleFiles,
    /// The reading file is compiled only with one readable file for every choice on the path, so
    /// the path names that file's module and the path proves what it reaches there.
    pub proven: bool,
}

impl RustConfiguredRead<'_> {
    /// Whether a build that compiles the reading file may compile `file`: it is below no choice,
    /// or its route agrees with the reading file's.
    pub fn may_compile(&self, file: &FileId) -> bool {
        self.modules
            .stems
            .get(file)
            .is_none_or(|stem| self.modules.route_agrees(stem, self.from))
    }
}

impl RustConfiguredModules {
    /// What `module` names, read from a file whose route is `from` (`None` for a file below no
    /// choice, or one another crate compiles): every file of the module that agrees with the
    /// route, so a file inside one alternative reaches that alternative's files alone. `None`
    /// when the module is below no choice.
    pub fn read<'s>(
        &'s self,
        module: &[String],
        from: Option<&'s RustModuleRoute>,
    ) -> Option<RustConfiguredRead<'s>> {
        let choice = (1..=module.len())
            .rev()
            .map(|len| &module[..len])
            .find(|prefix| self.choices.contains(*prefix))?;
        let choice_files = self.files_from(choice, from)?;
        let files = self.files_from(module, from);
        let proven = from.is_some()
            && files.as_ref().unwrap_or(&choice_files).is_single()
            && (1..=module.len())
                .map(|len| &module[..len])
                .filter(|prefix| self.choices.contains(*prefix))
                .all(|prefix| {
                    self.files_from(prefix, from)
                        .is_some_and(|files| files.is_single())
                });
        Some(RustConfiguredRead {
            modules: self,
            from,
            choice: choice.to_vec(),
            files,
            choice_files,
            proven,
        })
    }

    /// The route of the file at `stem`, when the tree reaches it by one route alone and it holds
    /// `placed`, the module the tree places it at, if any: a file also declared as another module
    /// is compiled with no choice made.
    pub fn route_of(&self, stem: &str, placed: Option<&[String]>) -> Option<&RustModuleRoute> {
        self.routes
            .get(stem)
            .filter(|route| placed.is_none_or(|placed| placed == route.module.as_slice()))
    }

    /// The files of `module` a build that compiles a file of route `from` may compile it from.
    fn files_from(
        &self,
        module: &[String],
        from: Option<&RustModuleRoute>,
    ) -> Option<RustModuleFiles> {
        let files = self.files.get(module)?;
        let Some(from) = from else {
            return Some(files.clone());
        };
        // The reading file is compiled only with the file its own route names for the module.
        if let Some(own) = from.files.get(module) {
            return Some(RustModuleFiles {
                files: vec![own.clone()],
                unread: false,
            });
        }
        Some(RustModuleFiles {
            files: files
                .files
                .iter()
                .filter(|stem| self.route_agrees(stem, Some(from)))
                .cloned()
                .collect(),
            unread: files.unread
                && self.unread_by.get(module).is_none_or(|declaring| {
                    declaring
                        .iter()
                        .any(|stem| self.route_agrees(stem, Some(from)))
                }),
        })
    }

    /// Whether the file at `stem` may be compiled with a file of route `from`. A file without a
    /// route of its own may be.
    fn route_agrees(&self, stem: &str, from: Option<&RustModuleRoute>) -> bool {
        match (self.routes.get(stem), from) {
            (Some(route), Some(from)) => route.agrees_with(from),
            _ => true,
        }
    }
}

/// What the `mod` item a module symbol names turned out to be.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ModuleBody<'s> {
    /// An inline `mod name { .. }` block, and its scope.
    Inline(&'s Scope),
    /// `mod name;`, whose items live in another file.
    OutOfLine,
    /// The index cannot tell.
    Unknown,
}

impl ScopeIndex {
    pub fn build(scopes: Vec<Scope>) -> Self {
        let mut index = Self::default();
        for scope in &scopes {
            if scope.kind == ScopeKind::Module {
                if let Some(owner) = &scope.owner_symbol_id {
                    index
                        .modules_by_owner
                        .insert(owner.clone(), scope.id.clone());
                }
            }
        }
        let module_scopes = index.modules_by_owner.values().collect::<HashSet<_>>();
        let modules_with_children = scopes
            .iter()
            .filter_map(|scope| scope.parent_id.as_ref())
            .filter(|parent| module_scopes.contains(parent))
            .cloned()
            .collect();
        index.modules_with_children = modules_with_children;
        for scope in scopes {
            index.scopes.insert(scope.id.clone(), scope);
        }
        index
    }

    /// Records which `mod` scopes have a body. A declaration and the scope of its item share
    /// the enclosing scope and the item's range.
    pub fn record_module_declarations(&mut self, declarations: &[ModuleDeclarationSite]) {
        let by_site = self
            .modules_by_owner
            .values()
            .filter_map(|id| self.scopes.get(id))
            .map(|scope| {
                (
                    (scope.parent_id.as_ref(), range_key(&scope.range)),
                    &scope.id,
                )
            })
            .collect::<HashMap<_, _>>();
        let mut has_body = HashMap::new();
        for declaration in declarations {
            let key = (declaration.scope_id.as_ref(), range_key(&declaration.range));
            if let Some(scope_id) = by_site.get(&key) {
                has_body.insert((*scope_id).clone(), declaration.has_body);
            }
        }
        self.module_has_body.extend(has_body);
    }

    /// Records where the declared module tree places Rust files. A Rust `crate::`, `self::` or
    /// `super::` path whose target the resolver spells from file paths starts only in a recorded
    /// file, is read against that file's own crate, and ends only in a file placed in that crate.
    pub fn record_rust_module_placements(
        &mut self,
        placements: HashMap<FileId, RustModulePlacement>,
    ) {
        self.rust_module_placements.extend(placements);
    }

    /// Where `file` sits in its crate's module tree, when it is recorded.
    pub(crate) fn rust_module_placement(&self, file: &FileId) -> Option<&RustModulePlacement> {
        self.rust_module_placements.get(file)
    }

    /// Records the modules configuration selects a file for, by crate root.
    pub fn record_rust_configured_modules(
        &mut self,
        modules: HashMap<String, RustConfiguredModules>,
    ) {
        self.rust_configured_modules.extend(modules);
    }

    /// Whether `module` of a crate of `placement` is at or below a module whose file
    /// configuration selects, and which files may hold it, read from `caller`, the file the path
    /// is written in (`None` for one in another crate). `None` also when the tree places the
    /// caller below the same choice: that file is compiled only with the file of the choice that
    /// holds it, so a path that stays below the choice names that file's modules alone, as the
    /// tree places them, unless a `mod` item outside the choice also reaches it. A caller the
    /// tree does not place, such as a file a `path` attribute mounts, is read by its route
    /// instead (#624).
    pub(crate) fn rust_configured_module(
        &self,
        placement: &RustModulePlacement,
        caller: Option<&FileId>,
        module: &[String],
    ) -> Option<RustConfiguredRead<'_>> {
        placement.crate_roots.iter().find_map(|root| {
            let configured = self.rust_configured_modules.get(root)?;
            let caller_module = caller.and(placement.module.as_deref());
            let stem = caller.and_then(|caller| configured.stems.get(caller));
            let from = stem.and_then(|stem| configured.route_of(stem, caller_module));
            let read = configured.read(module, from)?;
            // A placed file the walk found no one route to, such as one a `path` attribute
            // outside the choice also mounts, is compiled whichever file the choice takes.
            if caller_module.is_some_and(|caller| caller.starts_with(&read.choice))
                && (stem.is_none() || from.is_some())
            {
                return None;
            }
            Some(read)
        })
    }

    /// The module `file` holds by its route through the modules of `placement`'s crate whose file
    /// configuration selects (#633): for a file a `path` attribute mounts inside one alternative,
    /// which the tree does not place, the module that alternative makes it, so a `self::` or
    /// `super::` path written there is read off that module. `None` for a file with no one route,
    /// such as one both alternatives mount, and when the crates of `placement` disagree.
    pub(crate) fn rust_routed_module(
        &self,
        placement: &RustModulePlacement,
        file: &FileId,
    ) -> Option<&[String]> {
        let mut found = None;
        for root in &placement.crate_roots {
            let configured = self.rust_configured_modules.get(root)?;
            let stem = configured.stems.get(file)?;
            let module = configured.route_of(stem, None)?.module.as_slice();
            if found.is_some_and(|known| known != module) {
                return None;
            }
            found = Some(module);
        }
        found
    }

    /// Whether `name` in `file` names a crate its package can name: a dependency its manifest
    /// declares, inside the repository or not, its own library from another of its crates, or
    /// `std`, `core` or `alloc`. A Rust path whose first segment names such a crate and a module
    /// in scope may start from either (#626, #632).
    pub fn rust_names_crate(&self, file: &FileId, name: &str) -> bool {
        self.rust_named_crate(file, name).is_some() || self.rust_names_external_crate(file, name)
    }

    /// Records which library crates each Rust file names by crate name. A path through one of
    /// those names is read from that crate's root.
    pub fn record_rust_crate_names(&mut self, names: HashMap<FileId, Arc<RustCrateNames>>) {
        self.rust_crate_names.extend(names);
    }

    /// The library crate `file` names `crate_name`, when it names one of the repository.
    pub(crate) fn rust_named_crate(
        &self,
        file: &FileId,
        crate_name: &str,
    ) -> Option<&RustModulePlacement> {
        self.rust_crate_names.get(file)?.get(crate_name)
    }

    /// Records which crates outside the repository each Rust file names by crate name: the
    /// dependencies its package's manifest places outside the repository.
    pub fn record_rust_external_crates(&mut self, names: HashMap<FileId, Arc<BTreeSet<String>>>) {
        self.rust_external_crates.extend(names);
    }

    /// Whether `crate_name` in `file` names a crate outside the repository: the standard library's
    /// `std`, `core` or `alloc`, or a dependency the file's package places outside the repository,
    /// unless a crate of the repository answers to the name.
    pub(crate) fn rust_names_external_crate(&self, file: &FileId, crate_name: &str) -> bool {
        if self.rust_named_crate(file, crate_name).is_some() {
            return false;
        }
        matches!(crate_name, "std" | "core" | "alloc")
            || self
                .rust_external_crates
                .get(file)
                .is_some_and(|crates| crates.contains(crate_name))
    }

    /// Whether the declared module tree places `file` at its path in a crate of `placement`.
    pub(crate) fn is_placed_in_crate_of(
        &self,
        file: &FileId,
        placement: &RustModulePlacement,
    ) -> bool {
        self.rust_module_placement(file).is_some_and(|target| {
            target.module.is_some()
                && target.crate_dir == placement.crate_dir
                && target
                    .crate_roots
                    .iter()
                    .any(|root| placement.crate_roots.contains(root))
        })
    }

    pub fn get(&self, id: &ScopeId) -> Option<&Scope> {
        self.scopes.get(id)
    }

    /// What the `mod` item of `module` is. Without its declaration, a `mod` scope enclosing
    /// another scope is a block, and an empty one is either.
    pub(crate) fn module_body(&self, module: &SymbolId) -> ModuleBody<'_> {
        let Some(scope) = self
            .modules_by_owner
            .get(module)
            .and_then(|id| self.scopes.get(id))
        else {
            return ModuleBody::Unknown;
        };
        match self.module_has_body.get(&scope.id) {
            Some(true) => ModuleBody::Inline(scope),
            Some(false) => ModuleBody::OutOfLine,
            None if self.modules_with_children.contains(&scope.id) => ModuleBody::Inline(scope),
            None => ModuleBody::Unknown,
        }
    }
}

fn range_key(range: &SourceRange) -> (u32, u32, u32, u32) {
    (
        range.start_line,
        range.start_column,
        range.end_line,
        range.end_column,
    )
}

#[derive(Debug, Clone, Default)]
pub struct BindingIndex {
    pub bindings_by_scope_name: HashMap<(ScopeId, String), Vec<Binding>>,
}

impl BindingIndex {
    pub fn build(bindings: Vec<Binding>) -> Self {
        let mut index = Self::default();
        for binding in bindings {
            index
                .bindings_by_scope_name
                .entry((binding.scope_id.clone(), binding.name.clone()))
                .or_default()
                .push(binding);
        }
        for values in index.bindings_by_scope_name.values_mut() {
            values.sort_by(|left, right| {
                (
                    left.range.start_line,
                    left.range.start_column,
                    left.range.end_line,
                    left.range.end_column,
                )
                    .cmp(&(
                        right.range.start_line,
                        right.range.start_column,
                        right.range.end_line,
                        right.range.end_column,
                    ))
                    .then_with(|| left.id.0.cmp(&right.id.0))
            });
        }
        index
    }

    pub fn resolve_before(
        &self,
        scope_id: &ScopeId,
        name: &str,
        call_range: &SourceRange,
        scopes: &ScopeIndex,
    ) -> Option<&Binding> {
        let mut current = Some(scope_id.clone());
        let mut visited = std::collections::HashSet::new();
        let call_pos = (call_range.start_line, call_range.start_column);
        while let Some(sid) = current {
            if !visited.insert(sid.clone()) {
                break;
            }
            if let Some(list) = self
                .bindings_by_scope_name
                .get(&(sid.clone(), name.to_string()))
            {
                if let Some(binding) = list.iter().rev().find(|binding| {
                    (binding.range.start_line, binding.range.start_column) <= call_pos
                }) {
                    return Some(binding);
                }
            }
            current = scopes.get(&sid).and_then(|scope| scope.parent_id.clone());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        BindingId, Confidence, EvidenceSourceType, Language, LineRange, SymbolKind, Visibility,
    };

    fn symbol(id: &str, name: &str, file: &str) -> Symbol {
        Symbol {
            id: SymbolId::new(id),
            name: name.into(),
            qualified_name: format!("pkg::{id}"),
            kind: SymbolKind::Function,
            file_id: FileId::new(file),
            range: Some(LineRange { start: 1, end: 2 }),
            language: Language::Rust,
            confidence: Confidence::Exact,
            provenance: EvidenceSourceType::TreeSitter,
            module_id: None,
            parent_symbol_id: None,
            scope_id: None,
            signature: None,
            visibility: Visibility::Public,
            alias_of: None,
        }
    }

    #[test]
    fn symbol_candidate_indexes_do_not_depend_on_insertion_order() {
        let first = symbol("symbol:a", "run", "file:lib.rs");
        let second = symbol("symbol:b", "run", "file:lib.rs");

        let forward = SymbolIndex::build(vec![first.clone(), second.clone()]);
        let reversed = SymbolIndex::build(vec![second, first]);

        assert_eq!(forward.lookup_name("run"), reversed.lookup_name("run"));
        assert_eq!(
            forward.by_file.get(&FileId::new("file:lib.rs")),
            reversed.by_file.get(&FileId::new("file:lib.rs"))
        );
        assert_eq!(
            forward
                .lookup_name("run")
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["symbol:a", "symbol:b"]
        );
        assert_eq!(
            forward
                .lookup_file_scope_name(
                    &FileId::new("file:lib.rs"),
                    &ScopeId::new("scope:file"),
                    "run"
                )
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            Vec::<&str>::new()
        );
        assert_eq!(
            forward
                .lookup_file_name(&FileId::new("file:lib.rs"), "run")
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["symbol:a", "symbol:b"]
        );
    }

    #[test]
    fn file_scope_name_lookup_is_deterministic_and_exact() {
        let mut first = symbol("symbol:a", "run", "file:lib.rs");
        first.scope_id = Some(ScopeId::new("scope:inner"));
        let mut second = symbol("symbol:b", "run", "file:lib.rs");
        second.scope_id = Some(ScopeId::new("scope:inner"));
        let mut third = symbol("symbol:c", "run", "file:lib.rs");
        third.scope_id = Some(ScopeId::new("scope:outer"));

        let forward = SymbolIndex::build(vec![first.clone(), second.clone(), third.clone()]);
        let reversed = SymbolIndex::build(vec![third, second, first]);
        let file_id = FileId::new("file:lib.rs");
        let inner = ScopeId::new("scope:inner");

        assert_eq!(
            forward.lookup_file_scope_name(&file_id, &inner, "run"),
            reversed.lookup_file_scope_name(&file_id, &inner, "run")
        );
        assert_eq!(
            forward
                .lookup_file_scope_name(&file_id, &inner, "run")
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["symbol:a", "symbol:b"]
        );
        assert!(forward
            .lookup_file_scope_name(&file_id, &inner, "other")
            .is_empty());
        assert_eq!(
            forward
                .lookup_file_name(&file_id, "run")
                .iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["symbol:a", "symbol:b", "symbol:c"]
        );
    }

    #[test]
    fn binding_ties_have_a_deterministic_order() {
        let scope = ScopeId::new("scope:file");
        let range = SourceRange {
            start_line: 4,
            start_column: 2,
            end_line: 4,
            end_column: 8,
        };
        let make = |id: &str| Binding {
            id: BindingId::new(id),
            file_id: FileId::new("file:lib.rs"),
            scope_id: scope.clone(),
            name: "value".into(),
            declared_type: Some("Thing".into()),
            inferred_type: None,
            range: range.clone(),
        };

        let index = BindingIndex::build(vec![make("binding:z"), make("binding:a")]);
        let ids = index
            .bindings_by_scope_name
            .get(&(scope, "value".into()))
            .unwrap()
            .iter()
            .map(|binding| binding.id.0.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["binding:a", "binding:z"]);
    }
}
