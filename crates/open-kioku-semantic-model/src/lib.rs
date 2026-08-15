use open_kioku_core::{EvidenceId, FileId, Language, ModuleId, ScopeId, SymbolId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ModuleInfo {
    pub id: ModuleId,
    pub language: Language,
    pub semantic_path: String,
    pub project_root: PathBuf,
    pub source_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProjectRoot {
    pub path: PathBuf,
    pub language: Language,
    pub package_name: Option<String>,
    pub source_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PathAlias {
    pub owner_root: PathBuf,
    pub pattern: String,
    pub targets: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ProjectModel {
    pub roots: Vec<ProjectRoot>,
    pub modules: HashMap<ModuleId, ModuleInfo>,
    pub aliases: Vec<PathAlias>,
    pub dependencies: HashSet<String>,
}

impl ProjectModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects the nearest owning project root for a given file path.
    pub fn nearest_root_for(
        &self,
        file_path: &std::path::Path,
        language: Language,
    ) -> Option<&ProjectRoot> {
        let matching_roots = self.roots.iter().filter(|r| r.language == language);
        let mut best: Option<(&ProjectRoot, usize)> = None;

        for root in matching_roots {
            if file_path.starts_with(&root.path) {
                let depth = root.path.components().count();
                match best {
                    Some((_, best_depth)) if depth > best_depth => {
                        best = Some((root, depth));
                    }
                    None => {
                        best = Some((root, depth));
                    }
                    _ => {}
                }
            }
        }

        best.map(|(root, _)| root)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportOrigin {
    Internal,
    External,
    Builtin,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImportBinding {
    pub file_id: FileId,
    pub scope_id: ScopeId,
    pub local_name: String,
    pub imported_name: String,
    pub source_module: String,
    pub resolved_module: Option<ModuleId>,
    pub target_file: Option<FileId>,
    pub target_symbol: Option<SymbolId>,
    pub origin: ImportOrigin,
    pub is_type_only: bool,
    pub is_glob: bool,
    pub evidence: Vec<EvidenceId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ImportIndex {
    pub by_file_local_name: HashMap<(FileId, String), Vec<ImportBinding>>,
    pub by_scope_local_name: HashMap<(ScopeId, String), Vec<ImportBinding>>,
}

impl ImportIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, binding: ImportBinding) {
        self.by_file_local_name
            .entry((binding.file_id.clone(), binding.local_name.clone()))
            .or_default()
            .push(binding.clone());

        self.by_scope_local_name
            .entry((binding.scope_id.clone(), binding.local_name.clone()))
            .or_default()
            .push(binding);
    }

    pub fn lookup(
        &self,
        file_id: &FileId,
        scope_id: Option<&ScopeId>,
        local_name: &str,
    ) -> Vec<&ImportBinding> {
        if let Some(sid) = scope_id {
            if let Some(bindings) = self
                .by_scope_local_name
                .get(&(sid.clone(), local_name.to_string()))
            {
                if !bindings.is_empty() {
                    return bindings.iter().collect();
                }
            }
        }

        if let Some(bindings) = self
            .by_file_local_name
            .get(&(file_id.clone(), local_name.to_string()))
        {
            return bindings.iter().collect();
        }

        Vec::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExportBinding {
    pub file_id: FileId,
    pub exported_name: String,
    pub origin_symbol: Option<SymbolId>,
    pub source_module: Option<ModuleId>,
    pub is_type_only: bool,
    pub is_glob: bool,
    pub evidence: Vec<EvidenceId>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ExportIndex {
    pub by_module_exported_name: HashMap<(ModuleId, String), Vec<ExportBinding>>,
}

impl ExportIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, module_id: ModuleId, binding: ExportBinding) {
        self.by_module_exported_name
            .entry((module_id, binding.exported_name.clone()))
            .or_default()
            .push(binding);
    }

    pub fn lookup(&self, module_id: &ModuleId, exported_name: &str) -> Vec<&ExportBinding> {
        self.by_module_exported_name
            .get(&(module_id.clone(), exported_name.to_string()))
            .map(|list| list.iter().collect())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SemanticRepository {
    pub project: ProjectModel,
    pub imports: ImportIndex,
    pub exports: ExportIndex,
}

impl SemanticRepository {
    pub fn new() -> Self {
        Self::default()
    }
}
