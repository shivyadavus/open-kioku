//! C# `using` directives, placed through the .NET project model (#681).
//!
//! A `using` directive names a namespace, and a namespace is no file: `Acme.Ledger` may be
//! declared by any file of any project. The files a directive leads to are therefore those that
//! declare the types the importing file names through it, which only C#'s own name lookup can
//! tell. For each type name the parser read ([`CSharpNameFacts`]), this follows that lookup: out
//! from the innermost namespace the name is written in, the namespaces and types of each
//! enclosing namespace come before the directives of its declaration, and the compilation unit's
//! directives come last, the project's `global using` directives among them (any file's, the
//! project file's `<Using>` items and the SDK's implicit ones). A type is a candidate only when
//! its project is the importing file's or one it references, and its accessibility lets the
//! importing project name it.
//!
//! What comes out:
//! - an `IMPORTS` fact from the importing file to each file declaring a type it names through a
//!   directive, every part of a partial type included. It is high confidence, which the graph
//!   proves as an import binding, only where the lookup is settled: the name stands where only a
//!   type can, no nearer directive imports a namespace from outside the repository (whose types
//!   the index cannot see), no error node may declare the name nearer, the type was read whole,
//!   no syntax error of the file stands where a directive may, and the file belongs to project
//!   files the index read, which agree on the answer. A name written only as a member-access receiver
//!   (`Rules.Apply()`) may be a member inherited from a base type outside the repository, so it
//!   proves no edge.
//! - an ambiguous `IMPORTS` fact, and no edge, when imported namespaces declare the name twice;
//! - a `DEPENDS_ON` package fact for a directive naming a namespace no project of the
//!   repository declares (`System.*` as the standard library, anything else as a package), and an
//!   unresolved fact for one only projects the importer does not reference declare;
//! - an import binding per name, so the symbol registry never matches a name that only a
//!   namespace from outside the repository can supply to a repository symbol of that name.
//!
//! Not read: types a `using static` directive brings in as the static type's nested types, and
//! namespaces an extern alias names, which are taken to be outside the repository.

use crate::resolver::analysis_fact_for_import_resolution;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use open_kioku_core::csharp::{
    CSharpNameContext, CSharpNameFacts, CSharpNameUse, CSharpTypeDeclaration, CSharpUsing,
    CSharpUsingKind,
};
use open_kioku_core::{
    identity, AnalysisFact, Confidence, EvidenceSourceType, File, FileId, GraphEdgeType,
    GraphNodeType, Import, ImportResolution, Language, LineRange, QualityNote, QualityNoteKind,
    ResolutionStatus, ScopeId, Symbol, SymbolId, SymbolKind, Visibility,
};
use open_kioku_semantic_model::{
    DotnetModel, DotnetProject, ImportBinding, ImportBindingRule, ImportOrigin,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Prefix of every fact this resolver writes, shared with the generic import resolver: the
/// graph proves a high-confidence file `IMPORTS` fact of either as an import binding.
const SOURCE_PREFIX: &str = "open-kioku-import-resolver/";

/// The namespaces `<ImplicitUsings>enable</ImplicitUsings>` makes `global using` directives of
/// every file of a `Microsoft.NET.Sdk` project, and of every other SDK, which adds its own.
const SDK_IMPLICIT_USINGS: [&str; 7] = [
    "System",
    "System.Collections.Generic",
    "System.IO",
    "System.Linq",
    "System.Net.Http",
    "System.Threading",
    "System.Threading.Tasks",
];
/// What `Microsoft.NET.Sdk.Web` adds.
const WEB_IMPLICIT_USINGS: [&str; 9] = [
    "System.Net.Http.Json",
    "Microsoft.AspNetCore.Builder",
    "Microsoft.AspNetCore.Hosting",
    "Microsoft.AspNetCore.Http",
    "Microsoft.AspNetCore.Routing",
    "Microsoft.Extensions.Configuration",
    "Microsoft.Extensions.DependencyInjection",
    "Microsoft.Extensions.Hosting",
    "Microsoft.Extensions.Logging",
];
/// What `Microsoft.NET.Sdk.Worker` adds.
const WORKER_IMPLICIT_USINGS: [&str; 4] = [
    "Microsoft.Extensions.Configuration",
    "Microsoft.Extensions.DependencyInjection",
    "Microsoft.Extensions.Hosting",
    "Microsoft.Extensions.Logging",
];

/// Most files and examples a quality note names.
const NOTE_EXAMPLES: usize = 5;
/// Most enclosing and base types one shadowing check reads, against a cyclic base list.
const MAX_SHADOW_TYPES: usize = 64;

/// What the C# pass adds to the import resolver's report.
#[derive(Debug, Default)]
pub(crate) struct CSharpImportReport {
    pub(crate) resolutions: Vec<ImportResolution>,
    pub(crate) analysis_facts: Vec<AnalysisFact>,
    pub(crate) quality_notes: Vec<QualityNote>,
    /// Import bindings for the semantic repository: each type name a C# file writes, bound where
    /// the lookup placed it, and each alias.
    pub(crate) bindings: Vec<ImportBinding>,
}

/// Places the `using` directives and type names of every C# file of `files`. `names` holds each
/// C# file's parsed name facts; `imports` the stored import rows, which the resolutions name.
pub(crate) fn resolve_csharp_imports(
    model: &DotnetModel,
    files: &[File],
    symbols: &[Symbol],
    names: &HashMap<FileId, CSharpNameFacts>,
    imports: &[Import],
) -> CSharpImportReport {
    let csharp_files = files
        .iter()
        .filter(|file| file.language == Language::CSharp && names.contains_key(&file.id))
        .collect::<Vec<_>>();
    if csharp_files.is_empty() {
        return CSharpImportReport::default();
    }
    let index = Index::build(model, &csharp_files, symbols, names);
    let rows = imports
        .iter()
        .filter_map(|import| Some(((&import.file_id, import.range.as_ref()?.start), import)))
        .collect::<HashMap<_, _>>();
    let mut report = CSharpImportReport::default();
    let mut ambiguous_names = 0usize;
    for file in &csharp_files {
        let Some(facts) = names.get(&file.id) else {
            continue;
        };
        let resolved = index.resolve_file(file, facts);
        ambiguous_names += resolved.ambiguous_names();
        resolved.write(file, facts, &index, &rows, &mut report);
    }
    report.quality_notes.extend(index.notes(ambiguous_names));
    report.analysis_facts.sort_by(|a, b| a.id.cmp(&b.id));
    report.analysis_facts.dedup_by(|a, b| a.id == b.id);
    report.quality_notes.sort();
    report.quality_notes.dedup();
    report
}

/// The project file each C# file of `files` is compiled into, by the projects' default glob and
/// `Compile` items (see [`file_owners`]): `Some` for a file one project compiles, `None` for a
/// file several compile (a linked or shared file). A file no project compiles is left out.
///
/// A `partial` type's parts are one type only within one assembly, so this is what groups them
/// (`open_kioku_languages::csharp::unify_partial_type_visibility`), wherever the files sit.
pub(crate) fn csharp_file_projects(
    model: &DotnetModel,
    files: &[File],
) -> HashMap<FileId, Option<PathBuf>> {
    if model.projects.is_empty() {
        return HashMap::new();
    }
    let projects = read_projects(model);
    files
        .iter()
        .filter(|file| file.language == Language::CSharp)
        .filter_map(|file| {
            let project = match file_owners(&projects, &slash(&file.path)).as_slice() {
                [] => return None,
                [only] => Some(projects[*only].model.project_file.clone()),
                _ => None,
            };
            Some((file.id.clone(), project))
        })
        .collect()
}

/// The model's project files, with their `Compile` patterns read; references, friends and
/// `global using` directives are filled in by [`Index::build`].
fn read_projects(model: &DotnetModel) -> Vec<Project<'_>> {
    model
        .projects
        .iter()
        .map(|project| Project {
            model: project,
            dir: slash(project.project_file.parent().unwrap_or(Path::new(""))),
            include: glob_set(&project.compile_include),
            remove: glob_set(&project.compile_remove),
            conditional_compile: glob_set(
                &[
                    project.conditional_compile_include.as_slice(),
                    project.conditional_compile_remove.as_slice(),
                ]
                .concat(),
            ),
            friends: project.internals_visible_to.iter().cloned().collect(),
            visible: BTreeSet::new(),
            conditionally_visible: BTreeSet::new(),
            globals: project_file_globals(project),
        })
        .collect()
}

/// One project file, with what file ownership and visibility need.
struct Project<'m> {
    model: &'m DotnetProject,
    /// Repository-relative directory of the project file, `""` at the root.
    dir: String,
    include: Option<GlobSet>,
    remove: Option<GlobSet>,
    /// `Compile` patterns a `Condition` guards, included or removed.
    conditional_compile: Option<GlobSet>,
    /// Assemblies that may name the project's `internal` types.
    friends: HashSet<String>,
    /// The projects whose types the project's files can name: itself and those it references.
    visible: BTreeSet<usize>,
    /// Those of `visible` it reaches only through a `ProjectReference` a `Condition` guards.
    conditionally_visible: BTreeSet<usize>,
    /// The project's `global using` directives, from every file, the project file and the SDK.
    globals: Vec<GlobalUsing>,
}

/// A `global using` directive, wherever it is declared.
#[derive(Debug, Clone)]
struct GlobalUsing {
    kind: CSharpUsingKind,
    target: String,
    arity: u16,
    alias: Option<String>,
    extern_alias: bool,
    origin: GlobalOrigin,
    /// Under `#if`, or a `<Using>` item a `Condition` guards.
    conditional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GlobalOrigin {
    /// A `global using` line of a C# file.
    File { file: FileId, line: u32 },
    /// A `<Using>` item of the project file.
    ProjectFile,
    /// The SDK's implicit usings.
    Implicit,
}

/// The parts of one type: a partial type declared in several files of one project is one type.
struct TypeGroup {
    owner: Option<usize>,
    /// `Acme::Ledger::Entry`, as the parser qualifies it.
    qualified: String,
    /// `Acme.Ledger`; empty for the global namespace.
    namespace: String,
    parts: Vec<(SymbolId, FileId)>,
    visibility: Visibility,
    /// Why the parts may not be the type as the compiler sees it, when they may not: a part read
    /// through syntax errors or by the pattern fallback, or declarations of one name that are
    /// not all `partial`, of which a build configuration picks one. An edge to such a type is
    /// never proven.
    inexact: Option<&'static str>,
}

impl TypeGroup {
    fn files(&self) -> BTreeSet<&FileId> {
        self.parts.iter().map(|(_, file)| file).collect()
    }
}

/// Whether the importer sees a namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamespaceState {
    /// Declared by a project the importer can name types of.
    Visible,
    /// Declared in the repository, only by projects the importer does not reference.
    Hidden,
    /// Declared nowhere in the repository.
    Outside,
}

/// The names that an enclosing type takes before any namespace: its nested types, and for a
/// receiver its members, its own and those of its repository base types.
#[derive(Debug, Default)]
struct ShadowNames {
    types: HashSet<(String, u16)>,
    /// Nested types whose arity no declaration the parser read says: they may be of any.
    types_of_any_arity: HashSet<String>,
    members: HashSet<String>,
}

/// Shadowing names by enclosing type group and importing project.
type ShadowCache = RefCell<HashMap<(usize, Option<usize>), Rc<ShadowNames>>>;

struct Index<'a> {
    projects: Vec<Project<'a>>,
    groups: Vec<TypeGroup>,
    /// Top-level type groups by namespace, name and arity.
    by_key: HashMap<(String, String, u16), Vec<usize>>,
    /// Top-level type groups of unknown arity by namespace and name: they may be of any.
    unknown_arity: HashMap<(String, String), Vec<usize>>,
    /// The namespaces and type names some file's error node may hide a declaration of.
    hidden: HashSet<(String, String)>,
    group_of_symbol: HashMap<SymbolId, usize>,
    /// Every namespace a C# file declares or a type is in, each prefix included, with the owners
    /// of the files declaring it.
    namespaces: HashMap<String, BTreeSet<Option<usize>>>,
    owners: HashMap<FileId, Vec<usize>>,
    files: HashMap<FileId, &'a File>,
    names: &'a HashMap<FileId, CSharpNameFacts>,
    symbols: HashMap<&'a SymbolId, &'a Symbol>,
    children: HashMap<&'a SymbolId, Vec<&'a Symbol>>,
    declarations: HashMap<&'a SymbolId, &'a CSharpTypeDeclaration>,
    /// C# files that belong to no project file the index read.
    unowned: Vec<&'a File>,
    /// C# files a `Compile` item a `Condition` guards includes or removes.
    conditionally_owned: HashSet<FileId>,
    /// Shadowing names by enclosing type group and importing project, computed once.
    shadows: ShadowCache,
}

impl<'a> Index<'a> {
    fn build(
        model: &'a DotnetModel,
        files: &[&'a File],
        symbols: &'a [Symbol],
        names: &'a HashMap<FileId, CSharpNameFacts>,
    ) -> Self {
        let mut projects = read_projects(model);
        let by_path = projects
            .iter()
            .enumerate()
            .map(|(index, project)| (project.model.project_file.as_path(), index))
            .collect::<HashMap<_, _>>();
        let visible = (0..projects.len())
            .map(|index| {
                let all = visible_projects(&projects, &by_path, index, true);
                let firm = visible_projects(&projects, &by_path, index, false);
                let conditional = all.difference(&firm).copied().collect::<BTreeSet<_>>();
                (all, conditional)
            })
            .collect::<Vec<_>>();
        for (project, (visible, conditional)) in projects.iter_mut().zip(visible) {
            project.visible = visible;
            project.conditionally_visible = conditional;
        }

        let mut owners = HashMap::new();
        let mut unowned = Vec::new();
        let mut conditionally_owned = HashSet::new();
        for file in files {
            let path = slash(&file.path);
            let found = file_owners(&projects, &path);
            if projects.iter().any(|project| {
                project
                    .conditional_compile
                    .as_ref()
                    .is_some_and(|patterns| patterns.is_match(&path))
            }) {
                conditionally_owned.insert(file.id.clone());
            }
            if found.is_empty() {
                unowned.push(*file);
            } else {
                owners.insert(file.id.clone(), found);
            }
        }
        // The friends and `global using` directives a project's files declare.
        for file in files {
            let Some(facts) = names.get(&file.id) else {
                continue;
            };
            for &owner in owners.get(&file.id).into_iter().flatten() {
                let project = &mut projects[owner];
                project
                    .friends
                    .extend(facts.internals_visible_to.iter().cloned());
                project.globals.extend(file_globals(file, facts));
            }
        }

        let symbol_map = symbols
            .iter()
            .filter(|symbol| symbol.language == Language::CSharp)
            .map(|symbol| (&symbol.id, symbol))
            .collect::<HashMap<_, _>>();
        let mut children = HashMap::<&SymbolId, Vec<&Symbol>>::new();
        for symbol in symbol_map.values() {
            if let Some(parent) = &symbol.parent_symbol_id {
                children.entry(parent).or_default().push(symbol);
            }
        }
        let declarations = names
            .values()
            .flat_map(|facts| facts.types.iter())
            .map(|declared| (&declared.symbol_id, declared))
            .collect::<HashMap<_, _>>();

        let mut index = Index {
            projects,
            groups: Vec::new(),
            by_key: HashMap::new(),
            unknown_arity: HashMap::new(),
            hidden: names
                .values()
                .flat_map(|facts| facts.hidden_types.iter())
                .map(|hidden| (hidden.namespace.clone(), hidden.name.clone()))
                .collect(),
            group_of_symbol: HashMap::new(),
            namespaces: HashMap::new(),
            owners,
            files: files.iter().map(|file| (file.id.clone(), *file)).collect(),
            names,
            symbols: symbol_map,
            children,
            declarations,
            unowned,
            conditionally_owned,
            shadows: RefCell::new(HashMap::new()),
        };
        index.index_types(symbols);
        index.index_namespaces(files);
        index
    }

    /// The owners a file's declarations belong to: its projects, or `None` for a file of none.
    fn file_owner_keys(&self, file: &FileId) -> Vec<Option<usize>> {
        match self.owners.get(file) {
            Some(owners) => owners.iter().copied().map(Some).collect(),
            None => vec![None],
        }
    }

    fn index_types(&mut self, symbols: &'a [Symbol]) {
        let types = symbols
            .iter()
            .filter(|symbol| {
                symbol.language == Language::CSharp
                    && matches!(symbol.kind, SymbolKind::Class | SymbolKind::Interface)
            })
            .collect::<Vec<_>>();
        // A part is one type with the others of its qualified name and arity in one project. A
        // symbol whose declaration the parser did not read (the pattern fallback's) has no known
        // arity: it joins every type of its qualified name, or stands alone with none.
        let mut groups = BTreeMap::<(Option<usize>, String, Option<u16>), TypeGroup>::new();
        let mut unread = Vec::new();
        for symbol in &types {
            let Some(arity) = self
                .declarations
                .get(&symbol.id)
                .map(|declared| declared.arity)
            else {
                unread.push(*symbol);
                continue;
            };
            for owner in self.file_owner_keys(&symbol.file_id) {
                add_part(&mut groups, owner, symbol, Some(arity));
            }
        }
        for symbol in unread {
            for owner in self.file_owner_keys(&symbol.file_id) {
                let joined = groups
                    .iter_mut()
                    .filter(|((group_owner, qualified, _), _)| {
                        *group_owner == owner && *qualified == symbol.qualified_name
                    })
                    .map(|(_, group)| join_part(group, symbol))
                    .count();
                if joined == 0 {
                    add_part(&mut groups, owner, symbol, None);
                }
            }
        }
        for ((_, _, arity), mut group) in groups {
            group.inexact = group.inexact.or_else(|| self.inexact_parts(&group));
            let id = self.groups.len();
            for (symbol, _) in &group.parts {
                self.group_of_symbol.entry(symbol.clone()).or_insert(id);
            }
            // Only a type directly in a namespace is a member of it; a nested type is reached
            // through its enclosing type.
            let top_level = group.parts.iter().all(|(symbol, _)| {
                self.symbols
                    .get(symbol)
                    .and_then(|symbol| symbol.parent_symbol_id.as_ref())
                    .and_then(|parent| self.symbols.get(parent))
                    .is_none_or(|parent| parent.kind == SymbolKind::Package)
            });
            if top_level {
                let name = group
                    .qualified
                    .rsplit("::")
                    .next()
                    .unwrap_or(&group.qualified)
                    .to_string();
                match arity {
                    Some(arity) => self
                        .by_key
                        .entry((group.namespace.clone(), name, arity))
                        .or_default()
                        .push(id),
                    None => self
                        .unknown_arity
                        .entry((group.namespace.clone(), name))
                        .or_default()
                        .push(id),
                }
            }
            self.groups.push(group);
        }
    }

    /// Why a type's parts may not be the type the compiler sees: see [`TypeGroup::inexact`].
    fn inexact_parts(&self, group: &TypeGroup) -> Option<&'static str> {
        let parts = group
            .parts
            .iter()
            .filter_map(|(symbol, _)| self.symbols.get(symbol).copied())
            .collect::<Vec<&Symbol>>();
        if parts.iter().any(|symbol| {
            symbol.confidence != Confidence::High
                || symbol.provenance != EvidenceSourceType::TreeSitter
        }) {
            return Some(
                "the type is declared in a file the parser read through syntax errors or by its pattern fallback",
            );
        }
        let partial = |symbol: &&Symbol| {
            symbol
                .signature
                .as_deref()
                .is_some_and(|signature| signature.split_whitespace().any(|word| word == "partial"))
        };
        if parts.len() > 1 && !parts.iter().all(partial) {
            return Some(
                "the type is declared more than once without `partial`, so a build configuration picks one",
            );
        }
        None
    }

    fn index_namespaces(&mut self, files: &[&'a File]) {
        let mut declared = self
            .groups
            .iter()
            .map(|group| (group.namespace.clone(), group.owner))
            .collect::<Vec<_>>();
        for file in files {
            let owners = self.file_owner_keys(&file.id);
            let blocks = self
                .names
                .get(&file.id)
                .map(|facts| facts.blocks.as_slice());
            for block in blocks.unwrap_or_default() {
                for owner in &owners {
                    declared.push((block.namespace.clone(), *owner));
                }
            }
        }
        for (namespace, owner) in declared {
            let mut current = namespace.as_str();
            while !current.is_empty() {
                self.namespaces
                    .entry(current.to_string())
                    .or_default()
                    .insert(owner);
                current = current.rsplit_once('.').map_or("", |(outer, _)| outer);
            }
        }
    }

    fn namespace_state(&self, namespace: &str, importer: Option<usize>) -> NamespaceState {
        let Some(owners) = self.namespaces.get(namespace) else {
            return NamespaceState::Outside;
        };
        let visible = match importer {
            None => true,
            Some(project) => owners.iter().any(|owner| {
                owner.is_some_and(|owner| self.projects[project].visible.contains(&owner))
            }),
        };
        if visible {
            NamespaceState::Visible
        } else {
            NamespaceState::Hidden
        }
    }

    /// Whether code of `file`, compiled by `importer`, can name the type `group`.
    fn accessible(&self, group: &TypeGroup, importer: Option<usize>, file: &FileId) -> bool {
        if group.visibility == Visibility::Private {
            // A `file` type: only its own file names it.
            return group.parts.iter().any(|(_, part)| part == file);
        }
        let Some(project) = importer else {
            // A file of no project: nothing says what it compiles against.
            return true;
        };
        // A file of no project is compiled into none.
        let Some(owner) = group.owner else {
            return false;
        };
        if !self.projects[project].visible.contains(&owner) {
            return false;
        }
        owner == project
            || matches!(group.visibility, Visibility::Public | Visibility::Protected)
            || self.projects[owner]
                .friends
                .contains(&self.projects[project].model.assembly_name)
    }

    /// The accessible top-level types `name` with `arity` in `namespace`.
    fn types_in(
        &self,
        namespace: &str,
        name: &str,
        arity: u16,
        importer: Option<usize>,
        file: &FileId,
    ) -> Vec<usize> {
        let any_arity = self
            .unknown_arity
            .get(&(namespace.to_string(), name.to_string()));
        self.by_key
            .get(&(namespace.to_string(), name.to_string(), arity))
            .into_iter()
            .chain(any_arity)
            .flatten()
            .copied()
            .filter(|&group| self.accessible(&self.groups[group], importer, file))
            .collect()
    }

    fn resolve_file(&self, file: &File, facts: &CSharpNameFacts) -> ResolvedFile {
        let owners = self.file_owner_keys(&file.id);
        let runs = owners
            .iter()
            .map(|&owner| Lookup::new(self, file, facts, owner).run())
            .collect::<Vec<_>>();
        let mut resolved = ResolvedFile::merge(runs, &owners, facts.hidden_directives);
        resolved.conditionally_owned = self.conditionally_owned.contains(&file.id);
        resolved
    }

    /// The names the type `group` and the types enclosing it take before any namespace, as
    /// code compiled by `importer` sees them.
    fn shadow_names(&self, group: usize, importer: Option<usize>) -> Rc<ShadowNames> {
        if let Some(known) = self.shadows.borrow().get(&(group, importer)) {
            return known.clone();
        }
        let mut names = ShadowNames::default();
        let mut seen = HashSet::new();
        let mut pending = vec![group];
        while let Some(current) = pending.pop() {
            if !seen.insert(current) || seen.len() > MAX_SHADOW_TYPES {
                continue;
            }
            for (part, _) in &self.groups[current].parts {
                for child in self.children.get(part).into_iter().flatten() {
                    if matches!(child.kind, SymbolKind::Class | SymbolKind::Interface) {
                        match self.declarations.get(&child.id) {
                            Some(declared) => {
                                names.types.insert((child.name.clone(), declared.arity));
                            }
                            None => {
                                names.types_of_any_arity.insert(child.name.clone());
                            }
                        }
                    }
                    names.members.insert(child.name.clone());
                }
                // The type enclosing this one, then the repository types its base list names.
                if let Some(parent) = self
                    .symbols
                    .get(part)
                    .and_then(|symbol| symbol.parent_symbol_id.as_ref())
                    .and_then(|parent| self.group_of_symbol.get(parent))
                {
                    pending.push(*parent);
                }
                pending.extend(self.base_groups(part, importer));
            }
        }
        let names = Rc::new(names);
        self.shadows
            .borrow_mut()
            .insert((group, importer), names.clone());
        names
    }

    /// The repository types the base list of the type part `part` names, read in its own file.
    fn base_groups(&self, part: &SymbolId, importer: Option<usize>) -> Vec<usize> {
        let Some(declared) = self.declarations.get(part) else {
            return Vec::new();
        };
        if declared.bases.is_empty() {
            return Vec::new();
        }
        let Some((file, facts)) = self
            .symbols
            .get(part)
            .and_then(|symbol| self.files.get(&symbol.file_id))
            .and_then(|file| Some((*file, self.names.get(&file.id)?)))
        else {
            return Vec::new();
        };
        let lookup = Lookup::new(self, file, facts, importer);
        declared
            .bases
            .iter()
            .filter_map(|base| lookup.base_group(base))
            .collect()
    }

    fn notes(&self, ambiguous_names: usize) -> Vec<QualityNote> {
        let mut notes = Vec::new();
        let note =
            |message: String| QualityNote::new(QualityNoteKind::ImportResolverCaveat, message);
        let examples = |items: &[String]| {
            items
                .iter()
                .take(NOTE_EXAMPLES)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        if !self.unowned.is_empty() {
            let unowned = self
                .unowned
                .iter()
                .map(|file| file.path.display().to_string())
                .collect::<Vec<_>>();
            notes.push(note(if self.projects.is_empty() {
                format!(
                    "C# imports: no .csproj project file was found, so the `using` edges of {} C# files are unproven (e.g. {})",
                    unowned.len(),
                    examples(&unowned)
                )
            } else {
                format!(
                    "C# imports: {} C# files belong to no project file the index read, so their `using` edges are unproven (e.g. {})",
                    unowned.len(),
                    examples(&unowned)
                )
            }));
        }
        let unreadable = self
            .projects
            .iter()
            .filter(|project| project.model.unreadable)
            .map(|project| project.model.project_file.display().to_string())
            .collect::<Vec<_>>();
        if !unreadable.is_empty() {
            notes.push(note(format!(
                "C# imports: {} project files are not XML the index can read, and compile no file it knows of: {}",
                unreadable.len(),
                examples(&unreadable)
            )));
        }
        let unresolved = self
            .projects
            .iter()
            .flat_map(|project| {
                project
                    .model
                    .unresolved_project_references
                    .iter()
                    .map(move |reference| {
                        format!("{} -> {reference}", project.model.project_file.display())
                    })
            })
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            notes.push(note(format!(
                "C# imports: {} ProjectReference items name no project file the index found, so no type they provide is visible: {}",
                unresolved.len(),
                examples(&unresolved)
            )));
        }
        if ambiguous_names > 0 {
            notes.push(note(format!(
                "C# imports: {ambiguous_names} type names a file writes are declared in more than one namespace it imports; no edge is emitted for them"
            )));
        }
        notes
    }
}

/// Adds `symbol` as a part of the type `(owner, qualified name, arity)`.
fn add_part(
    groups: &mut BTreeMap<(Option<usize>, String, Option<u16>), TypeGroup>,
    owner: Option<usize>,
    symbol: &Symbol,
    arity: Option<u16>,
) {
    let group = groups
        .entry((owner, symbol.qualified_name.clone(), arity))
        .or_insert_with(|| TypeGroup {
            owner,
            qualified: symbol.qualified_name.clone(),
            namespace: match symbol.qualified_name.rsplit_once("::") {
                Some((outer, _)) => outer.replace("::", "."),
                None => String::new(),
            },
            parts: Vec::new(),
            visibility: Visibility::Unknown,
            inexact: None,
        });
    join_part(group, symbol);
}

fn join_part(group: &mut TypeGroup, symbol: &Symbol) {
    group
        .parts
        .push((symbol.id.clone(), symbol.file_id.clone()));
    if access_rank(symbol.visibility) > access_rank(group.visibility) {
        group.visibility = symbol.visibility;
    }
}

/// The projects whose files compile `path` (repository-relative): the deepest project whose
/// directory holds it by the SDK's default `Compile` glob, unless one of its `Compile Remove`
/// patterns takes it out, and every project whose `Compile Include` names it.
///
/// MSBuild's default glob also reaches into a project nested in another's directory; a project
/// laid out that way removes the nested one's files (or builds them twice), so only the deepest
/// project takes them by default.
fn file_owners(projects: &[Project<'_>], path: &str) -> Vec<usize> {
    let mut defaults = Vec::<(usize, usize)>::new();
    for (index, project) in projects.iter().enumerate() {
        if !project.model.default_compile_items {
            continue;
        }
        let Some(relative) = strip_dir(path, &project.dir) else {
            continue;
        };
        // The SDK leaves `bin/`, `obj/` and hidden directories out of its default items.
        let directories = relative.rsplit_once('/').map_or("", |(dir, _)| dir);
        let mut segments = directories.split('/').filter(|segment| !segment.is_empty());
        let first = segments.clone().next();
        if first.is_some_and(|first| {
            first.eq_ignore_ascii_case("bin") || first.eq_ignore_ascii_case("obj")
        }) || segments.any(|segment| segment.starts_with('.'))
        {
            continue;
        }
        let depth = if project.dir.is_empty() {
            0
        } else {
            project.dir.split('/').count()
        };
        defaults.push((index, depth));
    }
    let deepest = defaults.iter().map(|(_, depth)| *depth).max();
    let mut owners = defaults
        .into_iter()
        .filter(|(_, depth)| Some(*depth) == deepest)
        .map(|(index, _)| index)
        .filter(|&index| {
            !projects[index]
                .remove
                .as_ref()
                .is_some_and(|remove| remove.is_match(path))
        })
        .collect::<Vec<_>>();
    for (index, project) in projects.iter().enumerate() {
        if project
            .include
            .as_ref()
            .is_some_and(|include| include.is_match(path))
            && !owners.contains(&index)
        {
            owners.push(index);
        }
    }
    owners.sort_unstable();
    owners
}

/// `path` relative to `dir` (both repository-relative), when `dir` holds it.
fn strip_dir<'p>(path: &'p str, dir: &str) -> Option<&'p str> {
    if dir.is_empty() {
        return Some(path);
    }
    path.strip_prefix(dir)?.strip_prefix('/')
}

/// The projects `index` can name types of: itself and the projects it references, and with
/// transitive references (an SDK-style project's default) everything those reference in turn.
fn visible_projects(
    projects: &[Project<'_>],
    by_path: &HashMap<&Path, usize>,
    index: usize,
    conditional: bool,
) -> BTreeSet<usize> {
    let transitive = projects[index].model.transitive_project_references;
    let mut visible = BTreeSet::from([index]);
    let mut pending = vec![index];
    while let Some(current) = pending.pop() {
        let model = projects[current].model;
        for reference in &model.project_references {
            if !conditional && model.conditional_project_references.contains(reference) {
                continue;
            }
            let Some(&referenced) = by_path.get(reference.as_path()) else {
                continue;
            };
            if visible.insert(referenced) && transitive {
                pending.push(referenced);
            }
        }
    }
    visible
}

/// A glob set of repository-relative MSBuild patterns, matched case-insensitively as MSBuild on
/// Windows does; `None` when there are none or none compiles.
fn glob_set(patterns: &[String]) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut added = false;
    for pattern in patterns {
        if let Ok(glob) = GlobBuilder::new(pattern)
            .literal_separator(true)
            .case_insensitive(true)
            .build()
        {
            builder.add(glob);
            added = true;
        }
    }
    if !added {
        return None;
    }
    builder.build().ok()
}

/// The `global using` directives of one file.
fn file_globals(file: &File, facts: &CSharpNameFacts) -> Vec<GlobalUsing> {
    facts
        .usings
        .iter()
        .filter(|using| using.global)
        .map(|using| GlobalUsing {
            kind: using.kind,
            target: using.target.clone(),
            arity: using.arity,
            alias: using.alias.clone(),
            extern_alias: using.extern_alias.is_some(),
            origin: GlobalOrigin::File {
                file: file.id.clone(),
                line: using.line,
            },
            conditional: using.conditional,
        })
        .collect()
}

/// The `global using` directives a project file declares: with implicit usings on, its SDK's
/// defaults, then its `<Using>` items, less the namespaces `<Using Remove>` takes out.
fn project_file_globals(project: &DotnetProject) -> Vec<GlobalUsing> {
    let mut globals = Vec::new();
    if project.implicit_usings {
        let extra: &[&str] = match project.sdk.as_deref() {
            Some("Microsoft.NET.Sdk.Web") => &WEB_IMPLICIT_USINGS,
            Some("Microsoft.NET.Sdk.Worker") => &WORKER_IMPLICIT_USINGS,
            _ => &[],
        };
        for target in SDK_IMPLICIT_USINGS.iter().chain(extra) {
            globals.push(GlobalUsing {
                kind: CSharpUsingKind::Namespace,
                target: (*target).to_string(),
                arity: 0,
                alias: None,
                extern_alias: false,
                origin: GlobalOrigin::Implicit,
                conditional: false,
            });
        }
    }
    for using in &project.usings {
        globals.push(GlobalUsing {
            kind: if using.alias.is_some() {
                CSharpUsingKind::Alias
            } else if using.is_static {
                CSharpUsingKind::Static
            } else {
                CSharpUsingKind::Namespace
            },
            target: using.target.clone(),
            arity: 0,
            alias: using.alias.clone(),
            extern_alias: false,
            origin: GlobalOrigin::ProjectFile,
            conditional: using.conditional,
        });
    }
    globals
        .retain(|using| using.alias.is_some() || !project.removed_usings.contains(&using.target));
    globals
}

/// Accessibility as a rank: who can name a declaration, widest last.
fn access_rank(visibility: Visibility) -> u8 {
    match visibility {
        Visibility::Unknown => 0,
        Visibility::Private => 1,
        Visibility::Crate | Visibility::Package => 2,
        Visibility::Protected => 3,
        Visibility::Public => 4,
    }
}

/// The directive that imports a name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Via {
    /// A directive of the file's own, not `global`.
    Local { line: u32, kind: CSharpUsingKind },
    Global {
        origin: GlobalOrigin,
        kind: CSharpUsingKind,
    },
}

impl Via {
    /// The resolver strategy an edge through this directive records.
    fn strategy(&self) -> &'static str {
        match self {
            Via::Local { kind, .. } | Via::Global { kind, .. }
                if *kind == CSharpUsingKind::Alias =>
            {
                "csharp-using-alias"
            }
            Via::Local { .. } => "csharp-using-namespace",
            Via::Global {
                origin: GlobalOrigin::File { .. },
                ..
            } => "csharp-global-using",
            Via::Global {
                origin: GlobalOrigin::ProjectFile,
                ..
            } => "csharp-project-using",
            Via::Global {
                origin: GlobalOrigin::Implicit,
                ..
            } => "csharp-implicit-using",
        }
    }

    /// The line of `file` an edge through this directive is recorded at: the directive's own
    /// line when `file` writes it, else the line the name is written on.
    fn line_in(&self, file: &FileId, use_line: u32) -> u32 {
        match self {
            Via::Local { line, .. } => *line,
            Via::Global {
                origin:
                    GlobalOrigin::File {
                        file: declaring,
                        line,
                    },
                ..
            } if declaring == file => *line,
            Via::Global { .. } => use_line,
        }
    }

    fn describe(&self) -> String {
        match self {
            Via::Local { line, .. } => format!("the `using` on line {line}"),
            Via::Global {
                origin: GlobalOrigin::File { line, .. },
                ..
            } => format!("a `global using` (line {line} of its file)"),
            Via::Global {
                origin: GlobalOrigin::ProjectFile,
                ..
            } => "a `<Using>` item of its project file".into(),
            Via::Global {
                origin: GlobalOrigin::Implicit,
                ..
            } => "its SDK's implicit usings".into(),
        }
    }
}

/// What a name lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Found {
    /// A type parameter, nested type, variable or member of the same name takes it.
    Shadowed,
    /// A namespace.
    Namespace,
    /// A type of an enclosing namespace, which no directive imports.
    Declared(Option<usize>),
    /// A type a directive imports. `doubt` says why something the index cannot see may take
    /// the name first, when something may.
    Imported {
        group: usize,
        via: Via,
        doubt: Option<String>,
    },
    /// Types several directives import, of which C# would pick none.
    Ambiguous { candidates: Vec<(usize, Via)> },
    /// Only a namespace from outside the repository can supply it.
    Outside { namespace: String },
    /// Nothing of the repository or outside it the lookup can see.
    Nothing,
}

/// What an alias or `using static` directive's target names.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AliasTarget {
    Namespace(String),
    Type(Vec<usize>),
    Outside(String),
    /// Declared in the repository, but not where the importer can name it.
    Hidden,
}

/// One C# file's lookups as compiled by one project (or none).
struct Lookup<'i, 'a> {
    index: &'i Index<'a>,
    file: &'i File,
    facts: &'i CSharpNameFacts,
    owner: Option<usize>,
    /// The file's own directives (not `global`), by block.
    locals: Vec<Vec<&'i CSharpUsing>>,
    /// The `global using` directives in scope: the project's, or the file's own with no project.
    globals: Rc<[GlobalUsing]>,
    type_parameters: HashSet<&'i str>,
    variables: HashSet<&'i str>,
}

/// What one file's lookups add up to, for one project.
#[derive(Debug, Default)]
struct FileRun {
    /// Each file reached through a directive, with the best evidence for it.
    edges: BTreeMap<FileId, Edge>,
    ambiguous: Vec<AmbiguousUse>,
    /// The names bound in the file, for the registry.
    bindings: BTreeMap<String, NameBinding>,
    /// The file's own directives, placed, by line.
    directives: BTreeMap<u32, DirectivePlace>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Edge {
    line: u32,
    strategy: &'static str,
    /// Some use reaching it stands where only a type can and no doubt holds for it.
    proven: bool,
    /// Every use reaching it is a member-access receiver.
    receivers_only: bool,
    /// Why the uses in type positions reaching it prove nothing, when none does.
    doubts: BTreeSet<String>,
    names: BTreeSet<String>,
    via: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AmbiguousUse {
    name: String,
    use_line: u32,
    /// Each candidate's namespace, declaring files and directive.
    candidates: Vec<(String, String, Via)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NameBinding {
    Internal {
        namespace: String,
        target_file: Option<FileId>,
        target_symbol: Option<SymbolId>,
    },
    Outside {
        namespace: String,
    },
}

/// Where one of the file's own directives leads, whatever names the file writes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectivePlace {
    /// A namespace or type of the repository the file can name.
    Repository,
    /// Declared in the repository only where the file cannot name it.
    Hidden,
    /// The standard library (`System`, `System.*`).
    Builtin,
    /// A package, or an assembly the project references.
    Package,
}

impl<'i, 'a> Lookup<'i, 'a> {
    fn new(
        index: &'i Index<'a>,
        file: &'i File,
        facts: &'i CSharpNameFacts,
        owner: Option<usize>,
    ) -> Self {
        let mut locals = vec![Vec::new(); facts.blocks.len()];
        for using in facts.usings.iter().filter(|using| !using.global) {
            if let Some(block) = locals.get_mut(using.block as usize) {
                block.push(using);
            }
        }
        let globals: Rc<[GlobalUsing]> = match owner {
            Some(project) => index.projects[project].globals.clone().into(),
            None => file_globals(file, facts).into(),
        };
        Self {
            index,
            file,
            facts,
            owner,
            locals,
            globals,
            type_parameters: facts.type_parameters.iter().map(String::as_str).collect(),
            variables: facts.variables.iter().map(String::as_str).collect(),
        }
    }

    fn run(&self) -> FileRun {
        let mut run = FileRun::default();
        for using in &self.facts.usings {
            let (place, named) = self.directive_place(using);
            // A `using static` or an alias of a type names that type, whatever the file writes.
            if let Some(group) = named.map(|group| &self.index.groups[group]) {
                let strategy = match using.kind {
                    CSharpUsingKind::Static => "csharp-using-static",
                    _ => "csharp-using-alias",
                };
                for target in group.files() {
                    let doubt = using
                        .conditional
                        .then(|| "the directive is under `#if`".to_string())
                        .or_else(|| self.target_doubt(group, target));
                    run.add_edge(
                        target,
                        Reach {
                            line: using.line,
                            strategy,
                            typed: true,
                            doubt,
                            name: &using.target,
                            via: "the directive naming it",
                        },
                    );
                }
            }
            run.directives.insert(using.line, place);
        }
        for use_ in &self.facts.uses {
            let found = self.find(use_);
            self.record(use_, found, &mut run);
        }
        run
    }

    fn record(&self, use_: &CSharpNameUse, found: Found, run: &mut FileRun) {
        match found {
            Found::Imported { group, via, doubt } => {
                let group = &self.index.groups[group];
                let line = via.line_in(&self.file.id, use_.line);
                let typed = use_.context != CSharpNameContext::Receiver;
                let doubt = doubt.or_else(|| {
                    use_.conditional
                        .then(|| "the name is written under `#if`".to_string())
                });
                for target in group.files() {
                    run.add_edge(
                        target,
                        Reach {
                            line,
                            strategy: via.strategy(),
                            typed,
                            doubt: doubt.clone().or_else(|| self.target_doubt(group, target)),
                            name: &use_.name,
                            via: &via.describe(),
                        },
                    );
                }
                let files = group.files();
                let target_file = match files.iter().collect::<Vec<_>>().as_slice() {
                    [file] => Some((**file).clone()),
                    _ => None,
                };
                let target_symbol = match group.parts.as_slice() {
                    [(symbol, _)] => Some(symbol.clone()),
                    _ => None,
                };
                run.bindings
                    .entry(use_.name.clone())
                    .or_insert_with(|| NameBinding::Internal {
                        namespace: group.namespace.clone(),
                        target_file,
                        target_symbol,
                    });
            }
            Found::Ambiguous { candidates } => {
                let candidates = candidates
                    .into_iter()
                    .map(|(group, via)| {
                        let group = &self.index.groups[group];
                        let files = group
                            .files()
                            .into_iter()
                            .filter_map(|file| self.index.files.get(file))
                            .map(|file| file.path.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ");
                        (group.namespace.clone(), files, via)
                    })
                    .collect();
                let ambiguous = AmbiguousUse {
                    name: use_.name.clone(),
                    use_line: use_.line,
                    candidates,
                };
                if !run.ambiguous.contains(&ambiguous) {
                    run.ambiguous.push(ambiguous);
                }
            }
            Found::Outside { namespace } => {
                // Nothing of the repository takes the name here: variables, the enclosing types'
                // members and their repository base types' were read first. A receiver left
                // over is a type or an inherited member from outside, either way no repository
                // symbol.
                run.bindings
                    .entry(use_.name.clone())
                    .or_insert(NameBinding::Outside { namespace });
            }
            Found::Shadowed | Found::Namespace | Found::Declared(_) | Found::Nothing => {}
        }
    }

    /// Why an edge to `target`, a file declaring `group`, may not hold on every build: the
    /// type's parts may not be the compiler's, `target` declares its part only under `#if`, or
    /// the type's project is referenced only under a `Condition`.
    fn target_doubt(&self, group: &TypeGroup, target: &FileId) -> Option<String> {
        if let Some(inexact) = group.inexact {
            return Some(inexact.to_string());
        }
        let parts_here = group
            .parts
            .iter()
            .filter(|(_, file)| file == target)
            .collect::<Vec<_>>();
        if !parts_here.is_empty()
            && parts_here.iter().all(|(symbol, _)| {
                self.index
                    .declarations
                    .get(symbol)
                    .is_some_and(|declared| declared.conditional)
            })
        {
            return Some("the file declares the type only under `#if`".to_string());
        }
        let conditional_reference = match (self.owner, group.owner) {
            (Some(project), Some(owner)) => self.index.projects[project]
                .conditionally_visible
                .contains(&owner),
            _ => false,
        };
        conditional_reference
            .then(|| "the type's project is referenced only under a `Condition`".to_string())
    }

    /// The lookup C# makes for the name `use_` writes.
    fn find(&self, use_: &CSharpNameUse) -> Found {
        if self.shadowed(use_) {
            return Found::Shadowed;
        }
        if use_.context == CSharpNameContext::Attribute && !use_.name.ends_with("Attribute") {
            // `[Entry]` names `EntryAttribute` or `Entry`; the suffixed one is tried first.
            let suffixed = CSharpNameUse {
                name: format!("{}Attribute", use_.name),
                ..use_.clone()
            };
            let found = self.find_in_namespaces(&suffixed);
            if !matches!(found, Found::Nothing | Found::Outside { .. }) {
                return found;
            }
        }
        self.find_in_namespaces(use_)
    }

    /// The lookup from the innermost namespace of the use out to the global namespace.
    fn find_in_namespaces(&self, use_: &CSharpNameUse) -> Found {
        let chain = self.block_chain(use_.block);
        let innermost = chain
            .first()
            .map(|&block| self.facts.blocks[block].namespace.as_str())
            .unwrap_or_default();
        let mut outside = None;
        let mut hidden = None;
        for level in namespace_levels(innermost) {
            // A type an error node hides in this namespace would be found here first.
            if hidden.is_none()
                && self
                    .index
                    .hidden
                    .contains(&(level.clone(), use_.name.clone()))
            {
                hidden = Some(format!(
                    "an error node of some file may declare `{}` in namespace `{}`, which would be found first",
                    use_.name,
                    if level.is_empty() { "(global)" } else { level.as_str() }
                ));
            }
            if use_.arity == 0
                && self
                    .index
                    .namespace_state(&join_namespace(&level, &use_.name), self.owner)
                    == NamespaceState::Visible
            {
                return Found::Namespace;
            }
            let declared =
                self.index
                    .types_in(&level, &use_.name, use_.arity, self.owner, &self.file.id);
            if !declared.is_empty() {
                return Found::Declared((declared.len() == 1).then(|| declared[0]));
            }
            let Some(block) = chain
                .iter()
                .copied()
                .find(|&block| self.facts.blocks[block].namespace == level)
            else {
                continue;
            };
            let globals: &[GlobalUsing] = if level.is_empty() { &self.globals } else { &[] };
            match self.through_directives(use_, block, globals) {
                Found::Nothing => {}
                Found::Outside { namespace } => {
                    outside.get_or_insert(namespace);
                }
                Found::Imported { group, via, doubt } => {
                    let nearer = outside.map(|namespace| {
                        format!(
                            "a nearer `using` imports `{namespace}`, outside the repository, which may declare the name first"
                        )
                    });
                    return Found::Imported {
                        group,
                        via,
                        doubt: doubt.or(nearer).or(hidden),
                    };
                }
                found => return found,
            }
        }
        match outside {
            Some(namespace) => Found::Outside { namespace },
            None => Found::Nothing,
        }
    }

    /// What a block's directives (and, for the compilation unit, the global ones) supply.
    fn through_directives(
        &self,
        use_: &CSharpNameUse,
        block: usize,
        globals: &[GlobalUsing],
    ) -> Found {
        let block_namespace = self.facts.blocks[block].namespace.as_str();
        let directives = self.locals[block]
            .iter()
            .map(|using| Directive {
                kind: using.kind,
                target: &using.target,
                arity: using.arity,
                alias: using.alias.as_deref(),
                extern_alias: using.extern_alias.is_some(),
                via: Via::Local {
                    line: using.line,
                    kind: using.kind,
                },
                conditional: using.conditional,
            })
            .chain(globals.iter().map(|using| Directive {
                kind: using.kind,
                target: &using.target,
                arity: using.arity,
                alias: using.alias.as_deref(),
                extern_alias: using.extern_alias,
                via: Via::Global {
                    origin: using.origin.clone(),
                    kind: using.kind,
                },
                conditional: using.conditional,
            }))
            .collect::<Vec<_>>();
        // An alias or an extern alias of the name comes before the namespaces directives open.
        if use_.arity == 0 {
            if block == 0 && self.facts.extern_aliases.contains(&use_.name) {
                return Found::Outside {
                    namespace: format!("{}::", use_.name),
                };
            }
            if let Some(directive) = directives.iter().find(|directive| {
                directive.kind == CSharpUsingKind::Alias
                    && directive.alias == Some(use_.name.as_str())
            }) {
                return self.through_alias(use_, directive, block_namespace);
            }
        }
        let mut candidates = Vec::<(usize, Via)>::new();
        let mut conditional = BTreeSet::new();
        let mut outside = None;
        for directive in &directives {
            if directive.kind != CSharpUsingKind::Namespace {
                continue;
            }
            if directive.extern_alias {
                outside.get_or_insert_with(|| directive.target.to_string());
                continue;
            }
            let namespace = self.directive_namespace(directive.target, block_namespace);
            match self.index.namespace_state(&namespace, self.owner) {
                NamespaceState::Visible => {
                    for group in self.index.types_in(
                        &namespace,
                        &use_.name,
                        use_.arity,
                        self.owner,
                        &self.file.id,
                    ) {
                        if !candidates.iter().any(|(known, _)| *known == group) {
                            candidates.push((group, directive.via.clone()));
                            if directive.conditional {
                                conditional.insert(group);
                            }
                        }
                    }
                    // A namespace the repository declares may hold types from outside too.
                    outside.get_or_insert(namespace);
                }
                NamespaceState::Outside => {
                    outside.get_or_insert(namespace);
                }
                NamespaceState::Hidden => {}
            }
        }
        match candidates.as_slice() {
            [] => match outside {
                Some(namespace) => Found::Outside { namespace },
                None => Found::Nothing,
            },
            [(group, via)] => Found::Imported {
                group: *group,
                via: via.clone(),
                doubt: conditional.contains(group).then(|| {
                    "the `using` that imports it is under `#if` or a `Condition`".to_string()
                }),
            },
            _ => Found::Ambiguous { candidates },
        }
    }

    /// What a name an alias directive declares stands for.
    fn through_alias(
        &self,
        use_: &CSharpNameUse,
        directive: &Directive<'_>,
        block_namespace: &str,
    ) -> Found {
        if directive.extern_alias {
            return Found::Outside {
                namespace: directive.target.to_string(),
            };
        }
        let groups = match self.alias_target(directive.target, directive.arity, block_namespace) {
            AliasTarget::Type(groups) => groups,
            AliasTarget::Namespace(namespace) => {
                let Some(next) = &use_.next else {
                    return Found::Namespace;
                };
                let groups = self.index.types_in(
                    &namespace,
                    next,
                    use_.next_arity,
                    self.owner,
                    &self.file.id,
                );
                if groups.is_empty() {
                    return Found::Outside { namespace };
                }
                groups
            }
            AliasTarget::Outside(namespace) => return Found::Outside { namespace },
            AliasTarget::Hidden => return Found::Nothing,
        };
        match groups.as_slice() {
            [group] => Found::Imported {
                group: *group,
                via: directive.via.clone(),
                doubt: directive.conditional.then(|| {
                    "the `using` that imports it is under `#if` or a `Condition`".to_string()
                }),
            },
            _ => Found::Ambiguous {
                candidates: groups
                    .iter()
                    .map(|&group| (group, directive.via.clone()))
                    .collect(),
            },
        }
    }

    /// The namespace a `using` directive written in a block of `block_namespace` names:
    /// relative to the namespaces enclosing it when one of those holds it, else as written.
    fn directive_namespace(&self, target: &str, block_namespace: &str) -> String {
        namespace_levels(block_namespace)
            .into_iter()
            .filter(|level| !level.is_empty())
            .map(|level| join_namespace(&level, target))
            .find(|candidate| self.index.namespaces.contains_key(candidate))
            .unwrap_or_else(|| target.to_string())
    }

    /// What an alias or `using static` target, written in a block of `block_namespace`, names:
    /// relative to the namespaces enclosing the block first, as for any directive.
    fn alias_target(&self, target: &str, arity: u16, block_namespace: &str) -> AliasTarget {
        for level in namespace_levels(block_namespace) {
            let full = join_namespace(&level, target);
            if arity == 0 {
                match self.index.namespace_state(&full, self.owner) {
                    NamespaceState::Visible => return AliasTarget::Namespace(full),
                    NamespaceState::Hidden => return AliasTarget::Hidden,
                    NamespaceState::Outside => {}
                }
            }
            let (namespace, name) = full.rsplit_once('.').unwrap_or(("", full.as_str()));
            let groups = self
                .index
                .types_in(namespace, name, arity, self.owner, &self.file.id);
            if !groups.is_empty() {
                return AliasTarget::Type(groups);
            }
            let declared = (namespace.to_string(), name.to_string());
            if self
                .index
                .by_key
                .contains_key(&(declared.0.clone(), declared.1.clone(), arity))
                || self.index.unknown_arity.contains_key(&declared)
            {
                return AliasTarget::Hidden;
            }
        }
        AliasTarget::Outside(target.to_string())
    }

    /// Where one of the file's own directives leads, and for a `using static` or an alias of a
    /// repository type, that type.
    fn directive_place(&self, using: &CSharpUsing) -> (DirectivePlace, Option<usize>) {
        if using.extern_alias.is_some() {
            return (DirectivePlace::Package, None);
        }
        let block_namespace = self
            .facts
            .blocks
            .get(using.block as usize)
            .map(|block| block.namespace.as_str())
            .unwrap_or_default();
        if using.kind == CSharpUsingKind::Namespace {
            let namespace = self.directive_namespace(&using.target, block_namespace);
            let place = match self.index.namespace_state(&namespace, self.owner) {
                NamespaceState::Visible => DirectivePlace::Repository,
                NamespaceState::Hidden => DirectivePlace::Hidden,
                NamespaceState::Outside => outside_place(&namespace),
            };
            return (place, None);
        }
        match self.alias_target(&using.target, using.arity, block_namespace) {
            AliasTarget::Type(groups) => match groups.as_slice() {
                [group] => (DirectivePlace::Repository, Some(*group)),
                // Two accessible types of the name are no one place.
                _ => (DirectivePlace::Hidden, None),
            },
            AliasTarget::Namespace(_) => (DirectivePlace::Repository, None),
            AliasTarget::Hidden => (DirectivePlace::Hidden, None),
            AliasTarget::Outside(target) => (outside_place(&target), None),
        }
    }

    /// Blocks from `block` out to the compilation unit.
    fn block_chain(&self, block: u32) -> Vec<usize> {
        let mut chain = Vec::new();
        let mut current = Some(block as usize);
        while let Some(index) = current {
            let Some(found) = self.facts.blocks.get(index) else {
                break;
            };
            chain.push(index);
            current = found.parent.map(|parent| parent as usize);
        }
        chain
    }

    /// Whether something nearer than any namespace takes the name: a type parameter, a type
    /// nested in an enclosing type or in its repository base types, and for a receiver a
    /// variable or a member of one of those types.
    fn shadowed(&self, use_: &CSharpNameUse) -> bool {
        if use_.arity == 0 && self.type_parameters.contains(use_.name.as_str()) {
            return true;
        }
        let receiver = use_.context == CSharpNameContext::Receiver;
        if receiver && self.variables.contains(use_.name.as_str()) {
            return true;
        }
        let Some(group) = use_
            .enclosing_type
            .as_ref()
            .and_then(|id| self.index.group_of_symbol.get(id))
        else {
            return false;
        };
        let names = self.index.shadow_names(*group, self.owner);
        names.types.contains(&(use_.name.clone(), use_.arity))
            || names.types_of_any_arity.contains(&use_.name)
            || (receiver && names.members.contains(&use_.name))
    }

    /// The repository type a base list entry names, read in this file where the declaration
    /// stands: no enclosing type's nested types first, since a base is read outside its type.
    fn base_group(&self, base: &CSharpNameUse) -> Option<usize> {
        if base.arity == 0 && self.type_parameters.contains(base.name.as_str()) {
            return None;
        }
        match self.find_in_namespaces(base) {
            Found::Imported { group, .. } | Found::Declared(Some(group)) => Some(group),
            _ => None,
        }
    }
}

/// A directive in scope at one level of a lookup.
struct Directive<'d> {
    kind: CSharpUsingKind,
    target: &'d str,
    arity: u16,
    alias: Option<&'d str>,
    extern_alias: bool,
    via: Via,
    conditional: bool,
}

/// One use, or directive, reaching a file.
struct Reach<'r> {
    line: u32,
    strategy: &'static str,
    /// Written where only a type can stand.
    typed: bool,
    doubt: Option<String>,
    name: &'r str,
    via: &'r str,
}

impl FileRun {
    fn add_edge(&mut self, target: &FileId, reach: Reach<'_>) {
        let edge = self.edges.entry(target.clone()).or_insert_with(|| Edge {
            line: reach.line,
            strategy: reach.strategy,
            proven: false,
            receivers_only: true,
            doubts: BTreeSet::new(),
            names: BTreeSet::new(),
            via: reach.via.to_string(),
        });
        // The earliest line is where the edge is recorded; a proving use anywhere proves it.
        if reach.line < edge.line {
            edge.line = reach.line;
            edge.strategy = reach.strategy;
            edge.via = reach.via.to_string();
        }
        edge.receivers_only &= !reach.typed;
        if reach.typed {
            match reach.doubt {
                None => edge.proven = true,
                Some(doubt) => {
                    edge.doubts.insert(doubt);
                }
            }
        }
        edge.names.insert(reach.name.to_string());
    }
}

/// A file's runs, one per project compiling it, merged.
struct ResolvedFile {
    /// Each edge, and whether every project's run reaches it.
    edges: BTreeMap<FileId, (Edge, bool)>,
    ambiguous: Vec<AmbiguousUse>,
    bindings: BTreeMap<String, NameBinding>,
    directives: BTreeMap<u32, DirectivePlace>,
    /// The file belongs to no project file the index read.
    unowned: bool,
    /// An error node of the file may hide a `using` directive.
    hidden_directives: bool,
    /// A `Compile` item a `Condition` guards includes or removes the file.
    conditionally_owned: bool,
}

impl ResolvedFile {
    fn merge(runs: Vec<FileRun>, owners: &[Option<usize>], hidden_directives: bool) -> Self {
        let count = runs.len();
        let mut merged = ResolvedFile {
            edges: BTreeMap::new(),
            ambiguous: Vec::new(),
            bindings: BTreeMap::new(),
            directives: BTreeMap::new(),
            unowned: owners.iter().all(Option::is_none),
            hidden_directives,
            conditionally_owned: false,
        };
        let mut reached_by = BTreeMap::<FileId, usize>::new();
        let mut bindings = BTreeMap::<String, Vec<NameBinding>>::new();
        for run in runs {
            for (target, edge) in run.edges {
                *reached_by.entry(target.clone()).or_default() += 1;
                merged
                    .edges
                    .entry(target)
                    .and_modify(|(known, _)| {
                        known.proven &= edge.proven;
                        known.receivers_only &= edge.receivers_only;
                        known.doubts.extend(edge.doubts.iter().cloned());
                        known.names.extend(edge.names.iter().cloned());
                    })
                    .or_insert((edge, true));
            }
            for ambiguous in run.ambiguous {
                if !merged.ambiguous.contains(&ambiguous) {
                    merged.ambiguous.push(ambiguous);
                }
            }
            for (name, binding) in run.bindings {
                bindings.entry(name).or_default().push(binding);
            }
            for (line, place) in run.directives {
                merged.directives.entry(line).or_insert(place);
            }
        }
        // An edge only some of the file's projects reach is no edge of the file's own.
        for (target, (_, agreed)) in &mut merged.edges {
            *agreed = reached_by.get(target) == Some(&count);
        }
        // A name the projects compiling the file bind differently binds nowhere.
        merged.bindings = bindings
            .into_iter()
            .filter(|(_, found)| {
                found.len() == count && found.windows(2).all(|pair| pair[0] == pair[1])
            })
            .filter_map(|(name, mut found)| Some((name, found.pop()?)))
            .collect();
        merged
    }

    fn ambiguous_names(&self) -> usize {
        self.ambiguous
            .iter()
            .map(|ambiguous| &ambiguous.name)
            .collect::<BTreeSet<_>>()
            .len()
    }

    fn write(
        self,
        file: &File,
        facts: &CSharpNameFacts,
        index: &Index<'_>,
        rows: &HashMap<(&FileId, u32), &Import>,
        report: &mut CSharpImportReport,
    ) {
        let path = file.path.display().to_string();
        let row = |line: u32, imported: &str| -> Import {
            rows.get(&(&file.id, line))
                .map(|import| (*import).clone())
                .unwrap_or_else(|| Import {
                    file_id: file.id.clone(),
                    imported: imported.to_string(),
                    range: Some(LineRange::single(line)),
                    confidence: Confidence::High,
                })
        };
        let mut caveats = Vec::new();
        if self.unowned {
            caveats.push("the file belongs to no project file the index read".to_string());
        }
        if self.conditionally_owned {
            caveats.push(
                "a `Compile` item a `Condition` guards decides whether a project compiles the file"
                    .to_string(),
            );
        }
        if self.hidden_directives {
            caveats.push("an error node of the file may hide a `using` directive".to_string());
        }
        for (target, (edge, agreed)) in &self.edges {
            if target == &file.id {
                continue;
            }
            let Some(target_file) = index.files.get(target) else {
                continue;
            };
            let Ok(target_path) = identity::normalize_repo_path(&target_file.path) else {
                continue;
            };
            let mut reasons = caveats.clone();
            if edge.receivers_only {
                reasons.push(
                    "every name reaching it is a member-access receiver, which a member inherited from a base type outside the repository would take".to_string(),
                );
            } else if !edge.proven {
                reasons.extend(edge.doubts.iter().cloned());
            }
            if !agreed {
                reasons.push("not every project compiling the file reaches it".to_string());
            }
            let proven = reasons.is_empty();
            let confidence = if proven {
                Confidence::High
            } else {
                Confidence::Medium
            };
            let names = edge.names.iter().cloned().collect::<Vec<_>>().join(", ");
            let mut message = format!(
                "{path} names {names} through {}, declared in {}",
                edge.via,
                target_file.path.display()
            );
            if !proven {
                message.push_str("; unproven: ");
                message.push_str(&reasons.join("; "));
            }
            report.analysis_facts.push(AnalysisFact {
                id: identity::stable_hash(&format!(
                    "csharp-import:{path}:{}:{target_path}",
                    edge.strategy
                )),
                file_id: file.id.clone(),
                symbol_id: None,
                target: target_path,
                target_kind: GraphNodeType::File,
                target_symbol_id: None,
                ambiguity: Vec::new(),
                edge_type: GraphEdgeType::Imports,
                range: Some(LineRange::single(edge.line)),
                confidence,
                source: format!("{SOURCE_PREFIX}{}", edge.strategy).into(),
                source_type: EvidenceSourceType::StaticAnalysis,
                message: message.into(),
            });
            let imported = facts
                .usings
                .iter()
                .find(|using| using.line == edge.line)
                .map_or_else(|| names.clone(), |using| using.target.clone());
            report.resolutions.push(ImportResolution {
                import: row(edge.line, &imported),
                status: ResolutionStatus::Resolved,
                target_file: Some(target.clone()),
                target_symbol: None,
                confidence,
                strategy: edge.strategy.to_string(),
                caveats: Vec::new(),
            });
        }
        for ambiguous in &self.ambiguous {
            let described = ambiguous
                .candidates
                .iter()
                .map(|(namespace, files, _)| format!("{namespace}.{} ({files})", ambiguous.name))
                .collect::<Vec<_>>();
            for (namespace, _, via) in &ambiguous.candidates {
                let line = via.line_in(&file.id, ambiguous.use_line);
                report.analysis_facts.push(AnalysisFact {
                    id: identity::stable_hash(&format!(
                        "csharp-import-ambiguous:{path}:{line}:{namespace}:{}",
                        ambiguous.name
                    )),
                    file_id: file.id.clone(),
                    symbol_id: None,
                    target: namespace.clone(),
                    target_kind: GraphNodeType::Module,
                    target_symbol_id: None,
                    ambiguity: described.clone(),
                    edge_type: GraphEdgeType::Imports,
                    range: Some(LineRange::single(line)),
                    confidence: Confidence::Low,
                    source: format!("{SOURCE_PREFIX}csharp-ambiguous").into(),
                    source_type: EvidenceSourceType::StaticAnalysis,
                    message: format!(
                        "{path} names `{}` on line {}, which more than one namespace it imports declares; no edge emitted: {}",
                        ambiguous.name,
                        ambiguous.use_line,
                        described.join(" and ")
                    )
                    .into(),
                });
            }
        }
        for using in &facts.usings {
            let Some(place) = self.directives.get(&using.line) else {
                continue;
            };
            let written = match &using.extern_alias {
                Some(alias) => format!("{alias}::{}", using.target),
                None => using.target.clone(),
            };
            let (status, confidence, strategy, caveats) = match place {
                // Its edges are the files above, one per type the file names through it.
                DirectivePlace::Repository => (
                    ResolutionStatus::Resolved,
                    Confidence::High,
                    "csharp-namespace",
                    Vec::new(),
                ),
                DirectivePlace::Builtin => (
                    ResolutionStatus::Builtin,
                    Confidence::High,
                    "csharp-standard-library",
                    Vec::new(),
                ),
                DirectivePlace::Package => (
                    ResolutionStatus::ExternalPackage,
                    Confidence::Medium,
                    "csharp-external-namespace",
                    Vec::new(),
                ),
                DirectivePlace::Hidden => (
                    ResolutionStatus::Unresolved,
                    Confidence::Low,
                    "csharp-namespace-not-referenced",
                    vec![format!(
                        "`{}` is declared in the repository only where the file's project cannot name it",
                        using.target
                    )],
                ),
            };
            let resolution = ImportResolution {
                import: row(using.line, &written),
                status,
                target_file: None,
                target_symbol: None,
                confidence,
                strategy: strategy.into(),
                caveats,
            };
            if *place != DirectivePlace::Repository {
                if let Some(fact) = analysis_fact_for_import_resolution(file, &resolution) {
                    report.analysis_facts.push(fact);
                }
                if !resolution.caveats.is_empty() {
                    report.quality_notes.push(QualityNote::new(
                        QualityNoteKind::ImportResolverCaveat,
                        format!(
                            "import resolver caveat in {path} for `{written}`: {}",
                            resolution.caveats.join("; ")
                        ),
                    ));
                }
            }
            report.resolutions.push(resolution);
        }
        let scope = ScopeId::new("global");
        for (name, binding) in self.bindings {
            let (source_module, target_file, target_symbol, origin) = match binding {
                NameBinding::Internal {
                    namespace,
                    target_file,
                    target_symbol,
                } => (
                    namespace,
                    target_file,
                    target_symbol,
                    ImportOrigin::Internal,
                ),
                NameBinding::Outside { namespace } => {
                    (namespace, None, None, ImportOrigin::External)
                }
            };
            report.bindings.push(ImportBinding {
                file_id: file.id.clone(),
                scope_id: scope.clone(),
                local_name: name.clone(),
                imported_name: name,
                source_module,
                resolved_module: None,
                target_file,
                target_symbol,
                origin,
                is_type_only: true,
                is_glob: false,
                evidence: Vec::new(),
                rule: ImportBindingRule::ModuleKey,
                configured_targets: None,
            });
        }
    }
}

/// Where a directive naming what no project of the repository declares leads.
fn outside_place(target: &str) -> DirectivePlace {
    if target.split('.').next() == Some("System") {
        DirectivePlace::Builtin
    } else {
        DirectivePlace::Package
    }
}

/// `namespace` and each namespace enclosing it, innermost first, ending at the global
/// namespace `""`.
fn namespace_levels(namespace: &str) -> Vec<String> {
    let mut levels = Vec::new();
    let mut current = namespace;
    while !current.is_empty() {
        levels.push(current.to_string());
        current = current.rsplit_once('.').map_or("", |(outer, _)| outer);
    }
    levels.push(String::new());
    levels
}

fn join_namespace(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{namespace}.{name}")
    }
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use crate::{IndexSnapshot, Indexer};
    use open_kioku_config::OkConfig;
    use open_kioku_core::{Confidence, GraphEdgeType, GraphNodeType, IndexMode, Visibility};

    const SDK: &str = "<Project Sdk=\"Microsoft.NET.Sdk\">\n</Project>\n";

    /// A project file referencing the projects at `references` (paths relative to it).
    fn sdk_project(references: &[&str]) -> String {
        let items = references
            .iter()
            .map(|reference| format!("    <ProjectReference Include=\"{reference}\" />\n"))
            .collect::<String>();
        format!("<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n{items}  </ItemGroup>\n</Project>\n")
    }

    fn index(files: &[(&str, &str)]) -> IndexSnapshot {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path();
        for (path, content) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(path, content).expect("write");
        }
        let mut config = OkConfig::default();
        config.scip.enabled = false;
        config.history.enabled = false;
        Indexer::default()
            .index_repo_with_mode(root, &config, IndexMode::Full)
            .expect("index")
    }

    fn path_of(snapshot: &IndexSnapshot, file: &open_kioku_core::FileId) -> String {
        snapshot
            .files
            .iter()
            .find(|candidate| candidate.id == *file)
            .map(|file| file.path.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default()
    }

    /// `(importing file, target file, line, proven, strategy)` of every C# file `IMPORTS` fact.
    fn edges(snapshot: &IndexSnapshot) -> Vec<(String, String, u32, bool, String)> {
        let mut edges = snapshot
            .analysis_facts
            .iter()
            .filter(|fact| {
                fact.edge_type == GraphEdgeType::Imports
                    && fact.target_kind == GraphNodeType::File
                    && fact.source.starts_with("open-kioku-import-resolver/csharp")
            })
            .map(|fact| {
                (
                    path_of(snapshot, &fact.file_id),
                    fact.target.clone(),
                    fact.range.as_ref().map_or(0, |range| range.start),
                    fact.confidence == Confidence::High,
                    fact.source
                        .trim_start_matches("open-kioku-import-resolver/")
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();
        edges.sort();
        edges
    }

    fn edge(
        from: &str,
        to: &str,
        line: u32,
        proven: bool,
        strategy: &str,
    ) -> (String, String, u32, bool, String) {
        (from.into(), to.into(), line, proven, strategy.into())
    }

    /// `(importing file, target, line, source)` of every other C# resolver fact.
    fn other_facts(snapshot: &IndexSnapshot) -> Vec<(String, GraphEdgeType, String, u32, String)> {
        let mut facts = snapshot
            .analysis_facts
            .iter()
            .filter(|fact| {
                fact.source.starts_with("open-kioku-import-resolver/csharp")
                    && fact.target_kind != GraphNodeType::File
            })
            .map(|fact| {
                (
                    path_of(snapshot, &fact.file_id),
                    fact.edge_type.clone(),
                    fact.target.clone(),
                    fact.range.as_ref().map_or(0, |range| range.start),
                    fact.source
                        .trim_start_matches("open-kioku-import-resolver/")
                        .to_string(),
                )
            })
            .collect::<Vec<_>>();
        facts.sort();
        facts
    }

    const ENTRY: &str = "namespace Acme.Ledger;\n\npublic class Entry { }\n";

    #[test]
    fn a_namespace_import_leads_to_the_files_declaring_the_types_the_file_names() {
        let snapshot = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", ENTRY),
            (
                "src/Ledger/Book.cs",
                "namespace Acme.Ledger\n{\n    public partial class Book { }\n    public class Unused { }\n}\n",
            ),
            (
                "src/Ledger/Book.Audit.cs",
                "namespace Acme.Ledger\n{\n    public partial class Book { }\n}\n",
            ),
            ("src/Web/Web.csproj", &sdk_project(&["..\\Ledger\\Ledger.csproj"])),
            (
                "src/Web/Page.cs",
                "using System;\nusing Acme.Ledger;\nusing Ledger.Json;\n\nnamespace Acme.Web;\n\npublic class Page\n{\n    private Entry entry;\n    public Book Book() => null;\n}\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot),
            vec![
                // Every part of the partial type is a file declaring it.
                edge(
                    "src/Web/Page.cs",
                    "src/Ledger/Book.Audit.cs",
                    2,
                    true,
                    "csharp-using-namespace"
                ),
                edge(
                    "src/Web/Page.cs",
                    "src/Ledger/Book.cs",
                    2,
                    true,
                    "csharp-using-namespace"
                ),
                edge(
                    "src/Web/Page.cs",
                    "src/Ledger/Entry.cs",
                    2,
                    true,
                    "csharp-using-namespace"
                ),
            ]
        );
        // The standard library and a package namespace are dependencies outside the repository.
        assert_eq!(
            other_facts(&snapshot),
            vec![
                (
                    "src/Web/Page.cs".into(),
                    GraphEdgeType::DependsOn,
                    "Ledger.Json".into(),
                    3,
                    "csharp-external-namespace".into()
                ),
                (
                    "src/Web/Page.cs".into(),
                    GraphEdgeType::DependsOn,
                    "System".into(),
                    1,
                    "csharp-standard-library".into()
                ),
            ]
        );
    }

    #[test]
    fn a_namespace_only_an_unreferenced_project_declares_leads_nowhere() {
        let snapshot = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", ENTRY),
            ("src/Web/Web.csproj", SDK),
            (
                "src/Web/Page.cs",
                "using Acme.Ledger;\n\nnamespace Acme.Web;\n\npublic class Page { private Entry entry; }\n",
            ),
        ]);
        assert!(edges(&snapshot).is_empty(), "{:?}", edges(&snapshot));
        assert_eq!(
            other_facts(&snapshot),
            vec![(
                "src/Web/Page.cs".into(),
                GraphEdgeType::Imports,
                "Acme.Ledger".into(),
                1,
                "csharp-namespace-not-referenced".into()
            )]
        );
    }

    #[test]
    fn transitive_references_follow_sdk_style_projects() {
        let snapshot = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", ENTRY),
            ("src/Core/Core.csproj", &sdk_project(&["../Ledger/Ledger.csproj"])),
            ("src/Web/Web.csproj", &sdk_project(&["../Core/Core.csproj"])),
            (
                "src/Web/Page.cs",
                "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page { private Entry entry; }\n",
            ),
            (
                "src/Strict/Strict.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <DisableTransitiveProjectReferences>true</DisableTransitiveProjectReferences>\n  </PropertyGroup>\n  <ItemGroup>\n    <ProjectReference Include=\"../Core/Core.csproj\" />\n  </ItemGroup>\n</Project>\n",
            ),
            (
                "src/Strict/Page.cs",
                "using Acme.Ledger;\nnamespace Acme.Strict;\npublic class Page { private Entry entry; }\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot),
            vec![edge(
                "src/Web/Page.cs",
                "src/Ledger/Entry.cs",
                1,
                true,
                "csharp-using-namespace"
            )]
        );
    }

    #[test]
    fn a_namespace_spanning_projects_offers_only_the_referenced_projects_types() {
        // `Acme.Ledger` is declared by a referenced project and by one that is not; only the
        // referenced one's types are candidates.
        let snapshot = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Book.cs", "namespace Acme.Ledger;\npublic class Book { }\n"),
            ("src/Tools/Tools.csproj", SDK),
            ("src/Tools/Entry.cs", ENTRY),
            ("src/Web/Web.csproj", &sdk_project(&["../Ledger/Ledger.csproj"])),
            (
                "src/Web/Page.cs",
                "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page { private Entry entry; private Book book; }\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot),
            vec![edge(
                "src/Web/Page.cs",
                "src/Ledger/Book.cs",
                1,
                true,
                "csharp-using-namespace"
            )]
        );
    }

    #[test]
    fn a_name_two_imported_namespaces_declare_is_ambiguous() {
        let snapshot = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            ("Billing/Entry.cs", "namespace Acme.Billing;\npublic class Entry { }\n"),
            (
                "Web/Page.cs",
                "using Acme.Ledger;\nusing Acme.Billing;\nnamespace Acme.Web;\npublic class Page { private Entry entry; }\n",
            ),
        ]);
        assert!(edges(&snapshot).is_empty(), "{:?}", edges(&snapshot));
        assert_eq!(
            other_facts(&snapshot),
            vec![
                (
                    "Web/Page.cs".into(),
                    GraphEdgeType::Imports,
                    "Acme.Billing".into(),
                    2,
                    "csharp-ambiguous".into()
                ),
                (
                    "Web/Page.cs".into(),
                    GraphEdgeType::Imports,
                    "Acme.Ledger".into(),
                    1,
                    "csharp-ambiguous".into()
                ),
            ]
        );
        let ambiguous = snapshot
            .analysis_facts
            .iter()
            .find(|fact| fact.source.ends_with("csharp-ambiguous"))
            .expect("an ambiguous fact");
        assert_eq!(ambiguous.confidence, Confidence::Low);
        assert_eq!(ambiguous.ambiguity.len(), 2, "{:?}", ambiguous.ambiguity);
    }

    #[test]
    fn enclosing_namespaces_take_a_name_before_any_directive() {
        let snapshot = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            ("Ledger/Ledger.cs", "namespace Acme.Ledger;\npublic class Ledger { }\n"),
            ("Web/Entry.cs", "namespace Acme.Web;\npublic class Entry { }\n"),
            (
                "Web/Page.cs",
                // `Entry` is the page's own namespace's; `Ledger` is the namespace
                // `Acme.Ledger` seen from `Acme`, never the class through the directive.
                "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page { private Entry entry; private Ledger ledger; }\n",
            ),
        ]);
        assert!(edges(&snapshot).is_empty(), "{:?}", edges(&snapshot));
    }

    #[test]
    fn type_parameters_nested_types_and_variables_take_a_name_first() {
        let snapshot = index(&[
            ("App.csproj", SDK),
            (
                "Ledger/Types.cs",
                "namespace Acme.Ledger;\npublic class Entry { }\npublic class Rule { }\npublic static class Rules { }\npublic class Line { }\n",
            ),
            (
                "Web/Page.cs",
                "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page<Entry>\n{\n    private Entry entry;\n    public class Rule { }\n    private Rule rule;\n    public int Rules { get; }\n    void M(int Line) { var x = Rules.ToString(); var y = Line.ToString(); }\n}\n",
            ),
        ]);
        assert!(edges(&snapshot).is_empty(), "{:?}", edges(&snapshot));
    }

    #[test]
    fn a_receiver_alone_proves_no_edge() {
        let snapshot = index(&[
            ("App.csproj", SDK),
            (
                "Ledger/Rules.cs",
                "namespace Acme.Ledger;\npublic static class Rules { public static int Apply() => 1; }\n",
            ),
            (
                "Web/Page.cs",
                "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page { int M() => Rules.Apply(); }\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot),
            vec![edge(
                "Web/Page.cs",
                "Ledger/Rules.cs",
                1,
                false,
                "csharp-using-namespace"
            )]
        );
    }

    #[test]
    fn internal_types_are_visible_to_their_project_and_its_friends_alone() {
        let internal = "namespace Acme.Ledger;\ninternal class Entry { }\n";
        let page =
            "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page { private Entry entry; }\n";
        let snapshot = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", internal),
            ("src/Ledger/Same.cs", "using Acme.Ledger;\nnamespace Acme.Other;\npublic class Same { private Entry entry; }\n"),
            ("src/Web/Web.csproj", &sdk_project(&["../Ledger/Ledger.csproj"])),
            ("src/Web/Page.cs", page),
        ]);
        assert_eq!(
            edges(&snapshot),
            vec![edge(
                "src/Ledger/Same.cs",
                "src/Ledger/Entry.cs",
                1,
                true,
                "csharp-using-namespace"
            )]
        );
        let friends = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", internal),
            (
                "src/Ledger/Properties/AssemblyInfo.cs",
                "using System.Runtime.CompilerServices;\n[assembly: InternalsVisibleTo(\"Web\")]\n",
            ),
            (
                "src/Web/Web.csproj",
                &sdk_project(&["../Ledger/Ledger.csproj"]),
            ),
            ("src/Web/Page.cs", page),
        ]);
        assert_eq!(
            edges(&friends),
            vec![edge(
                "src/Web/Page.cs",
                "src/Ledger/Entry.cs",
                1,
                true,
                "csharp-using-namespace"
            )]
        );
    }

    #[test]
    fn global_usings_apply_to_every_file_of_their_project() {
        let snapshot = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", ENTRY),
            ("src/Ledger/Rule.cs", "namespace Acme.Rules;\npublic class Rule { }\n"),
            (
                "src/Web/Web.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk.Web\">\n  <PropertyGroup>\n    <ImplicitUsings>enable</ImplicitUsings>\n  </PropertyGroup>\n  <ItemGroup>\n    <ProjectReference Include=\"../Ledger/Ledger.csproj\" />\n    <Using Include=\"Acme.Rules\" />\n  </ItemGroup>\n</Project>\n",
            ),
            ("src/Web/GlobalUsings.cs", "global using Acme.Ledger;\n"),
            (
                "src/Web/Page.cs",
                "namespace Acme.Web;\n\npublic class Page\n{\n    private Entry entry;\n    private Rule rule;\n}\n",
            ),
            (
                "src/Other/Other.csproj",
                &sdk_project(&["../Ledger/Ledger.csproj"]),
            ),
            (
                "src/Other/Page.cs",
                "namespace Acme.Other;\npublic class Page { private Entry entry; }\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot),
            vec![
                // A directive another file declares is recorded where the name is written.
                edge(
                    "src/Web/Page.cs",
                    "src/Ledger/Entry.cs",
                    5,
                    true,
                    "csharp-global-using"
                ),
                edge(
                    "src/Web/Page.cs",
                    "src/Ledger/Rule.cs",
                    6,
                    true,
                    "csharp-project-using"
                ),
            ]
        );
    }

    #[test]
    fn a_name_only_a_nearer_outside_namespace_could_hold_is_unproven() {
        let snapshot = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            (
                "Web/Page.cs",
                "using Acme.Ledger;\nnamespace Acme.Web\n{\n    using Vendor.Models;\n    public class Page { private Entry entry; }\n}\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot),
            vec![edge(
                "Web/Page.cs",
                "Ledger/Entry.cs",
                1,
                false,
                "csharp-using-namespace"
            )]
        );
    }

    #[test]
    fn aliases_and_static_imports_name_their_targets() {
        let snapshot = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            (
                "Ledger/Rules.cs",
                "namespace Acme.Ledger;\npublic static class Rules { public static int Apply() => 1; }\n",
            ),
            (
                "Web/Page.cs",
                "using static Acme.Ledger.Rules;\nusing L = Acme.Ledger;\nnamespace Acme.Web;\npublic class Page { private L.Entry entry; int M() => Apply(); }\n",
            ),
            (
                "Web/Alias.cs",
                "using Item = Acme.Ledger.Entry;\nnamespace Acme.Web;\npublic class Alias { }\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot),
            vec![
                edge(
                    "Web/Alias.cs",
                    "Ledger/Entry.cs",
                    1,
                    true,
                    "csharp-using-alias"
                ),
                edge(
                    "Web/Page.cs",
                    "Ledger/Entry.cs",
                    2,
                    true,
                    "csharp-using-alias"
                ),
                edge(
                    "Web/Page.cs",
                    "Ledger/Rules.cs",
                    1,
                    true,
                    "csharp-using-static"
                ),
            ]
        );
    }

    #[test]
    fn generic_arity_and_attribute_suffixes_pick_the_type() {
        let snapshot = index(&[
            ("App.csproj", SDK),
            ("Ledger/Result.cs", "namespace Acme.Ledger;\npublic class Result { }\n"),
            ("Ledger/ResultOfT.cs", "namespace Acme.Ledger;\npublic class Result<T> { }\n"),
            (
                "Ledger/AuditedAttribute.cs",
                "namespace Acme.Ledger;\npublic class AuditedAttribute : System.Attribute { }\n",
            ),
            (
                "Web/Page.cs",
                "using Acme.Ledger;\nnamespace Acme.Web;\n[Audited]\npublic class Page { public Result<int> Get() => null; }\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot),
            vec![
                edge(
                    "Web/Page.cs",
                    "Ledger/AuditedAttribute.cs",
                    1,
                    true,
                    "csharp-using-namespace"
                ),
                edge(
                    "Web/Page.cs",
                    "Ledger/ResultOfT.cs",
                    1,
                    true,
                    "csharp-using-namespace"
                ),
            ]
        );
    }

    #[test]
    fn files_of_no_project_and_files_with_syntax_errors_prove_nothing() {
        let page =
            "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page { private Entry entry; }\n";
        let unowned = index(&[("Ledger/Entry.cs", ENTRY), ("Web/Page.cs", page)]);
        assert_eq!(
            edges(&unowned),
            vec![edge(
                "Web/Page.cs",
                "Ledger/Entry.cs",
                1,
                false,
                "csharp-using-namespace"
            )]
        );
        let broken = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            (
                "Web/Page.cs",
                // Recovery misreads the second directive into a statement: one may be missing.
                "using Acme.Ledger;\nusing using Acme.Billing;\nnamespace Acme.Web;\npublic class Page\n{\n    private Entry entry;\n}\n",
            ),
        ]);
        assert_eq!(
            edges(&broken),
            vec![edge(
                "Web/Page.cs",
                "Ledger/Entry.cs",
                1,
                false,
                "csharp-using-namespace"
            )]
        );
    }

    #[test]
    fn a_type_the_parser_did_not_read_whole_proves_no_edge() {
        let page = "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page { private Broken broken; private Entry entry; }\n";
        // Recovery keeps only the namespace of `Broken.cs`; the pattern fallback names the type.
        let fallback = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            (
                "Ledger/Broken.cs",
                "namespace Acme.Ledger;\npublic sealed class Broken\n{\n    public void Lost() { if (x { }\n",
            ),
            ("Web/Page.cs", page),
        ]);
        assert_eq!(
            edges(&fallback),
            vec![
                edge(
                    "Web/Page.cs",
                    "Ledger/Broken.cs",
                    1,
                    false,
                    "csharp-using-namespace"
                ),
                edge(
                    "Web/Page.cs",
                    "Ledger/Entry.cs",
                    1,
                    true,
                    "csharp-using-namespace"
                ),
            ]
        );
        // Two declarations of one type, not both `partial`: a build configuration picks one.
        let duplicate = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            (
                "Ledger/EntryCompat.cs",
                "namespace Acme.Ledger\n{\n#if LEGACY\n    public class Entry { }\n#endif\n}\n",
            ),
            (
                "Web/Page.cs",
                "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page { private Entry entry; }\n",
            ),
        ]);
        assert_eq!(
            edges(&duplicate),
            vec![
                edge(
                    "Web/Page.cs",
                    "Ledger/Entry.cs",
                    1,
                    false,
                    "csharp-using-namespace"
                ),
                edge(
                    "Web/Page.cs",
                    "Ledger/EntryCompat.cs",
                    1,
                    false,
                    "csharp-using-namespace"
                ),
            ]
        );
    }

    #[test]
    fn an_error_among_an_enclosing_namespaces_declarations_leaves_the_lookup_unproven() {
        let page =
            "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page { private Entry entry; }\n";
        let broken = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            // An `Acme.Web.Entry` here would take the name before the directive.
            (
                "Web/Broken.cs",
                "namespace Acme.Web;\npublic class Kept { }\npublic class Entry { public void Lost() { if (x { }\n",
            ),
            ("Web/Page.cs", page),
        ]);
        assert_eq!(
            edges(&broken),
            vec![edge(
                "Web/Page.cs",
                "Ledger/Entry.cs",
                1,
                false,
                "csharp-using-namespace"
            )]
        );
        // An error inside a method body hides no declaration.
        let body = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            (
                "Web/Other.cs",
                "namespace Acme.Web;\npublic class Other\n{\n    void M() { var x = 1 +; }\n}\n",
            ),
            ("Web/Page.cs", page),
        ]);
        assert_eq!(
            edges(&body),
            vec![edge(
                "Web/Page.cs",
                "Ledger/Entry.cs",
                1,
                true,
                "csharp-using-namespace"
            )]
        );
    }

    #[test]
    fn what_a_build_configuration_decides_proves_no_edge() {
        let page = |body: &str| {
            format!("using Acme.Ledger;\nnamespace Acme.Web;\npublic class Page\n{{\n{body}}}\n")
        };
        // A type the file declares only under `#if`, as a polyfill for older frameworks is.
        let polyfill = index(&[
            ("App.csproj", SDK),
            (
                "Ledger/Entry.cs",
                "#if !NET7_0_OR_GREATER\nnamespace Acme.Ledger\n{\n    internal sealed class Entry { }\n}\n#endif\n",
            ),
            ("Web/Page.cs", &page("    private Entry entry;\n")),
        ]);
        assert_eq!(
            edges(&polyfill),
            vec![edge(
                "Web/Page.cs",
                "Ledger/Entry.cs",
                1,
                false,
                "csharp-using-namespace"
            )]
        );
        // A name written only under `#if`; once written outside one too, the edge holds.
        let only_conditional = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            (
                "Web/Page.cs",
                &page("#if DEBUG\n    private Entry entry;\n#endif\n"),
            ),
        ]);
        assert_eq!(
            edges(&only_conditional),
            vec![edge(
                "Web/Page.cs",
                "Ledger/Entry.cs",
                1,
                false,
                "csharp-using-namespace"
            )]
        );
        let also_unconditional = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            (
                "Web/Page.cs",
                &page("#if DEBUG\n    private Entry entry;\n#endif\n    private Entry other;\n"),
            ),
        ]);
        assert_eq!(
            edges(&also_unconditional),
            vec![edge(
                "Web/Page.cs",
                "Ledger/Entry.cs",
                1,
                true,
                "csharp-using-namespace"
            )]
        );
        // A directive under `#if`.
        let directive = index(&[
            ("App.csproj", SDK),
            ("Ledger/Entry.cs", ENTRY),
            (
                "Web/Page.cs",
                "#if NET6_0\nusing Acme.Ledger;\n#endif\nnamespace Acme.Web;\npublic class Page { private Entry entry; }\n",
            ),
        ]);
        assert_eq!(
            edges(&directive),
            vec![edge(
                "Web/Page.cs",
                "Ledger/Entry.cs",
                2,
                false,
                "csharp-using-namespace"
            )]
        );
        // A `ProjectReference` and a `Compile Remove` a `Condition` guards.
        let web = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup Condition=\"'$(TargetFramework)' == 'net48'\">\n    <ProjectReference Include=\"../Ledger/Ledger.csproj\" />\n    <Compile Remove=\"Old/**\" />\n  </ItemGroup>\n</Project>\n";
        let msbuild = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", ENTRY),
            ("src/Web/Web.csproj", web),
            ("src/Web/Page.cs", &page("    private Entry entry;\n")),
            (
                "src/Other/Other.csproj",
                &sdk_project(&["../Ledger/Ledger.csproj"]),
            ),
            ("src/Other/Page.cs", &page("    private Entry entry;\n")),
        ]);
        assert_eq!(
            edges(&msbuild),
            vec![
                edge(
                    "src/Other/Page.cs",
                    "src/Ledger/Entry.cs",
                    1,
                    true,
                    "csharp-using-namespace"
                ),
                edge(
                    "src/Web/Page.cs",
                    "src/Ledger/Entry.cs",
                    1,
                    false,
                    "csharp-using-namespace"
                ),
            ]
        );
        let removed = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", ENTRY),
            ("src/Web/Web.csproj", web),
            ("src/Web/Old/Page.cs", &page("    private Entry entry;\n")),
        ]);
        assert!(
            edges(&removed).iter().all(|(_, _, _, proven, _)| !proven),
            "{:?}",
            edges(&removed)
        );
    }

    #[test]
    fn compile_items_decide_which_project_compiles_a_file() {
        let snapshot = index(&[
            ("src/Ledger/Ledger.csproj", SDK),
            ("src/Ledger/Entry.cs", ENTRY),
            (
                "src/Web/Web.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <ProjectReference Include=\"../Ledger/Ledger.csproj\" />\n    <Compile Remove=\"Old/**\" />\n  </ItemGroup>\n</Project>\n",
            ),
            (
                "src/Web/Old/Page.cs",
                "using Acme.Ledger;\nnamespace Acme.Web;\npublic class Old { private Entry entry; }\n",
            ),
            (
                "legacy/Legacy.csproj",
                "<Project ToolsVersion=\"15.0\" xmlns=\"http://schemas.microsoft.com/developer/msbuild/2003\">\n  <ItemGroup>\n    <Compile Include=\"Listed.cs\" />\n    <ProjectReference Include=\"..\\src\\Ledger\\Ledger.csproj\" />\n  </ItemGroup>\n</Project>\n",
            ),
            (
                "legacy/Listed.cs",
                "using Acme.Ledger;\nnamespace Acme.Legacy;\npublic class Listed { private Entry entry; }\n",
            ),
            (
                "legacy/Unlisted.cs",
                "using Acme.Ledger;\nnamespace Acme.Legacy;\npublic class Unlisted { private Entry entry; }\n",
            ),
        ]);
        assert_eq!(
            edges(&snapshot),
            vec![
                edge(
                    "legacy/Listed.cs",
                    "src/Ledger/Entry.cs",
                    1,
                    true,
                    "csharp-using-namespace"
                ),
                // A file no project compiles sees every type, unproven.
                edge(
                    "legacy/Unlisted.cs",
                    "src/Ledger/Entry.cs",
                    1,
                    false,
                    "csharp-using-namespace"
                ),
                edge(
                    "src/Web/Old/Page.cs",
                    "src/Ledger/Entry.cs",
                    1,
                    false,
                    "csharp-using-namespace"
                ),
            ]
        );
    }

    #[test]
    fn a_partial_types_parts_are_grouped_by_the_project_compiling_them() {
        // The ledger project compiles a part that sits in the reports project's directory, which
        // removes it; the reports project's own `Book` is another type.
        let snapshot = index(&[
            (
                "src/Ledger/Ledger.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <Compile Include=\"../Reports/Shared/Book.Audit.cs\" />\n  </ItemGroup>\n</Project>\n",
            ),
            (
                "src/Ledger/Book.cs",
                "namespace Acme.Ledger;\n\npublic partial class Book { }\n",
            ),
            (
                "src/Reports/Reports.csproj",
                "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <ItemGroup>\n    <Compile Remove=\"Shared/**\" />\n  </ItemGroup>\n</Project>\n",
            ),
            (
                "src/Reports/Shared/Book.Audit.cs",
                "namespace Acme.Ledger;\n\npartial class Book { }\n",
            ),
            (
                "src/Reports/Book.cs",
                "namespace Acme.Ledger;\n\npartial class Book { }\n",
            ),
            (
                "src/Reports/Book.Totals.cs",
                "namespace Acme.Ledger;\n\npartial class Book { }\n",
            ),
        ]);
        let mut parts = snapshot
            .symbols
            .iter()
            .filter(|symbol| symbol.qualified_name == "Acme::Ledger::Book")
            .map(|symbol| (path_of(&snapshot, &symbol.file_id), symbol.visibility))
            .collect::<Vec<_>>();
        parts.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            parts,
            vec![
                ("src/Ledger/Book.cs".to_string(), Visibility::Public),
                ("src/Reports/Book.Totals.cs".to_string(), Visibility::Crate),
                ("src/Reports/Book.cs".to_string(), Visibility::Crate),
                // Its directory is the reports project's, but the ledger project compiles it.
                (
                    "src/Reports/Shared/Book.Audit.cs".to_string(),
                    Visibility::Public
                ),
            ]
        );
    }

    #[test]
    fn a_name_only_an_outside_namespace_supplies_never_matches_a_repository_symbol() {
        // The repository's `Storage.File` is not imported; `File` here is a package's, from a
        // namespace that shares its root with the repository's own (as `Microsoft.Extensions.*`
        // does where a repository declares extension classes there), which the registry's path
        // reading would take for the repository's. It must not match it or its members by name.
        let snapshot = index(&[
            ("App.csproj", SDK),
            (
                "Storage/File.cs",
                "namespace Acme.Storage;\npublic static class File { public static string ReadAllText(string path) => path; }\n",
            ),
            (
                "Web/Page.cs",
                "using Acme.Vendor.IO;\nnamespace Acme.Web;\npublic class Page { string M(File f) => File.ReadAllText(\"a\"); }\n",
            ),
        ]);
        let storage = snapshot
            .symbols
            .iter()
            .filter(|symbol| symbol.qualified_name.starts_with("Acme::Storage"))
            .map(|symbol| symbol.id.clone())
            .collect::<Vec<_>>();
        let page = snapshot
            .files
            .iter()
            .find(|file| file.path.ends_with("Page.cs"))
            .expect("page")
            .id
            .clone();
        let leaks = snapshot
            .analysis_facts
            .iter()
            .filter(|fact| {
                fact.file_id == page && fact.source.starts_with("open-kioku-symbol-registry/")
            })
            .filter(|fact| {
                fact.target_symbol_id
                    .as_ref()
                    .is_some_and(|target| storage.contains(target))
                    || fact.target.contains("Storage")
            })
            .map(|fact| (fact.source.to_string(), fact.target.clone()))
            .collect::<Vec<_>>();
        assert!(leaks.is_empty(), "{leaks:?}");
    }
}
