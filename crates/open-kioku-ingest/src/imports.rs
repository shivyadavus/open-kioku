use open_kioku_core::{FileId, ImportSite};
pub use open_kioku_semantic_model::{
    ExportBinding, ExportIndex, ImportBinding, ImportIndex, ImportOrigin,
};
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct ImportRegistry {
    pub index: ImportIndex,
}

impl ImportRegistry {
    pub fn resolve_site(&mut self, site: &ImportSite, file_map: &HashMap<String, FileId>) {
        let has_relative_prefix = site.source.starts_with("./") || site.source.starts_with("../");
        let resolves_to_known_file = file_map.contains_key(&site.source);
        let origin = if has_relative_prefix || resolves_to_known_file {
            ImportOrigin::Internal
        } else {
            ImportOrigin::Unknown
        };

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
                target_file: file_map.get(&site.source).cloned(),
                target_symbol: None,
                origin,
                is_type_only: site.is_type_only,
                is_glob: site.is_glob,
                evidence: Vec::new(),
            };
            self.index.insert(import_binding);
        }
    }

    pub fn resolve_symbols(
        &mut self,
        symbols: &open_kioku_resolution::SymbolIndex,
        module_to_file: &HashMap<String, FileId>,
    ) {
        for list in self.index.by_file_local_name.values_mut() {
            for binding in list.iter_mut() {
                let target_file_id = binding
                    .target_file
                    .clone()
                    .or_else(|| module_to_file.get(&binding.source_module).cloned())
                    .or_else(|| {
                        if binding.source_module.contains('.') {
                            let (pkg, cls) = binding.source_module.rsplit_once('.')?;
                            if let Some(fid) = module_to_file.get(pkg) {
                                return Some(fid.clone());
                            }
                            module_to_file.get(&format!("{pkg}.{cls}")).cloned()
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

                if binding.target_symbol.is_none() {
                    if let Some(syms) = symbols.by_name.get(&binding.imported_name) {
                        if syms.len() == 1 {
                            binding.target_symbol = Some(syms[0].clone());
                            if let Some(sym) = symbols.get(&syms[0]) {
                                binding.target_file = Some(sym.file_id.clone());
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{FileId, ImportSite, ImportedName, SourceRange};
    use std::collections::HashMap;

    #[test]
    fn resolves_internal_and_external_imports() {
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
        let lookups = registry
            .index
            .lookup(&FileId::new("file:main.ts"), None, "Repo");
        assert_eq!(lookups.len(), 1);
        assert_eq!(lookups[0].local_name, "Repo");
        assert_eq!(lookups[0].origin, ImportOrigin::Internal);
        assert_eq!(lookups[0].target_file, Some(FileId::new("file:repo.ts")));
    }
}
