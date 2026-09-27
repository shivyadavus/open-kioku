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
    /// Repository-relative root file of a Rust package's library crate when its manifest sets one
    /// with `[lib] path`; `None` means the default `src/lib.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_root: Option<PathBuf>,
    /// The binary, integration test, example and bench crate roots a Rust package's manifest
    /// names, and the target kinds whose auto-discovery it turns off.
    #[serde(default, skip_serializing_if = "CargoTargets::is_empty")]
    pub cargo_targets: CargoTargets,
    /// What a Rust package's manifest declares about the package itself and the packages it
    /// depends on, when the manifest parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_manifest: Option<CargoManifest>,
}

/// A parsed `Cargo.toml`: its package, its workspace, and the dependencies it declares on other
/// packages of the repository.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CargoManifest {
    /// `[package] name`; `None` for a virtual workspace manifest, which declares no package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// `[lib] proc-macro = true`: the library is a procedural-macro crate, whose macros expand
    /// into code of the crates that use them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub proc_macro: bool,
    /// The manifest has a `[workspace]` table.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub workspace: bool,
    /// Repository-relative path prefixes of the workspace's `members`, each glob cut at its first
    /// wildcard; a literal member ends in `/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_members: Vec<String>,
    /// Dependencies on packages whose manifests are in the repository, found through a `path`
    /// (directly, or from `[workspace.dependencies]` for `name.workspace = true`). A registry or
    /// git dependency names no directory here and is left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<CargoDependency>,
    /// Dependencies the manifest places outside the repository: a registry or git dependency, or
    /// a `path` that leaves the repository, whose package the workspace root's `[patch]` or
    /// `[replace]` does not point at a `path` in the repository. No item of their crates is
    /// indexed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_dependencies: Vec<CargoExternalDependency>,
}

/// One dependency of a Rust package on a package outside the repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CargoExternalDependency {
    /// The name the dependent's code writes the dependency's crate under: the dependency table's
    /// key with `-` read as `_`.
    pub crate_name: String,
    pub kind: CargoDependencyKind,
}

impl CargoExternalDependency {
    /// Whether code of the crate rooted at `importer` can name this dependency; see
    /// [`CargoDependency::visible_to`].
    pub fn visible_to(&self, importer: CargoImporter) -> bool {
        match importer {
            CargoImporter::BuildScript => self.kind == CargoDependencyKind::Build,
            CargoImporter::Crate => self.kind != CargoDependencyKind::Build,
        }
    }
}

/// One dependency of a Rust package on another package of the repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CargoDependency {
    /// The name the dependent's code writes the dependency's crate under, with `-` read as `_`:
    /// the key of a renamed dependency (`alias = { package = "real" }`), otherwise the
    /// dependency's library crate name. `None` while the dependency's own manifest has not been
    /// read, and for one whose directory holds no discovered manifest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crate_name: Option<String>,
    /// The key the dependency table writes, which is the package name unless it renames one.
    pub key: String,
    /// The package the dependency names: `package = "..."`, or the key.
    pub package: String,
    /// Repository-relative directory of the dependency's manifest, keyed by path rather than
    /// package name: two workspaces may each have a package of one name.
    pub manifest_dir: PathBuf,
    pub kind: CargoDependencyKind,
    /// Declared under `[target.'cfg(...)'.*dependencies]`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub target_specific: bool,
    /// Declared with `workspace = true` and read from the workspace's `[workspace.dependencies]`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub inherited: bool,
}

impl CargoDependency {
    /// Whether code of the crate rooted at `importer` can name this dependency: a build script
    /// only its build dependencies, every other crate of the package its normal and dev ones
    /// (`#[cfg(test)]` code of the library and binaries included).
    pub fn visible_to(&self, importer: CargoImporter) -> bool {
        match importer {
            CargoImporter::BuildScript => self.kind == CargoDependencyKind::Build,
            CargoImporter::Crate => self.kind != CargoDependencyKind::Build,
        }
    }
}

/// Which dependency table declared a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CargoDependencyKind {
    Normal,
    Dev,
    Build,
}

/// The kind of crate a file compiled into, as far as which dependencies it can name goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CargoImporter {
    BuildScript,
    Crate,
}

/// What a Cargo manifest says about its package's non-library targets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CargoTargets {
    /// Repository-relative crate root files of the `[[bin]]`, `[[test]]`, `[[example]]` and
    /// `[[bench]]` targets whose `path` is set, and of those a target kind with auto-discovery
    /// turned off names without one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<PathBuf>,
    /// Target kinds whose files Cargo does not discover (`autobins = false`, `autotests = false`,
    /// `autoexamples = false`, `autobenches = false`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_autodiscovered: Vec<CargoTargetKind>,
}

impl CargoTargets {
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty() && self.not_autodiscovered.is_empty()
    }

    /// Whether Cargo discovers the crate roots of `kind` from the package's file layout.
    pub fn autodiscovers(&self, kind: CargoTargetKind) -> bool {
        !self.not_autodiscovered.contains(&kind)
    }
}

/// A kind of Cargo target other than the library. Cargo's target kinds are a closed set, so the
/// enum is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetKind {
    Bin,
    Test,
    Example,
    Bench,
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

    /// The Rust package whose manifest is in `dir`, repository-relative.
    pub fn rust_root_at(&self, dir: &std::path::Path) -> Option<&ProjectRoot> {
        self.roots
            .iter()
            .find(|root| root.language == Language::Rust && root.path == dir)
    }

    /// The package `crate_name` names in code of `importer`'s package, when that package declares
    /// exactly one dependency visible to `kind` under that name and its manifest is in the
    /// repository. Two declarations of one name pointing at different directories name neither.
    pub fn rust_dependency(
        &self,
        importer: &ProjectRoot,
        crate_name: &str,
        kind: CargoImporter,
    ) -> Option<&ProjectRoot> {
        let manifest = importer.cargo_manifest.as_ref()?;
        let mut dirs = manifest
            .dependencies
            .iter()
            .filter(|dependency| {
                dependency.visible_to(kind) && dependency.crate_name.as_deref() == Some(crate_name)
            })
            .map(|dependency| dependency.manifest_dir.as_path())
            .collect::<Vec<_>>();
        dirs.sort();
        dirs.dedup();
        match dirs.as_slice() {
            [dir] => self.rust_root_at(dir),
            _ => None,
        }
    }

    /// Whether `crate_name` in code of `importer`'s package names a crate outside the repository:
    /// the package declares a dependency visible to `kind` under that name that the manifest
    /// places outside the repository, and none under that name that it places inside.
    pub fn rust_external_dependency(
        &self,
        importer: &ProjectRoot,
        crate_name: &str,
        kind: CargoImporter,
    ) -> bool {
        let Some(manifest) = importer.cargo_manifest.as_ref() else {
            return false;
        };
        manifest
            .external_dependencies
            .iter()
            .any(|dependency| dependency.visible_to(kind) && dependency.crate_name == crate_name)
            && !manifest.dependencies.iter().any(|dependency| {
                dependency.visible_to(kind)
                    && (dependency.crate_name.as_deref() == Some(crate_name)
                        || dependency.key.replace('-', "_") == crate_name)
            })
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

/// Local name under which the import registry records a glob import (`use a::*;`), so name lookup
/// can tell that a glob in scope may supply a name.
pub const GLOB_IMPORT_LOCAL_NAME: &str = "*";

/// The rule that set an import binding's `target_file` or `target_symbol`, so a resolver can name
/// the evidence a relationship through the binding rests on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ImportBindingRule {
    /// Unresolved, or resolved from the module-key map and a name match in the target file.
    #[default]
    ModuleKey,
    /// A Rust `use` path followed through declared file modules from the importer's crate root.
    RustModulePath,
    /// A Rust `use` path through a crate name that reaches its item only by following `pub use`
    /// re-exports of that crate's modules.
    RustReexport,
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
    #[serde(default)]
    pub rule: ImportBindingRule,
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
