//! Rust package structure for two impact questions, read from what the index stores rather than
//! from manifests or `use` paths parsed here:
//!
//! - which files in other crates import the changed file: the import resolver's own resolutions
//!   of `use` paths written through a crate name, which follow the dependency the importer's
//!   package declares, the dependency's module tree and its `pub use` re-exports;
//! - which lexical matches cannot be affected at all: a Rust file in a package that does not
//!   depend on the changed file's package, directly or through other packages, or in no package
//!   (a fixture tree under a virtual workspace manifest that no member covers).
//!
//! The package model is the facts ingest stores on each indexed `Cargo.toml`
//! ([`open_kioku_core::cargo_manifest`]), keyed by manifest path: two workspaces each with a
//! package of one name are two packages, and a dependency is the manifest its `path` places, never
//! a package that shares its name. A manifest with no such fact did not parse, and makes its
//! files' membership unknown; unknown never prunes anything.

use open_kioku_core::cargo_manifest::{
    parse_cargo_package_label, CARGO_BUILD_DEPENDENCY_SOURCE, CARGO_DEPENDENCY_SOURCE,
    CARGO_DEV_DEPENDENCY_SOURCE, CARGO_LIBRARY_ROOT_SOURCE, CARGO_MANIFEST_SOURCE_PREFIX,
    CARGO_PACKAGE_SOURCE, CARGO_PROC_MACRO_PACKAGE_SOURCE, CARGO_VIRTUAL_WORKSPACE_SOURCE,
    CARGO_WORKSPACE_MEMBER_SOURCE,
};
use open_kioku_core::{
    is_test_path, search_result_evidence_ids, EvidenceSourceType, File, FileId, GraphEdgeType,
    GraphNodeType, Import, Language, LineRange, ScoreComponent, SearchResult, Symbol, SymbolKind,
    Visibility,
};
use open_kioku_errors::Result;
use open_kioku_storage::MetadataStore;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Score-breakdown signal of a file that imports a public item of the changed file.
pub(crate) const CRATE_IMPORT_SIGNAL: &str = "crate_import";
/// Score-breakdown signal of a file that names such an item in a package that imports it.
pub(crate) const CRATE_IMPORT_USE_SIGNAL: &str = "crate_import_use";

/// Files of dependent packages read for use sites of imported names, and for `use` rows into
/// the changed crate the resolver left unresolved. Past it the rest are counted, not read.
const MAX_CRATE_USE_SCAN_FILES: usize = 2_000;

/// Prefix of the stored resolution of a Rust `use` path the module tree answered.
const RUST_IMPORT_RESOLUTION_SOURCE_PREFIX: &str = "open-kioku-import-resolver/rust-";

/// Names too common to read as a use of the imported item when they appear without an import in
/// the same file: the prelude and the standard library's most used types.
const PRELUDE_NAMES: [&str; 20] = [
    "Box", "Clone", "Debug", "Default", "Err", "Error", "HashMap", "HashSet", "Into", "Iterator",
    "None", "Ok", "Option", "Path", "PathBuf", "Result", "Some", "String", "ToString", "Vec",
];

#[derive(Debug, Clone)]
struct CargoPackage {
    /// The name other crates write for the package's library in a `use` path.
    crate_name: String,
    /// Repository-relative directory of the package's manifest.
    dir: PathBuf,
    /// Repository-relative root file of the library crate, when it is indexed.
    library_root: Option<PathBuf>,
    /// A procedural-macro crate: its macros expand into code of the crates that use them, so it
    /// can name a crate it does not depend on.
    proc_macro: bool,
    /// The packages whose manifests its own declares a dependency on, of any kind.
    dependencies: BTreeSet<usize>,
}

#[derive(Debug, Clone)]
enum Manifest {
    /// `members` are set when the package's manifest is also a workspace root.
    Package { index: usize, members: Vec<String> },
    /// A manifest with no `[package]`: a virtual workspace root. `members` are the path prefixes
    /// its member globs name, so a member whose own manifest is not indexed stays unknown.
    Virtual { members: Vec<String> },
    /// An indexed manifest the index stores no package fact for: it did not parse.
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
    /// The package model stored on each indexed `Cargo.toml` among `files`.
    pub(crate) fn load(store: &dyn MetadataStore, files: &[File]) -> Result<Self> {
        let mut workspace = Self::default();
        let indexed = files
            .iter()
            .map(|file| file.path.as_path())
            .collect::<std::collections::HashSet<_>>();
        // Dependencies name manifests; they are placed once every package has an index.
        let mut declared = Vec::<(usize, PathBuf)>::new();
        for file in files.iter().filter(|file| {
            file.path
                .file_name()
                .is_some_and(|name| name == "Cargo.toml")
        }) {
            let dir = file.path.parent().unwrap_or(Path::new("")).to_path_buf();
            let facts = store
                .analysis_facts_for_file(
                    &file.id,
                    Some(EvidenceSourceType::StaticAnalysis),
                    usize::MAX,
                )?
                .into_iter()
                .filter(|fact| fact.source.starts_with(CARGO_MANIFEST_SOURCE_PREFIX))
                .collect::<Vec<_>>();
            let of = |source: &'static str| {
                facts
                    .iter()
                    .filter(move |fact| fact.source.as_str() == source)
            };
            let members = of(CARGO_WORKSPACE_MEMBER_SOURCE)
                .filter(|fact| fact.edge_type == GraphEdgeType::Contains)
                .map(|fact| fact.target.clone())
                .collect::<Vec<_>>();
            let package = of(CARGO_PACKAGE_SOURCE)
                .chain(of(CARGO_PROC_MACRO_PACKAGE_SOURCE))
                .find(|fact| fact.target_kind == GraphNodeType::BuildTarget)
                .and_then(|fact| {
                    let (crate_name, _) = parse_cargo_package_label(&fact.target)?;
                    Some((
                        crate_name.to_string(),
                        fact.source.as_str() == CARGO_PROC_MACRO_PACKAGE_SOURCE,
                    ))
                });
            let manifest = match package {
                Some((crate_name, proc_macro)) => {
                    let index = workspace.packages.len();
                    let library_root = of(CARGO_LIBRARY_ROOT_SOURCE)
                        .map(|fact| PathBuf::from(&fact.target))
                        .find(|root| indexed.contains(root.as_path()));
                    workspace.packages.push(CargoPackage {
                        crate_name,
                        dir: dir.clone(),
                        library_root,
                        proc_macro,
                        dependencies: BTreeSet::new(),
                    });
                    for source in [
                        CARGO_DEPENDENCY_SOURCE,
                        CARGO_DEV_DEPENDENCY_SOURCE,
                        CARGO_BUILD_DEPENDENCY_SOURCE,
                    ] {
                        declared.extend(
                            of(source)
                                .filter(|fact| fact.edge_type == GraphEdgeType::DependsOn)
                                .map(|fact| {
                                    let manifest = PathBuf::from(&fact.target);
                                    let dir =
                                        manifest.parent().unwrap_or(Path::new("")).to_path_buf();
                                    (index, dir)
                                }),
                        );
                    }
                    Manifest::Package { index, members }
                }
                None if of(CARGO_VIRTUAL_WORKSPACE_SOURCE).next().is_some() => {
                    Manifest::Virtual { members }
                }
                // A manifest the index holds no fact for proves nothing about the files under it.
                None => Manifest::Unreadable,
            };
            workspace.manifests.insert(dir, manifest);
        }
        let by_dir = workspace
            .packages
            .iter()
            .enumerate()
            .map(|(index, package)| (package.dir.clone(), index))
            .collect::<HashMap<_, _>>();
        for (dependent, dir) in declared {
            if let Some(dependency) = by_dir.get(&dir) {
                workspace.packages[dependent]
                    .dependencies
                    .insert(*dependency);
            }
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

    /// How the reasons name a package: its library's crate name and its manifest's directory,
    /// since two workspaces may each hold a package of one name.
    pub(crate) fn package_label(&self, index: usize) -> String {
        let package = &self.packages[index];
        let dir = package.dir.to_string_lossy();
        format!(
            "`{}` (`{}`)",
            package.crate_name,
            if dir.is_empty() { "." } else { &dir }
        )
    }

    /// `package` and every package that depends on it, directly or through other packages.
    pub(crate) fn dependents_closure(&self, package: usize) -> BTreeSet<usize> {
        let mut closure = BTreeSet::from([package]);
        let mut frontier = vec![package];
        while let Some(current) = frontier.pop() {
            for (candidate, dependent) in self.packages.iter().enumerate() {
                if !closure.contains(&candidate) && dependent.dependencies.contains(&current) {
                    closure.insert(candidate);
                    frontier.push(candidate);
                }
            }
        }
        closure
    }

    /// Whether `path` is a module of `package`'s indexed library crate, which another crate can
    /// import: the root file, or a file in the root's directory tree that is not a binary target's
    /// (`bin/`, `main.rs`).
    fn is_library_module(&self, package: usize, path: &Path) -> bool {
        let Some(root) = &self.packages[package].library_root else {
            return false;
        };
        if path == root {
            return true;
        }
        let Some(relative) = root
            .parent()
            .and_then(|source_root| path.strip_prefix(source_root).ok())
        else {
            return false;
        };
        let segments = relative
            .with_extension("")
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        path.extension().and_then(|ext| ext.to_str()) == Some("rs")
            && !segments.first().is_some_and(|first| first == "bin")
            && segments.as_slice() != ["main"]
    }
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
    /// `use` declarations naming the changed file's crate, in its package and its dependents,
    /// that the import resolver resolved to no file: an importer of the changed file may be among
    /// them.
    pub(crate) unresolved_imports: usize,
    /// Files holding such rows, past the scan cap, whose resolutions were not read.
    pub(crate) unchecked_import_files: usize,
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

/// Whether a stored `use` path is written through a crate name rather than `crate::`, `self::`
/// or `super::`: the only way code of another crate, or a test, example or binary of the same
/// package, reaches a library item.
fn through_crate_name(imported: &str) -> bool {
    !matches!(
        imported.split("::").next().map(str::trim),
        None | Some("crate" | "self" | "super" | "")
    )
}

/// Files that import a public item the changed file defines through a crate name, and files of
/// the importing packages that name such an item.
///
/// An importer is a file whose `use` row the import resolver resolved to the changed file, when
/// the path starts with a crate name: the resolver followed the dependency the importer's package
/// declares, the dependency's declared module tree, and its `pub use` re-exports, so the row names
/// the changed file exactly. A use site is weaker: an identifier equal to an imported name in
/// another file of an importing package, outside comments, string and character literals, not
/// after a `.`, and not the tail of a path that starts with another crate, where that package
/// defines no item of the same name and the file imports no item of that name from elsewhere. It
/// covers what an import row cannot show, such as a file `include!`d into the crate root that
/// imports the item, or a sibling module reaching it through `super::*`.
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
                "no indexed Cargo.toml the index read says which package compiles the file",
            ))
        }
    };
    if !workspace.is_library_module(package, &target_file.path) {
        return Ok(CrateDependents::not_measured(format!(
            "the file is not a module of an indexed library crate of {} (a binary, test, bench, example or build target, or a package with no library), which other crates cannot import; a library module it is included into is not traced",
            workspace.package_label(package)
        )));
    }
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
    let imports = store.imports()?;
    let mut imports_by_file = HashMap::<&FileId, Vec<&Import>>::new();
    for import in &imports {
        if rust_files.contains_key(&import.file_id) {
            imports_by_file
                .entry(&import.file_id)
                .or_default()
                .push(import);
        }
    }
    let target_path = target_file.path.to_string_lossy().replace('\\', "/");
    let resolutions = store.analysis_facts_targeting(
        &target_path,
        Some(EvidenceSourceType::StaticAnalysis),
        usize::MAX,
    )?;

    // Importing file -> (import rows naming the changed file, the item names they import, the
    // crate names they write, the importer's package).
    let mut importers =
        BTreeMap::<&Path, (Vec<&Import>, BTreeSet<String>, BTreeSet<String>, usize)>::new();
    for fact in resolutions.iter().filter(|fact| {
        fact.edge_type == GraphEdgeType::Imports
            && fact.target_kind == GraphNodeType::File
            && fact
                .source
                .starts_with(RUST_IMPORT_RESOLUTION_SOURCE_PREFIX)
            && fact.file_id != target_file.id
    }) {
        let Some((file, Membership::Package(importer_package))) = rust_files.get(&fact.file_id)
        else {
            continue;
        };
        let Some(line) = fact.range.as_ref().map(|range| range.start) else {
            continue;
        };
        // One `use` declaration stores a row per path it imports, all on its line; the row this
        // resolution answered is the one naming an item the changed file defines, or any of them
        // for a path naming the module itself.
        let on_line = imports_by_file
            .get(&fact.file_id)
            .into_iter()
            .flatten()
            .copied()
            .filter(|import| {
                import.range.as_ref().map(|range| range.start) == Some(line)
                    && through_crate_name(&import.imported)
            })
            .collect::<Vec<_>>();
        let named = on_line
            .iter()
            .copied()
            .filter(|import| {
                import
                    .imported
                    .rsplit("::")
                    .next()
                    .is_some_and(|last| items.contains(last.trim()))
            })
            .collect::<Vec<_>>();
        let rows = if named.is_empty() { on_line } else { named };
        if rows.is_empty() {
            continue;
        }
        let entry = importers.entry(file.path.as_path()).or_insert_with(|| {
            (
                Vec::new(),
                BTreeSet::new(),
                BTreeSet::new(),
                *importer_package,
            )
        });
        for import in rows {
            let mut segments = import.imported.split("::").map(str::trim);
            if let Some(head) = segments.next() {
                entry.2.insert(head.to_string());
            }
            if let Some(last) = import.imported.rsplit("::").next().map(str::trim) {
                if items.contains(last) {
                    entry.1.insert(last.to_string());
                }
            }
            if !entry.0.iter().any(|row| std::ptr::eq(*row, import)) {
                entry.0.push(import);
            }
        }
    }

    let mut dependents = CrateDependents::default();
    // Per importing package: the imported names, and the crate names it writes for the changed
    // file's crate.
    let mut names_by_package = BTreeMap::<usize, (BTreeSet<String>, BTreeSet<String>)>::new();
    for (path, (rows, names, heads, importer_package)) in &importers {
        if *importer_package != package {
            let entry = names_by_package.entry(*importer_package).or_default();
            entry.0.extend(names.iter().cloned());
            entry.1.extend(heads.iter().cloned());
        }
        let crate_name = heads
            .iter()
            .next()
            .map(String::as_str)
            .unwrap_or(workspace.packages[package].crate_name.as_str());
        dependents
            .results
            .push(crate_import_result(path, rows, crate_name, target_file));
    }
    dependents.importing_files = importers.len();

    let mut scanned = 0usize;
    for (dependent_package, (names, heads)) in &names_by_package {
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
                    // `other_crate::Name` is another crate's item; only a path through a name
                    // this package gives the dependency reaches the changed file.
                    if identifier
                        .path_head
                        .is_some_and(|head| !heads.contains(head))
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
                let crate_name = heads
                    .iter()
                    .next()
                    .map(String::as_str)
                    .unwrap_or(workspace.packages[package].crate_name.as_str());
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
        .map(|(_, _, _, importer_package)| *importer_package)
        .collect::<BTreeSet<_>>()
        .len();
    count_unresolved_imports(
        store,
        workspace,
        package,
        &rust_files,
        &imports_by_file,
        &importers,
        &mut dependents,
    )?;
    Ok(dependents)
}

/// Counts the `use` rows, in files of the changed file's package and of the packages declaring a
/// dependency on it, that start with a crate name those packages write for it and that the import
/// resolver did not resolve to any file. The resolver leaves a path unresolved when the declared
/// module tree cannot follow it (a module or item a macro generates, a re-export from an inline
/// module, a library root outside `src/`), and an importer of the changed file may be among them,
/// so they are reported instead of read as absence.
fn count_unresolved_imports(
    store: &dyn MetadataStore,
    workspace: &CargoWorkspace,
    package: usize,
    rust_files: &HashMap<&FileId, (&File, Membership)>,
    imports_by_file: &HashMap<&FileId, Vec<&Import>>,
    importers: &BTreeMap<&Path, (Vec<&Import>, BTreeSet<String>, BTreeSet<String>, usize)>,
    dependents: &mut CrateDependents,
) -> Result<()> {
    let mut crate_names = BTreeSet::from([workspace.packages[package].crate_name.clone()]);
    for (_, _, heads, _) in importers.values() {
        crate_names.extend(heads.iter().cloned());
    }
    let mut files = imports_by_file
        .iter()
        .filter_map(|(file_id, rows)| {
            let (file, membership) = rust_files.get(file_id)?;
            let Membership::Package(owner) = membership else {
                return None;
            };
            let declares =
                *owner == package || workspace.packages[*owner].dependencies.contains(&package);
            let lines = rows
                .iter()
                .filter(|row| {
                    row.imported
                        .split("::")
                        .next()
                        .is_some_and(|head| crate_names.contains(head.trim()))
                })
                .filter_map(|row| row.range.as_ref().map(|range| range.start))
                .collect::<BTreeSet<_>>();
            (declares && !lines.is_empty()).then_some((*file, lines))
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.0.path.cmp(&right.0.path));
    for (index, (file, lines)) in files.into_iter().enumerate() {
        if index >= MAX_CRATE_USE_SCAN_FILES {
            dependents.unchecked_import_files += 1;
            continue;
        }
        let resolved = store
            .analysis_facts_for_file(
                &file.id,
                Some(EvidenceSourceType::StaticAnalysis),
                usize::MAX,
            )?
            .into_iter()
            .filter(|fact| {
                fact.edge_type == GraphEdgeType::Imports
                    && fact.target_kind == GraphNodeType::File
                    && fact
                        .source
                        .starts_with(RUST_IMPORT_RESOLUTION_SOURCE_PREFIX)
            })
            .filter_map(|fact| fact.range.map(|range| range.start))
            .collect::<BTreeSet<_>>();
        dependents.unresolved_imports += lines.difference(&resolved).count();
    }
    Ok(())
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
            "stored `use` rows written through a crate name that the import resolver resolved to the changed file",
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

    fn package(crate_name: &str, dir: &str, dependencies: &[usize]) -> CargoPackage {
        CargoPackage {
            crate_name: crate_name.into(),
            dir: PathBuf::from(dir),
            library_root: Some(PathBuf::from(dir).join("src/lib.rs")),
            proc_macro: false,
            dependencies: dependencies.iter().copied().collect(),
        }
    }

    #[test]
    fn membership_is_unknown_unless_a_manifest_proves_it() {
        let mut workspace = CargoWorkspace::default();
        workspace
            .packages
            .push(package("engine", "crates/engine", &[]));
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
                members: vec!["crates/".into(), "tools/gen/".into()],
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
    fn a_root_package_does_not_claim_its_workspace_members_files() {
        let mut workspace = CargoWorkspace::default();
        workspace.packages.push(package("root", "", &[]));
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
    fn dependents_follow_declared_manifests_not_package_names() {
        // Two packages whose library is `engine`, in two workspaces: `app` depends on the first
        // only, `cli` on `app`, and `tool` on the second.
        let mut workspace = CargoWorkspace::default();
        workspace.packages.extend([
            package("engine", "crates/engine", &[]),
            package("engine", "other/engine", &[]),
            package("app", "crates/app", &[0]),
            package("cli", "crates/cli", &[2]),
            package("tool", "other/tool", &[1]),
        ]);
        assert_eq!(workspace.dependents_closure(0), BTreeSet::from([0, 2, 3]));
        assert_eq!(workspace.dependents_closure(1), BTreeSet::from([1, 4]));
        assert_eq!(workspace.package_label(1), "`engine` (`other/engine`)");
    }

    #[test]
    fn only_a_module_of_the_indexed_library_is_importable() {
        let mut workspace = CargoWorkspace::default();
        workspace
            .packages
            .push(package("engine", "crates/engine", &[]));
        let mut binaries_only = package("tool", "crates/tool", &[]);
        binaries_only.library_root = None;
        workspace.packages.push(binaries_only);
        let library = |index, path: &str| workspace.is_library_module(index, Path::new(path));
        assert!(library(0, "crates/engine/src/lib.rs"));
        assert!(library(0, "crates/engine/src/plan/mod.rs"));
        assert!(!library(0, "crates/engine/src/main.rs"));
        assert!(!library(0, "crates/engine/src/bin/tool.rs"));
        assert!(!library(0, "crates/engine/tests/smoke.rs"));
        assert!(!library(1, "crates/tool/src/lib.rs"));
        assert!(through_crate_name("engine::plan::Plan"));
        assert!(!through_crate_name("crate::plan::Plan"));
        assert!(!through_crate_name("super::Plan"));
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
}
