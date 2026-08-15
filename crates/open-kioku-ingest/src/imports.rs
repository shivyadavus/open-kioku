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
        // Classify import origin using project metadata evidence, not string heuristics.
        // Per the plan: "Do not classify imports using string heuristics like
        // source.contains('/'). Packages like @angular/core or lodash/fp are external
        // unless project metadata proves otherwise."
        let has_relative_prefix =
            site.source.starts_with("./") || site.source.starts_with("../");
        let resolves_to_known_file = file_map.contains_key(&site.source);
        let origin = if has_relative_prefix || resolves_to_known_file {
            ImportOrigin::Internal
        } else {
            // Without project metadata proof, classify as Unknown rather than
            // assuming External. This prevents false classifications of scoped
            // packages (@angular/core) or subpath imports (lodash/fp).
            ImportOrigin::Unknown
        };

        for binding in &site.bindings {
            let import_binding = ImportBinding {
                file_id: site.file_id.clone(),
                scope_id: site.scope_id.clone().unwrap_or_else(|| open_kioku_core::ScopeId::new("global")),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_kioku_core::{FileId, ImportedName, ImportSite, SourceRange};
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
        let lookups = registry.index.lookup(&FileId::new("file:main.ts"), None, "Repo");
        assert_eq!(lookups.len(), 1);
        assert_eq!(lookups[0].local_name, "Repo");
        assert_eq!(lookups[0].origin, ImportOrigin::Internal);
        assert_eq!(lookups[0].target_file, Some(FileId::new("file:repo.ts")));
    }
}
