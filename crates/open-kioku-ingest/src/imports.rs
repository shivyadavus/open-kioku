use open_kioku_core::{EvidenceId, FileId, ImportedName, ImportSite, SymbolId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOrigin {
    Internal,
    External,
    Builtin,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ResolvedImportBinding {
    pub local_name: String,
    pub origin: ImportOrigin,
    pub target_file: Option<FileId>,
    pub target_symbol: Option<SymbolId>,
    pub evidence: Vec<EvidenceId>,
}

#[derive(Debug, Clone, Default)]
pub struct ImportRegistry {
    pub bindings: Vec<ResolvedImportBinding>,
}

impl ImportRegistry {
    pub fn resolve_site(&mut self, site: &ImportSite, file_map: &std::collections::HashMap<String, FileId>) {
        let origin = if site.source.starts_with('.') || site.source.contains('/') {
            ImportOrigin::Internal
        } else {
            ImportOrigin::External
        };

        for binding in &site.bindings {
            self.bindings.push(ResolvedImportBinding {
                local_name: binding.local.clone(),
                origin: origin.clone(),
                target_file: file_map.get(&site.source).cloned(),
                target_symbol: None,
                evidence: Vec::new(),
            });
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
        assert_eq!(registry.bindings.len(), 1);
        assert_eq!(registry.bindings[0].local_name, "Repo");
        assert_eq!(registry.bindings[0].origin, ImportOrigin::Internal);
        assert_eq!(registry.bindings[0].target_file, Some(FileId::new("file:repo.ts")));
    }
}
