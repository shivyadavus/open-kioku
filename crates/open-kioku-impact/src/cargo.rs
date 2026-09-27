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
    /// `[lib] proc-macro = true`: the crate's macros expand into code of the crates that use
    /// them, so it can name a crate it does not depend on.
    proc_macro: bool,
    /// Package name of each dependency, with the crate name this package writes for it when the
    /// manifest renames it (`alias = { package = "real-name" }`); `None` means the dependency's
    /// own library crate name.
    dependencies: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone)]
enum Manifest {
    /// `members` are set when the package's manifest is also a workspace root.
    Package {
        index: usize,
        members: Vec<String>,
    },
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
        let mut tables = Vec::new();
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
            // A manifest the index holds no text for proves nothing about the files under it.
            match text.parse::<toml::Table>() {
                Ok(table) if !text.trim().is_empty() => tables.push((dir, table)),
                _ => {
                    workspace.manifests.insert(dir, Manifest::Unreadable);
                }
            }
        }
        // `alias.workspace = true` inherits the workspace's `alias = { package = "real" }`.
        let inherited = tables
            .iter()
            .filter_map(|(_, table)| table.get("workspace")?.get("dependencies")?.as_table())
            .flatten()
            .filter_map(|(key, value)| {
                Some((key.clone(), value.get("package")?.as_str()?.to_string()))
            })
            .collect::<HashMap<_, _>>();
        for (dir, table) in tables {
            let members = workspace_members(&table, &dir);
            let manifest = match parse_package(&table, &dir, &inherited) {
                Some(package) => {
                    workspace.packages.push(package);
                    Manifest::Package {
                        index: workspace.packages.len() - 1,
                        members,
                    }
                }
                None => Manifest::Virtual { members },
            };
            workspace.manifests.insert(dir, manifest);
        }
        Ok(workspace)
    }

    pub(crate) fn membership(&self, path: &Path) -> Membership {
        let normalized = path.to_string_lossy().replace('\\', "/");
        let in_member = |members: &[String]| {
            members
                .iter()
                .any(|member| normalized.starts_with(member.as_str()))
        };
        let mut dir = path.parent();
        while let Some(current) = dir {
            match self.manifests.get(current) {
                // Under a member directory of a root package whose own manifest was not
                // found on the way up: that member's package, not the root's, compiles it.
                Some(Manifest::Package { index, members }) => {
                    return if in_member(members) {
                        Membership::Unknown
                    } else {
                        Membership::Package(*index)
                    };
                }
                Some(Manifest::Unreadable) => return Membership::Unknown,
                Some(Manifest::Virtual { members }) => {
                    return if in_member(members) {
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

    /// Whether `index` is a procedural-macro crate. Cargo puts it upstream of the crates that use
    /// its macros, but the code it emits (`quote! { ::engine::Engine::new() }`) is theirs, so a
    /// change it names can reach it against the dependency direction.
    pub(crate) fn is_proc_macro(&self, index: usize) -> bool {
        self.packages[index].proc_macro
    }

    pub(crate) fn package_name(&self, index: usize) -> &str {
        &self.packages[index].name
    }

    fn depends_on(&self, dependent: usize, dependency: usize) -> bool {
        self.packages[dependent]
            .dependencies
            .contains_key(&self.packages[dependency].name)
    }

    /// The crate name `dependent` writes in a `use` path to reach `dependency`, when it declares
    /// the dependency or is the package itself (its tests, benches and examples).
    fn crate_name_in(&self, dependent: usize, dependency: usize) -> Option<&str> {
        let own = self.packages[dependency].lib_crate.as_str();
        if dependent == dependency {
            return Some(own);
        }
        match self.packages[dependent]
            .dependencies
            .get(&self.packages[dependency].name)?
        {
            Some(renamed) => Some(renamed.as_str()),
            None => Some(own),
        }
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

fn parse_package(
    table: &toml::Table,
    dir: &Path,
    inherited: &HashMap<String, String>,
) -> Option<CargoPackage> {
    let name = table.get("package")?.get("name")?.as_str()?.to_string();
    let lib = table.get("lib");
    let proc_macro = lib
        .and_then(|lib| lib.get("proc-macro").or_else(|| lib.get("proc_macro")))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
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
    let mut dependencies = BTreeMap::new();
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
                // `alias = { package = "real-name" }` depends on `real-name`, and this package's
                // code names it `alias`.
                let inherits = value
                    .get("workspace")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false);
                let package = value
                    .get("package")
                    .and_then(toml::Value::as_str)
                    .or_else(|| inherits.then(|| inherited.get(key).map(String::as_str))?)
                    .unwrap_or(key);
                let renamed = (package != key).then(|| key.replace('-', "_"));
                dependencies.insert(package.to_string(), renamed);
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
        proc_macro,
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
    /// Why no downstream crate was looked for, when none was: an empty result is then absence of
    /// measurement, not absence of dependents.
    pub(crate) not_measured: Option<String>,
}

impl CrateDependents {
    fn not_measured(reason: impl Into<String>) -> Self {
        Self {
            not_measured: Some(reason.into()),
            ..Self::default()
        }
    }
}

/// Files that import a public item the changed file defines through its crate's name, and files
/// of the importing packages that name such an item.
///
/// An importer is found from its own stored `use` row, in a package whose manifest declares the
/// dependency, naming a public top-level item the changed file defines: the path is the crate name
/// that package uses for it, the file's module path, and the item. A use site is weaker: an
/// identifier equal to an imported name in another file of an importing package, outside comments,
/// string and character literals, not after a `.`, and not the tail of a path that starts with
/// another crate, where that package defines no item of the same name and the file imports no
/// item of that name from elsewhere. It covers what an import row cannot
/// show, such as a file `include!`d into the crate root that imports the item, or a sibling
/// module reaching it through `super::*`.
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
    let package = match workspace.membership(&target_file.path) {
        Membership::Package(package) => package,
        Membership::Outside => {
            return Ok(CrateDependents::not_measured(
                "the file is under a Cargo workspace manifest but in none of its members, so no package compiles it",
            ))
        }
        Membership::Unknown => {
            return Ok(CrateDependents::not_measured(
                "no indexed, readable Cargo.toml says which package compiles the file",
            ))
        }
    };
    // A package with only binaries has no library crate for another crate to import.
    let library_indexed = files
        .iter()
        .any(|file| file.path == workspace.packages[package].lib_root);
    let module_path = workspace
        .library_module_path(package, &target_file.path)
        .filter(|_| library_indexed);
    let Some(module_path) = module_path else {
        return Ok(CrateDependents::not_measured(format!(
            "the file is not a module of an indexed library crate of `{}` (a binary, test, bench, example or build target, or a package with no library), which other crates cannot import; a library module it is included into is not traced",
            workspace.packages[package].name
        )));
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
        return Ok(CrateDependents::not_measured(
            "the file defines no public top-level item another crate could import by name",
        ));
    }
    // Membership is read once per Rust file, not once per file per dependent package.
    let rust_files = files
        .iter()
        .filter(|file| file.language == Language::Rust)
        .map(|file| (&file.id, (file, workspace.membership(&file.path))))
        .collect::<HashMap<&FileId, (&File, Membership)>>();
    let own_crate = workspace.packages[package].lib_crate.as_str();
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
    let mut importers = BTreeMap::<&Path, (Vec<&Import>, BTreeSet<String>, usize)>::new();
    let mut imports_by_file = HashMap::<&FileId, Vec<&Import>>::new();
    for import in &imports {
        let Some((file, membership)) = rust_files.get(&import.file_id) else {
            continue;
        };
        imports_by_file.entry(&file.id).or_default().push(import);
        if file.id == target_file.id {
            continue;
        }
        let Membership::Package(importer_package) = *membership else {
            continue;
        };
        let Some(crate_name) = workspace.crate_name_in(importer_package, package) else {
            continue;
        };
        let Some(item) =
            imported_item(&import.imported, crate_name, &module_path, &items).or_else(|| {
                (!reexported.is_empty())
                    .then(|| imported_item(&import.imported, crate_name, &[], &reexported))
                    .flatten()
            })
        else {
            continue;
        };
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
    for (path, (rows, names, importer_package)) in &importers {
        if *importer_package != package {
            names_by_package
                .entry(*importer_package)
                .or_default()
                .extend(names.iter().cloned());
        }
        dependents
            .results
            .push(crate_import_result(path, rows, own_crate, target_file));
    }
    dependents.importing_files = importers.len();

    let mut scanned = 0usize;
    for (dependent_package, names) in &names_by_package {
        let dependent_package = *dependent_package;
        let mut names = names
            .iter()
            .filter(|name| !PRELUDE_NAMES.contains(&name.as_str()))
            .cloned()
            .collect::<BTreeSet<_>>();
        // A package defining its own item of the same name makes a bare use of the name
        // ambiguous; only the import row itself can then attribute a use to the changed file.
        for name in names.clone() {
            let shadowed = store
                .symbols_named(&name, usize::MAX)?
                .iter()
                .any(|symbol| {
                    symbol.name == name
                        && rust_files
                            .get(&symbol.file_id)
                            .is_some_and(|(_, membership)| {
                                *membership == Membership::Package(dependent_package)
                            })
                });
            if shadowed {
                names.remove(&name);
            }
        }
        if names.is_empty() {
            continue;
        }
        let Some(dependency_crate) = workspace.crate_name_in(dependent_package, package) else {
            continue;
        };
        let mut candidates = rust_files
            .values()
            .filter(|(file, membership)| {
                *membership == Membership::Package(dependent_package)
                    && file.id != target_file.id
                    && !importers.contains_key(file.path.as_path())
            })
            .map(|(file, _)| *file)
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.path.cmp(&right.path));
        for file in candidates {
            // A file importing the same name from another path means that item, not this one.
            let own_names = names
                .iter()
                .filter(|name| {
                    !imports_by_file.get(&file.id).is_some_and(|rows| {
                        rows.iter()
                            .any(|row| row.imported.rsplit("::").next() == Some(name.as_str()))
                    })
                })
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            if own_names.is_empty() {
                continue;
            }
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
                // A chunk starts at an item, where no string or comment is open, so the scan
                // restarts per chunk and carries its state across the chunk's lines.
                for identifier in code_identifiers(&chunk.text) {
                    let Some(name) = own_names.get(identifier.name) else {
                        continue;
                    };
                    // `other_crate::Name` is another crate's item; only a path through the
                    // name this package gives the dependency reaches the changed file.
                    if identifier
                        .path_head
                        .is_some_and(|head| head != dependency_crate)
                    {
                        continue;
                    }
                    uses += 1;
                    if first.is_none() {
                        first = Some((
                            chunk.range.start + identifier.line as u32,
                            (*name).to_string(),
                            chunk
                                .text
                                .lines()
                                .nth(identifier.line)
                                .unwrap_or_default()
                                .trim()
                                .chars()
                                .take(240)
                                .collect(),
                        ));
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
                    own_crate,
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

/// An identifier used as code, with the 0-based line of the text it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeIdentifier<'a> {
    line: usize,
    name: &'a str,
    /// First segment of the path the identifier ends, when it follows `::`: `a` for
    /// `a::b::Name`. `None` for a bare name or a path's own first segment.
    path_head: Option<&'a str>,
}

/// Identifiers a text uses as code: outside line and block comments, string, raw string and
/// character literals, and not the member of a `receiver.` (a method or field of some other type).
/// String and comment state carries across lines, so a multi-line string's continuation lines
/// are not read as code.
fn code_identifiers(text: &str) -> Vec<CodeIdentifier<'_>> {
    let bytes = text.as_bytes();
    let mut identifiers = Vec::new();
    let mut line = 0usize;
    let mut index = 0usize;
    // The path the previous identifier started or continued, and whether `::` followed it.
    let mut path_head = None::<&str>;
    let mut after_path_separator = false;
    let mut after_dot = false;
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let count_lines =
        |from: usize, to: usize| bytes[from..to].iter().filter(|b| **b == b'\n').count();
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'\n' => {
                line += 1;
                index += 1;
            }
            b' ' | b'\t' | b'\r' => index += 1,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                let start = index;
                let mut depth = 0usize;
                while index < bytes.len() {
                    if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                        depth += 1;
                        index += 2;
                    } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                        depth -= 1;
                        index += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
                line += count_lines(start, index);
                path_head = None;
                after_path_separator = false;
                after_dot = false;
            }
            b'"' => {
                let start = index;
                index = skip_string(bytes, index + 1, 0);
                line += count_lines(start, index);
                path_head = None;
                after_path_separator = false;
                after_dot = false;
            }
            b'\'' => {
                // A character literal ('x', '\n', '"', '\u{1F600}'); otherwise a lifetime.
                let end = if bytes.get(index + 1) == Some(&b'\\') {
                    bytes[index + 2..]
                        .iter()
                        .position(|b| *b == b'\'' || *b == b'\n')
                        .map(|offset| index + 2 + offset)
                        .filter(|end| bytes[*end] == b'\'')
                } else {
                    let width = text[index + 1..].chars().next().map_or(0, char::len_utf8);
                    let close = index + 1 + width;
                    (width > 0 && bytes.get(close) == Some(&b'\'')).then_some(close)
                };
                index = end.map_or(index + 1, |end| end + 1);
                path_head = None;
                after_path_separator = false;
                after_dot = false;
            }
            b':' if bytes.get(index + 1) == Some(&b':') => {
                after_path_separator = true;
                after_dot = false;
                index += 2;
            }
            b'.' => {
                after_dot = true;
                after_path_separator = false;
                path_head = None;
                index += 1;
            }
            _ if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                while index < bytes.len() && is_ident(bytes[index]) {
                    index += 1;
                }
                let word = &text[start..index];
                // `r"…"`, `r#"…"#`, `b"…"`, `br#"…"#`: a raw or byte string, not an identifier.
                if matches!(word, "r" | "b" | "br")
                    && matches!(bytes.get(index), Some(b'"') | Some(b'#'))
                {
                    let raw = word != "b";
                    let hashes = bytes[index..].iter().take_while(|b| **b == b'#').count();
                    if bytes.get(index + hashes) == Some(&b'"') && (raw || hashes == 0) {
                        let open = index;
                        index = skip_string(
                            bytes,
                            index + hashes + 1,
                            if raw { hashes + 1 } else { 0 },
                        );
                        line += count_lines(open, index);
                        path_head = None;
                        after_path_separator = false;
                        after_dot = false;
                        continue;
                    }
                }
                let continues_path = after_path_separator && path_head.is_some();
                if !after_dot {
                    identifiers.push(CodeIdentifier {
                        line,
                        name: word,
                        path_head: if continues_path { path_head } else { None },
                    });
                }
                if !continues_path {
                    path_head = Some(word);
                }
                after_path_separator = false;
                after_dot = false;
            }
            _ if byte.is_ascii_digit() => {
                while index < bytes.len() && is_ident(bytes[index]) {
                    index += 1;
                }
                path_head = None;
                after_path_separator = false;
                after_dot = false;
            }
            _ => {
                // A leading `::` (`::engine::Name`) opens a path whose head is the next word.
                path_head = None;
                after_path_separator = false;
                after_dot = false;
                index += 1;
            }
        }
    }
    identifiers
}

/// The index just past a string whose body starts at `index`. `raw_hashes` is 0 for an escaped
/// string, or one more than the number of `#`s of a raw string, which ignores escapes and ends at
/// `"` followed by that many `#`s. An unterminated string runs to the end of the text.
fn skip_string(bytes: &[u8], mut index: usize, raw_hashes: usize) -> usize {
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if raw_hashes == 0 => index += 2,
            b'"' => {
                let hashes = raw_hashes.saturating_sub(1);
                let closes = bytes[index + 1..]
                    .iter()
                    .take(hashes)
                    .filter(|b| **b == b'#')
                    .count()
                    == hashes;
                if closes {
                    return (index + 1 + hashes).min(bytes.len());
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    bytes.len()
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
    imports: &[&Import],
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
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(package.lib_crate, "app_core");
        assert_eq!(package.lib_root, PathBuf::from("crates/app/lib/root.rs"));
        assert_eq!(
            package.dependencies,
            BTreeMap::from([
                ("plan-engine".to_string(), Some("engine".to_string())),
                ("unix-helper".to_string(), None),
            ])
        );
    }

    #[test]
    fn membership_is_unknown_unless_a_manifest_proves_it() {
        let mut workspace = CargoWorkspace::default();
        workspace.packages.push(
            parse_package(
                &manifest("[package]\nname = \"engine\"\n"),
                Path::new("crates/engine"),
                &HashMap::new(),
            )
            .unwrap(),
        );
        workspace.manifests.insert(
            PathBuf::from("crates/engine"),
            Manifest::Package {
                index: 0,
                members: Vec::new(),
            },
        );
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

    fn names(text: &str) -> Vec<(usize, &str, Option<&str>)> {
        code_identifiers(text)
            .into_iter()
            .map(|identifier| (identifier.line, identifier.name, identifier.path_head))
            .collect()
    }

    #[test]
    fn code_identifiers_skip_comments_strings_and_members() {
        assert_eq!(
            names(r#"let x = Engine::new("Engine \" Engine").run(); // Engine"#),
            vec![
                (0, "let", None),
                (0, "x", None),
                (0, "Engine", None),
                (0, "new", Some("Engine"))
            ]
        );
        assert_eq!(
            names("value.Engine + 2u8 /* Engine */"),
            vec![(0, "value", None)]
        );
    }

    #[test]
    fn code_identifiers_carry_string_and_char_state_across_lines() {
        let text = "let s = \"first\n  Engine in prose\n\";\nlet q = '\"'; Engine::new();\nlet r = r#\"a \" Engine\n\"#; let l: &'a str = x;\n";
        let found = names(text);
        assert_eq!(
            found
                .iter()
                .filter(|(_, name, _)| *name == "Engine")
                .copied()
                .collect::<Vec<_>>(),
            vec![(3, "Engine", None)],
            "{found:?}"
        );
        // A lifetime is not a character literal: the code after it is still read.
        assert!(found.contains(&(5, "str", None)), "{found:?}");
    }

    #[test]
    fn a_path_through_another_crate_names_its_head() {
        assert_eq!(
            names("other_cfg::Config::default(); ::engine::Config::new(); Config"),
            vec![
                (0, "other_cfg", None),
                (0, "Config", Some("other_cfg")),
                (0, "default", Some("other_cfg")),
                (0, "engine", None),
                (0, "Config", Some("engine")),
                (0, "new", Some("engine")),
                (0, "Config", None),
            ]
        );
    }

    #[test]
    fn a_root_package_does_not_claim_its_workspace_members_files() {
        let mut workspace = CargoWorkspace::default();
        workspace.packages.push(
            parse_package(
                &manifest("[package]\nname = \"root\"\n"),
                Path::new(""),
                &HashMap::new(),
            )
            .unwrap(),
        );
        workspace.manifests.insert(
            PathBuf::new(),
            Manifest::Package {
                index: 0,
                members: vec!["crates/".into()],
            },
        );
        assert_eq!(
            workspace.membership(Path::new("src/lib.rs")),
            Membership::Package(0)
        );
        assert_eq!(
            workspace.membership(Path::new("crates/unindexed/src/lib.rs")),
            Membership::Unknown
        );
    }

    #[test]
    fn an_inherited_workspace_dependency_keeps_its_rename() {
        let inherited = HashMap::from([("engine".to_string(), "plan-engine".to_string())]);
        let package = parse_package(
            &manifest("[package]\nname = \"app\"\n\n[dependencies]\nengine.workspace = true\n"),
            Path::new("crates/app"),
            &inherited,
        )
        .unwrap();
        assert_eq!(
            package.dependencies,
            BTreeMap::from([("plan-engine".to_string(), Some("engine".to_string()))])
        );
    }
}
