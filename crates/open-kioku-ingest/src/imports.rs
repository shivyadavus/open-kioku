use crate::rust_use_path::{map_rust_use_path, RustUsePath};
use open_kioku_core::{
    File, FileId, ImportSite, Language, ModuleDeclarationSite, ScopeId, ScopeKind, SymbolId,
    SymbolKind,
};
use open_kioku_semantic_model::ProjectModel;
pub use open_kioku_semantic_model::{
    ExportBinding, ExportIndex, ImportBinding, ImportBindingRule, ImportIndex, ImportOrigin,
    GLOB_IMPORT_LOCAL_NAME,
};
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
    project: &'a ProjectModel,
    /// `(declaring file without `.rs`, module name)` for each file-backed declaration: `mod name;`
    /// with no body and no `path` attribute, outside any inline module.
    file_modules: HashSet<(String, String)>,
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
                Some((rust_file_stem(path)?, declaration.name.clone()))
            })
            .collect();
        Self {
            files,
            files_by_stem,
            project,
            file_modules,
        }
    }

    /// Whether every module on `module`, from the crate root down, is declared as a file by the
    /// module above it. A stale `auth.rs` beside `#[path = "auth_v2.rs"] mod auth;` or an inline
    /// `mod auth { ... }` is not the module `crate::auth` names.
    fn declares_file_modules(&self, path: &RustUsePath, module: &[String]) -> bool {
        (0..module.len()).all(|depth| {
            let declares =
                |stem: String| self.file_modules.contains(&(stem, module[depth].clone()));
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
    /// both roots declare the importer's module, a path must be declared under both.
    fn crate_roots(&self, path: &RustUsePath) -> Vec<String> {
        if let Some(root) = path.importer_root {
            return vec![format!("{}/{root}", path.src_root)];
        }
        let roots = path
            .module_file_stems(&[])
            .into_iter()
            .filter(|stem| self.files_by_stem.contains_key(stem))
            .collect::<Vec<_>>();
        let Some(top) = path.importer_module.first() else {
            return roots;
        };
        let declaring = roots
            .iter()
            .filter(|stem| self.file_modules.contains(&((*stem).clone(), top.clone())))
            .cloned()
            .collect::<Vec<_>>();
        if declaring.is_empty() {
            roots
        } else {
            declaring
        }
    }

    /// Extension-less paths of the files that can hold `module` in the importer's crate.
    fn module_stems(&self, path: &RustUsePath, module: &[String]) -> Vec<String> {
        if module.is_empty() {
            self.crate_roots(path)
        } else {
            path.module_file_stems(module).to_vec()
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
    let crate_dir = &modules
        .project
        .nearest_root_for(importer, Language::Rust)?
        .path;
    let path = map_rust_use_path(crate_dir, importer, &binding.source_module)?;
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

fn is_inside_inline_module(scope_id: &ScopeId, scopes: &open_kioku_resolution::ScopeIndex) -> bool {
    std::iter::successors(scopes.get(scope_id), |scope| {
        scope
            .parent_id
            .as_ref()
            .and_then(|parent| scopes.get(parent))
    })
    .take(scopes.scopes.len())
    .any(|scope| matches!(scope.kind, ScopeKind::Module))
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{
        Confidence, EvidenceSourceType, FileId, ImportSite, ImportedName, Language, RepositoryId,
        Scope, SourceRange, Symbol, SymbolId, SymbolKind,
    };
    use open_kioku_semantic_model::ProjectRoot;
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
}
