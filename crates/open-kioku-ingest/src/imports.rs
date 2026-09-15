use crate::rust_use_path::{map_rust_use_path, RustUsePath};
use open_kioku_core::{
    File, FileId, ImportSite, Language, ModuleDeclarationSite, ScopeId, ScopeKind, SymbolId,
    SymbolKind,
};
use open_kioku_semantic_model::ProjectModel;
pub use open_kioku_semantic_model::{
    ExportBinding, ExportIndex, ImportBinding, ImportIndex, ImportOrigin,
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

/// The Rust module tree as declared, which item-import binding follows instead of trusting file
/// layout alone.
pub(crate) struct RustModuleTree<'a> {
    files: HashMap<FileId, &'a Path>,
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
                let path = files
                    .get(&declaration.file_id)?
                    .to_string_lossy()
                    .replace('\\', "/");
                Some((
                    path.strip_suffix(".rs")?.to_string(),
                    declaration.name.clone(),
                ))
            })
            .collect();
        Self {
            files,
            project,
            file_modules,
        }
    }

    /// Whether every module on `module`, from the crate root down, is declared as a file by the
    /// module above it. A stale `auth.rs` beside `#[path = "auth_v2.rs"] mod auth;` or an inline
    /// `mod auth { ... }` is not the module `crate::auth` names.
    fn declares_file_modules(&self, path: &RustUsePath, module: &[String]) -> bool {
        (0..module.len()).all(|depth| {
            path.module_file_stems(&module[..depth])
                .into_iter()
                .any(|stem| self.file_modules.contains(&(stem, module[depth].clone())))
        })
    }
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

        for binding in &site.bindings {
            let import_binding = ImportBinding {
                file_id: site.file_id.clone(),
                scope_id: site
                    .scope_id
                    .clone()
                    .unwrap_or_else(|| open_kioku_core::ScopeId::new("global")),
                local_name: binding.local.clone(),
                imported_name: binding.imported.clone(),
                source_module: site.source.clone(),
                resolved_module: None,
                target_file: target_file.clone(),
                target_symbol: None,
                origin,
                is_type_only: site.is_type_only,
                is_glob: site.is_glob,
                evidence: Vec::new(),
            };
            self.index.insert(import_binding);
        }
    }

    pub fn resolve_symbols<M: FileLookup>(
        &mut self,
        symbols: &open_kioku_resolution::SymbolIndex,
        module_to_file: &M,
    ) {
        for list in self.index.by_file_local_name.values_mut() {
            for binding in list.iter_mut() {
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

    /// Binds each Rust item import (`use crate::auth::issue_token;`, one path of a grouped
    /// import, or an aliased one) that `resolve_symbols` left unbound to the module-level symbol
    /// its path names, when exactly one symbol does.
    ///
    /// The parent path is the module and the last segment the item. `crate::` starts at the
    /// `src/` of the nearest `Cargo.toml`, every module on the path must be declared as a file by
    /// the module above it, and the target is looked up by the qualified name the parser gives a
    /// module-level item in that module's file. Nothing is matched by bare name, so an
    /// extern-crate path, a method or nested item of the same name, and an item defined in both
    /// `x.rs` and `x/mod.rs` all stay unbound.
    pub(crate) fn resolve_rust_item_imports(
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
            for binding in list
                .iter_mut()
                .filter(|binding| binding.target_symbol.is_none() && !binding.is_glob)
            {
                let Some(importer) = modules.files.get(&binding.file_id) else {
                    continue;
                };
                if let Some(target) =
                    rust_item_import_target(binding, importer, symbols, scopes, modules)
                {
                    binding.target_symbol = Some(target);
                    binding.origin = ImportOrigin::Internal;
                }
            }
        }
    }
}

fn rust_item_import_target(
    binding: &ImportBinding,
    importer: &Path,
    symbols: &open_kioku_resolution::SymbolIndex,
    scopes: &open_kioku_resolution::ScopeIndex,
    modules: &RustModuleTree<'_>,
) -> Option<SymbolId> {
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
    let (item, module) = path.segments.split_last()?;
    if *item != binding.imported_name || !modules.declares_file_modules(&path, module) {
        return None;
    }
    let mut targets = path
        .module_file_stems(module)
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

    /// Runs the registry over `files` in packages rooted at `manifests` (directories holding a
    /// `Cargo.toml`, `""` for the repository root).
    fn bind_rust_imports(
        files: &[&str],
        manifests: &[&str],
        declarations: Vec<ModuleDeclarationSite>,
        sites: &[ImportSite],
        symbols: Vec<Symbol>,
        scopes: Vec<Scope>,
    ) -> ImportRegistry {
        let mut registry = ImportRegistry::default();
        let file_map = FileMap::new();
        for site in sites {
            registry.resolve_site(site, &file_map);
        }
        let symbols = open_kioku_resolution::SymbolIndex::build(symbols);
        registry.resolve_symbols(&symbols, &file_map);
        let files = files.iter().copied().map(source_file).collect::<Vec<_>>();
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
        registry.resolve_rust_item_imports(&symbols, &scopes, &modules);
        registry
    }

    fn bound_target(registry: &ImportRegistry, importer: &str, local: &str) -> Option<String> {
        let lookups = registry
            .index
            .lookup(&FileId::new(format!("file:{importer}")), None, local);
        assert_eq!(lookups.len(), 1, "one `{local}` binding in {importer}");
        lookups[0].target_symbol.as_ref().map(|id| id.0.clone())
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
        let binding =
            registry
                .index
                .lookup(&FileId::new("file:src/session.rs"), None, "issue_token")[0];
        assert_eq!(binding.origin, ImportOrigin::Internal);
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
