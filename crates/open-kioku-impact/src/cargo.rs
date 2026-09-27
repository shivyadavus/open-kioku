//! Rust package structure read back from the indexed `Cargo.toml` files, for two impact
//! questions the relationship graph cannot answer today:
//!
//! - which files in other crates import a public item of the changed file. The import resolver
//!   answers only paths of the importer's own crate; `use other_crate::Item` stays an unresolved
//!   import row, so no exact reference or relationship edge reaches the changed file from a
//!   downstream crate;
//! - which lexical matches cannot be affected at all: a Rust file in a package that does not
//!   depend on the changed file's package, directly or through other packages, or in no package
//!   (a fixture tree under a virtual workspace manifest that no member covers).
//!
//! Everything here is read from the index: manifests from their chunks, imports from the stored
//! import rows. A manifest that does not parse makes its files' membership unknown, and unknown
//! never prunes anything.

use open_kioku_core::{
    is_test_path, search_result_evidence_ids, File, FileId, Import, Language, LineRange,
    ScoreComponent, SearchResult, Symbol, SymbolKind, Visibility,
};
use open_kioku_errors::Result;
use open_kioku_storage::MetadataStore;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Score-breakdown signal of a file that imports a public item of the changed file.
pub(crate) const CRATE_IMPORT_SIGNAL: &str = "crate_import";
/// Score-breakdown signal of a file that names such an item in a package that imports it.
pub(crate) const CRATE_IMPORT_USE_SIGNAL: &str = "crate_import_use";

/// Files of dependent packages read for use sites of imported names. Past it the rest are
/// counted, not read.
const MAX_CRATE_USE_SCAN_FILES: usize = 2_000;

/// Names too common to read as a use of the imported item when they appear without an import in
/// the same file: the prelude and the standard library's most used types.
const PRELUDE_NAMES: [&str; 20] = [
    "Box", "Clone", "Debug", "Default", "Err", "Error", "HashMap", "HashSet", "Into", "Iterator",
    "None", "Ok", "Option", "Path", "PathBuf", "Result", "Some", "String", "ToString", "Vec",
];

#[derive(Debug, Clone)]
struct CargoPackage {
    name: String,
    /// The name other crates write in a `use` path: `[lib] name`, or the package name with `-`
    /// read as `_`.
    lib_crate: String,
    /// Repository-relative root file of the library crate.
    lib_root: PathBuf,
    dependencies: BTreeSet<String>,
}

#[derive(Debug, Clone)]
enum Manifest {
    Package(usize),
    /// A manifest with no `[package]`: a virtual workspace root. `members` are the path prefixes
    /// its member globs name, so a member whose own manifest is not indexed stays unknown.
    Virtual {
        members: Vec<String>,
    },
    Unreadable,
}

/// Which package compiles a file, as far as the indexed manifests say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Membership {
    Package(usize),
    /// Under a virtual workspace manifest and none of its members: no package compiles it.
    Outside,
    /// No indexed manifest above it, or one that did not parse.
    Unknown,
}

#[derive(Debug, Default)]
pub(crate) struct CargoWorkspace {
    packages: Vec<CargoPackage>,
    manifests: BTreeMap<PathBuf, Manifest>,
}

impl CargoWorkspace {
    pub(crate) fn load(store: &dyn MetadataStore, files: &[File]) -> Result<Self> {
        let mut workspace = Self::default();
        for file in files.iter().filter(|file| {
            file.path
                .file_name()
                .is_some_and(|name| name == "Cargo.toml")
        }) {
            let dir = file.path.parent().unwrap_or(Path::new("")).to_path_buf();
            let mut chunks = store.chunks_for_file(&file.id)?;
            chunks.sort_by_key(|chunk| chunk.range.start);
            let text = chunks
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let manifest = match text.parse::<toml::Table>() {
                Ok(table) => match parse_package(&table, &dir) {
                    Some(package) => {
                        workspace.packages.push(package);
                        Manifest::Package(workspace.packages.len() - 1)
                    }
                    None => Manifest::Virtual {
                        members: workspace_members(&table, &dir),
                    },
                },
                Err(_) => Manifest::Unreadable,
            };
            workspace.manifests.insert(dir, manifest);
        }
        Ok(workspace)
    }

    pub(crate) fn membership(&self, path: &Path) -> Membership {
        let normalized = path.to_string_lossy().replace('\\', "/");
        let mut dir = path.parent();
        while let Some(current) = dir {
            match self.manifests.get(current) {
                Some(Manifest::Package(index)) => return Membership::Package(*index),
                Some(Manifest::Unreadable) => return Membership::Unknown,
                Some(Manifest::Virtual { members }) => {
                    return if members
                        .iter()
                        .any(|member| normalized.starts_with(member.as_str()))
                    {
                        Membership::Unknown
                    } else {
                        Membership::Outside
                    };
                }
                None => dir = current.parent(),
            }
        }
        Membership::Unknown
    }

    pub(crate) fn package_name(&self, index: usize) -> &str {
        &self.packages[index].name
    }

    fn depends_on(&self, dependent: usize, dependency: usize) -> bool {
        self.packages[dependent]
            .dependencies
            .contains(&self.packages[dependency].name)
    }

    /// `package` and every package that depends on it, directly or through other packages.
    pub(crate) fn dependents_closure(&self, package: usize) -> BTreeSet<usize> {
        let mut closure = BTreeSet::from([package]);
        let mut frontier = vec![package];
        while let Some(current) = frontier.pop() {
            for candidate in 0..self.packages.len() {
                if !closure.contains(&candidate) && self.depends_on(candidate, current) {
                    closure.insert(candidate);
                    frontier.push(candidate);
                }
            }
        }
        closure
    }

    /// The module path other crates write to reach `path` in `package`'s library crate: empty
    /// for the crate root, `a::b` for `src/a/b.rs` or `src/a/b/mod.rs`. `None` for a file outside
    /// the library's source tree, such as a binary, test, bench or example target.
    fn library_module_path(&self, package: usize, path: &Path) -> Option<Vec<String>> {
        let package = &self.packages[package];
        if path == package.lib_root {
            return Some(Vec::new());
        }
        let source_root = package.lib_root.parent()?;
        let relative = path.strip_prefix(source_root).ok()?;
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            return None;
        }
        let mut segments = relative
            .with_extension("")
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        if segments.first().is_some_and(|first| first == "bin") || segments.as_slice() == ["main"] {
            return None;
        }
        if segments.last().is_some_and(|last| last == "mod") {
            segments.pop();
        }
        (!segments.is_empty()).then_some(segments)
    }
}

fn parse_package(table: &toml::Table, dir: &Path) -> Option<CargoPackage> {
    let name = table.get("package")?.get("name")?.as_str()?.to_string();
    let lib = table.get("lib");
    let lib_crate = lib
        .and_then(|lib| lib.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| name.replace('-', "_"));
    let lib_root = dir.join(
        lib.and_then(|lib| lib.get("path"))
            .and_then(toml::Value::as_str)
            .unwrap_or("src/lib.rs"),
    );
    let mut dependencies = BTreeSet::new();
    let mut read_tables = |scope: &toml::Table| {
        for section in [
            "dependencies",
            "dev-dependencies",
            "dev_dependencies",
            "build-dependencies",
            "build_dependencies",
        ] {
            let Some(entries) = scope.get(section).and_then(toml::Value::as_table) else {
                continue;
            };
            for (key, value) in entries {
                // `alias = { package = "real-name" }` depends on `real-name`.
                let package = value
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .unwrap_or(key);
                dependencies.insert(package.to_string());
            }
        }
    };
    read_tables(table);
    if let Some(targets) = table.get("target").and_then(toml::Value::as_table) {
        for target in targets.values().filter_map(toml::Value::as_table) {
            read_tables(target);
        }
    }
    Some(CargoPackage {
        name,
        lib_crate,
        lib_root,
        dependencies,
    })
}

/// Path prefixes of a workspace's `members`, each glob cut at its first wildcard.
fn workspace_members(table: &toml::Table, dir: &Path) -> Vec<String> {
    table
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(|member| {
            let prefix = member.split(['*', '?', '[']).next().unwrap_or(member);
            let joined = dir.join(prefix).to_string_lossy().replace('\\', "/");
            if member.contains(['*', '?', '[']) {
                joined
            } else {
                format!("{}/", joined.trim_end_matches('/'))
            }
        })
        .collect()
}

/// Files of downstream crates that depend on the changed file through its public items.
#[derive(Debug, Default)]
pub(crate) struct CrateDependents {
    pub(crate) results: Vec<SearchResult>,
    pub(crate) importing_files: usize,
    pub(crate) use_files: usize,
    pub(crate) packages: usize,
    /// Candidate use-site files past [`MAX_CRATE_USE_SCAN_FILES`], not read.
    pub(crate) unscanned_files: usize,
}

/// Files that import a public item the changed file defines through its crate's name, and files
/// of the importing packages that name such an item.
///
/// An importer is found from its own stored `use` row, in a package whose manifest declares the
/// dependency, naming a public top-level item the changed file defines: the path is the crate
/// name, the file's module path, and the item. A use site is weaker: an identifier token equal
/// to an imported name, outside line comments, in another file of an importing package, where
/// that package defines no item of the same name. It covers what an import row cannot show,
/// such as a file `include!`d into the crate root that imports the item, or a sibling module
/// reaching it through `super::*`.
///
/// Test files rank below production files: they depend on the item too, but the plan selects
/// them as validation, and a change breaks production dependents first.
pub(crate) fn crate_dependent_impacts(
    store: &dyn MetadataStore,
    workspace: &CargoWorkspace,
    files: &[File],
    target_file: &File,
    target_symbols: &[Symbol],
) -> Result<CrateDependents> {
    let Membership::Package(package) = workspace.membership(&target_file.path) else {
        return Ok(CrateDependents::default());
    };
    let Some(module_path) = workspace.library_module_path(package, &target_file.path) else {
        return Ok(CrateDependents::default());
    };
    let items = target_symbols
        .iter()
        .filter(|symbol| {
            symbol.file_id == target_file.id
                && symbol.parent_symbol_id.is_none()
                && symbol.visibility == Visibility::Public
                && symbol.kind != SymbolKind::Module
        })
        .map(|symbol| symbol.name.as_str())
        .collect::<BTreeSet<_>>();
    if items.is_empty() {
        return Ok(CrateDependents::default());
    }
    let files_by_id = files
        .iter()
        .map(|file| (&file.id, file))
        .collect::<HashMap<&FileId, &File>>();
    let crate_name = workspace.packages[package].lib_crate.as_str();
    let imports = store.imports()?;
    let reexported = crate_root_reexports(
        store,
        workspace,
        package,
        files,
        &imports,
        &module_path,
        &items,
    )?;

    // Importing file -> (import rows naming the changed file, the item names they import).
    let mut importers = BTreeMap::<&Path, (Vec<Import>, BTreeSet<String>, usize)>::new();
    for import in imports {
        let Some(file) = files_by_id.get(&import.file_id) else {
            continue;
        };
        if file.id == target_file.id || file.language != Language::Rust {
            continue;
        }
        let Some(item) =
            imported_item(&import.imported, crate_name, &module_path, &items).or_else(|| {
                (!reexported.is_empty())
                    .then(|| imported_item(&import.imported, crate_name, &[], &reexported))
                    .flatten()
            })
        else {
            continue;
        };
        let Membership::Package(importer_package) = workspace.membership(&file.path) else {
            continue;
        };
        if importer_package != package && !workspace.depends_on(importer_package, package) {
            continue;
        }
        let entry = importers
            .entry(file.path.as_path())
            .or_insert_with(|| (Vec::new(), BTreeSet::new(), importer_package));
        if let ImportedItem::Named(name) = item {
            entry.1.insert(name.to_string());
        }
        entry.0.push(import);
    }

    let mut dependents = CrateDependents::default();
    let mut names_by_package = BTreeMap::<usize, BTreeSet<String>>::new();
    for (path, (imports, names, importer_package)) in &importers {
        if *importer_package != package {
            names_by_package
                .entry(*importer_package)
                .or_default()
                .extend(names.iter().cloned());
        }
        dependents
            .results
            .push(crate_import_result(path, imports, crate_name, target_file));
    }
    dependents.importing_files = importers.len();

    let mut scanned = 0usize;
    for (dependent_package, names) in &names_by_package {
        let mut names = names
            .iter()
            .filter(|name| !PRELUDE_NAMES.contains(&name.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>();
        // A package defining its own item of the same name makes a bare use of the name
        // ambiguous; only the import row itself can then attribute a use to the changed file.
        for name in names.clone() {
            let shadowed = store.symbols_named(&name, 64)?.iter().any(|symbol| {
                symbol.name == name
                    && files_by_id.get(&symbol.file_id).is_some_and(|file| {
                        workspace.membership(&file.path) == Membership::Package(*dependent_package)
                    })
            });
            if shadowed {
                names.remove(&name);
            }
        }
        if names.is_empty() {
            continue;
        }
        let dependent_package = *dependent_package;
        let candidates = files.iter().filter(|file| {
            file.language == Language::Rust
                && file.id != target_file.id
                && !importers.contains_key(file.path.as_path())
                && workspace.membership(&file.path) == Membership::Package(dependent_package)
        });
        for file in candidates {
            if scanned >= MAX_CRATE_USE_SCAN_FILES {
                dependents.unscanned_files += 1;
                continue;
            }
            scanned += 1;
            let mut chunks = store.chunks_for_file(&file.id)?;
            chunks.sort_by_key(|chunk| chunk.range.start);
            let mut uses = 0usize;
            let mut first = None::<(u32, String, String)>;
            for chunk in &chunks {
                for (offset, line) in chunk.text.lines().enumerate() {
                    if line.trim_start().starts_with("//") {
                        continue;
                    }
                    for token in identifier_tokens(line) {
                        if let Some(name) = names.get(token) {
                            uses += 1;
                            if first.is_none() {
                                first = Some((
                                    chunk.range.start + offset as u32,
                                    name.clone(),
                                    line.trim().chars().take(240).collect(),
                                ));
                            }
                        }
                    }
                }
            }
            if let Some((line, name, snippet)) = first {
                dependents.results.push(crate_use_result(
                    file,
                    line,
                    &name,
                    snippet,
                    uses,
                    crate_name,
                    target_file,
                ));
                dependents.use_files += 1;
            }
        }
    }
    dependents.packages = importers
        .values()
        .map(|(_, _, importer_package)| *importer_package)
        .collect::<BTreeSet<_>>()
        .len();
    Ok(dependents)
}

/// Items of the changed file that the crate root re-exports with `pub use`, so other crates
/// reach them as `crate_name::Item`. Read from the root's stored import rows, each kept only when
/// its source line is a `pub use`: a private `use` in the root re-exports nothing.
fn crate_root_reexports<'a>(
    store: &dyn MetadataStore,
    workspace: &CargoWorkspace,
    package: usize,
    files: &[File],
    imports: &[Import],
    module_path: &[String],
    items: &BTreeSet<&'a str>,
) -> Result<BTreeSet<&'a str>> {
    let mut reexported = BTreeSet::<&'a str>::new();
    if module_path.is_empty() {
        return Ok(reexported);
    }
    let lib_root = &workspace.packages[package].lib_root;
    let Some(root) = files.iter().find(|file| &file.path == lib_root) else {
        return Ok(reexported);
    };
    let mut root_lines = None::<Vec<(u32, String)>>;
    for import in imports.iter().filter(|import| import.file_id == root.id) {
        let path = import
            .imported
            .strip_prefix("crate::")
            .or_else(|| import.imported.strip_prefix("self::"))
            .unwrap_or(&import.imported);
        let segments = path.split("::").map(str::trim).collect::<Vec<_>>();
        let Some((last, parent)) = segments.split_last() else {
            continue;
        };
        if parent != module_path {
            continue;
        }
        let names = if *last == "*" {
            items.iter().copied().collect::<Vec<_>>()
        } else {
            items.iter().copied().filter(|item| item == last).collect()
        };
        if names.is_empty() {
            continue;
        }
        let Some(line) = import.range.as_ref().map(|range| range.start) else {
            continue;
        };
        if root_lines.is_none() {
            let mut lines = Vec::new();
            for chunk in store.chunks_for_file(&root.id)? {
                for (offset, text) in chunk.text.lines().enumerate() {
                    lines.push((chunk.range.start + offset as u32, text.trim().to_string()));
                }
            }
            root_lines = Some(lines);
        }
        let is_pub_use = root_lines.as_ref().is_some_and(|lines| {
            lines
                .iter()
                .any(|(number, text)| *number == line && text.starts_with("pub use "))
        });
        if is_pub_use {
            reexported.extend(names);
        }
    }
    Ok(reexported)
}

enum ImportedItem<'a> {
    /// `crate::module::Item`, or a path through the item such as `Enum::Variant`.
    Named(&'a str),
    /// `crate::module::*` or `crate::module` itself.
    Module,
}

fn imported_item<'a>(
    imported: &'a str,
    crate_name: &str,
    module_path: &[String],
    items: &BTreeSet<&str>,
) -> Option<ImportedItem<'a>> {
    let mut segments = imported.split("::").map(str::trim);
    if segments.next()? != crate_name {
        return None;
    }
    for module in module_path {
        if segments.next()? != module {
            return None;
        }
    }
    match segments.next() {
        None if !module_path.is_empty() => Some(ImportedItem::Module),
        None => None,
        Some("*") => Some(ImportedItem::Module),
        Some(name) => items.get(name).map(|_| ImportedItem::Named(name)),
    }
}

fn identifier_tokens(line: &str) -> impl Iterator<Item = &str> {
    line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| {
            token
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        })
}

/// Production dependents first; within each, an importer before a use site, and more import
/// rows or use sites before fewer.
fn dependent_score(path: &Path, base: f32, count: usize) -> f32 {
    let weight = if is_test_path(&path.to_string_lossy()) {
        0.5
    } else {
        1.0
    };
    base * weight + 0.01 * count.min(20) as f32
}

fn crate_import_result(
    path: &Path,
    imports: &[Import],
    crate_name: &str,
    target_file: &File,
) -> SearchResult {
    let line_range = imports.first().and_then(|import| import.range.clone());
    let evidence = imports
        .iter()
        .map(|import| {
            format!(
                "imports `{}`{}, a public item `{}` defines, through the `{crate_name}` crate its package depends on",
                import.imported,
                import
                    .range
                    .as_ref()
                    .map(|range| format!(" at line {}", range.start))
                    .unwrap_or_default(),
                target_file.path.display()
            )
        })
        .collect::<Vec<_>>();
    let evidence_refs = search_result_evidence_ids(path, &line_range, evidence.len());
    let score = dependent_score(path, 1.0, imports.len());
    SearchResult {
        path: path.to_path_buf(),
        snippet: imports
            .first()
            .map(|import| format!("use {};", import.imported))
            .unwrap_or_default(),
        line_range,
        symbol: None,
        score,
        match_reason: format!("imports a public item of the changed file from `{crate_name}`"),
        evidence,
        evidence_refs: evidence_refs.clone(),
        confidence: 0.8,
        score_breakdown: vec![ScoreComponent::single(
            CRATE_IMPORT_SIGNAL,
            score,
            evidence_refs,
            "stored `use` rows naming a public item of the changed file through its crate name",
        )],
        exact_reference_provenance: None,
    }
}

fn crate_use_result(
    file: &File,
    line: u32,
    name: &str,
    snippet: String,
    uses: usize,
    crate_name: &str,
    target_file: &File,
) -> SearchResult {
    let line_range = Some(LineRange {
        start: line,
        end: line,
    });
    let evidence = vec![format!(
        "names `{name}` {uses} time(s); its package imports `{name}` from `{crate_name}`, where `{}` defines it, and defines no item of that name",
        target_file.path.display()
    )];
    let evidence_refs = search_result_evidence_ids(&file.path, &line_range, evidence.len());
    let score = dependent_score(&file.path, 0.8, uses);
    SearchResult {
        path: file.path.clone(),
        line_range,
        snippet,
        symbol: None,
        score,
        match_reason: format!(
            "names a public item of the changed file that its package imports from `{crate_name}`"
        ),
        evidence,
        evidence_refs: evidence_refs.clone(),
        confidence: 0.5,
        score_breakdown: vec![ScoreComponent::single(
            CRATE_IMPORT_USE_SIGNAL,
            score,
            evidence_refs,
            "identifier uses of a name the file's package imports from the changed file",
        )],
        exact_reference_provenance: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(text: &str) -> toml::Table {
        text.parse::<toml::Table>().unwrap()
    }

    #[test]
    fn a_renamed_or_target_specific_dependency_names_the_real_package() {
        let package = parse_package(
            &manifest(
                "[package]\nname = \"app\"\n\n[lib]\nname = \"app_core\"\npath = \"lib/root.rs\"\n\n[dependencies]\nengine = { package = \"plan-engine\", path = \"../engine\" }\n\n[target.'cfg(unix)'.dev-dependencies]\nunix-helper = \"1\"\n",
            ),
            Path::new("crates/app"),
        )
        .unwrap();
        assert_eq!(package.lib_crate, "app_core");
        assert_eq!(package.lib_root, PathBuf::from("crates/app/lib/root.rs"));
        assert_eq!(
            package.dependencies,
            BTreeSet::from(["plan-engine".to_string(), "unix-helper".to_string()])
        );
    }

    #[test]
    fn membership_is_unknown_unless_a_manifest_proves_it() {
        let mut workspace = CargoWorkspace::default();
        workspace.packages.push(
            parse_package(
                &manifest("[package]\nname = \"engine\"\n"),
                Path::new("crates/engine"),
            )
            .unwrap(),
        );
        workspace
            .manifests
            .insert(PathBuf::from("crates/engine"), Manifest::Package(0));
        workspace.manifests.insert(
            PathBuf::new(),
            Manifest::Virtual {
                members: workspace_members(
                    &manifest("[workspace]\nmembers = [\"crates/*\", \"tools/gen\"]\n"),
                    Path::new(""),
                ),
            },
        );
        workspace
            .manifests
            .insert(PathBuf::from("vendor/broken"), Manifest::Unreadable);

        let membership = |path: &str| workspace.membership(Path::new(path));
        assert_eq!(
            membership("crates/engine/src/lib.rs"),
            Membership::Package(0)
        );
        // A member whose own manifest is not indexed is not proven outside every package.
        assert_eq!(
            membership("crates/unindexed/src/lib.rs"),
            Membership::Unknown
        );
        assert_eq!(membership("tools/gen/src/main.rs"), Membership::Unknown);
        assert_eq!(
            membership("tools/generator/src/main.rs"),
            Membership::Outside
        );
        assert_eq!(
            membership("benchmarks/fixture/src/lib.rs"),
            Membership::Outside
        );
        assert_eq!(membership("vendor/broken/src/lib.rs"), Membership::Unknown);
    }

    #[test]
    fn an_import_names_the_changed_file_only_through_its_module_path() {
        let items = BTreeSet::from(["Builder"]);
        let module = vec!["builder".to_string()];
        let named = |imported: &str| {
            matches!(
                imported_item(imported, "engine", &module, &items),
                Some(ImportedItem::Named("Builder"))
            )
        };
        assert!(named("engine::builder::Builder"));
        assert!(named("engine::builder::Builder::new"));
        assert!(!named("engine::Builder"));
        assert!(!named("engine::other::Builder"));
        assert!(!named("other_engine::builder::Builder"));
        assert!(matches!(
            imported_item("engine::builder::*", "engine", &module, &items),
            Some(ImportedItem::Module)
        ));
        assert!(imported_item("engine::builder::Missing", "engine", &module, &items).is_none());
    }
}
